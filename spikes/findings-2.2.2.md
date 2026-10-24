# M0 代码级结论（QwenPaw **v2.2.2** tag 逐文件核对）

> 重要：这些结论**推翻了 PLAN.md 里来自 main 分支的假设**。以本文件为准。

## 结论 1：2.2.2 没有「新 SDK」，`plugin.json` 里没有 `sdk.version` / `capabilities`

`v2.2.2` 的 `src/qwenpaw/plugins/` 只有：
`__init__.py, api.py, architecture.py, browser_access.py, download_catalog.py, install_lock.py, loader.py, marketplace_registry.py, module_isolation.py, registry.py, runtime.py, validation.py`

**没有** `sdk.py / tools.py / markets.py / compliance.py`（`qwenpaw/plugins/markets.py` 在 2.2.2 返回 404）。
`qwenpaw.plugins.__all__` = `PluginLoader, PluginRegistry, PluginApi, PluginManifest, PluginRecord, get_tool_config`。

`PluginManifest`（`plugins/architecture.py:125`）字段：
`id, version, name, description, description_i18n, author, entry{frontend,backend}, dependencies, min_version="0.1.0", max_version, qwenpaw_version, meta, plugin_type`

→ **PLAN.md 里 `sdk.version = "1.0.0"`、`capabilities = [...]` 在 2.2.2 是废的**，那是 main / 2.3.x 的契约。兼容性靠 `min_version / max_version / qwenpaw_version` 约束。

## 结论 2：后端入口契约（`plugins/loader.py:515-625`）

- 入口文件由 `entry.backend` 指定（支持嵌套如 `backend/main.py`）
- 模块必须导出 **`plugin`** 对象（或 PawApp 的 `app`），并实现 **`register(api)`**；可以是协程
- `api` 是 `PluginApi(plugin_id, config, manifest_dict)`
- 注册前后由 `market_registry.begin_load/finish_load` 包裹 → 半成品注册不可见

## 结论 3：市场源契约（`market/providers/base.py`，duck-typed Protocol）

```python
class MarketProvider(Protocol):
    key: str
    label: str
    supports_browse: bool
    def available(self) -> tuple[bool, str | None]: ...
    def search(self, query: str, limit: int, page: int) -> Awaitable[tuple[list[MarketResult], bool, int|None]]: ...

class MarketInstallProvider(Protocol):
    def matches_url(self, url: str) -> bool: ...
    def fetch_bundle(self, url: str, requested_version: str) -> Awaitable[tuple[dict[str, Any], str]]: ...
```

注册：`api.register_market_provider(provider, priority=100)`（`plugins/api.py:395`）→ `market_registry.register(plugin_id, provider, priority)`。

`MarketRegistry.register` 的硬约束：
- 两个 Protocol 必须同时满足，缺方法直接 `ValueError`
- `provider.key` 重复直接 `ValueError("Duplicate hub key")`
- 卸载时若存在进行中的请求 → `MarketProviderBusyError`

`MarketResult`（`market/schema.py`）字段：`source, slug, name, description, source_url, version, author, icon_url, stats` —— **`source` 必填**，且必须等于 provider 的 `key`。

## 结论 4：搜���路径里没有任何「上下文对象」，所以没有 tenant（spike 1 的实际答案）

`market/service.py:search_market(query, provider_pages, limit, lang, category)` —— **没有 context 参数**；`_search_provider` 直接 `await provider.search(...)`。
HTTP 入口 `POST /api/market/search`（`app/routers/market.py`）也只收 `query/provider_pages/limit/lang/category`。

→ 租户身份**只能**来自插件运行进程的环境变量（`QWENPAW_TENANT_ID`）。且 provider 与宿主同进程（`_run_one` 是 in-process async 调用），env 可读。
→ `search()` 签名不接受 `lang/category`（`_supported_kwargs` 按签名过滤）→ 我们的实现**不要**声明这两个参数，否则拿不到也不报错。

## 结论 5：安装链路在 2.2.2 同样成立

`app/routers/skills.py:424 HubInstallRequest{provider_key, bundle_url, version, enable, target_name}`，路由 `POST /hub/install/start`，内部 `install_skill_from_hub`，`SkillScanError` 硬拦（`_scan_error_response`）。

## 结论 6：前端侧边栏入口可行（`console/src/plugins/registry/sdk.ts`）

2.2.2 已暴露 `window.QwenPaw.{menu, route, slot, audit}`：

```ts
menu.add(pluginId, { id, location, parentId, before, after, order, label, icon? })
route.add(pluginId, { id, path, component, meta? })
slot.fill(pluginId, name, render, opts)
```

`MenuItem.location` 决定侧边栏桶（默认 `primary.settings`），`parentId` 支持 `"plugins-group"` 等分组，`before/after/order` 控制位置。
另有 `console/src/plugins/hostSdk/{fetch,hooks,install}.ts`（`fetch.ts` 带宿主鉴权），`pawapp-sdk/` 提供 `host/api/ui/task`。

→ 「在对话界面侧边栏加显眼入口」不需要改宿主代码，`type: "frontend"` + `entry.frontend` 即可。

## 结论 7：可用性原因会被原样展示（spike 4 的实际答案）

`GET /api/market/providers` → `ProviderInfoSpec{key,label,available,reason,supports_browse}`，`reason` 来自 `provider.available()[1]`，`base.py` 注释写明 *"Reason is shown verbatim to the user in the UI tooltip"*。
注意 `_search_provider`：`available()` 为假时该 provider 直接变成一条 `MarketSearchError(provider=key, message=reason)` → 用户看到的是你的 reason 文案。

## 结论 8：`market_ready` 事件是个坑

`routers/market.py:_wait_for_market_ready` 会等 `app.state.market_ready`，且若 `market_startup_failed` 为真直接 503。
→ 我们的 provider 若在 `available()` 里做阻塞网络探测，会拖慢/卡住市场初始化。**`available()` 必须是非阻塞的本地判定**（例如读一个近期健康检查缓存），真实探测放后台任务。

## 结论 9（最重要）：宿主扫描器默认**不拦** CRITICAL

`security/skill_scanner/__init__.py:_get_scan_mode`：优先级 `env QWENPAW_SKILL_SCAN_MODE` > `config.security.skill_scanner.mode` > `"block"`；
但 **config.json 生成时写入的是 `"mode": "warn"`** → 新装环境实际是 `warn`。

实测（同一次安装，只改模式）：

| 模式 | 结果 |
| --- | --- |
| `warn`（默认） | 日志 `Scan ... 1 finding(s), safe=False`、`blocking is disabled – proceeding anyway` → **安装完成并 enable**，`exec(input())` 落在 `workspaces/default/skills/<name>/scripts/run.py` |
| `block` | 安装 `failed`，`error.result.type = "security_scan_failed"`，带 `rule_id/findings/file_path`，**磁盘无残留**（回滚干净） |

→ **部署强制项：每个租户 runtime 都要 `QWENPAW_SKILL_SCAN_MODE=block`**。
→ 插件的 `available()` 应自查该模式：不是 `block` 就返回 `(False, "...")`，让市场**拒绝服务**而不是静默分发。
→ `spikes/s5_scan_mode_gate.py` 是这个检查的可执行版本（`--json` 便于后端/CI 调用）。

---

## 对 PLAN.md 的修订项

| PLAN.md 原文 | 修正为 |
| --- | --- |
| `plugin.json`: `sdk.version = "1.0.0"`, `capabilities=[...]` | `{id, version, name, entry:{backend, frontend}, type:"hub", min_version, qwenpaw_version}`，无 sdk 块 |
| `MarketSpec(key,label,search,fetch,matches_url,available,supports_browse,priority)` | 两个 Protocol（`MarketProvider` + `MarketInstallProvider`），方法名是 `search / fetch_bundle / matches_url / available`，`priority` 走注册参数 |
| `search()` 带 `lang` | `search(query, limit, page)`，不要声明 lang/category |
| `SkillBundle(name, files, resolved_url, version)` | `fetch_bundle(url, requested_version) -> ({"name","files"}, resolved_url)` |
| `python -m qwenpaw.plugins.check --runtime` | 2.2.2 无此工具 → 用 `qwenpaw plugin install ./hub_plugin && qwenpaw plugin list`，再起 app 验证 `/api/market/providers` |
| `MarketResult` 无 `source` | `source` 必填，等于 provider key |
| 安装侧扫描 = 安全兜底 | **默认不拦**；必须 `QWENPAW_SKILL_SCAN_MODE=block` + 插件 `available()` fail-closed |

## 结论 10：插件包可以正常分包（实测）

宿主 loader 会把插件的裸绝对导入重定向到私有命名空间（`build_plugin_builtins`），所以在 `plugin/` 里写成
`offline_market/` 包 + `from offline_market.provider import X` 是可行的：`qwenpaw plugin install` 后
`/api/market/providers` 与 `/api/offline-market/config` 均正常响应。**不必**把插件摊平成单个文件。

## 结论 11：宿主"已安装技能"列表认不出安装来源（M3 实测）

- `GET /api/skills` 返回 `SkillSpec[]`，其中 `source` 对 hub 安装的技能一律是 **`"customized"`**，
  **没有** `installed_from`。想判断"这条是不是从我们市场装的"，列表层面做不到。
- 只有 `GET /api/skills/{display_name}` 详情里才有 `installed_from: "offline-market"`。
- 宿主的安装目录名是**展示名**（bundle 的 `name`，如 `Demo Skill`），不是市场 slug（`demo`）；
  `/api/skills/demo` 会 404。

→ 因此后端加了 `GET /api/v1/skills/lookup?names=a,b`：按展示名返回 `slug + state`，
并且**包含已撤回/已下架**的技能（市场列表会隐藏它们，靠列表反查会在下架后失效）。
插件侧对应 `/api/offline-market/lookup`，"已安装"页据此显示 `正常 / 已撤回 / 已下架`。
