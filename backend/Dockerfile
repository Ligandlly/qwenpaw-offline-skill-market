# 构建与运行分离：build 阶段用 alpine 的 musl 工具链，产出的二进制是
# 静态链接的；运行阶段因此不需要任何 libc/apt 包，只装这 1 个文件。
#
# 构建需要网络（拉 crates）。内网/离线机器别在这里 build，直接用 ghcr
# 预构建镜像，或者 docker save / load 的 tarball（见 docs/OPS.md §1.1）。
FROM rust:1-alpine AS build
WORKDIR /src
# crates.io 直连慢/不稳时走镜像源（默认阿里云；传空串则用官方源）
ARG CARGO_MIRROR="https://mirrors.aliyun.com/crates.io-index/"
RUN if [ -n "$CARGO_MIRROR" ]; then \
      mkdir -p "$CARGO_HOME" && \
      printf '[source.crates-io]\nreplace-with = "mirror"\n\n[source.mirror]\nregistry = "sparse+%s"\n' \
        "$CARGO_MIRROR" > "$CARGO_HOME/config.toml"; \
    fi

# 依赖单独成层：先在空壳源码上把依赖编一遍。之后只改 src/ 时这一层直接
# 命中缓存，重建从「整棵依赖树 ~2 分钟」降到「只编本 crate ~1 分钟」。
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && : > src/lib.rs
RUN cargo build --release --locked 2>/dev/null || cargo build --release

COPY src ./src
# 关键：docker COPY 会把上下文里文件的 mtime 一起带过来（可能比刚才编依赖
# 更新的 dummy 产物还旧），cargo 按 mtime 判断新鲜度会直接跳过本 crate 的编译，
# 镜像里就带上了空壳二进制。touch 一下强制重编。
RUN find src -name '*.rs' -exec touch {} +
# --locked 用来保证依赖版本和 Cargo.lock 一致；锁文件被改动过时才退回自动更新。
RUN cargo build --release --locked 2>/dev/null || cargo build --release
# 符号表在线上零用（有结构化日志），删掉省几 MB。Cargo.toml 里已经配了
# strip = "symbols"，这里再兜一次，不依赖工具链版本。
RUN strip /src/target/release/offline-market-backend

# alpine 只为了 busybox 的 wget（compose 的 healthcheck 用它）和一个能进
# 去排障的 sh；/etc/passwd 里有 nobody。整套运行期依赖就这些。
FROM alpine:3.20
COPY --from=build /src/target/release/offline-market-backend /usr/local/bin/offline-market
# /data 要归 nobody：进程以 nobody 跑，卷初始化时会继承这里的属主，
# 否则 root 属主的 /data 建不出 SQLite 库（unable to open database file）。
RUN mkdir -p /data && chown nobody:nogroup /data
ENV MARKET_DB=/data/market.sqlite3
EXPOSE 8080
USER nobody
CMD ["offline-market"]
