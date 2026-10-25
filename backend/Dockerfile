# Build stage. Requires network for crates; for air-gapped targets build here
# and copy target/release/offline-market-backend into the image instead.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
# crates.io 直连慢/不稳时走镜像源（默认阿里云；传空串则用官方源）
ARG CARGO_MIRROR="https://mirrors.aliyun.com/crates.io-index/"
RUN if [ -n "$CARGO_MIRROR" ]; then \
      mkdir -p "$CARGO_HOME" && \
      printf '[source.crates-io]\nreplace-with = "mirror"\n\n[source.mirror]\nregistry = "sparse+%s"\n' \
        "$CARGO_MIRROR" > "$CARGO_HOME/config.toml"; \
    fi
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release --locked 2>/dev/null || cargo build --release

FROM debian:bookworm-slim
# 构建机时钟可能不准，apt 拿本机时钟比仓库元数据的 Date/Valid-Until，
# 时钟偏了就报 "Release file ... is not valid yet"。关掉这两项检查
# （签名仍然校验）。内网机器上构建镜像最常踩这个坑。
# wget 是 compose 的 healthcheck 用的（slim 基础镜像默认没有 wget/curl）。
RUN apt-get update -o Acquire::Check-Date=false -o Acquire::Check-Valid-Until=false \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/offline-market-backend /usr/local/bin/offline-market
# /data 要归 nobody：进程以 nobody 跑，卷初始化时会继承这里的属主，
# 否则 root 属主的 /data 建不出 SQLite 库（unable to open database file）。
RUN mkdir -p /data && chown nobody:nogroup /data
ENV MARKET_DB=/data/market.sqlite3
EXPOSE 8080
USER nobody
CMD ["offline-market"]
