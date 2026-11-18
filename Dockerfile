# Multi-stage build for bb10-music163 gateway (M5).
FROM rust:1-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY web ./web

RUN cargo build --release --locked -p gateway

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /data gateway \
    && mkdir -p /data/state /etc/bb10-music163 \
    && chown -R gateway:gateway /data

COPY --from=builder /src/target/release/gateway /usr/local/bin/gateway
COPY config/gateway.example.toml /etc/bb10-music163/gateway.example.toml

USER gateway
WORKDIR /data

ENV GATEWAY_LISTEN=0.0.0.0:8090
ENV GATEWAY_STATE_DIR=/data/state

EXPOSE 8090

HEALTHCHECK --interval=5s --timeout=3s --start-period=2s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8090/health >/dev/null || exit 1

ENTRYPOINT ["/usr/local/bin/gateway"]
CMD ["serve"]
