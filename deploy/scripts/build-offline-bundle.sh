#!/usr/bin/env bash
# 打一个可以拷进内网的离线包：后端静态二进制 + 插件 + 部署脚本 + 文档。
#
#   ./build-offline-bundle.sh            # -> dist/offline-market-<ver>-<os>-<arch>.tar.gz
#
# 内网那台机器只需要：解包 → 起后端 → qwenpaw plugin install ./plugin → 配 env。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
version="$(grep -m1 '^version' "$root/backend/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
name="offline-market-${version}-${os}-${arch}"
dist="$root/dist"
stage="$dist/$name"

echo "==> building release binary"
( cd "$root/backend" && cargo build --release --locked 2>/dev/null || cargo build --release )

binary="$root/backend/target/release/offline-market-backend"
[ -f "$binary" ] || { echo "构建产物不存在：$binary"; exit 1; }

echo "==> staging $stage"
rm -rf "$stage"
mkdir -p "$stage/bin" "$stage/deploy" "$stage/docs"

cp "$binary" "$stage/bin/offline-market-backend"
chmod +x "$stage/bin/offline-market-backend"

cp -r "$root/deploy/docker-compose.yml" "$root/deploy/.env.example" "$stage/deploy/"
cp -r "$root/deploy/systemd" "$root/deploy/scripts" "$stage/deploy/"
cp -r "$root/plugin" "$stage/plugin"
cp "$root/docs/OPS.md" "$root/docs/ACCEPTANCE.md" "$root/docs/AGENT_MARKET.md" "$stage/docs/"

# Local caches must never ship: they are host-specific and break "same commit,
# same package" expectations. The plugin zip builder already does this.
find "$stage" \( -name __pycache__ -o -name .pytest_cache -o -name .ruff_cache \) \
  -prune -exec rm -rf {} +
find "$stage" -name '*.pyc' -delete

cat > "$stage/README.txt" <<'TXT'
离线技能与智能体市场 —— 内网部署包
==================================

目录
  bin/offline-market-backend     后端（静态二进制，含 SQLite，无运行期依赖）
  plugin/                        QwenPaw 2.2.2 插件（qwenpaw plugin install ./plugin）
  deploy/docker-compose.yml      容器方式部署后端
  deploy/systemd/                裸二进制方式部署后端
  deploy/scripts/                自检 / e2e / 容器探针 / 扫描模式强制脚本
  docs/OPS.md                    部署与运维手册（先读这个；Docker Hub 见 §2.6）
  docs/ACCEPTANCE.md             验收标准与命令
  docs/AGENT_MARKET.md           智能体市场设计与契约

最短路径
  1) 起后端
       cp deploy/.env.example deploy/.env && vi deploy/.env   # 改 ADMIN_PASSWORD 与 MARKET_PUBLIC_URL
       docker compose -f deploy/docker-compose.yml up -d      # .env 必须与 compose 文件同目录
     或者裸二进制：见 deploy/systemd/offline-market.service
  2) 每个租户 runtime 开启技能扫描（block 推荐，warn 允许，off 拒绝）
       python3 deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes> --dry-run
       python3 deploy/scripts/enforce-scan-mode.py --runtimes-dir <hub runtimes>
  3) 装插件并配置市场地址
       qwenpaw plugin install ./plugin
       # 市场地址走 hub 的 credentials 注入 OFFLINE_MARKET_URL（QWENPAW_* 前缀无法注入），
       # 且必须与后端的 MARKET_PUBLIC_URL 逐字符相同（见 docs/OPS.md §2.1/§2.6）
  4) 验收
       python3 deploy/scripts/e2e.py --binary bin/offline-market-backend
       python3 deploy/scripts/e2e.py --skip-a --app-url http://<qwenpaw> --market-url http://<market> --tenant <租户>
       # Docker 管理的 Hub runtime（智能体）：需要 docker 权限
       python3 deploy/scripts/e2e.py --skip-a --skip-b \
         --runtime-container <runtime容器> --market-url http://<宿主机网关>:8080 --tenant <租户>
TXT

echo "==> packing"
( cd "$dist" && tar czf "$name.tar.gz" "$name" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256" )
rm -rf "$stage"
echo
echo "产物：$dist/$name.tar.gz"
echo "校验：$dist/$name.tar.gz.sha256"
