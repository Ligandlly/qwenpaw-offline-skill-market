# Spike 2：能不能加侧边栏入口 + 调宿主安装接口

针对 **v2.2.2** 逐文件核对。结论：**可行，不需要改宿主代码**。

## 加载链路（`console/src/plugins/usePluginLoader.ts` @ v2.2.2）

```
Console 启动
  → GET /api/frontend_plugin                 列出所有带 entry.frontend 的插件
  → GET /api/frontend_plugin/{id}/files/{entry}   取 JS
  → new Blob([...], {type:"application/javascript"}) → import(blobUrl)
```

即：**前端入口是一个 ES module，被 import 时自己上去注册**。pluginId 要自己写在 bundle 里（宿主在 `loadFrontendPlugin(pluginId)` 时也只用于按 id 找 bundle，不会注入变量）。

## 注册 API（`console/src/plugins/registry/sdk.ts` @ v2.2.2）

```ts
window.QwenPaw.menu.add(pluginId, {
  id: "offline-market.market",
  location: "primary.settings",        // 侧边栏桶
  parentId: "plugins-group",           // 也可挂到 core.control-group / agent-group 等
  before: "core.settings",             // 或者 after / order
  label: () => "技能市场",
  // icon 字段见 registry/types.ts MenuItem
})
window.QwenPaw.menu.add(pluginId, {...})   // 可一次传数组
window.QwenPaw.route.add(pluginId, {
  id: "offline-market.page",
  path: "/offline-market",             // 支持 "/x/*"
  component: MarketPage,               // React 组件，React/AntD 由宿主 external 提供
})
window.QwenPaw.slot.fill(pluginId, name, render, opts)
```

`MenuItem` 字段（`registry/types.ts:72`）：`id, location, parentId, before, after, order, label, ...`。

## 调宿主后端（`console/src/plugins/hostSdk/fetch.ts` @ v2.2.2）

```ts
import { hostFetch } from ".../hostSdk/fetch";   // 内部：buildAuthHeaders() + getApiUrl()
await hostFetch("/api/skills/hub/install/start", {
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify({
    provider_key: "offline-market",
    bundle_url: "http://market.local:8080/skills/foo/1.2.0",
    version: "1.2.0",
    enable: true,
  }),
});
```

`provider_key` 存在的意义就是：让宿主用**指定**的 market provider 去 `fetch_bundle`，而不是靠 `matches_url` 猜。

安装是异步任务：`POST /hub/install/start` 返回 `task_id`，再 `GET /hub/install/status/{task_id}` 轮询、`POST /hub/install/cancel/{task_id}` 取消（`console/src/api/modules/skill.ts:338-390`）。**市场页的"安装"按钮必须实现这三步，不能只 fire-and-forget。**

## 仍然需要在浏览器里实测的点（M0 收尾）

1. `window.QwenPaw.menu` 在我们的 bundle `import` 时是否已经挂载（决定要不要轮询等待）。
2. `parentId: "plugins-group"` 在侧边栏的实际可见层级（"显眼"到什么程度）。
3. 我们的市场页里 `hostFetch("/api/plugins/...")` 到插件自定义路由的路径前缀到底长什么样（宿主 `register_http_router` 的挂载点）。
4. 未登录 / 未授权时 `hostFetch` 的行为与错误体格式。

---

# 实测结果（2026-10-24，真机 Chromium）

自动化脚本：`spikes/v_frontend.py`（Playwright，复用 QwenPaw 已下载的 chromium-1187）。

| 检查 | 结果 |
| --- | --- |
| `window.QwenPaw.host.React` 在 bundle import 时可用 | ✅ 真（`has_host: true`） |
| `menu.add` 注册后进入侧边栏注册表 | ✅ `{"id":"offline-market.menu","location":"primary.settings","parentId":"plugins-group","label":"技能市场","route":"offline-market.page"}` |
| `route.add` 的页面可直达 `/offline-market` | ✅ 渲染出标题、状态横幅、技能列表（数据来自真后端） |
| 页面内的"安装"按钮走宿主安装任务 | ✅ 点击 → `installed_from: offline-market` → 页面出现"已安装 Demo Skill" |
| console / page errors | ✅ 0 |

## 三个必须知道的坑（都是实测撞出来的）

1. **`getApiUrl()` 会自动加 `/api`**（`console/src/api/config.ts`）。
   所以页面里要写 `getApiUrl("/offline-market/config")`，写成 `/api/offline-market/config` 会得到
   `/api/api/offline-market/config` → 404。第一次验证就是被这个 404 打回来的。
2. **`window.QwenPaw.host` 的实际内容**：`React, ReactDOM, antd, antdIcons, apiBaseUrl, getApiUrl, getApiToken, prepareBrowserSession, usesBrowserSession`。
   拿 React/antd 从这里取，不要 import 打包自己的副本。
3. **鉴权头**：`getApiToken()` 在无鉴权部署下返回空；开了鉴权时要自己加 `Authorization: Bearer <token>`，
   `hostFetch` 才自动带。我们的页面两条都兼容（有 token 就带）。

## 环境小贴士

QwenPaw 自带的 playwright 包期望 chromium-1248，而机器上是 1187。
`p.chromium.launch(executable_path=.../chromium-1187/chrome-win/chrome.exe)` 直接用现有浏览器，不必重新下载 150 MB。

## 遗留（M3 决策项）

宿主自己已有一个内置入口 `core.marketplace`（在 `primary.agentScoped`）。我们的"技能市场"挂在
`primary.settings → plugins-group` 下，两者名字/位置可能让用户分不清。M3 里需要定：
叫"离线技能市场"全名，还是把入口放到更显眼的位置（`primary.agentScoped`）。
