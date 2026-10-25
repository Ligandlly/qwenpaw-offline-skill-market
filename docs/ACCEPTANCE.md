# 验收标准

"做完了"由可执行的检查定义，不靠口头确认。全部命令都在离线环境可跑（除 `cargo`/`pytest` 首次装依赖）。

## 1. 单元与集成测试

```bash
# 后端：10 单测（闸门/slug/版本/lookup）+ 11 集成（隔离、不可变、下架语义、审计）
cd backend && cargo test

# 插件：29 测试（扫描闸门、provider 契约、错误透传、路由、不联网）
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

## 4. 浏览器端到端（UI）

```bash
# 需要 backend + qwenpaw app 已启动，且本机有 playwright/chromium
python3 spikes/m3_verify.py
```

覆盖 8 项：侧边栏入口（名称/位置）、四个页签、UI 点安装到完成、UI 上传 zip 发布、
我的发布、已安装显示"正常"、管理员下架后显示"已下架"、**0 console & page error**。

## 5. 部署自检（只读）

```bash
python3 deploy/scripts/preflight.py \
  --market-url http://market.internal:8080 --admin-password "$ADMIN_PASSWORD" \
  --runtimes-dir <hub runtimes> --app-url http://127.0.0.1:8123 \
  --qwenpaw "$(command -v qwenpaw)"
```

任一 FAIL 即部署不合格；`--admin-password` 缺失时该项降级为 WARN。

## 6. 明确不在验收范围内

- Windows 上的后端运行（PLAN 定的是 Linux x86_64；Windows 只用于开发验证）。
- 多节点/水平扩展（SQLite 单机）。
- 人工审核流程、智能体协同审核（PLAN 明示后续工作）。
- 远程吊销已安装副本（PLAN 明示不做）。
- 二进制文件发布（后端 bundle 是文本；zip 上传中的非 UTF-8 文件会被拒绝）。

## 7. 回归清单（改代码后至少跑这些）

| 改动位置 | 必跑 |
| --- | --- |
| `backend/src/limits.rs` | `cargo test`（闸门边界都在这） |
| `backend/src/api.rs` | `cargo test` + `e2e.py` phase A |
| `plugin/offline_market/*` | `pytest` + `e2e.py` phase B |
| `plugin/frontend/market.js` | `m3_verify.py` |
| `deploy/*` | `preflight.py` + `e2e.py` |
