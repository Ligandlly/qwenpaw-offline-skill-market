# Build stage. Requires network for crates; for air-gapped targets build here
# and copy target/release/offline-market-backend into the image instead.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release --locked 2>/dev/null || cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/offline-market-backend /usr/local/bin/offline-market
RUN mkdir -p /data
ENV MARKET_DB=/data/market.sqlite3
EXPOSE 8080
USER nobody
CMD ["offline-market"]
