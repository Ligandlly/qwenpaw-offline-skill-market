# M0 Spikes — 实测结果

目标版本：**QwenPaw v2.2.2**（PyPI `qwenpaw==2.2.2`，`requires_python >=3.11,<3.14`）
实测环境：Windows，Python 3.12.12，`2026-10-24`

## 状态

| Spike | 问题 | 结果 |
| --- | --- | --- |
| S1 | `search/fetch` 是否在租户运行时进程内，`QWENPAW_TENANT_ID` 读不读得到 | **通过（实测）** |
| S2 | 能否加侧边栏入口、调宿主安装接口 | **代码级通过**，浏览器实测待做 |
| S3 | 插件能否被 2.2.2 真正安装并加载 | **通过（实测）** |
| S4 | 后端挂掉时用户看到什么 | **通过（实测）** |
| S5 | 宿主扫描能不能拦住 CRITICAL 包 | **默认拦不住；`block` 模式能拦住** |

## 证据

### S1 `s1_search_env.py` → `all_ok: true`

```
C_tenant_visible: pid_host=2116 pid_provider=2116 same_process=true
                  tenant_seen="tenant-spike-42"
D_duplicate_key : ValueError: Duplicate hub key: 'offline-market'
E_busy_guard    : MarketProviderBusyError: Hub plugin 'offline-market-spike' is in use; please retry later
C2_fetch_contract: matches_url=true files=["SKILL.md"]
```

→ 身份方案的地基成立：市场路径没有 context 参数，但 provider 与宿主**同进程**，`QWENPAW_TENANT_ID` 可读。

### S3 真实加载

```
$ qwenpaw plugin install ./hub_plugin
Plugin validation successful
✅ Plugin 'Offline Skill Market (spike)' installed successfully!

$ curl /api/market/providers
[..., {"key":"offline-market","label":"Offline Market","available":true,...}, ...]
$ curl -X POST /api/market/search -d '{"query":"hello","provider_pages":{"offline-market":1}}'
{"results":[{"source":"offline-market","source_label":"Offline Market","slug":"spike-hello",...}]}
```

### 完整安装链路（最关键的一步）

```
POST /api/skills/hub/install/start
     {"provider_key":"offline-market","bundle_url":".../skills/spike-hello/1.0.0",...}
→ {"status":"completed","result":{"installed":true,"enabled":true,
                                 "installed_from":"offline-market"}}
落盘：~/.qwenpaw/workspaces/default/skills/spike-hello/SKILL.md
```

→ **宿主确实保留了校验/扫描/安装**，我们只提供 `fetch_bundle`。自建安装管线一行都不用写。

### S5 扫描这件事是个陷阱

同一插件，`fetch_bundle` 返回的包里加 `scripts/run.py` = `exec(input())`：

| 扫描模式 | 结果 |
| --- | --- |
| 默认（`~/.qwenpaw/config.json` 里 `security.skill_scanner.mode = "warn"`） | 扫描命中 **CRITICAL**，日志 `blocking is disabled – proceeding anyway`，**安装成功并 enable**，文件在盘上 |
| `QWENPAW_SKILL_SCAN_MODE=block` | `status:"failed"`, `type:"security_scan_failed"`, `[CRITICAL] Dangerous code execution functions... (scripts\run.py:1)`，**磁盘无残留** |

→ 「靠安装侧扫描兜底」**默认不成立**。必须逐租户 runtime 强制 `QWENPAW_SKILL_SCAN_MODE=block`。
→ 我们的 provider 应当在 `available()` 里自查该模式，不是 `block` 就 `(False, reason)` 拒绝服务 —— 这样市场宁可不可用，也不分发未拦截的代码。
→ 现成脚本：`s5_scan_mode_gate.py`（`--json` 可给后端/CI 用），可用作部署 preflight。

## 环境

```bash
cd spikes
uv venv --python 3.12 .venv                                         # 完整依赖（要起 app）
uv venv --python 3.12 .venv-lite                                    # 只跑 S1/S4 的单测
uv pip install --python .venv/Scripts/python.exe \
   --index-url https://mirrors.aliyun.com/pypi/simple "qwenpaw==2.2.2"
```

> 直连 PyPI 下载 38MB wheel + 84 个依赖在本机极慢（>50 分钟且会卡死）；阿里云镜像 + `UV_HTTP_TIMEOUT=180` 几分钟完成。

## 跑

```bash
# S1 / S4：不需要起服务
.venv-lite/Scripts/python.exe s1_search_env.py
.venv-lite/Scripts/python.exe s4_available_reason.py

# S5：扫描闸门 preflight（warn → exit 1；block → exit 0）
.venv/Scripts/python.exe s5_scan_mode_gate.py

# S3：真实安装 + 真实 HTTP（两终端）
.venv/Scripts/qwenpaw.exe plugin install ./hub_plugin --force
QWENPAW_SKILL_SCAN_MODE=block .venv/Scripts/qwenpaw.exe app --port 8123
curl -s http://127.0.0.1:8123/api/market/providers
```

## 目录

```
spikes/
  findings-2.2.2.md      ← 9 条代码级结论 + PLAN 修订项（先读）
  s2_frontend_ui.md      ← 侧边栏入口与安装调用的具体写法
  hub_plugin/            ← 最小 hub 插件（plugin.json + backend.py，含 evil 开关）
  s1_search_env.py       ← S1 断言
  s4_available_reason.py ← S4 断言
  s5_scan_mode_gate.py   ← S5 部署闸门
```

## 已知坑（都是踩过的）

- `available()` 在每次 `/api/market/providers` 和每次搜索时都会被调用，**必须非阻塞**，否则拖死市场初始化。
- `provider.key` 重复 → `ValueError`；卸载时有在途请求 → `MarketProviderBusyError`（用户侧表现为"稍后重试"）。
- `search()` 的额外 kwargs 被 `_supported_kwargs` 按签名静默丢弃 —— 别声明 `lang/category`。
- `MarketResult.source` 必须等于 `provider.key`，否则前端拿不到 label。
- 装好的插件在 `~/.qwenpaw/plugins/<id>/` 是**副本**；改 repo 里的源码必须 `plugin install --force` 才生效。
