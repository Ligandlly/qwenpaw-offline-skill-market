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
cp deploy/.env.example .env        # 必改：ADMIN_PASSWORD、MARKET_PUBLIC_URL
docker compose --env-file .env -f deploy/docker-compose.yml up -d
curl -s http://localhost:8080/health
```

`MARKET_PUBLIC_URL` **必须**是租户 runtime 视角下可达的地址（内网 IP / 内网 DNS），
它会被写进每个技能的 `install_url`。写错的症状：市场能看到技能、点安装报"下载失败"。

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

### 1.3 管理员账号

- 空库首次启动：用 `ADMIN_USER` / `ADMIN_PASSWORD` 创建。
- **没给 `ADMIN_PASSWORD`**：随机生成并只打印一次到 stdout（`docker logs` 可见）。
- 库非空后再改这些环境变量**不会**改密码（需要直接操作 `admin_users` 表）。

---

## 2. 把配置送进每个租户 runtime（Hub 接线）

这是整套部署最容易踩坑的一步，两个变量的路完全不同。

### 2.1 `OFFLINE_MARKET_URL`：走 hub credentials

Hub 的 `PUT /api/hub/credentials` 会把凭据投影成 runtime 环境变量（只要名字不被
`runtime_credential_name_allowed` 拦掉）：

```bash
curl -X PUT http://<hub>/api/hub/credentials \
  -H 'content-type: application/json' -b <admin-cookie> \
  -d '{"scope":"runtime","name":"OFFLINE_MARKET_URL","value":"http://market.internal:8080"}'
```

租户下次启动 runtime 时生效。也可以在插件配置里填 `market_url`（优先级更高），
但那要逐个租户配，批量部署用 credentials 更省事。

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

**为什么非要 block**：默认是 `warn`，实测一个含 `exec(input())` 的包会被扫出
CRITICAL、记一条日志、然后**照常安装并启用**。`block` 才会让安装失败且磁盘无残留。
插件在这种情况下会拒绝服务（`available:false` + 中文原因），所以漏配的表现是
"技能市场不可用"，而不是"静默分发恶意代码"。

### 2.3 安装插件

```bash
qwenpaw plugin install ./plugin        # 已装则加 --force
qwenpaw plugin list                    # 期望看到 ID: offline-market
```

插件目录是副本：更新插件必须 `--force` 重装，然后重启 runtime。

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

python3 deploy/scripts/e2e.py --skip-a \
    --app-url http://127.0.0.1:8123 --market-url http://market.internal:8080 \
    --tenant <租户id> --admin-password "$ADMIN_PASSWORD"
```

详细标准见 `docs/ACCEPTANCE.md`。

---

## 4. 日常运维

| 任务 | 做法 |
| --- | --- |
| 发布技能 | 租户在「技能市场（内网）→ 发布」上传；版本不可变，同一 slug 换版本号 |
| 撤回自己发的 | 我的发布 → 撤回；可恢复 |
| 管理员下架 | 后端 `POST /admin/api/skills/{slug}/takedown {"reason":"..."}`；整包下架、禁止再分发、属主不能自恢复 |
| 恢复上架 | `POST /admin/api/skills/{slug}/restore` |
| 调限额 | 改环境变量后**重启后端**（限额只认 env，这是 PLAN 的既定选择） |
| 看审计 | `GET /admin/api/audit?limit=200`（只到租户粒度，无按人） |
| 备份 | 停机或 `sqlite3 market.sqlite3 ".backup /backup/market-$(date +%F).sqlite3"`；包体也存在库里，一个文件就是全部 |
| 恢复 | 停后端 → 覆盖 sqlite 文件 → 启动；`-wal`/`-shm` 一并清掉 |
| 升级后端 | 换二进制/镜像，重启；schema 目前是 `CREATE TABLE IF NOT EXISTS`，无迁移步骤 |
| 升级插件 | `qwenpaw plugin install ./plugin --force` 后重启 runtime |

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
先看插件给的 reason（市场页顶部横幅 / `/api/market/providers` 的 `reason` 字段）：
- 提到 `block` → 扫描模式没设，跑 `enforce-scan-mode.py`；
- 提到 `OFFLINE_MARKET_URL` → 环境没注入；
- 提到 `QWENPAW_TENANT_ID` → 该 runtime 不是 hub 托管的；
- 提到"不可达" → 网络/DNS/防火墙，或 `MARKET_PUBLIC_URL` 写错。

---

## 6. 已知边界（都写进了 PLAN 的风险表，不是 bug）

| 边界 | 影响 |
| --- | --- |
| 无远程吊销 | 下架治不了已安装副本，只能人工清理 |
| 无人工审核 | 任意租户可向全员发布；唯一自动防线是安装侧的 block 扫描 |
| 不限制文件类型 | `.so` / `.exe` 也能发布；扫描器对二进制基本无感 |
| `X-Tenant-Id` 是信任不是认证 | 后端必须只在内网暴露；能访问内网的人可以伪造租户 |
| 审计只到租户 | 一租户一人的前提下等于到人；多用户租户需要再加身份 |
| 限额改动需重启 | 环境变量是唯一来源 |
| 发布是文本文件 | Web 上传按文本读；二进制附件目前发不了（走 API 也只能传文本） |

---

## 7. 容量与位置

- 单机 SQLite，目标规模：几十租户 / 百量级技能。
- 单包上限默认 2 MiB、100 文件、单文件 256 KiB（`MARKET_MAX_*`）。
- 数据全在一个文件：`$MARKET_DB`（默认 `data/market.sqlite3`），容器里是 named volume `market-data`。
- 日志：容器 `docker logs offline-market`；裸机 systemd `journalctl -u offline-market`。
