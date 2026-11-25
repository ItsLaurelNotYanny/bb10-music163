# 版本更新记录 (Changelog)

[English](CHANGELOG.md) | 简体中文

## v0.2.0 — 2026-11-25

### 单容器原生 TLS 与稳定性加固
- **移除 `Traefik` 与 `Sablier` 附加容器**：用单容器常驻配置（`docker-compose.nas-tls.yml`）替代原有的三容器休眠方案（`docker-compose.nas-autosleep.yml`），彻底消除群晖 Container Manager 每小时 `SIGKILL (137)` 意外停止告警、`30s` 健康检查撞上唤醒超时以及 IP 访问回退 `TRAEFIK DEFAULT CERT` 假证书的问题。
- **原生双端口 TLS（`crates/gateway/src/tls.rs`）**：基于现有 `rustls`（AWS-LC）新增可选 HTTPS 监听（`GATEWAY_TLS_LISTEN`、`GATEWAY_TLS_CERT`、`GATEWAY_TLS_KEY`），支持 TLS 1.2/1.3、无 ALPN 的 HTTP/1.1（兼容 BB10 `LocalTlsBridge`）及无 SNI 单证书响应；启动期对整条证书链逐项做 X.509 DER 校验并 fail-fast。
- **受信任局域网显式绑定 IP**：`docker-compose.nas-tls.yml` 映射 `:16380 -> :8443`（HTTPS）与 `${GATEWAY_LAN_BIND_IP}:16381 -> :8090`（受信任局域网 HTTP），避免明文 HTTP 暴露在 `0.0.0.0`。
- **有界优雅退出与音频流生命周期清理（`crates/gateway/src/server.rs`）**：
  - 新增 `SIGTERM`/`SIGINT` 处理、持久化 shutdown 状态、立即唤醒 15s 长轮询、中止停滞的媒体建流（`503`），并在 4 秒总预算内排空退出（退出码 `0`，Compose `stop_grace_period: 10s`）。
  - 使用 `StreamRegistry` 互斥锁串行化关闭清扫与流准入，并在拉取上游响应头后再次原子校验代际所有权。
  - 新增 `TrackedMediaStream` `Drop` 守卫，保证正常播完 EOF、客户端断连及 `HEAD` 请求立即从 `active_streams` 释放。

### 播放器体验与防忘关自动停播（`web/player.html`）
- **本地即时暂停 + 网络确认恢复**：点暂停时 0ms 本地立刻停声并更新按钮（不受 HTTP/1.1 同源连接排队影响），后台异步同步网关；点恢复仍由服务端确认，严格守住 M4 单播放权互斥。
- **睡眠定时与 3 小时无人工操作自动停播**：播放条新增纯 ES5 睡眠定时按钮（`关 / 30m / 60m / 120m`），并内置连续 3 小时无人工操作循环播放（`auto_next`）自动发送 `stop` 停播释放流资源的兜底保护。

## v0.1.0 — 2026-11-18
- 首个 M0–M5 完整版本：Rust 模块化单体网关（`provider-model`、`media-relay`、`playback-session`、`netease-adapter`、`gateway`）、ES5 黑胶网页播放器、内置 `LocalTlsBridge` 的 BB10 Cascades `.bar` 外壳及 OCI VPS 四层转发集成。
