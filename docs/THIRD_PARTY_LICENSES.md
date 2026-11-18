# Third-Party Licenses and Runtime Choices

This document records the pinned dependencies, async runtime selection, and license boundaries for the Rust workspace.

## Runtime Choice

- **Gateway HTTP & Async Runtime**: `tokio` (`1.54.1`) + `axum` (`0.8.9`).
- **Upstream NetEase API Client (`ncm-api`) & Media Relay (`media-relay`)**: `isahc` (`2.0.2`) using `static-curl` and `rustls-tls` (`default-tls`). `isahc` executes curl transfers on its internal background thread and exposes standard `Future`s, allowing `ncm-api` and `media-relay` to run directly inside `tokio` without adding a second application executor (`async-std`).
- **OpenSSL (`openssl` `0.10.82`, `vendored`)**: Enabled `vendored` (`openssl-src` `300.6.1+3.6.3`) because `ncm-api` (`encrypt.rs`) uses OpenSSL directly for AES/MD5/RSA; vendoring removes reliance on host `libssl-dev` and `pkg-config`.

## Workspace License Boundaries

| Workspace Crate | License | GPL Isolation Boundary |
| --- | --- | --- |
| `provider-model` | `MIT OR Apache-2.0` | Contains zero `ncm-api` or GPL-derived code; builds and tests independently. |
| `media-relay` | `MIT OR Apache-2.0` | Service-agnostic HTTP Range/HEAD media proxy client (`isahc`/`tokio`); zero NetEase or GPLv3 dependencies. |
| `playback-session` | `MIT OR Apache-2.0` | Pure playback ownership & monotonic generation state machine (`provider-model` only); zero NetEase, HTTP, UI, or GPLv3 dependencies. |
| `netease-adapter` | `GPL-3.0-only` | Directly links `netease-cloud-music-api` (GPLv3). |
| `gateway` | `GPL-3.0-only` | Binary linking `netease-adapter` alongside the permissively licensed shared crates. |

Shared reusable libraries (`provider-model`, `media-relay`, and `playback-session`) remain strictly free of NetEase / GPLv3 dependencies and are tested independently in `check.sh`.

## Pinned Key Dependencies (`Cargo.lock`)

| Crate | Pinned Version / Commit | License | Purpose |
| --- | --- | --- | --- |
| `netease-cloud-music-api` | `2.1.0` (`rev = 32b4043e83e6ec58543ef04fc1183a3ee496719f`) | `GPLv3` (per source headers) | Upstream NetEase protocol encryption & endpoints |
| `isahc` | `2.0.2` | `MIT` | Cookie-jar & SOCKS5h/TLS HTTP client (`ncm-api`, probe, and `media-relay`) |
| `curl` | `0.4.51` | `MIT` | `static-curl` transport backend |
| `rustls` | `0.23.46` | `Apache-2.0 OR ISC OR MIT` | TLS verification backend for `isahc` |
| `openssl` | `0.10.82` | `Apache-2.0` | Crypto primitives required by `ncm-api` |
| `openssl-src` | `300.6.1+3.6.3` | `MIT/Apache-2.0` | Vendored OpenSSL C build source |
| `tokio` | `1.54.1` | `MIT` | Async runtime |
| `axum` | `0.8.9` | `MIT` | Gateway HTTP API server |
| `bytes` | `1.12.1` | `MIT` | Bounded chunk buffers for `media-relay` streaming |
| `http` | `1.5.0` | `MIT OR Apache-2.0` | HTTP method/status/header types for `media-relay` |
| `http-body` | `1.1.0` | `MIT` | Streaming HTTP body traits in `gateway` |
| `http-body-util` | `0.1.5` | `MIT` | Streaming `StreamBody` / `BodyExt` helpers in `gateway` |
| `futures-lite` | `2.6.1` | `Apache-2.0 OR MIT` | Async stream/byte reading helpers for `media-relay` and bounded probe reads |
| `serde` | `1.0.229` | `MIT OR Apache-2.0` | Serialization |
| `toml` | `0.8.23` | `MIT OR Apache-2.0` | TOML configuration parsing |
| `clap` | `4.6.7` | `MIT OR Apache-2.0` | CLI argument parsing |
| `qrcode` | `0.14.1` | `MIT OR Apache-2.0` | Terminal & SVG QR code rendering |
