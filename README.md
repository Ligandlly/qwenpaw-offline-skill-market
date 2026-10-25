# QwenPaw 离线技能市场

[![backend](https://github.com/Ligandlly/qwenpaw-offline-skill-market/actions/workflows/backend.yml/badge.svg)](https://github.com/Ligandlly/qwenpaw-offline-skill-market/actions/workflows/backend.yml)
[![backend-image](https://github.com/Ligandlly/qwenpaw-offline-skill-market/actions/workflows/backend-image.yml/badge.svg)](https://github.com/Ligandlly/qwenpaw-offline-skill-market/actions/workflows/backend-image.yml)
[![plugin](https://github.com/Ligandlly/qwenpaw-offline-skill-market/actions/workflows/plugin.yml/badge.svg)](https://github.com/Ligandlly/qwenpaw-offline-skill-market/actions/workflows/plugin.yml)

给 **QwenPaw 2.2.2** 加一个可完全离线部署的内部技能市场：租户在 Console 里浏览/安装技能，也可以发布自己的技能；Hub 管理员在独立后台下架违规内容。

```
租户浏览器 ──▶ 租户 runtime（QwenPaw 2.2.2 + 本插件）──▶ 市场后端（Rust + SQLite）
                     │                                      ▲
                     └──── X-Tenant-Id = QWENPAW_TENANT_ID ──┘   管理员浏览器 ──▶ /admin/api
```

核心取舍：**安装始终由 QwenPaw 宿主完成**（bundle 校验、`skill_scanner` 扫描、落盘、可取消任务）。本仓库只做"存、发、授权、审计"，不实现第二条安装管线。

## 仓库结构

| 目录 | 内容 |
| --- | --- |
| `backend/` | 市场后端：Rust + axum + rusqlite（SQLite bundled），21 个测试 |
| `plugin/` | QwenPaw 2.2.2 插件：市场 provider + 自用 HTTP 路由 + 侧边栏页面，57 个测试 |
| `deploy/` | docker-compose / systemd / 离线打包 / preflight / 端到端验收脚本 |
| `docs/` | `OPS.md` 部署运维手册、`ACCEPTANCE.md` 验收标准 |
| `spikes/` | M0 阶段对 2.2.2 的实证结论（11 条），以及浏览器验证脚本 |
| `PLAN.md` | 设计定案、里程碑、风险表（含明确接受的取舍） |

## 快速开始

```bash
# 后端
cd backend && cargo test && cargo run          # 首次启动会打印随机管理员口令

# 插件（需要 QwenPaw 2.2.2）
cd plugin && python -m pytest
qwenpaw plugin install ./plugin
```

一键离线包：

```bash
bash deploy/scripts/build-offline-bundle.sh     # -> dist/offline-market-*.tar.gz
```

## 部署前必须知道的两件事

1. **每个租户 runtime 的技能扫描不能是 `off`**
   `block`（拦截）和 `warn`（扫描、告警、照装）插件都接受；`off` 什么都不扫，插件会拒绝服务。
   想拦住恶意包就得多花一步：`warn` 下实测一个含 `exec(input())` 的包被扫出 CRITICAL、记日志、然后**照常安装并启用**，只有 `block` 才会让安装失败且磁盘无残留。
   `QWENPAW_SKILL_SCAN_MODE` 无法从 Hub 注入（`system_environment()` 只继承 `LANG/LC_*`，credentials 又被 `QWENPAW_` 前缀过滤），必须落到 runtime 的 `config.json`：
   ```bash
   python3 deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes> --dry-run
   ```
   插件会自查该模式：`off`（或读不到 config）就拒绝服务（市场页顶部给出修复指引），而不是静默分发。

2. **市场地址走 Hub credentials 注入，且必须与后端一致**
   `PUT /api/hub/credentials` 写 `OFFLINE_MARKET_URL`（`scope` 用 `tenant` 或 `runtime:<id>`，
   不是 `runtime`），租户下次启动 runtime 时生效。它必须与后端 `.env` 里的
   `MARKET_PUBLIC_URL` **逐字符相同**（内网 IP / 内网 DNS、端口都要一致），否则安装会在
   宿主侧被拒：`URL does not belong to Hub 'offline-market'`。换机器/换地址的步骤见
   [`docs/OPS.md`](docs/OPS.md) §2.4。

   另见 [`docs/OPS.md`](docs/OPS.md) §2.5：**runtime 跑在容器里时，地址必须"从容器内可达"**。
   宿主机 curl 得通不算数——插件是在 runtime 进程里发请求的。Docker Desktop（Windows）上
   宿主机的局域网 IP 往往不通（发布端口只挂在 `127.0.0.1` 与网桥地址上），要用
   `host.docker.internal:8080` 或把后端容器接进同一 docker 网络用容器名。

详见 [`docs/OPS.md`](docs/OPS.md)。

## 验收

```bash
python3 deploy/scripts/e2e.py --binary dist/offline-market-*/bin/offline-market-backend   # 后端 20 项
python3 deploy/scripts/e2e.py --skip-a --app-url http://127.0.0.1:8123 \
        --market-url http://market.internal:8080 --tenant <租户>                            # 宿主 6 项
python3 spikes/m3_verify.py                                                               # 浏览器 8 项
python3 deploy/scripts/preflight.py --market-url ... --runtimes-dir ... --app-url ...      # 部署自检
```

标准见 [`docs/ACCEPTANCE.md`](docs/ACCEPTANCE.md)。

## 已知边界（设计取舍，不是缺陷）

- 无人工审核、不限文件类型：市场可分发任意内容，唯一自动防线是安装侧扫描（`warn` 只告警不拦，`off` 会被插件拒服务）。
- 无远程吊销：下架只停止分发，已安装副本需人工清理。
- `X-Tenant-Id` 是内网信任而非认证；审计只到租户粒度（前提是一租户一人）。
- 限额只认环境变量，调整需重启后端。

## CI 与发版

| 工作流 | 触发 | 内容 |
| --- | --- | --- |
| `backend.yml` | 改动 `backend/**`、推 tag `v*`、手动 | `cargo fmt --check` → `clippy -D warnings` → `cargo test --all-targets` → release 构建 → 上传 artifact；**推 `v*` tag 时自动建 Release 并附二进制 tarball + sha256** |
| `backend-image.yml` | 改动 `backend/**`、推 tag `v*`、手动 | 跑一遍测试 → 构建后端镜像（linux/amd64）→ **推 ghcr.io** → 导出 `docker save` tarball 上传 artifact（tag 时也挂到 Release）；PR 只构建不推送 |
| `plugin.yml` | 改动 `plugin/**`、手动 | `pytest`（57 项，不装 QwenPaw、不联网）+ `compileall` |

发一个版本：

```bash
git tag v0.1.0 && git push origin v0.1.0
# → Release 里会多出 offline-market-backend-<ver>-linux-x86_64.tar.gz(.sha256)
#   以及 offline-market-image-<ver>-linux-amd64.tar(.sha256)
```

镜像（ghcr.io/ligandlly/qwenpaw-offline-skill-market）：

```bash
docker pull ghcr.io/ligandlly/qwenpaw-offline-skill-market:latest   # main 分支
docker pull ghcr.io/ligandlly/qwenpaw-offline-skill-market:v0.1.0  # tag
# 内网机器：在有网的机器上 docker save，拷过去 docker load
```

CI 不安装 QwenPaw（40 MB wheel + 84 个依赖）；插件测试用 `qwenpaw.market` 的桩类型，
本地有真 QwenPaw 时自动用真的，`OFFLINE_MARKET_STUB_MARKET=1` 可强制走桩。
