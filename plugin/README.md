# offline-market plugin (QwenPaw 2.2.2)

离线技能市场的 QwenPaw 插件。**只做三件事**：让宿主原生 Skills 页认得这个市场、给自己的侧边栏页面提供 HTTP 路由、（M3）渲染那个页面。安装、校验、扫描、落盘全由宿主完成。

```bash
python -m pytest          # 64 个测试，不需要宿主、不需要网络
```

## 装机

源码目录直接装（开发机）：

```bash
qwenpaw plugin install ./plugin        # 或 --force 覆盖更新
qwenpaw plugin list                    # ID: offline-market
```

插件目录是**副本**：改完源码要 `--force` 重装才生效（`~/.qwenpaw/plugins/offline-market/`）。

### 离线 zip 包（Linux x64 内网机器）

CI（`plugin-package.yml`）和本地都能产出同一个包：

```bash
python3 deploy/scripts/build-plugin-zip.py
# -> dist/offline-market-plugin-<ver>-linux-x86_64.zip(.sha256)
```

包结构是 `offline-market/`（一个顶层目录，里面是 `plugin.json`、`backend.py`、
`frontend/`、`offline_market/`）；纯 Python + stdlib，没有编译产物、
没有 `requirements.txt`，所以包里没有任何跟 CPU/发行版绑定的东西。

```bash
sha256sum -c offline-market-plugin-<ver>-linux-x86_64.zip.sha256
unzip offline-market-plugin-<ver>-linux-x86_64.zip
qwenpaw plugin install ./offline-market --force
qwenpaw plugin list                    # ID: offline-market
```

**为什么先解压**：QwenPaw 2.2.2 的 `plugin install` 只有在 App **正在运行**
时才把本地 zip 传给 `POST /api/plugins/upload` 现场解压；App 停着时它走的是
"把参数当目录"的分支，直接 `plugin install xxx.zip` 会报 `plugin.json not found`。
内网首次安装通常是停机状态，所以统一按"解压 → 装目录"来。
（App 已运行时也可以 `qwenpaw plugin install <zip>` 热装，不用解压。）

## 运行时环境变量（每个租户 runtime 都必须有）

> 不想配 env / config.json？改 `offline_market/runtime.py` 顶部的硬编码入口
> `HARDCODED_MARKET_URL = "http://<market>"`，重装插件即可。非空时它优先级最高，
> 会忽略 `config.json` 的 `market_url` 和 `OFFLINE_MARKET_URL`。

| 变量 | 必需 | 说明 |
| --- | --- | --- |
| `OFFLINE_MARKET_URL` | 可选，默认 `http://localhost:8080` | 市场后端地址，如 `http://market.internal:8080`（也可用插件配置项 `market_url`，它优先）。**runtime 在容器里时，这个地址必须"从容器内可达"**（`host.docker.internal:8080` 或同网络容器名），见 `docs/OPS.md` §2.5 |
| `QWENPAW_TENANT_ID` | ✅（Hub 托管时自动注入） | 租户身份唯一来源；为空则市场不可用（fail-closed） |
| `QWENPAW_SKILL_SCAN_MODE` | ✅ 必须是 `block` 或 `warn` | `off` 则本插件拒绝服务，理由见下 |
| `OFFLINE_MARKET_TENANT` | 仅单机部署 | 显式指定租户，身份退化为配置字符串 |
| `OFFLINE_MARKET_TIMEOUT` | 可选，默认 10s | 访问后端的超时 |

## 扫描闸门：接受 `block` 和 `warn`，只拒绝 `off`

`block` 和 `warn` 都意味着扫描器真的扫了内容：`block` 拦下安装，`warn` 扫描、
记日志、然后照常安装。`off` 则什么都不扫，任何东西都能被装进 runtime。

所以 `available()` 只检查"扫描器有没有开"：模式不是 `block` / `warn` 就返回
`(False, 原因)`，市场**拒绝服务**而不是静默分发代码。宿主会把这句原因原样显示给用户：

```
技能市场已暂停分发：QwenPaw 技能扫描被关闭（当前 off，来源 config）。
```

读不到 `config.json` 时也照样拒绝（无法确认扫描是否开启）。

## 与宿主的契约（2.2.2 实测）

```python
class OfflineMarketProvider:          # MarketProvider + MarketInstallProvider
    key = "offline-market"
    label = "离线技能市场"
    supports_browse = True
    def available(self) -> tuple[bool, str | None]     # 非阻塞：不联网
    async def search(self, query, limit, page)         # -> (results, has_more, total)
    def matches_url(self, url) -> bool                 # 不联网
    async def fetch_bundle(self, url, requested_version)  # -> ({"name","files"}, resolved_url)
```

注册：`api.register_market_provider(provider, priority=5)`。

注意（都踩过）：

- `search()` 不能声明 `lang/category` 之类的额外参数，宿主会按签名**静默丢弃**。
- `MarketResult.source` 必须等于 `provider.key`。
- `available()` 在每次 provider 列表和每次搜索时都会被调用，**禁止联网/阻塞**；网络失败只记录到内部健康缓存，60 秒内通过 `available()` 的 reason 反映出去。
- 同步 `urllib` 调用放在 `asyncio.to_thread` 里，避免阻塞宿主事件循环。
- 插件包目录里的相对导入（`from offline_market.x import y`）在宿主的 loader 下可用（实测），无需摊平成一个文件。

## 前端页面（M3）

侧边栏「**技能市场（内网）**」，位置 `primary.agentScoped`、紧跟宿主内置的 `core.marketplace`；
路由 `/offline-market`。四个页签：

| 页签 | 内容 |
| --- | --- |
| 市场 | 搜索 + 表格（描述/可见性/发布者/版本数）+ 安装按钮（可取消，跟随宿主任务）；**已装在本机的技能显示灰色「已安装」** |
| 发布 | 名称/标识符/版本/可见性 + **技能压缩包（.zip）上传**，提交前按 `/limits` 预检；**发布成功后市场列表自动刷新一次** |
| 我的发布 | 撤回 / 恢复上架；被管理员下架的显示"无法自助恢复" |
| 已安装 | 从本市场安装的技能及其市场状态：正常 / 已撤回 / 已下架（含原因） |

无构建步骤：`frontend/market.js` 是普通 ES module，React/antd 从 `window.QwenPaw.host` 取。
端到端验证脚本：`spikes/m3_verify.py`（Playwright，覆盖菜单→安装→发布→下架后徽标）。

市场页的「已安装」判定和「已安装」页同源但更省：宿主的 `/skills` 列表只给展示名，
所以先取一次列表，再**只对市场行里出现的名字**查 `/skills/{name}` 的 `installed_from`
（hub 代理后面单条 2~3 秒），不用把整个技能目录都 fan-out 一遍；本页安装成功后直接
置灰该行，不必刷新。

### 发布者名字：从租户 id 到登录名

市场只认租户 id（`personal-179b…`），拿它当“发布者”不好看。页面因此在发布前调一次
`GET /api/auth/verify`（带 console 自己的 Bearer token，hub 模式和单机模式都答 `{username}`），
把登录名作为 `publisher_name` 随发布请求带给插件路由，再由市场按租户存下来：

- 列表“发布者”列显示 `owner_name`；没名字（功能上线前发的、或运行时没登录名）就回落显示租户 id。
- 自己发布的行多一个「（我）」：和 `/offline-market/config` 里的 `tenant` 比 `owner_tenant` 得出。
- 发布页会先告诉你会记下哪个名字（取不到时写明会退化成租户 id）。
- 名字只是**标签**：属主/撤回/下架判定全部在服务端用租户头，填什么都不影响权限。

主机内置市场（宿主自己的 marketplace）里的作者字段也一起用上了 `owner_name`。

### 浏览器控制台：banner + 调试日志

加载时无条件打印一段 ASCII banner（版本、页面名、开关提示）。日志默认静默，打开任意一层即可，
**不用重装插件**：

| 方式 | 生效范围 |
| --- | --- |
| 控制台执行 `OfflineMarketDebug.enable()` | 当前标签页，立即生效 |
| 同上 + 刷新（写 `localStorage["offline-market:debug"]="1"`） | 本浏览器持久生效；`disable()` 关掉 |
| 模块加载前设 `window.QWENPAW_OFFLINE_MARKET_DEBUG = true` | 覆盖 localStorage（可为 `false` 强制静默） |

打开后能看到：宿主就绪耗时与轮询次数、`apiUrl()` 的解析结果、每次请求的
方法/路径/状态码/耗时/agent 头/响应体摘要、超时与闸门/限额告警、安装任务每次轮询、
发布/撤回/恢复的结果，以及「已安装」页的详情 fan-out 进度。前缀统一是
`[offline-market]`（debug 行是 `[offline-market]·debug`），筛选关键词即可只看本插件。

### 发布用的压缩包规则

页面上传一个 `.zip`，插件在 runtime 内解压成后端的 `{路径: 内容}` 映射：

- **文本与二进制**：UTF-8 文本原样传输；`docx`/`pptx`/图片等非 UTF-8 文件自动转成
  `{"base64": "..."}`，发布、存储、下载都不再拒绝（后端会按解码后的真实字节算限额与 sha256）
- **安装限制**：QwenPaw 2.2.2 的 hub 安装把 `files` 的每个值都当文本写盘，因此含非 UTF-8
  附件的技能无法通过宿主的「市场安装」落盘；插件会明确报 `binary_bundle`，不会写出损坏文件
- 根目录必须有 `SKILL.md`；如果压缩的是整个技能文件夹，唯一的顶层目录会被自动去掉
- 自动跳过 macOS/Windows 的系统垃圾：`__MACOSX/`、`.DS_Store`、`Thumbs.db`、`desktop.ini`
- 拒绝：`..`/绝对路径/盘符、符号链接、加密条目、重名条目
- 安全上限（防 zip 炸弹，不等于分发限额）：压缩包 16 MiB、解压后 16 MiB、单文件 4 MiB、1000 个文件；真正的限额仍由后端按配置执行

解压走 stdlib `zipfile`，不引入任何依赖；上传用原始字节 body 而不是 multipart，避免给宿主 venv 加 `python-multipart`。

## 自己的 HTTP 路由（挂在 `/api/offline-market`）

浏览器 → 本插件 → 市场后端。**浏览器永不直连后端**：租户身份只存在于 runtime 环境变量里，交给浏览器等于交给用户随便改。

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/config` | 市场地址、租户、扫描闸门状态、阻塞原因（页面顶部横幅用） |
| GET | `/limits` | 透传后端限额，页面据此做预检 |
| GET | `/skills?q=&page=&limit=` | 浏览/搜索 |
| GET | `/mine` | 我发布的（含已撤回） |
| GET | `/skills/{slug}/status` | `active` / `withdrawn` / `taken_down` |
| GET | `/lookup?names=a,b` | 展示名 → `slug` + 状态（含已下架），"已安装"页用 |
| POST | `/skills` | 发布（JSON）：`{name, version, files:{路径:文本 或 {"base64":...}}, slug?, visibility?}`，供 API/脚本使用 |
| POST | `/skills/zip?name=&version=&slug=&visibility=` | 发布（压缩包）：请求体是 `.zip` 原始字节，插件解压后把文本/二进制分别编码转发 |
| POST | `/skills/{slug}/withdraw` | 撤回（可恢复） |
| POST | `/skills/{slug}/restore` | 恢复（被管理员下架的不可恢复，409） |

安装**不在这里**：页面直接调宿主 `POST /api/skills/hub/install/start`（`provider_key: "offline-market"`, `bundle_url`），然后轮询 `/status/{task_id}`。

## 结构

```
plugin.json                  # 2.2.2 manifest：type=hub, entry.backend, qwenpaw_version.min=2.2.2
backend.py                   # 导出 plugin 对象 + register(api)
offline_market/
  runtime.py                 # 配置解析 + 扫描闸门（纯本地、可测）
  client.py                  # stdlib urllib 客户端，transport 可注入
  bundle.py                  # .zip -> {路径: 字节} -> 后端 JSON（文本/base64）：路径/符号链接/大小闸门
  provider.py                # MarketProvider / MarketInstallProvider 实现
  routes.py                  # 自用 HTTP 路由（fastapi 延迟导入）
tests/                       # 64 个测试：闸门、映射、错误透传、传输层异常、路由、解压
```

## 实测链路（本机，backend 8099 + qwenpaw 8123）

| 步骤 | 结果 |
| --- | --- |
| `plugin install ./plugin` | 校验通过，`plugin list` 出现 `offline-market` |
| `GET /api/market/providers` | `{"key":"offline-market","label":"离线技能市场","available":true}` |
| `POST /api/market/search` | 返回 tenant-a 发布的 `demo` |
| `POST /api/skills/hub/install/start` | `completed`，`installed_from: offline-market`，两个文件落地 |
| `POST /api/offline-market/skills`（tenant-b 发布私有） | 201，`/mine` 可见 |
| 泄漏检查：tenant-a 列表 / 下载 b-private | 列表无该条目 / HTTP 403 |
| 把扫描模式设为 `off` 重启 | provider `available:false` + 中文原因，搜索返回 `MarketSearchError`（不是静默空列表） |
| 浏览器：菜单→市场→点安装 | 宿主任务完成，页面提示"已安装" |
| 浏览器：发布页上传 zip 压缩包并提交 | 201，出现在"我的发布" |
| 浏览器：管理员下架后看"已安装" | 出现红色「已下架」徽标 |
