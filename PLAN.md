# 离线 Skill 市场：两个项目的定案

目标 QwenPaw 版本：**2.2.2**（`v2.2.2` / `v2.2.2.post1`，2026-10-15 / 10-22）

部署前提：**一个租户 = 一个人使用**。

## 0. 已验证的宿主事实（不是猜测）

| 事实 | 出处 |
| --- | --- |
| 官方插件市场目录来自网络（platform.agentscope.io 代理），本地无法 seed | `e2e/mocks/plugin_market.py` 注释原文 |
| 市场源是公开 SPI：插件声明 `markets` capability，注册 `MarketSpec(key,label,search,fetch,matches_url,available,supports_browse,priority)` | `src/qwenpaw/plugins/markets.py` |
| 宿主保留 bundle 校验、skill_scanner 扫描、安装：*"The host retains bundle validation, scanning and installation"* | `src/qwenpaw/market/providers/base.py` |
| 安装入口 `POST /api/skills/hub/install/start`，入参 `{provider_key,bundle_url,version,enable,target_name}`，内部 `install_skill_from_hub`，硬拦 `SkillScanError` | `src/qwenpaw/app/routers/skills.py:1064` |
| bundle 契约 `{"name": "...", "files": {"SKILL.md": "..."}}`，无签名/无权限清单/无哈希 | `src/qwenpaw/plugins/markets.py` |
| 插件清单 `plugin.json` 含 `version` 与 `sdk.{version,capabilities}`；宿主只接受同 major 且不高于自身 | `website/public/docs/plugins.en.md` |
| Hub 多租户：tenants 表 + runtime registry + 每租户独立 runtime，注入 `QWENPAW_TENANT_ID` / `QWENPAW_RUNTIME_ID` / `QWENPAW_RUNTIME_INTERNAL_TOKEN` | `src/qwenpaw/hub/registry.py`, `hub/local_provisioner.py:373` |
| `ToolContext` 只有 `plugin_id/workspace_id/session_id`，**无 tenant** | `src/qwenpaw/plugins/tools.py` |
| 技能存在自动更新开关 `SkillAutomationRequest.auto_update` | `console/src/api/modules/skill.ts` |

## 1. 定案

| 议题 | 决定 |
| --- | --- |
| 下载链路 | 插件注册 `MarketSpec`，宿主原生 Skills 页 + 自写侧边栏页**双入口** |
| 安装实现 | 前端只调宿主 `/api/skills/hub/install/start`，扫描/校验/可取消全归宿主，自建管线一步不写 |
| **扫描闸门（实测新增，最重要）** | 宿主扫描器**默认 `warn`：CRITICAL 也放行并启用**。市场必须强制 `QWENPAW_SKILL_SCAN_MODE=block`；插件 `available()` 检测不到 `block` 就返回 `(False, reason)` 拒绝服务（fail-closed） |
| 发布入口 | 租户 console 侧边栏「技能市场」页，插件自带 HTTP 路由代发 |
| 发布链路的唯一形态 | 浏览器 → 插件路由 → 后端。**不能**让浏览器直连后端：租户身份只存在于 runtime 环境变量，交给浏览器等于交给用户随便改 |
| 管理后台 | 后端自带独立 Web 后台，管理员独立登录；首个账号来自 `ADMIN_USER`/`ADMIN_PASSWORD` 环境变量 |
| 租户身份 | 信任 `QWENPAW_TENANT_ID` + 内网隔离；后端首次见到新 tenant 自动 upsert |
| 审核 | 本阶段不做；预留 `status` 字段，后续接智能体 + 人工协同审核 |
| 文件类型准入 | **不限制任何类型**，市场侧不拦、不提示、不标记（但安装侧必须开 `block`，见上） |
| 实际闸门 | 只剩三条：单包 2 MB / 文件 ≤ 100 / 单文件 ≤ 256 KB；路径规则（拒 `..`、绝对路径、符号链接）；必须含 `SKILL.md` |
| 限额配置 | **仅环境变量**，改限额 = 重启后端容器 |
| 下载鉴权 | 服务端按 tenant 校验：公开包任意租户可下，私有包仅属主租户，否则 403。不用 capability URL |
| 撤回权限 | 租户内任何人可撤本租户的包（一租户一人 → 审计到租户即到人，省掉发布人字段） |
| 撤回可逆性 | 租户自主撤回可反向恢复（un-withdraw）；管理员下架不可自恢复；版本号始终不可变 |
| 管理员下架 | 整包下架，禁止再分发：搜索消失、bundle 端点 410、latest 不再指向 |
| 私有语义 | 保留 `visibility = public \| tenant`；`tenant` = 仅本租户可见 |
| 隔离失败姿态 | **fail-closed**：拿不到可信 tenant 就不返回任何私有包，bundle 一律 403 |
| 已装用户告知 | 不做下发链路；市场页"已安装"列表里把该条标为「已下架」 |
| 版本 | 不可变版本 + latest 指针，支持指定版本与回滚 |
| 后端栈 | Rust + SQLite + 本地磁盘，Linux x86_64，容器或裸二进制，规模几十租户 / 百量级 skill |
| 验收 | 各 repo 单测 + 跨 repo 离线 e2e（docker-compose 起后端，插件层用真实 `qwenpaw` 起 app 打真实 HTTP） |

## 2. 两个项目的边界

### 插件（Python，`qwenpaw.plugins` SDK 1.0.x）

- `plugin.json`：`sdk.version = "1.0.0"`，`capabilities` 按实际裁剪（`markets` 等）
- `MarketSpec(key="offline-market")`
  - `matches_url`：`http://<market>/skills/{slug}/{version}` 前缀匹配
  - `search()`：tenant 从环境变量取，转发后端；读不到 tenant 时只返回公开包（fail-closed）
  - `fetch()`：`GET /api/v1/skills/{slug}/{version}/bundle` → `SkillBundle`
  - `available()`：启动探测后端健康，失败给人话原因（宿主原样显示）
- 自带 HTTP 路由（挂 `/api` 下）：`POST /market/publish`、`GET /market/mine`、`POST /market/withdraw`、`POST /market/restore`
- 发布前预检（早失败，非安全边界）：体积、文件数、路径规则
- 配置项：后端地址（`config_fields`，允许环境变量覆盖）
- 前端侧边栏页：浏览 / 搜索 / 详情 / 发布 / 我的 / 已装（含「已下架」标记）；安装按钮调宿主安装端点，展示进度与取消

### 后端（Rust，axum + SQLite + 本地磁盘）

表：`tenants`、`skills`、`skill_versions`（不可变，SHA-256 + 文件清单）、`latest` 指针、`takedowns`、`audit_log`、`admin_users`

- 认证两套：租户侧 `X-Tenant-Id`（内网可信）；管理员侧 session cookie + Argon2，`ADMIN_*` 环境变量引导首个账号
- 存储：bundle 以 JSON 存在 `skill_versions.bundle_json`（**实现偏差**：原定 `data/packages/{slug}/{version}/`，改为单库存储以避免文件与 DB 不一致，2 MiB 上限下可控）
- 闸门在后端强制执行（插件预检只是 UX）
- 管理后台：列表 / 详情 / 下架 / 审计日志（API 已就绪，UI 在 M3）

## 3. M0 spike 结果（已在本机对 qwenpaw 2.2.2 实测，2026-10-24）

| # | 问题 | 结果 |
| --- | --- | --- |
| S1 | `search/fetch` 是否在租户运行时进程内、`QWENPAW_TENANT_ID` 是否可见 | **通过**：同 PID，`tenant_seen=tenant-spike-42`；重复 key → `ValueError`；在途请求时卸载 → `MarketProviderBusyError` |
| S2 | 侧边栏入口 + 调宿主安装接口 | **代码级通过**：`window.QwenPaw.menu/route.add`、`hostFetch`、`provider_key` 安装均在 2.2.2 存在；浏览器实测待做 |
| S3 | 插件能否被 2.2.2 真正安装并加载 | **通过**：`plugin install` 校验成功；`GET /api/market/providers` 出现 `offline-market`；`POST /api/market/search` 返回我们的结果 |
| S4 | 后端挂掉时用户看到什么 | **通过**：`reason` 原样出现在 `/api/market/providers`，且会变成 `MarketSearchError` 文案 |
| S5 | 宿主扫描能否拦住 CRITICAL | **失败 → 已定位修法**：默认 `warn`，`exec(input())` 照样安装并启用；`QWENPAW_SKILL_SCAN_MODE=block` 后安装 `failed: security_scan_failed` 且磁盘无残留 |

完整安装链路已跑通：`POST /api/skills/hub/install/start` → 我们的 `fetch_bundle` → 宿主校验/扫描/落盘/启用，`installed_from: "offline-market"`。

## 3b. 原计划的四个 spike（已被上面替代，保留作为回归项）

1. **`MarketSpec.search/fetch` 是否在租户 runtime 进程内被调用** —— 决定 `QWENPAW_TENANT_ID` 读不读得到。读不到则原生入口只能看公开包（已按 fail-closed 设计），私有包仅走自写页。
2. **前端 SDK 能否注册侧边栏菜单 + 调用宿主 `/api/skills/hub/install/start`**（`console/src/plugins/hostSdk/`）。不能则退化为原生 Skills 页单入口，发布功能改走 CLI。
3. `python -m qwenpaw.plugins.check --runtime` 跑通 hello 模板，确认 2.2.2 接受 `markets` capability。
4. 后端不可用时，`available()` 的 reason 在宿主原生 Skills 页的呈现效果。

## 4. 里程碑

- **M0** 四个 spike 全绿（任一红则回方案层）
- **M1 后端** —— **已完成**（`backend/`，Rust + axum + rusqlite，20 个测试全绿）
  - 已交付：schema、发布/列表/下载/状态、闸门、管理员登录、下架/撤回/恢复、审计、`/api/v1/limits`
  - 额外发现：宿主`validate_skill_content` 要求 `SKILL.md` frontmatter 有非空 `name`+`description`，缺失时**发布成功但安装失败** → 已前置到后端闸门
  - 已实测完整链路：t-a 发布 → tenant-b 的 QwenPaw 搜到 → 宿主安装成功（`installed_from: offline-market`，文件落地）→ 管理员下架 → 搜索消失 + 重装 `410 Gone`；私有包跨租户 `403`
- **M2 插件** —— **已完成**（`plugin/`，Python + stdlib client，28 个测试全绿）
  - 已交付：`MarketProvider` + `MarketInstallProvider`（search / matches_url / fetch_bundle / available）、`/api/offline-market/*` 路由（发布、我的、撤回、恢复、状态、限额、config）、`OFFLINE_MARKET_URL` 配置
  - 关键设计：`available()` 非阻塞 + 扫描闸门 fail-closed（非 `block` 则拒绝服务并给出修复指引）
  - 已实测：真机安装本插件 → 宿主搜到并安装 tenant-a 的包 → tenant-b 通过插件路由发布私有包 → tenant-a 不可见/403 → 去掉 `block` 后 provider `available:false`
- **M3 前端** —— **已完成并通过浏览器端到端验证**（`plugin/frontend/market.js`，无构建步骤）
  - 侧边栏「技能市场（内网）」，`primary.agentScoped`、紧跟内置 `core.marketplace`
  - 四个页签：市场（搜索/表格/安装+取消）、发布（zip 压缩包上传+限额预检）、我的发布（撤回/恢复）、已安装（市场状态徽标）
  - 验证脚本 `spikes/m3_verify.py`：菜单/四个页签/UI 安装到完成/UI 发布/我的发布/管理员下架后「已安装」显示已下架，**0 console & page error**
  - 新发现（已修）：宿主技能列表 `source` 恒为 `customized`、`installed_from` 只在详情里、安装目录名是展示名而非 slug → 新增 `/api/v1/skills/lookup`
- **M4** 离线 e2e + docker-compose / 裸二进制部署包 + 运维文档（含 `QWENPAW_SKILL_SCAN_MODE=block` 强制项）

## 5. 风险表（已签字接受）

| 风险 | 状态 |
| --- | --- |
| **不限制文件类型 + 不做审核 = 市场可分发 `.so`/`.exe`/加密脚本，且不提示、不标记。** 宿主 `skill_scanner` 扫的是 skill 危险指令语义，对二进制基本无感 | **接受（静默全放）**，但**前提是安装侧强制 `block`**（S5）。否则这条从"有风险"变成"无防线"：实测默认配置下 CRITICAL 包被扫描、告警、然后照样装好并启用。兜底措施：插件 `available()` 未检测到 `block` 就拒绝服务 |
| 无远程吊销 → 下架治不了已落地副本，`auto_update` 拉不到即失败 | 接受。运维文档写清紧急处置：人工进租户 runtime 删目录 |
| `X-Tenant-Id` 可被运行时内任意代码读取伪造，后端无法区分 | 接受（内网隔离兜底） |
| 限额只能靠环境变量，调整必须重启后端 | 接受 |
| 不做审核 → 后续补审核时需对已上架内容做一次性回溯 | 接受，排入后续工作 |
| 扫描模式可被运维改回 `warn`（环境变量优先级最高，但配置随时可改） | 靠插件 `available()` 自查 + 部署文档强制项；每租户 runtime 都要带上该环境变量 |
| QwenPaw 升 SDK major 会导致 `plugin.json` 被拒 | 版本钉 2.2.2，升级前跑 `plugins.check` |
| Rust 与 Python 生态割裂，契约靠文档维持 | 插件 repo 内存 OpenAPI 契约 + 双 repo 契约测试 |

## 6. 未决（不阻塞 M0/M1，M2 前定）

1. `slug` 生成与冲突规则（中文名转写、重名加后缀、是否允许改名）。
2. 私有包被下架时，属主是否仍可下载（建议：可以，属主不受影响）。
3. 管理员是否也是租户之一（若是，管理员 console 是否也出现市场入口）。
4. `SKILL.md` frontmatter 的最小校验（name / description 长度、必填字段）。
