# 验收标准

"做完了"由可执行的检查定义，不靠口头确认。全部命令都在离线环境可跑（除 `cargo`/`pytest` 首次装依赖）。

## 1. 单元与集成测试

```bash
# 后端：26 单测（闸门/slug/版本/智能体 manifest/发布试运行）+ 32 集成（隔离、不可变、下架语义、审计、列表 total/sort、拼音 slug、发布试运行）
cd backend && cargo test

# 插件：111 测试（扫描闸门、provider 契约、错误透传、路由 sort/total 透传、智能体快照/安装/回滚、概览统计、发布试运行、不联网）
cd plugin && python -m pytest
```

通过标准：全绿、0 warning。**这两组不需要 QwenPaw 本体，也不需要网络。**

## 2. 后端自检（自带临时库）

```bash
python3 deploy/scripts/e2e.py --binary bin/offline-market-backend
```

覆盖 20 项：启动/health、限额端点、发布、`install_url` 可达性、版本不可变 409、
路径穿越 400、缺 frontmatter 400、跨租户可见、bundle 下载、私有 403 与不出现在列表、
管理员登录、下架后双方 410、lookup 仍可查（供已安装页标记）、属主不能自恢复 409、
管理员恢复、审计含 publish/takedown、**重启后数据保留**。

## 3. 宿主集成（真实 QwenPaw 2.2.2）

```bash
python3 deploy/scripts/e2e.py --skip-a \
  --app-url http://127.0.0.1:8123 --market-url http://market.internal:8080 \
  --tenant <租户> --admin-password "$ADMIN_PASSWORD"
```

覆盖 6 项：provider 已加载、provider 可用、宿主搜索命中、宿主安装任务完成且
`installed_from: offline-market`、下架后搜索消失、下架后重装失败。

前置：该 app 的 runtime 已能解析市场地址（默认 `http://localhost:8080`）且扫描模式不是 `off`，插件已 `plugin install`。

## 3b. Docker runtime 智能体端到端（phase C）

```bash
# runtime 由 QwenPaw Hub 以 Docker 管理；需要宿主机 docker 权限
python3 deploy/scripts/e2e.py --skip-a --skip-b \
  --runtime-container <runtime 容器名> \
  --market-url http://172.18.0.1:8080 --tenant <租户id>
```

覆盖：发布临时智能体 → 容器内调插件安装 → 宿主中存在该 agent → `SOUL.md` 写入 →
捆绑技能已启用 → 本机账本已登记 → 卸载 → 宿主 404 → 撤回市场记录 → e2e 工作区清理。

> Docker Hub 上 phase B 直接打 runtime 的发布端口会遇到认证/边界 token；智能体验收推荐
> 直接使用 phase C（容器内调用），它复用插件自己的 loopback + runtime token 路径。

前置：runtime 容器内能访问市场地址（OPS §2.6）；插件已装；容器内 `127.0.0.1:${QWENPAW_PORT}`
可访问且带有 `QWENPAW_RUNTIME_INTERNAL_TOKEN`。脚本会留下一条已撤回的 `e2e-agent-*`
市场记录（后端没有删除接口），不会出现在列表里。

## 4. 浏览器端到端（UI）

```bash
# 需要 backend + qwenpaw app 已启动，且本机有 playwright/chromium
python3 spikes/m3_verify.py
```

覆盖 11 项：侧边栏入口（名称/位置）、技能四页签、UI 点安装到完成、**目录抽屉/排序/防抖搜索/列表视图**、
**列表接口 total+sort+download_count+updated_at**、UI 上传 zip 发布、我的发布、已安装显示"正常"、
管理员下架后显示"已下架"、智能体页签渲染、**0 console & page error**。

> 该脚本需要真宿主（backend + qwenpaw app + chromium），本轮 UI 升级在无宿主环境下改用下面的无头渲染冒烟
> 作为等价证据；部署环境仍须跑一遍 `m3_verify.py`。

### 4b. 无头 UI 冒烟（无需 QwenPaw、无需浏览器）

```bash
npm install --prefix /tmp/harness react@18 react-dom@18 antd@5 jsdom   # 一次性
node spikes/ui_smoke.mjs
```

用真实 `market.js` + 伪 `window.QwenPaw.host` + 伪插件 API 渲染整个市场页，覆盖 31 项：
卡片视图与结果计数、`安装/更新` 徽标（1.0.0 → 1.2.0）、详情抽屉、触底加载第 2 页、
300ms 防抖搜索、排序下拉改 `sort=downloads`、列表视图、502 错误态、智能体安装弹窗、
安装后已安装页自动刷新、暗色主题重渲染，以及**正常流程 0 console error**。输出 `ALL CHECKS PASSED` 才算通过。

## 5. 部署自检（只读）

```bash
python3 deploy/scripts/preflight.py \
  --market-url http://market.internal:8080 --admin-password "$ADMIN_PASSWORD" \
  --runtimes-dir <hub runtimes> --app-url http://127.0.0.1:8123 \
  --qwenpaw "$(command -v qwenpaw)"
```

任一 FAIL 即部署不合格；`--admin-password` 缺失时该项降级为 WARN。
加 `--runtime-container <容器名>` 时，还会在容器内检查市场可达性，以及
插件 `/api/offline-market/config` → `/host-health`（宿主 API）自调用。

## 6. 明确不在验收范围内

- Windows 上的后端运行（PLAN 定的是 Linux x86_64；Windows 只用于开发验证）。
- 多节点/水平扩展（SQLite 单机）。
- 人工审核流程、智能体协同审核（PLAN 明示后续工作）。
- 智能体市场的 Docker Hub 真实环境执行（phase C 已提供脚本；CI 无 docker 权限，
  需部署方在宿主机执行）。
- 远程吊销已安装副本（PLAN 明示不做）。
- 二进制附件经 hub 安装（市场可发布/存储/下载 `docx`/`pptx`，但 QwenPaw 2.2.2 的 hub 安装只接受文本；安装时会明确报 `binary_bundle`）。

## 7. 回归清单（改代码后至少跑这些）

| 改动位置 | 必跑 |
| --- | --- |
| `backend/src/limits.rs` | `cargo test`（闸门边界都在这） |
| `backend/src/api.rs` | `cargo test` + `e2e.py` phase A |
| `backend/src/agents.rs` | `cargo test`（智能体结构闸门与语义）+ `cargo clippy --all-targets -- -D warnings` |
| `plugin/offline_market/*` | `pytest` + `e2e.py` phase B |
| `plugin/offline_market/{host,agents}.py` | `pytest`（快照/安装顺序/回滚/账本；不联网） |
| `plugin/offline_market/routes.py` | `pytest` + preflight 的 `/host-health`（容器内） |
| `plugin/frontend/market.js` | `node --check` + `node spikes/ui_smoke.mjs` + `m3_verify.py` |
| `deploy/*` | `preflight.py` + `e2e.py` |
| `deploy/scripts/{e2e,preflight,runtime_probe}.py` | `py_compile` + Docker 宿主机跑 phase C |
