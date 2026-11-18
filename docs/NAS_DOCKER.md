# NAS Docker Deployment & Auto-Sleep Guide

## 1. Prepare Configuration

1. Copy `.env.example` to `.env` on the NAS (never commit `.env`):
   ```bash
   cp .env.example .env
   ```
2. Set `GATEWAY_TOKEN` (`openssl rand -hex 32`), `GATEWAY_API_PROXY` (e.g. `socks5h://host.docker.internal:1080` or `socks5h://<NAS-LAN-IP>:1080`), and `GATEWAY_HOST_PORT` (defaults to `16380` to avoid colliding with common NAS ports like `8080`/`8090`; verify with `ss -tlnp | grep 16380` on the NAS).
3. Leave `GATEWAY_MEDIA_PROXY=` empty so API/auth requests use your proxy while audio CDN streams directly (split routing).

## 2. Standard Mode (Always-On Low-Power)

The compiled Rust `gateway` binary uses Tokio epoll and consumes ~0% CPU when idle while keeping playlist and session caches warm in memory:

```bash
docker compose up -d --build
```

- Access the player UI at `http://<NAS-IP>:16380/player`.
- Health check endpoint: `http://<NAS-IP>:16380/health`.

## 3. Auto-Sleep Mode (Scale-to-Zero on Idle)

To stop the `bb10-music163-gateway` container completely when no client is connected or playing, use the [docker-compose.nas-autosleep.yml](../docker-compose.nas-autosleep.yml) overlay (Traefik + Sablier):

```bash
docker compose -f docker-compose.yml -f docker-compose.nas-autosleep.yml up -d --build
```

- **How it works**: Traefik listens on `GATEWAY_HOST_PORT` (default `16380`, HTTPS using `${CERTS_DIR}/server.crt` and `server.key` so it works seamlessly behind the OCI VPS `TLS_GUARD=1` relay). When all clients close (stopping `/api/v1/playback/events` long-polls) and no HTTP request arrives for `IDLE_SLEEP_DURATION` (default `10m`), Sablier runs `docker stop bb10-music163-gateway`.
- **On-demand wake**: Opening `/player` or launching the BB10 `.bar` app triggers Sablier to start `bb10-music163-gateway` automatically (~1s cold start) and forward the request.
- **Persistence**: Login cookies (`netease_cookies.json`) and playback generation state live in the `gateway_state` Docker volume (`/data/state`) and survive container stop/start.

## 4. Migrating Existing Login State to NAS

To reuse your current NetEase login cookies without scanning QR again, copy `state/netease_cookies.json` into the NAS `gateway_state` volume (with `0600` permissions) after creating the volume.
