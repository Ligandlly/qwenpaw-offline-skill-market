# 运维手册（OPS）

目标版本：**QwenPaw 2.2.2**。这套东西由三部分组成：市场后端（本仓库 `backend/`）、QwenPaw 插件（`plugin/`）、以及租户 runtime 的两项环境/配置要求。

```
租户浏览器
   │  (同一个 console，已登录)
   ▼
租户 runtime（QwenPaw 2.2.2 + offline-market 插件）
   │  X-Tenant-Id = QWENPAW_TENANT_ID
   ▼
市场后端（Rust + SQLite，内网）
```

**浏览器永不直连后端**：租户身份只存在于 runtime 进程环境里，插件代发。

---

## 1. 部署后端

### 1.1 容器

```bash
cp deploy/.env.example deploy/.env     # 必改：ADMIN_PASSWORD、MARKET_PUBLIC_URL
vi deploy/.env
docker compose -f deploy/docker-compose.yml up -d      # 不需要 --env-file
curl -s http://localhost:8080/health
```

**用 ghcr 上的预构建镜像**（省掉本地编译，内网机器尤其省事）：

```bash
docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.ghcr.yml up -d
```

镜像由 `.github/workflows/backend-image.yml` 推：`ghcr.io/ligandlly/qwenpaw-offline-skill-market`
（`:latest` = main 分支，`:v0.1.0` = tag）。完全离线的机器连 ghcr 也访问不了时，
在有网的机器上 `docker pull` + `docker save`，把 tarball 拷进去 `docker load`，
再用 `docker-compose.ghcr.yml` 把 `image:` 改成那个本地 tag。

`.env` **必须与 `docker-compose.yml` 同目录（`deploy/`）**：compose 的 `env_file: .env`
与 `${VAR}` 插值都以 compose 文件所在目录为基准（`docker compose config` 实测：
`deploy/.env` 一份文件同时供给插值和 `env_file`）。写到仓库根目录会直接起不来，
报 `env file .../deploy/.env not found`。

`MARKET_PUBLIC_URL` **必须**是租户 runtime 视角下可达的地址（内网 IP / 内网 DNS），
它会被写进每个技能的 `install_url`，而且必须与 runtime 的 `OFFLINE_MARKET_URL`
**逐字符相同**（见 §2.1 的不变式）。写错的症状不是"下载失败"，而是安装请求在宿主侧
就被拒：

```
URL does not belong to Hub 'offline-market'
```

改完**只需重启后端**：`install_url` 每次查询现算（`backend/src/api.rs` 的 `row_to_skill`）、
不落库，不需要重新发布技能，也不需要动数据库。

### 1.2 裸二进制

```bash
install -m0755 offline-market-backend /opt/offline-market/
install -d /var/lib/offline-market /etc/offline-market
cp deploy/.env.example /etc/offline-market/env && vi /etc/offline-market/env
# /etc/offline-market/env 里加：MARKET_DB=/var/lib/offline-market/market.sqlite3
cp deploy/systemd/offline-market.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now offline-market
```

二进制是静态链接 + bundled SQLite，运行期零依赖（不需要 libsqlite3、不需要 Python）。

`MARKET_PUBLIC_URL` 同样只在这里配，**且只在进程启动时读一次**（`backend/src/config.rs`）：

```bash
# POSIX 临时直跑
MARKET_PUBLIC_URL=http://10.137.157.214:8080 ./offline-market-backend
# Windows cmd
set MARKET_PUBLIC_URL=http://10.137.157.214:8080 && offline-market-backend.exe
```

### 1.3 管理员账号

- 空库首次启动：用 `ADMIN_USER` / `ADMIN_PASSWORD` 创建。
- **没给 `ADMIN_PASSWORD`**：随机生成并只打印一次到 stdout（`docker logs` 可见）。
- 库非空后再改这些环境变量**不会**改密码（需要直接操作 `admin_users` 表）。

---

## 2. 把配置送进每个租户 runtime（Hub 接线）

这是整套部署最容易踩坑的一步，两个变量的路完全不同。

### 2.1 `OFFLINE_MARKET_URL`：走 hub credentials

不配也能用：插件默认指向 `http://localhost:8080`（与 `deploy/docker-compose.yml` 的
单机部署一致）。多机部署才需要把真地址送进去。

Hub 的 `PUT /api/hub/credentials` 会把凭据投影成 runtime 环境变量（只要名字不被
`runtime_credential_name_allowed` 拦掉）：

```bash
curl -X PUT http://<hub>/api/hub/credentials \
  -H 'content-type: application/json' -b <admin-cookie> \
  -d '{"scope":"tenant","name":"OFFLINE_MARKET_URL","value":"http://10.137.157.214:8080"}'
```

`scope` 只有两个合法取值（`qwenpaw/hub/control_app.py` 的 `validate_credential_scope`）：

- `"tenant"`：该租户的**所有** runtime 都生效，批量部署用这个；
- `"runtime:<runtime_id>"`：只对一个 runtime 生效，并覆盖 `tenant` 那一层。

写成 `"scope":"runtime"` 会被拒（400 `Invalid credential scope`）——必须带 runtime id。
查询只能看到元数据：`GET /api/hub/credentials?scope=tenant`（没有明文读接口）。

改动在**租户下次启动 runtime 时生效**：凭据只在 runtime 启动时投影成环境变量，热改无效。
也可以在插件配置里填 `market_url`，但它**优先级高于** `OFFLINE_MARKET_URL`（`plugin/offline_market/runtime.py`），
逐个租户配、批量部署不划算。

两者都不想配（比如内网一次性打包）：直接改插件源码 `offline_market/runtime.py` 顶部的
`HARDCODED_MARKET_URL = "http://<market>"` 再重装插件。非空时它**优先级最高**，
`config.json` 与 `OFFLINE_MARKET_URL` 都会被忽略；留空则维持常规顺序。
生效地址可看 `GET /api/offline-market/config` 的 `market_url`。

#### 不变式：`MARKET_PUBLIC_URL` ≡ `OFFLINE_MARKET_URL`

两处地址必须**逐字符相等**（两边都会 trim 结尾 `/`；`http`/`https`、内网 IP/内网 DNS、
端口、大小写都算差异，不做 DNS 归一化）。

这个地址必须是 **runtime 视角**能访问的：`fetch_bundle` 在 runtime 进程里发请求，容器里的
`localhost` 是容器自己；浏览器能不能访问无关紧要。

为什么必须一致：宿主在安装前做归属校验 `provider.matches_url(bundle_url)`,
而 `bundle_url` 就是后端按 `MARKET_PUBLIC_URL` 现算出来的 `install_url`；插件只做前缀
比对（`plugin/offline_market/provider.py` 的 `matches_url`），不一致就在这一步被拒：

```
URL does not belong to Hub 'offline-market'        # qwenpaw/.../skill_system/hub.py
```

真正的下载走插件自己的 `OFFLINE_MARKET_URL`，`install_url` 里的 host 只充当身份标记。

### 2.2 `QWENPAW_SKILL_SCAN_MODE`：**注入不进去**，只能改 config.json

`src/qwenpaw/utils/runtime_environment.py` 里：

- `system_environment()` 只继承 `LANG/LC_ALL/LC_CTYPE`（**hub 进程的环境变量不会传下去**）；
- `_CONTROL_PREFIXES` 含 `QWENPAW_`，所以 credentials 投影也会把
  `QWENPAW_SKILL_SCAN_MODE` 挡掉。local / docker 两个 provisioner 都一样。

只能落到每个 runtime 的 `config.json`（`$QWENPAW_WORKING_DIR/config.json`）：

```bash
python3 deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes 目录> --dry-run
python3 deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes 目录>
# 改完重启对应 tenant runtime
```

脚本会写 `security.skill_scanner.mode = "block"`，原文件备份为 `config.json.bak`。

**不想要 `warn`**：`block` 才会让安装失败且磁盘无残留，`warn` 只是扫出
CRITICAL、记一条日志、然后**照常安装并启用**。插件两者都接受，但推荐 `block`。

**只有 `off` 会被插件拒绝**：`off` 不扫任何内容，插件在这种情况下会拒绝服务
（`available:false` + 中文原因），所以漏配的表现是"技能市场不可用"，
而不是"静默分发恶意代码"。

### 2.3 安装插件

两条路，按机器状态选。**App 没跑时只能用目录**（2.2.2 不会解压本地 zip）：

```bash
# A. App 已停止 / 内网首次安装：解压出 zip，再装目录
sha256sum -c offline-market-plugin-<ver>-linux-x86_64.zip.sha256
unzip offline-market-plugin-<ver>-linux-x86_64.zip
qwenpaw plugin install ./offline-market --force
qwenpaw plugin list                    # 期望看到 ID: offline-market

# B. App 正在运行：zip 直接热装（走 POST /api/plugins/upload）
qwenpaw plugin install offline-market-plugin-<ver>-linux-x86_64.zip --force

# C. 只有源码目录
qwenpaw plugin install ./plugin --force
```

zip 来自 GitHub Release / `plugin-package.yml` 的 artifact，或本地
`python3 deploy/scripts/build-plugin-zip.py`。插件是纯 Python + stdlib，
包里没有平台相关的二进制，`linux-x86_64` 只标明目标机器。

插件目录是副本：更新插件必须 `--force` 重装，然后重启 runtime。

### 2.4 换机器 / 换 IP / 换 DNS 名 / 换端口

整套东西只有两处地址，且必须一致（§2.1 的不变式）。照顺序做：

1. 先定一个 **runtime 视角**可达的规范地址（内网 IP 或内网 DNS，二选一，别混用）。
2. **后端侧**：改 `deploy/.env` 的 `MARKET_PUBLIC_URL` →
   `docker compose -f deploy/docker-compose.yml up -d` 重建容器；
   裸机则改 `/etc/offline-market/env` 后 `systemctl restart offline-market`。
3. **runtime 侧**：写 hub credentials（`scope:"tenant"` 一次覆盖全租户），见 §2.1。
4. **重启该租户的 runtime**（不重启不生效）。
5. 验收三条：

```bash
# ① 后端广告的地址已变（前缀应与你配的逐字符一致）
curl -s -H "x-tenant-id: <租户id>" http://<新地址>:8080/api/v1/skills?limit=1 \
  | grep -o '"install_url":"[^"]*"'
# ② runtime 能把新地址走通（在 runtime 容器里执行）
wget -qO- http://<新地址>:8080/health
# ③ 市场页横幅的"后端"URL 是同一个地址，然后实际装一个技能
```

两个坑：

- 某个租户若在**插件配置**里填过 `market_url`，它压过 credentials，必须逐个改（§2.1）。
- 换地址等于换机器：防火墙/DNS/端口映射要一并放通；`QWENPAW_` 前缀依然注入不进去（§2.2）。

### 2.5 runtime 跑在容器里时：地址必须"从容器内可达"

runtime（租户）在 Docker 里时，"可达"是**容器视角**的可达，不是宿主机视角。
宿主机上 `curl` 通，不代表容器里 `curl` 通——插件是在 runtime 进程里发请求的。

实测（Docker Desktop 29.6 / Windows 11，宿主 `10.137.157.214`，后端容器发布 `0.0.0.0:8080`）：

| 从哪访问 | `10.137.157.214:8080` | `host.docker.internal:8080` | `127.0.0.1:8080` | `172.17.0.1:8080` |
| --- | --- | --- | --- | --- |
| 宿主机 | **超时** | 超时 | 200 | — |
| runtime 容器 | **超时** | 200 | 连接被拒（容器自己的 loopback） | 200 |

Docker Desktop 把发布端口只挂在 `127.0.0.1` 和网桥地址上；**宿主机的局域网 IP 上根本没有
监听**，打到那个地址的包没人接——表现为**超时**（不是"拒绝连接"），插件等满 10 秒后回
`502 Bad Gateway`，页面横幅写"市场后端不可达"。

顺带两个同源坑：

- 宿主机上用 `http://127.0.0.1:8080`，**别用 `http://localhost:8080`**：Windows 上
  `localhost` 优先解析 `::1`，而发布端口可能只挂在 v4 上，`localhost` 会超时。
- `10.137.157.214` 是 WLAN 的 **DHCP** 地址，会变。要用它就得做 DHCP 保留或改静态。

三选一（**两个地方都要写**：`MARKET_PUBLIC_URL` 与 runtime 侧地址，见 §2.1 的不变式）：

1. **`http://host.docker.internal:8080`** —— Docker Desktop 提供的宿主机别名，容器内必然可达。
   本机 hub 自己的 `QWENPAW_HUB_MODEL_URL` 就是这么配的。Linux 上需要
   `--add-host=host.docker.internal:host-gateway`。
2. **`http://offline-market:8080`** —— 把后端容器接进 runtime 所在网络，用容器名。
   不依赖任何宿主机 IP，最稳：
   ```bash
   docker network connect <hub 网络名> offline-market
   ```
   compose 里长期化（`<hub 网络名>` 是 hub 自己 compose 建的网络）：
   ```yaml
   networks:
     default:
       external: true
       name: <hub 网络名>
   ```
3. **`http://<内网 IP>:8080`** —— 只有当宿主机与容器确实在同一内网、且该 IP 上真的有人
   监听时才成立。Docker Desktop 上"发布到 `0.0.0.0`"**不等于**"局域网 IP 上有监听"。

宿主机自己访问后端（管理员调 `/admin/api/*`）：用 `http://127.0.0.1:8080`。

改完地址后按 §2.4 的顺序走（后端改 env → 重建容器 → runtime 侧改地址 → 重启 runtime），
并用 preflight 从**容器内部**复验一次（§3 的 `--runtime-container`）。

---

## 3. 验收

```bash
# 后端自检（自带临时库，不碰生产数据）
python3 deploy/scripts/e2e.py --binary bin/offline-market-backend

# 线上部署（需要 QwenPaw 已运行、插件已装、env 已配）
python3 deploy/scripts/preflight.py --market-url http://market.internal:8080 \
    --admin-password "$ADMIN_PASSWORD" \
    --runtimes-dir <hub runtimes> \
    --app-url http://127.0.0.1:8123 --qwenpaw "$(command -v qwenpaw)"

# runtime 在容器里时，再加这两项（--public-url 是后端的 MARKET_PUBLIC_URL；
# --runtime-container 让 preflight 进容器里探一次，避免"宿主机通、容器不通"）
python3 deploy/scripts/preflight.py --market-url http://127.0.0.1:8080 \
    --public-url http://host.docker.internal:8080 \
    --runtimes-dir <hub runtimes> --runtime-container <runtime 容器名>

python3 deploy/scripts/e2e.py --skip-a \
    --app-url http://127.0.0.1:8123 --market-url http://market.internal:8080 \
    --tenant <租户id> --admin-password "$ADMIN_PASSWORD"
```

详细标准见 `docs/ACCEPTANCE.md`。

> `preflight.py` 现在会校验 §2.1 的不变式：`install_url` 前缀 == `MARKET_PUBLIC_URL`，
> 且 runtime `config.json` 里的 `plugins.offline-market.market_url` 与它逐字符相同。
> 没有可比对的技能时第一项是 WARN（后端还没有任何技能），换地址后请手工跑一遍 §2.4 的第 ① 条。

---

## 4. 日常运维

| 任务 | 做法 |
| --- | --- |
| 发布技能 | 租户在「技能市场（内网）→ 发布」上传技能 `.zip`（根目录或唯一顶层目录含 `SKILL.md`）；版本不可变，同一 slug 换版本号 |
| 撤回自己发的 | 我的发布 → 撤回；可恢复 |
| 管理员下架 | 后端 `POST /admin/api/skills/{slug}/takedown {"reason":"..."}`；整包下架、禁止再分发、属主不能自恢复 |
| 恢复上架 | `POST /admin/api/skills/{slug}/restore` |
| 调限额 | 改环境变量后**重启后端**（限额只认 env，这是 PLAN 的既定选择） |
| 看审计 | `GET /admin/api/audit?limit=200`（只到租户粒度，无按人） |
| 备份 | 停机或 `sqlite3 market.sqlite3 ".backup /backup/market-$(date +%F).sqlite3"`；包体也存在库里，一个文件就是全部 |
| 恢复 | 停后端 → 覆盖 sqlite 文件 → 启动；`-wal`/`-shm` 一并清掉 |
| 升级后端 | 换二进制/镜像，重启；schema 目前是 `CREATE TABLE IF NOT EXISTS`，无迁移步骤 |
| 升级插件 | `unzip <新 zip>` → `qwenpaw plugin install ./offline-market --force`（App 运行时可直接 `plugin install <新 zip> --force`）后重启 runtime |

---

## 5. 事故处置

**A. 恶意/错误技能已下架，但有人已经装了**
下架只停止分发，不会回收已落地的副本（PLAN 里明确排除了远程吊销）。处置：
1. 下架并在审计里记原因；
2. 定位装了它的租户：查各自 runtime 的 `workspaces/*/skills/`（或从「已安装」页看「已下架」徽标）；
3. 人工删除目录（`rm -rf <workspace>/skills/<name>`）并重启该 runtime。

**B. 误下架**
管理员 `restore` 即可，属主不需要重新发布。若属主先尝试自恢复会得到 409（设计如此）。

**C. 疑似有人在市场里投毒**
1. 先下架（止血）；
2. 用 `GET /admin/api/audit` 找 `publish` 的 `actor`（`tenant:<id>`）与时间；
3. 该租户名下的其它技能一并审查；
4. 需要的话拉黑该租户（当前版本没有封禁接口，需在反代/网络层限制）。

**D. 市场不可用**
先看后端日志里**有没有收到这次请求**（`docker logs offline-market` / `journalctl -u offline-market`，
每条请求一行，含方法、路径、租户、状态、耗时；级别用 `MARKET_LOG=debug` 看得更细）：

- 一条都没有 → 请求根本没到后端（地址 / 网络 / 防火墙），见下面几条；
- 有 `401` → 插件没带 `X-Tenant-Id`（runtime 不是 hub 托管的）；
- 有 `404` → 路径不对（后端版本与插件版本不匹配）。

再看插件给的 reason（市场页顶部横幅 / `/api/market/providers` 的 `reason` 字段）：
- 提到"扫描被关闭"/`off` → 扫描模式是 `off`，跑 `enforce-scan-mode.py` 或改成 `warn`；
- 提到 `QWENPAW_TENANT_ID` → 该 runtime 不是 hub 托管的（也没有 `OFFLINE_MARKET_TENANT`）；
- 走到 `http://localhost:8080` 上去了 → 没配地址，而市场不在本机 8080；
- 提到"不可达"/"超时" → 消息里现在**带具体地址**
  （`市场后端不可达（http://host.docker.internal:8080）：timed out`），照着这个地址
  **在 runtime 容器里**试一次：
  ```bash
  docker exec <runtime 容器> curl -sS -m 8 http://<横幅里的地址>/health
  ```
  宿主机上能通、容器里不通 → 见 §2.5（多半是"用了宿主机局域网 IP"）；
- 市场能用、但点安装报 `URL does not belong to Hub 'offline-market'` → 后端
  `MARKET_PUBLIC_URL` 与 runtime 的 `OFFLINE_MARKET_URL` 不一致，见 §1.1 与 §2.4，
  用下面这条确认后端到底在广告哪个地址：

```bash
curl -s -H "x-tenant-id: <租户id>" http://<市场地址>:8080/api/v1/skills?limit=1 \
  | grep -o '"install_url":"[^"]*"'
```

---

## 6. 已知边界（都写进了 PLAN 的风险表，不是 bug）

| 边界 | 影响 |
| --- | --- |
| 无远程吊销 | 下架治不了已安装副本，只能人工清理 |
| 无人工审核 | 任意租户可向全员发布；唯一自动防线是安装侧扫描（`warn` 只告警不拦） |
| 不限制文件类型 | `.so` / `.exe` 也能发布；扫描器对二进制基本无感 |
| `X-Tenant-Id` 是信任不是认证 | 后端必须只在内网暴露；能访问内网的人可以伪造租户 |
| 审计只到租户 | 一租户一人的前提下等于到人；多用户租户需要再加身份 |
| 限额改动需重启 | 环境变量是唯一来源 |
| 发布是文本文件 | 后端 bundle 契约是 `{路径: 文本}`：Web 端上传 `.zip` 后由插件解压成文本，非 UTF-8 文件会被明确拒绝（二进制附件目前发不了） |

---

## 7. 容量与位置

- 单机 SQLite，目标规模：几十租户 / 百量级技能。
- 单包上限默认 2 MiB、100 文件、单文件 256 KiB（`MARKET_MAX_*`）。
- 数据全在一个文件：`$MARKET_DB`（默认 `data/market.sqlite3`），容器里是 named volume `market-data`。
- 日志：容器 `docker logs offline-market`；裸机 systemd `journalctl -u offline-market`。
  级别由 `MARKET_LOG` 控制（默认 `info`），排查时改成 `debug` 并重启后端：
  每条请求一行（方法、路径、租户、状态、耗时），4xx 记 `WARN`、5xx 记 `ERROR`，
  `debug` 另记发布/撤回/下架、bundle 大小、闸门拒绝原因。详见 `backend/README.md` §日志。
