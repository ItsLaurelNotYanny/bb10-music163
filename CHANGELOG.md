# Changelog

English | [简体中文](CHANGELOG.zh-CN.md)

## v0.2.0 — 2026-11-25

### Single-Container Native TLS & Operational Hardening
- **Removed `Traefik` and `Sablier` sidecars**: Replaced the 3-container auto-sleep deployment (`docker-compose.nas-autosleep.yml`) with a single always-on container (`docker-compose.nas-tls.yml`), eliminating hourly Synology Container Manager `SIGKILL (137)` unexpected-stop alerts, `30s` healthcheck/wake-up timeouts, and `TRAEFIK DEFAULT CERT` IP fallback issues.
- **Native dual-port TLS (`crates/gateway/src/tls.rs`)**: Added optional `rustls` (AWS-LC) HTTPS listener (`GATEWAY_TLS_LISTEN`, `GATEWAY_TLS_CERT`, `GATEWAY_TLS_KEY`) supporting TLS 1.2/1.3, HTTP/1.1 without requiring ALPN (compatible with BB10 `LocalTlsBridge`), and single-certificate serving without SNI. Validates every X.509 DER entry in the configured certificate chain at startup (fail-fast).
- **Explicit trusted LAN bind IP**: `docker-compose.nas-tls.yml` binds `:16380 -> :8443` (HTTPS) and `${GATEWAY_LAN_BIND_IP}:16381 -> :8090` (trusted LAN HTTP) so plaintext HTTP is never published on wildcard `0.0.0.0`.
- **Bounded graceful shutdown & stream lifecycle (`crates/gateway/src/server.rs`)**:
  - Added `SIGTERM`/`SIGINT` handling with persistent shutdown state, immediate long-poll wakeup, stalled media setup abortion (`503`), and a 4-second drain budget (`stop_grace_period: 10s`, exits `0`).
  - Serialized shutdown sweep and stream admission under `StreamRegistry` mutex and added atomic generation revalidation after upstream header setup.
  - Added `TrackedMediaStream` `Drop` guard so `active_streams` entries are released immediately on EOF, client disconnect, and `HEAD` responses.

### Player UX & Resource Safeguards (`web/player.html`)
- **Local-immediate pause + server-confirmed resume**: Clicking Pause stops audio locally at `0ms` (unaffected by browser HTTP/1.1 connection queueing behind cover fetches) and syncs asynchronously to the gateway; Resume remains server-confirmed to uphold M4 single-owner playback exclusivity.
- **Sleep timer & unattended loop auto-stop**: Added a compact ES5 sleep timer toggle (`Off / 30m / 60m / 120m`) in the playback bar, plus a built-in 3-hour unattended `auto_next` safeguard that automatically issues `stop` and releases streams if loop playback is left running without manual interaction.

## v0.1.0 — 2026-11-18
- Initial M0–M5 release: Rust modular monolith gateway (`provider-model`, `media-relay`, `playback-session`, `netease-adapter`, `gateway`), ES5 vinyl web player, BB10 Cascades `.bar` shell with `LocalTlsBridge`, and OCI VPS L4 relay integration.
