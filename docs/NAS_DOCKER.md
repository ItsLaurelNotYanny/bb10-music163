# NAS Docker Deployment Guide (Single-Container Native TLS)

English | [简体中文](NAS_DOCKER.zh-CN.md)

## 1. Prepare Configuration

1. Copy `.env.example` to `.env` on the NAS (never commit `.env`):
   ```bash
   cp .env.example .env
   ```
2. Set `GATEWAY_TOKEN` (`openssl rand -hex 32`), `GATEWAY_API_PROXY` (e.g. `socks5h://host.docker.internal:1080` or `socks5h://<NAS-LAN-IP>:1080`), `GATEWAY_HOST_PORT=16380`, `GATEWAY_LAN_BIND_IP=<NAS-LAN-IP>`, `GATEWAY_LAN_HTTP_PORT=16381`, and `CERTS_DIR` (e.g. `/volume1/docker/bb10-music163/certs`).
3. Leave `GATEWAY_MEDIA_PROXY=` empty so API/auth requests use your proxy while audio CDN streams directly (split routing).

## 2. Single-Container Native TLS Mode (Recommended for NAS + OCI VPS)

The compiled Rust `gateway` binary uses Tokio epoll (~0% idle CPU) and natively serves both **HTTPS (`:16380 -> :8443`)** for the OCI VPS `TLS_GUARD=1` relay and **trusted LAN HTTP (`:16381 -> :8090`)** for the BB10 `.bar` LAN shortcut—with **zero** Traefik or Sablier sidecars:

```bash
docker compose -f docker-compose.nas-tls.yml up -d --build
```

- **External HTTPS**: `https://<DOMAIN-OR-NAS-IP>:16380/player` (serves `/certs/server.crt` even without SNI; TLS 1.2/1.3 + HTTP/1.1 without requiring ALPN).
- **Trusted LAN HTTP**: `http://<NAS-LAN-IP>:16381/player` (restrict `:16381` to your trusted LAN / firewall because it carries bearer tokens and media tickets in plaintext).
- **Health checks**: Container `HEALTHCHECK` probes `http://127.0.0.1:8090/health`; verify external HTTPS separately via `curl --cacert <ca.crt> https://<NAS-IP>:16380/health`.
- **Certificate permissions & renewal**: `bb10-music163-gateway` runs as non-root `UID 10001`. Ensure `server.crt` and `server.key` (and parent directory traversal) are readable by UID `10001` via a narrow group/ACL (avoid world-readable private keys). Certificates are loaded at startup; restart the container after renewing certs (restarting interrupts active playback).
- **Graceful shutdown**: On `SIGTERM`/`SIGINT`, the gateway drains both listeners and cancels active streams/long-polls within 4 seconds (exit code `0`), avoiding Docker's 10s `SIGKILL` timeout and Synology "stopped unexpectedly" alerts.

## 3. Standalone HTTP Mode (Local Development / LAN Only)

For local development without certificates:

```bash
docker compose up -d --build
```

- Player UI: `http://<HOST-IP>:16380/player`
- Health check: `http://<HOST-IP>:16380/health`

## 4. Migrating from Legacy `docker-compose.nas-autosleep.yml` (Traefik + Sablier)

The legacy `Traefik + Sablier` overlay (`docker-compose.nas-autosleep.yml`) is deprecated because stopping a ~0% idle Rust container caused cold-start timeouts and DSM unexpected-stop alerts. To migrate while preserving your `gateway_state` volume (login cookies and playback generation):

```bash
# Stop and remove legacy Traefik/Sablier/gateway containers WITHOUT deleting volumes (-v is NOT used!)
docker compose -f docker-compose.yml -f docker-compose.nas-autosleep.yml down
# Start the single-container native TLS gateway
docker compose -f docker-compose.nas-tls.yml up -d
```

## 5. Migrating Existing Login State to NAS

To reuse your current NetEase login cookies without scanning QR again, copy `state/netease_cookies.json` into the NAS `gateway_state` volume (with `0600` permissions) after creating the volume.
