# NAS Docker 部署指南（单容器原生 TLS）

[English](NAS_DOCKER.md) | 简体中文

## 1. 准备环境变量

1. 在 NAS 项目目录复制 `.env.example` 为 `.env`（切勿提交 `.env`）：
   ```bash
   cp .env.example .env
   ```
2. 设置 `GATEWAY_TOKEN`（可用 `openssl rand -hex 32` 生成）、`GATEWAY_API_PROXY`（例如 `socks5h://host.docker.internal:1080` 或 `socks5h://<NAS局域网IP>:1080`）、`GATEWAY_HOST_PORT=16380`、`GATEWAY_LAN_BIND_IP=<NAS局域网IP>`、`GATEWAY_LAN_HTTP_PORT=16381` 以及 `CERTS_DIR`（例如 `/volume1/docker/bb10-music163/certs`）。
3. 保持 `GATEWAY_MEDIA_PROXY=` 为空，即可启用**分流模式**（仅 API 与鉴权走代理，音频 CDN 直连）。

## 2. 单容器原生 TLS 模式（推荐，适配 NAS + OCI VPS）

Rust 编译的 `gateway` 使用 Tokio 异步等待，空闲 CPU 接近 `0%`，直接在同一容器内原生提供 **HTTPS（`:16380 -> :8443`）**（对接 OCI VPS `TLS_GUARD=1` 与 `.bar` 外网入口）和 **受信任局域网 HTTP（`:16381 -> :8090`）**（对接 `.bar` 内网入口），**无需再部署 Traefik 或 Sablier**：

```bash
docker compose -f docker-compose.nas-tls.yml up -d --build
```

- **外网 HTTPS**：`https://<域名或NAS_IP>:16380/player`（原生 `rustls` 单证书解析，支持 TLS 1.2/1.3、HTTP/1.1 无 ALPN 以及无 SNI IP 直连）。
- **受信任局域网 HTTP**：`http://<NAS局域网IP>:16381/player`（由于明文传输 Bearer Token 与媒体 Ticket，请通过防火墙将 `:16381` 限制在受信任局域网内）。
- **健康检查**：容器内置 `HEALTHCHECK` 探测 `http://127.0.0.1:8090/health`；外网 HTTPS 路径请额外通过 `curl --cacert <ca.crt> https://<NAS_IP>:16380/health` 验收。
- **证书权限与续期**：容器以非 root 用户 `UID 10001` 运行，请通过精细组权限或 Synology ACL 确保 `UID 10001` 可读 `server.crt` 与 `server.key`（切勿把私钥设为全局可读）。证书在启动时加载，续期后重启容器即可生效（重启会中断当前播放）。
- **有界优雅退出**：收到 `SIGTERM`/`SIGINT` 后，网关会在 4 秒内排空双监听并取消活跃流/长轮询后以退出码 `0` 正常结束，避免触发 Docker 10 秒 `SIGKILL`（退出码 `137`）和群晖“容器意外停止”告警。

## 3. 纯 HTTP 单端口模式（本地开发 / 仅局域网）

本地无证书开发调试可直接运行基础文件：

```bash
docker compose up -d --build
```

- 播放器入口：`http://<HOST-IP>:16380/player`
- 健康检查：`http://<HOST-IP>:16380/health`

## 4. 从旧版 `docker-compose.nas-autosleep.yml`（Traefik + Sablier）迁移

旧版 `Traefik + Sablier` 休眠叠加方案已废弃（对空闲 0% CPU 的 Rust 容器做频繁停启会引入冷启动超时和群晖异常停止告警）。按以下步骤平滑迁移并保留原有 `gateway_state` 数据卷（登录 Cookie 与播放代际不丢失）：

```bash
# 停止并移除旧的 Traefik / Sablier / gateway 容器（注意不要加 -v，保留数据卷！）
docker compose -f docker-compose.yml -f docker-compose.nas-autosleep.yml down
# 启动单容器原生 TLS 网关
docker compose -f docker-compose.nas-tls.yml up -d
```

## 5. 迁移现有登录态到 NAS

如需直接沿用本机已登录状态、免去重新扫码，可在创建 `gateway_state` 卷后，将本机 `state/netease_cookies.json` 复制到该卷中并保持 `0600` 权限。
