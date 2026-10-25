# offline-market-backend

QwenPaw 2.2.2 离线技能市场的后端（Rust + axum + SQLite）。**不做安装**：bundle 校验、扫描、安装全部由 QwenPaw 宿主保留，这里只负责存储、分发、权限、审计。

```bash
cargo run                 # 默认 0.0.0.0:8080, data/market.sqlite3
cargo test                # 20 个测试（10 单测 + 10 集成）
```

首次启动若未设置 `ADMIN_PASSWORD`，会生成一次性管理员密码并打印一次。

### 直跑（`cargo run`）时怎么改配置

`cargo run` 只是把**当前 shell 的环境变量**透传给子进程：没有 `.env` 解析（依赖里没有 dotenv），
所有变量都只在进程启动时读一次。所以改 `MARKET_PUBLIC_URL` 就是在 shell 里带上再跑：

```bash
cd backend                       # 必须在 backend/ 下跑，见下面的 cwd 陷阱
MARKET_PUBLIC_URL=http://10.137.157.214:8080 cargo run
```

```cmd
:: Windows cmd
set MARKET_PUBLIC_URL=http://10.137.157.214:8080
cargo run
```

```powershell
# Windows PowerShell（只影响当前会话；setx 写用户级，需新开 shell）
$env:MARKET_PUBLIC_URL="http://10.137.157.214:8080"; cargo run
```

两个坑：

- **cwd 决定数据库**：`MARKET_DB` 默认是相对路径 `data/market.sqlite3`，相对于进程 cwd。
  在 `backend/` 里跑 → `backend/data/market.sqlite3`；在仓库根目录跑
  `cargo run --manifest-path backend/Cargo.toml` → 会新建一个**空库**（市场看起来清空了，
  还会重新生成一次管理员口令）。要么固定在 `backend/` 里跑，要么显式
  `MARKET_DB=<绝对路径>`。
- **端口**：默认 `0.0.0.0:8080`，先停掉已在跑的实例，否则 `cargo run` 报
  `Address already in use`。

改完**只需重启进程**：`install_url` 每次查询现算、不落库，不用重新发布技能。

## 环境变量（改任何一项都要重启，PLAN §1）

| 变量 | 默认 | 说明 |
| --- | --- | --- |
| `MARKET_BIND` | `0.0.0.0:8080` | 监听地址 |
| `MARKET_DB` | `data/market.sqlite3` | SQLite 路径（WAL） |
| `MARKET_PUBLIC_URL` | `http://localhost:8080` | 生成 `install_url`；必须是租户运行时**能访问到**的地址，且与每个 runtime 的 `OFFLINE_MARKET_URL` **逐字符相同**（见 `docs/OPS.md` §2.1）、否则安装报 `URL does not belong to Hub 'offline-market'` |
| `ADMIN_USER` | `admin` | 首个管理员用户名 |
| `ADMIN_PASSWORD` | 空 → 随机生成并打印 | 首个管理员密码；`admin_users` 非空后不再生效 |
| `MARKET_LOG` | `info` | 日志级别：`off`/`error`/`warn`/`info`/`debug`/`trace`（改完重启生效） |
| `MARKET_MAX_BUNDLE_BYTES` | `2097152`（2 MiB） | 单包总字节 |
| `MARKET_MAX_FILES` | `100` | 文件数 |
| `MARKET_MAX_FILE_BYTES` | `262144`（256 KiB） | 单文件字节 |

## 两个身份面

| 面 | 认证方式 | 谁在调用 |
| --- | --- | --- |
| 租户面 `/api/v1/*`, `/skills/*` | `X-Tenant-Id` 头（插件从 `QWENPAW_TENANT_ID` 转发） | QwenPaw 插件 → 本后端（浏览器**不能**直连，否则等于把身份交给用户） |
| 管理面 `/admin/api/*` | 用户名 + Argon2 密码 → 服务端 session cookie（12h） | 管理员浏览器 |

`X-Tenant-Id` 不是认证，是**内网信任**。后端必须部署在只有租户运行时能访问的网段。

### 发布者名字（`publisher_name` / `owner_name`）

租户身份永远是 `personal-<hex>` 这种 id，拿它当“发布者”没法看。所以：

- 发布时插件可以多带一个 `publisher_name`（它从 console 自己的 `/api/auth/verify` 取登录名），
  后端把名字写进 `tenants.display_name`，不是每个技能一行。
- 列表 / `mine` / admin 列表因此多一个 `owner_name`（旧数据为 `null`，由前端回落显示租户 id）。
- **只影响显示**：属主、撤回、下架判定全部只看 `owner_tenant`；名字可以被发布者随便填，
  也允许不填（不填时 `COALESCE` 保留上一次的名字，不会抹掉）。长度上限 64 字符，控制字符会被剔除。
- 老库启动时自动 `ALTER TABLE tenants ADD COLUMN display_name TEXT`（`db::migrate`，幂等）。

## 租户面

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/v1/limits` | 插件据此对齐预检阈值，避免前后不一致 |
| GET | `/api/v1/skills?q=&page=&limit=` | 市场列表：公开 + 自己的私有；已撤回/已下架不出现（带 `owner_name`） |
| GET | `/api/v1/skills/mine` | 自己的全部（含已撤回） |
| GET | `/api/v1/skills/{slug}/status` | `active` / `withdrawn` / `taken_down`，用于"已安装但被下架"标记 |
| POST | `/api/v1/skills` | 发布不可变版本；可选 `publisher_name`（展示用，见下） |
| POST | `/api/v1/skills/{slug}/withdraw` | 属主撤回（可恢复） |
| POST | `/api/v1/skills/{slug}/restore` | 属主恢复；被管理员下架的不可自恢复（409） |
| GET | `/skills/{slug}/{version}` | **`bundle_url`**，返回 `{name, version, files, source_url}`；`version=latest` 走指针 |
| GET | `/api/v1/skills/{slug}/{version}/bundle` | 同上（别名） |

发布请求体：

```json
{
  "name": "Demo Skill",
  "slug": "demo",
  "version": "1.0.0",
  "visibility": "public",
  "files": { "SKILL.md": "---\nname: demo\ndescription: ...\n---\n", "scripts/hello.py": "print(1)\n" }
}
```

## 管理面

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/admin/api/login` | `{username,password}` → `Set-Cookie: market_admin=…` |
| POST | `/admin/api/logout` | 使会话失效 |
| GET | `/admin/api/skills?q=&limit=` | 全部技能（含已撤回/已下架） |
| POST | `/admin/api/skills/{slug}/takedown` | `{reason}` → 整包下架、禁止再分发 |
| POST | `/admin/api/skills/{slug}/restore` | 撤销下架 |
| GET | `/admin/api/audit?limit=` | 审计日志（租户粒度） |

## 日志

后端用 `tracing` 输出到 stdout（容器 `docker logs`、裸机 `journalctl -u offline-market`）。
级别由 `MARKET_LOG` 控制，改完**重启生效**：

```bash
MARKET_LOG=debug cargo run    # 排查问题时用；默认 info
```

每条请求都会记一行（`backend/src/log.rs` 的 `request_log` 中间件）：

```
INFO request tenant=tenant-a method=GET path=/api/v1/skills status=200 ms=2.1
WARN request rejected tenant=- method=GET path=/api/v1/skills status=401 ms=0.1
```

- `INFO` = 2xx/3xx；`WARN` = 4xx（调用方的问题：缺 `X-Tenant-Id`、越权、版本冲突）；
  `ERROR` = 5xx（后端自己的问题，多半是数据库）。
- `debug` 级别另记业务细节：发布/撤回/下架、bundle 下载大小、闸门拒绝原因、审计写入等。
- 审计（`GET /admin/api/audit`）是**业务记录**，日志是**运维记录**，两者不互相替代。

## 镜像

```bash
# 本地构建（crates 走阿里云镜像源，慢/被墙时 --build-arg CARGO_MIRROR= 用官方源）
docker build -t offline-market:0.1 .

# 推送与导出（CI 里由 .github/workflows/backend-image.yml 做）
docker tag offline-market:0.1 ghcr.io/ligandlly/qwenpaw-offline-skill-market:v0.1.0
docker push ghcr.io/ligandlly/qwenpaw-offline-skill-market:v0.1.0
docker save -o offline-market-image.tar ghcr.io/ligandlly/qwenpaw-offline-skill-market:v0.1.0

# 离线机器：拷 tarball 过去
docker load -i offline-market-image.tar
```

两个容易踩的点：

- **时钟**：apt 会拿本机时钟比仓库元数据的 `Date`，构建机时钟不准就报
  `Release file ... is not valid yet`。Dockerfile 里已关掉这项检查（签名照验）。
- **`/data` 属主**：进程以 `nobody` 跑，`/data` 必须归 `nobody`，否则建不出 SQLite 库
  （`unable to open database file`）。named volume 初始化时会继承镜像里的属主。

## 语义（都有测试覆盖）

- **版本不可变**：同 `slug+version` 二次发布 → 409 `version_exists`；`latest` 只前进不回退。
- **slug 归属**：别人已占用的 slug 发布 → 403，不能劫持。
- **私有**：非属主列表不可见、下载 403、状态查询 404（fail-closed）。
- **撤回 vs 下架**：撤回 = 属主行为，市场消失、他人 410、属主仍可取；下架 = 管理员行为，整包 410（含属主），且属主不能自恢复、不能再发布新版本覆盖。
- **发布闸门**：必须是合法路径（无 `..`/绝对路径/反斜杠/盘符/控制字符）、必须含 `SKILL.md`、`SKILL.md` frontmatter 必须有非空 `name` 与 `description`、体积与文件数达标。
  - frontmatter 这条不是洁癖：宿主 `qwenpaw/agents/skill_system/store.py:validate_skill_content` 会拒绝安装缺 `description` 的包，**实测**发布成功但安装失败。前置拦截只是把错误提前。
  - 闸门**不限制文件类型**（PLAN §1 的决定）：`.so`/`.exe` 能进市场。真正的恶意代码防线在安装侧，见下。

## 部署要求：`QWENPAW_SKILL_SCAN_MODE` 不能是 `off`

插件接受 `block` 与 `warn`，只有 `off` 会被拒服务（`available:false` + 中文原因）。
但不想要恶意包就得上 `block`：

宿主扫描器默认 `warn`：实测一个含 `exec(input())` 的包被扫出 CRITICAL、记日志、然后**照样安装并启用**。
只有 `block` 才会 `failed: security_scan_failed` 且磁盘无残留。

所以每个租户运行时的环境里**推荐**带：

```
QWENPAW_SKILL_SCAN_MODE=block
```

检查工具：`spikes/s5_scan_mode_gate.py`（`warn` 退出码 1，可用作部署 preflight）。

## 与 PLAN 的一处实现差异

PLAN 原写"包存 `data/packages/{slug}/{version}/` 本地磁盘"。实现改成 **bundle 直接以 JSON 存在 SQLite `skill_versions.bundle_json`**：

- 单包上限 2 MiB、量级百个 → 单库几 GB 级别可控；
- 避免"磁盘文件与 DB 行不一致"这类最难排查的问题（备份 = 拷一个 `.sqlite3` 文件）；
- 代价：包体不能做流式发送，读取时整块进内存（2 MiB 上限下可接受）。

## 镜像

```bash
docker build -t offline-market:0.1 .
docker run -p 8080:8080 -v $PWD/data:/data \
  -e MARKET_DB=/data/market.sqlite3 \
  -e MARKET_PUBLIC_URL=http://market.internal:8080 \
  -e ADMIN_USER=admin -e ADMIN_PASSWORD='...' \
  offline-market:0.1
```

离线环境：在有网的机器上 `cargo build --release`（或 build 镜像），把 `target/release/offline-market-backend` 静态二进制带进内网即可；运行期零依赖（SQLite 已 bundled 进二进制）。
