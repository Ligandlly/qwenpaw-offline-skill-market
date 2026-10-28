# 智能体市场：设计与契约

状态：**M1 后端 + M2 插件 + M3 前端 + M4 验收已落地**（分支 `feat/agent-market`，2026-10-27）
目标宿主：**QwenPaw 2.2.2**（`v2.2.2` / `v2.2.2.post1`）
前置阅读：[`PLAN.md`](../PLAN.md)（技能市场的定案与风险表）、[`docs/OPS.md`](OPS.md)

## 0. 一句话

复用现有「后端存/发/授权/审计 + 插件代理 + 侧边栏页面」的骨架，新增 **agent** 实体：
把租户现成的智能体（人设 MD + 安全配置子集 + 捆绑技能）发布为不可变版本；
安装时由**插件服务端**调宿主 API 新建 agent、写 MD、装技能，扫描与落盘仍归宿主。

已确认的三个决定（2026-10-27）：

| 议题 | 决定 |
| --- | --- |
| 智能体包内容 | 人设 + 配置 + **捆绑技能** |
| 安装链路 | 浏览器 → 插件路由 → **宿主 HTTP API**（不直连市场后端，不让前端编排） |
| 本轮范围 | 先出设计与契约；本文件确认后再写代码 |

## 0.1 实现状态（2026-10-27）

- **M1 后端已落地**（`backend/src/agents.rs` + `backend/tests/agents.rs`）：
  `agents` / `agent_versions` 表、`audit_log.kind`、`manifest.json` 结构校验、
  发布/列表/我的/状态/下载/撤回/恢复、管理端列表/下架/恢复、审计按 `kind` 过滤。
  24 个单测 + 23 个集成测试全绿（其中智能体 7 单测 + 8 集成）。
- **M2 插件已落地**（`plugin/offline_market/host.py`、`agents.py`、`routes.py`）：
  宿主 API 客户端（loopback 约束、内部 token/Authorization 转发、multipart 技能上传）、
  从现有 agent 的只读快照、安装状态机（创建→等就绪→写 MD→装技能→应用配置）、
  失败回滚、按 slug 并发锁、本机安装账本、卸载与对账。
  插件测试 108 项全绿（新增 34：host 11 + agents 11 + routes 12，另有 sort/total 透传用例），不联网、不需要宿主。
- **M3 前端已落地**（`plugin/frontend/market.js`）：顶层「技能 / 智能体 / 概览」三个页签；
  智能体四页签（市场/发布/我的发布/已安装）、安装弹窗（阶段提示、失败 step、回滚/保留工作区）、
  发布预览（新增 `/api/offline-market/agents/preview`）、卸载确认。无构建步骤；
  `node --check` 通过，`spikes/m3_verify.py` 已增加智能体页签渲染检查（真机执行需部署环境）。
- **M4 验收已落地**：Docker Hub 的 phase C（`e2e.py --runtime-container`）、
  容器内 preflight（市场可达 + 插件 `/host-health`）、`docs/OPS.md` §2.6 Docker 专章。
  当前会话没有 Docker socket 权限，phase C 未在本机真实执行（命令与前置条件见验收文档）。
- **安全加固与额度按本轮决定后置**：后端只保留“能否安装”的结构校验，
  不启用独立包/文件/技能额度；§3.4 与 §7 是目标设计，不是当前实现。
- M4（真实宿主 e2e、部署文档）尚未开始。

## 1. 边界

**打包**：

- 人设：`AGENTS.md` / `SOUL.md` / `PROFILE.md` / `HEARTBEAT.md` / `BOOTSTRAP.md`（只收这五个名字）
- 配置：`language`、`thinking_level` / `thinking_budget`、`plan.enabled`、**禁用**工具清单
- 技能：`skills/<name>/**` 全量文件（含脚本、参考文档）

**不打包**（隐私或不可移植）：

- 会话、记忆（`MEMORY.md`、`memory/`、`sessions/`、`history.db`、`media/`、`jobs.json`、`chats.json`）
- 模型与路由（`active_model`、`fallback_models`、`llm_routing`）：跨租户的 provider/model id 不可假设存在
- 渠道、MCP、邮箱、凭据、`backend_settings`、`running`（含工作区路径）
- `approval_level`：远程包不得降低审批强度，安装时沿用宿主当前值
- 工作区 `agent.json`：宿主自己维护，且会包含上面这些敏感字段，绝不整文件打包

**不做**（与技能市场保持一致）：

- 人工审核、远程吊销已安装副本、更新已有 agent（MVP 只支持新装 + 卸载）
- 模型/渠道等运行态配置的迁移

## 2. 已验证的宿主事实（QwenPaw v2.2.2）

> 以下均按 `v2.2.2` tag 逐文件核对。2.2.2 没有「智能体市场」SPI，也没有 agent bundle 安装端点。

| # | 事实 | 出处 |
| --- | --- | --- |
| A1 | 智能体 = `config.json` 里的 `agents.profiles[id]` + 工作区目录（`agent.json`、MD、`skills/`） | `config/config.py:2295 AgentProfileConfig`、`app/routers/agents.py:_persist_created_agent` |
| A2 | 创建：`POST /api/agents`，请求 `CreateAgentRequest{id?,name,description,workspace_dir?,language?,skill_names?,thinking_level?,thinking_budget?,backend?}`，返回 `AgentProfileRef{id,workspace_dir,enabled}`；会调度 agent 启动 | `app/routers/agents.py:795 create_agent` |
| A3 | 读取全量配置：`GET /api/agents/{agentId}` → `AgentProfileConfig`（含渠道/模型/邮箱等，**不可原样外发**） | `app/routers/agents.py:613 get_agent` |
| A4 | 局部更新：`PUT /api/agents/{agentId}` 是 `exclude_unset` 合并；但**嵌套对象是整体替换**（`tools` 不能只传一个字段） | `app/routers/agents.py:1127 update_agent` |
| A5 | 删除：`DELETE /api/agents/{agentId}` 只摘除配置，**不删除工作区**；`default` 不可删 | `app/routers/agents.py:1683 delete_agent` |
| A6 | 写 MD：`PUT /api/agents/{agentId}/workspace/files/{md_name}`，body `{content}`；文件名会被 `_sanitize_md_name` 归一化为单段，任意 `*.md` 可写 | `app/routers/workspace.py:208`、`agents/memory/agent_md_manager.py:161` |
| A7 | 读工作区：`GET /api/agents/{agentId}/workspace/tree?root=workspace&path=...`、`/file-content`、`/file-download`；`root=workspace` 指向 agent 自身目录 | `app/routers/workspace.py:399/462/534` |
| A8 | 技能安装：`POST /api/agents/{agentId}/skills/upload`（multipart 字段 `file`，`enable=true`）；内部 `SkillService.import_from_zip` 会先 `validate_skill_content` + **`scan_skill_dir_or_raise`**，再落盘 | `app/routers/skills.py:1200`、`agents/skill_system/workspace_service.py:605` |
| A9 | 技能列表/详情/文件：`GET /api/agents/{agentId}/skills`、`/skills/{name}`、`/skills/{name}/files/{path}` | `app/routers/skills.py` |
| A10 | agent id 规则：仅字母/数字/`-`/`_`，不能以 `-`/`_` 开头结尾，有保留字与长度限制 | `config/config.py:310 validate_agent_id` |
| A11 | 鉴权：`AuthMiddleware` 要求 `Authorization: Bearer <user token>`（除非关闭鉴权或在 `allow_no_auth_hosts`）；Hub 托管的 runtime 另有 `RuntimeBoundaryMiddleware`，要求 `X-QwenPaw-Runtime-Token` | `app/auth.py` |
| A12 | Hub 本地 runtime 注入 `QWENPAW_RUNTIME_API_URL=http://127.0.0.1:<port>`、`QWENPAW_RUNTIME_INTERNAL_TOKEN`、`QWENPAW_RUNTIME_ID`；Docker provisioner 只注入 token，不注入 URL（容器内固定 8088） | `hub/local_provisioner.py:371`、`hub/docker_provisioner.py` |
| A13 | 宿主官方提供了「如何安全地调自己」的参考实现：只接受 loopback URL，且同源才加内部 token | `utils/runtime_api.py` |
| A14 | 技能 zip 导入的扫描闸门与技能市场是同一套：`block` 拦截，`warn` 扫描后照装，`off` 不扫 | `security/skill_scanner`、`spikes/findings-2.2.2.md` 结论 9 |
| A15 | Hub 的 Docker runtime 网络显式关闭容器互访（`enable_icc=false`）；市场后端不能用同网络容器名，需走宿主机网桥网关 | `hub/docker_provisioner.py:_get_network` |

**推论**：安装一个智能体是「1 次创建 + N 次写 MD + 1 次技能 zip 上传 + 少量配置更新」，
没有宿主的原子安装入口，所以失败回滚和步骤状态必须由插件自己负责。

## 3. Bundle 契约

### 3.1 布局

```
manifest.json          # 唯一清单，必须在根
SOUL.md                # 以下 .md 均为可选，但必须在白名单内
PROFILE.md
AGENTS.md
HEARTBEAT.md
BOOTSTRAP.md
skills/<skill-name>/SKILL.md
skills/<skill-name>/... # 技能内任意文件
```

除 `manifest.json` 与白名单 MD 外，**根目录不允许出现其他文件**；
所有非 MD 文件必须位于某个 `skills/<skill-name>/` 下。

### 3.2 `manifest.json`

```json
{
  "schema": 1,
  "name": "QA 助手",
  "description": "面向 QwenPaw 配置与文档问答的智能体",
  "language": "zh",
  "profile": {
    "thinking_level": "medium",
    "thinking_budget": null,
    "plan_enabled": false,
    "disabled_tools": ["browser", "execute_shell_command"]
  },
  "md_files": ["SOUL.md", "PROFILE.md", "AGENTS.md"],
  "skills": ["qa-source-index"]
}
```

字段规则：

| 字段 | 规则（M1 实际校验） |
| --- | --- |
| `schema` | 必须为 `1`；其他值拒绝（为未来格式留位） |
| `name` | 非空（长度不设硬上限）；中文、大小写、空格都允许 |
| `description` | 展示用；不设硬上限 |
| `language` | 非空；插件发布时固定 `zh` / `en` |
| `profile.thinking_level` | 宿主枚举：`inherit/off/minimal/low/medium/high/xhigh/max/budget` |
| `profile.thinking_budget` | 仅当 `thinking_level == "budget"` 时为正整数，否则必须缺省 |
| `profile.plan_enabled` | bool，默认 `false` |
| `profile.disabled_tools` | 工具名 ≤64 字符；数量不设上限（额度后置）；安装时**只允许关闭，永不开启** |
| `md_files` | 白名单子集，无重复；每个文件必须在包内 |
| `skills` | 不设数量上限（额度后置），无重复；每个必须有 `skills/<name>/SKILL.md` |

`md_files` 与 `skills` 至少有一项非空；未知的顶层/`profile` 字段直接拒绝（契约保持稳定）。

技能名规则与宿主 `normalize_skill_dir_name` 保持一致：非空、无路径分隔符
（`/`、`\`）、无控制字符、不超过 128 字节；**不限制大小写或字符集**，
所以 `QA_source_index`、中文技能名都可以直接随智能体发布。
每个 `SKILL.md` 复用技能市场的前置校验：frontmatter 非空 `name` + `description`，
否则宿主 `validate_skill_content` 会拒绝安装。

### 3.3 文件编码

与技能市场一致：能按 UTF-8 解码的传字符串，否则传 `{"base64":"..."}`。
安装时技能文件走 zip，**二进制可保留原字节**；MD 必须 UTF-8 文本。
发布快照默认直接读工作区文件；如果走 HTTP 备用路径，文本用 `/file-content`，
二进制用 `/file-download`（§5.2）。

### 3.4 额度（本轮不实现）

按本轮决定，智能体市场**暂不启用独立额度**，`MARKET_MAX_AGENT_*` 尚未实现。
唯一的硬上限是 HTTP 传输层的 64 MiB 请求体（`DefaultBodyLimit`，防止一次 POST
把进程打爆），它不按文件/技能数计，也不属于市场语义。

后续启用时再按下面的目标值加（改需重启）：

| 变量 | 目标默认 | 说明 |
| --- | --- | --- |
| `MARKET_MAX_AGENT_BUNDLE_BYTES` | `8388608`（8 MiB） | 单包总字节 |
| `MARKET_MAX_AGENT_FILES` | `300` | 文件总数（含技能） |
| `MARKET_MAX_AGENT_FILE_BYTES` | `262144`（256 KiB） | 单文件 |
| `MARKET_MAX_AGENT_SKILLS` | `20` | 单包技能数 |

## 4. 后端（Rust）

### 4.1 表结构

```sql
CREATE TABLE IF NOT EXISTS agents (
    slug             TEXT PRIMARY KEY,
    name             TEXT NOT NULL,
    description      TEXT NOT NULL DEFAULT '',
    owner_tenant     TEXT NOT NULL,
    visibility       TEXT NOT NULL CHECK (visibility IN ('public','tenant')),
    latest_version   TEXT,
    created_at       INTEGER NOT NULL,
    withdrawn_at     INTEGER,
    withdrawn_by     TEXT,
    takedown_at      INTEGER,
    takedown_by      TEXT,
    takedown_reason  TEXT
);
CREATE INDEX IF NOT EXISTS idx_agents_owner ON agents(owner_tenant);

CREATE TABLE IF NOT EXISTS agent_versions (
    slug        TEXT NOT NULL REFERENCES agents(slug) ON DELETE CASCADE,
    version     TEXT NOT NULL,
    sha256      TEXT NOT NULL,
    size_bytes  INTEGER NOT NULL,
    file_count  INTEGER NOT NULL,
    skill_count INTEGER NOT NULL,
    bundle_json TEXT NOT NULL,     -- 整个 bundle，含 manifest.json
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (slug, version)
);

ALTER TABLE audit_log ADD COLUMN kind TEXT NOT NULL DEFAULT 'skill';  -- migrate()，幂等
```

语义与技能完全对齐：

- 版本不可变（同 `slug+version` → `409 version_exists`）；`latest` 只前进不回退
- slug 归属：别人占用的 slug → `403`；被管理员下架的 slug 不能再发新版本
- 公开/租户私有；非属主列表不可见、bundle `403`、状态查询 `404`（fail-closed）
- 撤回 = 属主，可恢复；下架 = 管理员，整包 `410`（含属主），不可自恢复
- agent 与 skill 是**两个独立命名空间**，同 slug 可并存

### 4.2 租户面 API

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/v1/agents?q=&page=&limit=` | 公开 + 自己的私有；已撤回/下架不出现 |
| GET | `/api/v1/agents/mine` | 自己的全部（含已撤回） |
| GET | `/api/v1/agents/{slug}/status` | `active` / `withdrawn` / `taken_down` |
| POST | `/api/v1/agents` | 发布不可变版本 |
| POST | `/api/v1/agents/{slug}/withdraw` | 属主撤回 |
| POST | `/api/v1/agents/{slug}/restore` | 属主恢复；被下架 → `409` |
| GET | `/api/v1/agents/{slug}/{version}/bundle` | 取 bundle；`version=latest` 走指针 |

发布请求体（`name` / `description` 以包内 `manifest.json` 为准，避免两处不一致）。
`slug` 可省略：名称能转成 ASCII slug 就按名称推导，否则使用稳定的
`agent-<sha256 前缀>`，所以中文名称的智能体可以直接发布，不需要手填标识符：

```json
{
  "slug": "qa-agent",
  "version": "1.0.0",
  "visibility": "public",
  "publisher_name": "张三",
  "files": {
    "manifest.json": "{...}",
    "SOUL.md": "...",
    "skills/qa-source-index/SKILL.md": "---\nname: qa-source-index\ndescription: ...\n---\n",
    "skills/qa-source-index/scripts/run.py": "..."
  }
}
```

响应（与技能市场同形）：

```json
{ "slug": "qa-agent", "version": "1.0.0", "sha256": "...",
  "size_bytes": 12345, "file_count": 9, "skill_count": 1, "bundle_url": "..." }
```

### 4.3 校验闸门（后端强制执行）

在 `check_bundle` 之外新增 `check_agent_bundle`：

1. 路径规则复用：拒绝绝对路径、`..`、反斜杠、盘符、控制字符、空段
2. 根文件白名单：仅 `manifest.json` + `md_files`
3. `manifest.json` 必须是 UTF-8 JSON、`schema=1`、字段合法
4. `md_files` 全部存在于包内，且文件名在白名单
5. `skills` 与目录一一对应：每个 `skills/<name>/SKILL.md` 存在，frontmatter 合法；
   `skills/` 下不允许出现未声明目录；不允许 `skills/<name>` 下再嵌套同构包
6. 额度：**本轮不启用**（§3.4）

### 4.4 管理面 API

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/admin/api/agents?q=&limit=` | 全部（含撤回/下架） |
| POST | `/admin/api/agents/{slug}/takedown` | `{reason}` |
| POST | `/admin/api/agents/{slug}/restore` | 撤销下架 |
| GET | `/admin/api/audit?limit=` | 增加 `kind` 过滤（`skill`/`agent`/`all`） |

`audit_log.kind` 让技能与智能体共用一张审计表；管理员下架、属主撤回/恢复都写对应 `kind`。

## 5. 插件（Python，纯 stdlib）

### 5.1 宿主 API 客户端

新增 `offline_market/host.py`，负责调本 runtime 的宿主 API：

```
base_url 解析顺序：
  1. 配置项 `host_api_url` / `OFFLINE_MARKET_HOST_API_URL`
  2. `QWENPAW_RUNTIME_API_URL`（必须 loopback、且带 `QWENPAW_RUNTIME_INTERNAL_TOKEN`）
  3. 显式 `QWENPAW_PORT`（容器/CLI 真实监听端口；Docker 的 49154 之类是宿主机映射，
     容器内不可用）
  4. 浏览器请求的 `request.base_url` —— **仅当它是 loopback**（单机自定义端口，且
     `QWENPAW_PORT` 未设置）
  5. `http://127.0.0.1:${QWENPAW_PORT:-8088}`（兜底）
```

连接被拒（`Connection refused`，即请求未到达服务端）时，`HostClient` 会再试一次
loopback 默认端口；成功后就记住该地址。显式配置的地址不会被静默回退。

每次请求带两个头：

- `Authorization`：转发浏览器调用插件路由时的原样 Bearer（没有就不带；关闭鉴权的环境自然通过）
- `X-QwenPaw-Runtime-Token`：`QWENPAW_RUNTIME_INTERNAL_TOKEN`（存在时）

明文 HTTP 只允许 loopback / `host.docker.internal`；非 loopback 一律拒绝（对齐
`utils/runtime_api.py` 的策略，避免把内部 token 发给任意地址）。

### 5.2 发布：从现有 agent 生成快照

`POST /api/offline-market/agents/publish`

```json
{ "source_agent_id": "qa", "slug": "qa-agent",
  "version": "1.0.0", "visibility": "public" }
```

步骤：

1. `GET /api/agents/{source_agent_id}` → 校验 `backend == "qwenpaw"`
2. 从 `workspace_dir` **只读**读取快照（不在浏览器做，也不信任浏览器传入的路径）：
   - 五个白名单 MD：存在则收集，按 UTF-8 读取（解码失败直接拒绝发布）；**跳过 `MEMORY.md`**
   - `GET /api/agents/{id}/skills` 取 `enabled == true` 的技能名；agent 未启动时宿主会按需启动，
     起不来就返回明确错误，不做半成品发布
   - 遍历 `<workspace>/skills/<name>/`（宿主会把旧 `skill/` 自动迁移为 `skills/`）：
     跳过符号链接，按大小/数量提前失败
3. 生成 `manifest.json`（§3.2），配置子集从 `AgentProfileConfig` 读取：
   - `language`、`thinking_level`、`thinking_budget`、`plan.enabled`
   - `disabled_tools`：记录当前 `enabled == false` 的工具；安装端只关闭“当前开启”的，
     默认关闭的工具不会产生额外变化（只降不升）
4. `POST /api/v1/agents` 发给市场后端
5. 返回后端结果；发布成功后前端刷新列表一次

> 为什么读磁盘而不是逐个文件走 HTTP：一个技能可能有几十个文件，逐个
> `/file-content` 会产生几十次请求；插件与宿主同进程、同用户，只读快照用
> 文件系统更可靠。**安装**仍严格走宿主 HTTP API，避免绕过扫描与配置事务。

### 5.3 安装：状态机

`POST /api/offline-market/agents/{slug}/install`

```json
{ "version": "latest", "target_name": "QA 助手（内网）" }
```

1. **取包**：`GET {market}/api/v1/agents/{slug}/{version}/bundle`（带 `X-Tenant-Id`）
2. **查重**：`GET /api/agents` 拿现有 id；若本地安装账本中已有该 `slug` → `409 already_installed`
3. **定 id**：`slug` 合法（宿主：2–64 字符、字母/数字/`-`/`_`、不能 `default`）则用；
   被占用则加 `-2`、`-3`…，超过 64 字符时先截断再加后缀；`target_name` 只改显示名。
   创建返回 `400 already exists` 时按同一规则换下一个后缀重试
4. **创建**：`POST /api/agents`
   ```json
   { "id": "qa-agent", "name": "QA 助手（内网）", "description": "...",
     "language": "zh", "thinking_level": "medium", "thinking_budget": null,
     "skill_names": [] }
   ```
   `thinking_level/budget` 未在 manifest 声明时不要发送，沿用宿主默认
5. **等就绪**：轮询 `GET /api/agents/{id}/workspace/files`，直到 200（超时 60s）；
   404/409/500 视为「启动中」继续等
6. **写人设**：对每个 MD 调
   `PUT /api/agents/{id}/workspace/files/{name}`，body `{"content": "..."}`
7. **装技能**：`skills` 为空则跳过。否则把每个 `skills/<name>/**` 打进内存 zip
   （条目 `<name>/<相对路径>`），`POST /api/agents/{id}/skills/upload?enable=true`，
   multipart 字段名 `file`，`Content-Type: application/zip`。
   技能冲突（理论上新 agent 不会）→ `409`，整步失败
8. **应用配置**：
   - `plan_enabled` 非默认 → `PUT /api/agents/{id}` body `{"plan":{"enabled":true}}`
   - `disabled_tools`：先 `GET /api/agents/{id}/tools`，只对**当前为 enabled 且出现在清单里**的
     工具调 `PATCH /api/agents/{id}/tools/{tool}/toggle`（该端点**无 body，执行取反**），
     再校验响应 `enabled == false`；清单外的工具一律不碰
   - **绝不**打开任何工具，也绝不发送 `tools` 整体（`update_agent` 对嵌套对象是整体替换，
     会覆盖租户现有工具配置）
9. **记账**：写本机账本（见 §5.4），返回
   ```json
   { "agent_id": "qa-agent", "name": "QA 助手（内网）", "workspace_dir": "...",
     "version": "1.0.0", "warnings": [] }
   ```

**失败回滚**：第 4 步成功后任何一步失败，调 `DELETE /api/agents/{agent_id}` 摘除配置；
不写账本；返回结构化错误。宿主不删工作区（A5），所以响应里带上
`workspace_kept: "<path>"`，并在页面提示「配置已回滚，工作区目录需手工清理」。
若 `DELETE` 也失败，标记 `rollback_failed: true`，避免页面显示成干净成功。

**并发**：按 `slug` 加进程内 `threading.Lock`，双击只会有一次创建；另加
「同 slug 已安装」检查作为第二道。

### 5.4 本机安装账本

`{QWENPAW_WORKING_DIR}/offline-market/installed-agents.json`（原子写；没有
`QWENPAW_WORKING_DIR` 时用 `~/.qwenpaw`）：

```json
{
  "version": 1,
  "items": {
    "qa-agent": {
      "agent_id": "qa-agent",
      "slug": "qa-agent",
      "version": "1.0.0",
      "name": "QA 助手（内网）",
      "workspace_dir": "/…/workspaces/qa-agent",
      "installed_at": 1761...
    }
  }
}
```

- 列表时与 `GET /api/agents` 对账：agent 已被用户手动删除 → 账本条目一并清掉
- 不使用 `template_id`、不依赖名称匹配：账本是唯一来源，避免污染宿主配置
- 「已安装」页再按 `slug` 查市场状态（正常/已撤回/已下架）

### 5.5 卸载

`POST /api/offline-market/agents/{agent_id}/uninstall`

1. 账本里必须存在该 `agent_id`（防止删任意 agent）
2. `DELETE /api/agents/{agent_id}`；启动中返回 `409` 时短暂重试
3. 删除账本条目，返回 `{removed:true, workspace_kept:"<path>"}`
4. 工作区**不自动删**：里面有用户可能已产生的记忆/会话；页面明确提示路径

### 5.6 插件路由总表

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/offline-market/stats` | 概览聚合（代理后端 `/api/v1/stats`） |
| GET | `/api/offline-market/agents?q=&page=&limit=` | 市场列表（代理后端） |
| GET | `/api/offline-market/agents/mine` | 我的发布 |
| GET | `/api/offline-market/agents/{slug}/status` | 市场状态 |
| GET | `/api/offline-market/host-agents` | 本机可发布的 agent（代理 `GET /api/agents`，过滤 `backend=qwenpaw`） |
| POST | `/api/offline-market/agents/publish` | 快照 + 发布 |
| POST | `/api/offline-market/agents/preview` | 只快照并返回将打包的人设/技能/大小，不发布 |
| POST | `/api/offline-market/agents/{slug}/withdraw` | 撤回 |
| POST | `/api/offline-market/agents/{slug}/restore` | 恢复 |
| POST | `/api/offline-market/agents/{slug}/install` | 安装（§5.3） |
| POST | `/api/offline-market/agents/{agent_id}/uninstall` | 卸载（§5.5） |
| GET | `/api/offline-market/agents/installed` | 账本 + 对账 + 市场状态 |
| GET | `/api/offline-market/host-health` | 容器内诊断：插件调一次宿主 `GET /api/agents` |

## 6. 前端（`plugin/frontend/market.js`）

侧边栏名称从「技能市场（内网）」改为「**内网市场**」。入口通过 `sider.top` 插槽放在
New Task 按钮正下方、折叠菜单之外；页面顶部三个一级页签：

```
内网市场
├── 技能      ← 现有四个页签原样保留
├── 智能体
│   ├── 市场   表格：名称/描述/发布者/技能数/版本数/可见性 + 「安装」
│   ├── 发布   选择本机 agent → 预览（人设文件、技能、文件数/大小）→ 发布
│   ├── 我的发布 撤回 / 恢复；被下架显示「无法自助恢复」
│   └── 已安装 账本条目 + 市场状态 + 「卸载」
└── 概览      只读 Dashboard：总数/下载量/本月新增/构成占比
```

安装弹窗：

1. 显示名（默认 manifest name）+ 版本（默认 `latest`）
2. 安装中显示阶段提示：`创建智能体 → 等待就绪 → 写入人设 → 安装技能 → 应用配置`
   （同步请求，阶段是前端合成的；**真实失败步骤**由后端 `step` 返回）
3. 成功：显示 agent id、工作区路径
4. 失败：显示失败步骤、原始错误；若发生回滚，明确写「已回滚」及保留的工作区路径
5. 顶部常驻提示：智能体人设会改变助手行为；技能仍由宿主扫描，`block` 才拦得住恶意代码

发布页保护：**不展示也不上传** `MEMORY.md`、会话、记忆、模型、渠道、MCP、邮箱；
发布前展示「将打包内容」摘要（人设文件名、技能名、文件数、大小）让作者确认。

## 7. 安全

> **本轮决定：安全加固后置。** M1 后端只做“能否安装”的结构校验（路径不出 `skills/`、
> 声明与文件一致、`SKILL.md` frontmatter 合法）；下表是 M2/M3 与后续的目标，
> 不是当前已交付内容。唯一必须在当前链路保留的是**技能安装走宿主扫描**——
> 这是宿主既有行为，不需要额外开发，也不应绕过。

| 风险 | 目标处置（后续） |
| --- | --- |
| 捆绑技能绕过扫描 | 安装必须走 `POST /skills/upload`，复用 `scan_skill_dir_or_raise`；插件继续 fail-closed 拒绝 `off` 模式。`warn` 允许但页面提示「只告警不拦截」 |
| 恶意人设（prompt injection） | 扫描器不扫 MD，这是市场固有风险；安装弹窗明示「会改变助手行为」，管理员可下架。后续接人工审核 |
| 配置降权 | 只应用白名单字段；`approval_level` 不接收；工具只能关闭不能开启；渠道/邮箱/MCP/模型/凭据不进入包 |
| 发布泄露隐私 | 快照只读五个 MD + 启用技能；硬编码排除 `MEMORY.md`/`sessions`/`memory`/`media`/`jobs.json`/`chats.json`/`agent.json` |
| 路径穿越 | 发布端跳过 symlink；后端复用 `check_path`；安装端文件名走宿主 `_sanitize_md_name`，技能条目不拼绝对路径 |
| 身份伪造 | 与技能市场相同：仅内网信任 `X-Tenant-Id`；浏览器不直连后端 |
| 内部 token 外发 | 宿主 API base 仅允许 loopback/`host.docker.internal`；非 loopback 直接拒绝 |
| 余额/资源 | 包、文件、技能数限额；安装等待与上传都有超时 |
| 卸载残留 | 宿主不删工作区；页面明确提示，运维文档给出手工清理路径 |

## 8. 里程碑与验收

### M1 后端 — ✅ 已完成（MVP，2026-10-27）

- 已交付：`agents` / `agent_versions` 表与迁移、`audit_log.kind`、
  `check_agent_bundle`（manifest/路径/技能 frontmatter）、发布/列表/我的/状态/下载/
  撤回/恢复、管理端列表/下架/恢复、审计 `kind` 过滤、64 MiB 传输上限
- 未交付（按本轮决定后置）：独立额度、发布侧配置白名单强制校验、恶意内容策略
- 测试：`cargo test` 24 单测 + 27 集成全绿（智能体新增 7 单测 + 8 集成）；
  `cargo clippy --all-targets -- -D warnings` 通过

### M2 插件 — ✅ 已完成（2026-10-27）

- 已交付：
  - `host.py`：base URL 解析（配置 > `QWENPAW_RUNTIME_API_URL` > 127.0.0.1:PORT > request）、
    loopback/`host.docker.internal` 限制、Authorization 透传 + runtime token、JSON/multipart
  - `agents.py`：只读快照（五种 MD + 启用技能，跳过 symlink/`__pycache__`）、
    安装状态机、rollback、按 slug 并发锁、原子账本、卸载、对账
  - `routes.py`：市场代理、`/host-agents`、发布、安装、卸载、已安装
- 测试：`pytest` 108 项全绿（新增 34：host 11 + agents 11 + routes 12）；
  `compileall` 通过；不联网、不需要 QwenPaw
- 明确未做：前端页签（M3）；安装进度是同步请求，页面只能显示最终 `step`/回滚结果

### M3 前端 — ✅ 已完成（代码，2026-10-27）

- 顶层「技能 / 智能体 / 概览」页签；技能四页签原样保留
- 智能体：市场（安装弹窗+阶段/回滚）、发布（本机 agent 选择 + 快照预览 + 发布）、
  我的发布（撤回/恢复）、已安装（账本/市场状态/卸载确认）
- 新增插件路由 `POST /api/offline-market/agents/preview`（只快照，不发布）
- 验证：`node --check plugin/frontend/market.js` 通过；`spikes/m3_verify.py` 已更新
  （菜单/标题改为「内网市场」，新增智能体页签渲染检查），**尚未在本机真机执行**

### M4 验收与部署 — ✅ 已完成（2026-10-27）

- phase C：`deploy/scripts/e2e.py --runtime-container`，通过 `docker exec` 在 runtime
  容器内发布→安装→校验 SOUL/技能/账本→卸载→撤回；容器内探针复用
  `127.0.0.1:${QWENPAW_PORT}` + runtime token
- preflight：`--runtime-container` 增加容器内市场可达性 + 插件 `/host-health`
- `docs/OPS.md` §2.6：Docker Hub 容器名、宿主 API 自调用、**ICC=false 导致不能用同网络
  容器名**（改用 bridge gateway）、工作区清理、验收命令
- 验证：脚本 `py_compile` + 探针源码 `compile()`；插件 108 测试；无 Docker 权限，
  phase C 未在当前环境真机执行

## 9. 未决（后续）

1. **更新语义**：是否支持同 slug 升级并替换已有 agent 的 MD/技能（技能删除很难回滚，
   倾向「新装 + 手工迁移」）。
2. **卸载是否清理工作区**：是否提供 `purge=true` 的二次确认选项。
3. **模型固定**：是否允许包里声明 `active_model`（跨租户 id 不可移植，倾向不做）。
4. **技能复用**：是否允许 `skills` 引用市场里已有的技能 slug+version，而不是内嵌文件
   （省体积，但安装要多一步依赖解析）。
5. **管理员 UI**：目前后端只有管理 API，没有后台页面；智能体下架先走 API，
   UI 与技能市场一起补。
6. **发布方式**：除「选本机 agent 快照」外，是否支持上传 `.zip`（给离线制作/迁移用）。

## 附录 A：宿主 API 清单（安装/发布会用到的全部）

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| GET | `/api/agents` | 现有 agent 列表（定 id、对账、发布选择） |
| POST | `/api/agents` | 创建 agent |
| GET | `/api/agents/{id}` | 读取全量配置（发布快照；仅服务端） |
| PUT | `/api/agents/{id}` | 应用 `plan.enabled` 等顶层配置 |
| DELETE | `/api/agents/{id}` | 卸载/回滚 |
| GET | `/api/agents/{id}/workspace/files` | 等就绪 + 列出工作 MD |
| GET/PUT | `/api/agents/{id}/workspace/files/{name}` | 读/写人设 MD |
| GET | `/api/agents/{id}/workspace/tree?root=workspace&path=...` | 枚举技能文件（备用） |
| GET | `/api/agents/{id}/workspace/file-content?root=workspace&path=...` | 读文本（备用） |
| GET | `/api/agents/{id}/workspace/file-download?root=workspace&path=...` | 读二进制（备用） |
| GET | `/api/agents/{id}/skills` | 技能列表（发布时筛启用项） |
| POST | `/api/agents/{id}/skills/upload?enable=true` | 安装捆绑技能（含扫描） |
| GET | `/api/agents/{id}/tools` | 读工具当前状态（决定是否需要关闭） |
| PATCH | `/api/agents/{id}/tools/{tool}/toggle` | 取反工具开关（无 body；只在 enabled→false 方向使用） |

## 附录 B：与技能市场的差异

| 维度 | 技能市场 | 智能体市场 |
| --- | --- | --- |
| 宿主入口 | 注册 `MarketProvider`，宿主原生 Skills 页可见 | 无 SPI，只能走自写侧边栏页 |
| 安装 | 宿主 `install/start` 全权负责 | 插件编排多个宿主 API，自行回滚 |
| 清单 | `SKILL.md` frontmatter | `manifest.json` + 每个技能自己的 `SKILL.md` |
| 二进制 | hub 安装只接受文本 | 技能 zip 安装支持原始字节 |
| 版本/可见性/撤回/下架/审计 | 已有 | 复用同一套语义，独立表与 `kind` |
