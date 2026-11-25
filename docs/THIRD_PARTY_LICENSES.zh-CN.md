# 第三方许可证与运行时选型说明

[English](THIRD_PARTY_LICENSES.md) | 简体中文

本文档记录 Rust Workspace 锁定的依赖版本、异步运行时选型以及许可证隔离边界。

## 运行时选型

- **网关 HTTP 服务与异步运行时**：`tokio` (`1.54.1`) + `axum` (`0.8.9`)。
- **上游网易云 API 客户端 (`ncm-api`) 与媒体中继 (`media-relay`)**：`isahc` (`2.0.2`，使用 `static-curl` 与 `rustls-tls` (`default-tls`))。`isahc` 通过其内置后台线程驱动 curl 传输并暴露标准 `Future`，使得 `ncm-api` 与 `media-relay` 可以直接在 `tokio` 内部运行，无需引入第二个应用层执行器 (`async-std`)。
- **OpenSSL (`openssl` `0.10.82`, `vendored`)**：启用 `vendored` 特性（`openssl-src` `300.6.1+3.6.3`），因为 `ncm-api`（`encrypt.rs`）直接使用 OpenSSL 进行 AES/MD5/RSA 处理；vendored 源码消除了对宿主机 `libssl-dev` 与 `pkg-config` 的依赖。

## Workspace 许可证隔离边界

| Workspace Crate | 许可证 | GPL 隔离边界 |
| --- | --- | --- |
| `provider-model` | `MIT OR Apache-2.0` | 不包含任何 `ncm-api` 或 GPL 衍生代码；可独立构建与测试。 |
| `media-relay` | `MIT OR Apache-2.0` | 与业务解耦的 HTTP Range/HEAD 媒体代理客户端 (`isahc`/`tokio`)；零网易云与零 GPLv3 依赖。 |
| `playback-session` | `MIT OR Apache-2.0` | 纯播放归属权与单调递增 generation 状态机（仅依赖 `provider-model`）；零网易云、HTTP、UI 或 GPLv3 依赖。 |
| `netease-adapter` | `GPL-3.0-only` | 直接链接 `netease-cloud-music-api` (GPLv3)。 |
| `gateway` | `GPL-3.0-only` | 二进制程序，将 `netease-adapter` 与宽松许可的共享 crate 一同链接。 |

共享可复用库（`provider-model`、`media-relay` 与 `playback-session`）严格保持零网易云 / GPLv3 依赖，并在 `check.sh` 中独立进行测试。

## 已锁定的关键依赖 (`Cargo.lock`)

| Crate | 锁定版本 / Commit | 许可证 | 用途说明 |
| --- | --- | --- | --- |
| `netease-cloud-music-api` | `2.1.0` (`rev = 32b4043e83e6ec58543ef04fc1183a3ee496719f`) | `GPLv3`（源码头部声明） | 上游协议加密与接口封装 |
| `isahc` | `2.0.2` | `MIT` | CookieJar 与 SOCKS5h/TLS HTTP 客户端（`ncm-api`、探测与 `media-relay`） |
| `curl` | `0.4.51` | `MIT` | `static-curl` 传输后端 |
| `rustls` | `0.23.46` | `Apache-2.0 OR ISC OR MIT` | `isahc` 的 TLS 验证后端 |
| `openssl` | `0.10.82` | `Apache-2.0` | `ncm-api` 所需的加解密原语 |
| `openssl-src` | `300.6.1+3.6.3` | `MIT/Apache-2.0` | OpenSSL 静态内嵌编译源码 |
| `tokio` | `1.54.1` | `MIT` | 异步运行时 |
| `axum` | `0.8.9` | `MIT` | 网关 HTTP API 服务 |
| `bytes` | `1.12.1` | `MIT` | 用于 `media-relay` 流式传输的有界分块缓冲区 |
| `http` | `1.5.0` | `MIT OR Apache-2.0` | 用于 `media-relay` 的 HTTP 方法/状态码/头部类型 |
| `http-body` | `1.1.0` | `MIT` | `gateway` 中的流式 HTTP body trait |
| `http-body-util` | `0.1.5` | `MIT` | `gateway` 中的流式 `StreamBody` / `BodyExt` 辅助工具 |
| `futures-lite` | `2.6.1` | `Apache-2.0 OR MIT` | 用于 `media-relay` 与有界探测读取的异步流/字节读取辅助工具 |
| `serde` | `1.0.229` | `MIT OR Apache-2.0` | 序列化 |
| `toml` | `0.8.23` | `MIT OR Apache-2.0` | TOML 配置解析 |
| `clap` | `4.6.7` | `MIT OR Apache-2.0` | CLI 参数解析 |
| `qrcode` | `0.14.1` | `MIT OR Apache-2.0` | 终端文本与 SVG 二维码生成 |
