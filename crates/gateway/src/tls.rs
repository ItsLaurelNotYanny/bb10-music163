//! Native TLS configuration (rustls + AWS-LC, HTTP/1.1 + no-ALPN + no-SNI)
//! and coordinated dual-listener lifecycle with bounded graceful shutdown.

use crate::server::AppState;
use anyhow::{bail, Context, Result};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use axum_server::Handle;
use rustls::ServerConfig;
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::net::{SocketAddr, TcpListener as StdTcpListener};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Graceful drain budget per shutdown; strictly below Docker's 10s SIGKILL window.
pub const DEFAULT_SHUTDOWN_BUDGET: Duration = Duration::from_secs(4);

/// Build a rustls `RustlsConfig` using the workspace's AWS-LC provider.
///
/// Enforces:
/// - TLS 1.2 + TLS 1.3 (required for BB10 LocalTlsBridge / OpenSSL and modern clients)
/// - HTTP/1.1 only over TLS; clients omitting ALPN (such as `LocalTlsBridge.cpp`) are accepted
/// - Single certificate resolver (`with_single_cert`) so the configured cert is always served
///   even when SNI is absent (direct IP access)
/// - Fail-fast validation of file readability, PEM structure, non-empty chain, and cert/key match
pub fn build_tls_config(cert_path: &Path, key_path: &Path) -> Result<RustlsConfig> {
    let cert_file = File::open(cert_path).with_context(|| {
        format!(
            "failed to open TLS certificate {} (ensure readable by container UID)",
            cert_path.display()
        )
    })?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to parse PEM certificates from {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!(
            "TLS certificate file {} contains no PEM certificates",
            cert_path.display()
        );
    }
    for (idx, cert) in certs.iter().enumerate() {
        webpki::EndEntityCert::try_from(cert).map_err(|err| {
            anyhow::anyhow!(
                "TLS certificate chain entry #{idx} in {} is malformed X.509 DER: {err:?}",
                cert_path.display()
            )
        })?;
    }

    let key_file = File::open(key_path).with_context(|| {
        format!(
            "failed to open TLS private key {} (ensure readable by container UID)",
            key_path.display()
        )
    })?;
    let mut key_reader = BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .with_context(|| format!("failed to parse PEM private key from {}", key_path.display()))?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "TLS private key file {} contains no supported PEM private key",
                key_path.display()
            )
        })?;

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut server_config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .context("failed to configure TLS 1.2/1.3 protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .with_context(|| {
            format!(
                "TLS certificate {} does not match private key {}",
                cert_path.display(),
                key_path.display()
            )
        })?;

    // Advertise HTTP/1.1 only; rustls automatically accepts clients that omit ALPN.
    server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(RustlsConfig::from_config(Arc::new(server_config)))
}

/// Bind a non-blocking std `TcpListener` for `axum-server` before declaring readiness.
pub fn bind_std_listener(addr: &str) -> Result<StdTcpListener> {
    let listener = StdTcpListener::bind(addr)
        .with_context(|| format!("failed to bind listener on {addr}"))?;
    listener
        .set_nonblocking(true)
        .with_context(|| format!("failed to set nonblocking on {addr}"))?;
    Ok(listener)
}

/// Wait for SIGTERM or SIGINT on Unix (or Ctrl-C on non-Unix).
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Run the HTTP listener and optional HTTPS listener sharing one `AppState` and `Router`.
///
/// Lifecycle guarantees:
/// - Both sockets and TLS material are bound/validated before calling this function.
/// - On `shutdown_fut` completion (SIGTERM/SIGINT): marks persistent shutdown state,
///   wakes long-polls, cancels active streams, and drains both servers within `shutdown_budget`.
///   If any connection remains after `shutdown_budget`, forces immediate close via `Handle::shutdown`.
/// - On unexpected exit/error/panic of either listener: shuts down the peer listener under the
///   same bounded budget and returns `Err` (non-zero process exit).
pub async fn serve_listeners<F>(
    http_listener: StdTcpListener,
    tls_listener: Option<(StdTcpListener, RustlsConfig)>,
    app: Router,
    state: AppState,
    shutdown_fut: F,
    shutdown_budget: Duration,
) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let http_handle: Handle<SocketAddr> = Handle::new();
    let http_handle_task = http_handle.clone();
    let http_app = app.clone();
    let http_server = axum_server::from_tcp(http_listener)
        .context("failed to initialize HTTP server from listener")?;

    let mut http_task = tokio::spawn(async move {
        http_server
            .handle(http_handle_task)
            .serve(http_app.into_make_service())
            .await
            .context("HTTP listener terminated with error")
    });

    let (tls_handle, mut tls_task) = match tls_listener {
        Some((listener, rustls_cfg)) => {
            let h: Handle<SocketAddr> = Handle::new();
            let h_task = h.clone();
            let tls_app = app;
            let tls_server = axum_server::from_tcp_rustls(listener, rustls_cfg)
                .context("failed to initialize HTTPS server from listener")?;
            let task = tokio::spawn(async move {
                tls_server
                    .handle(h_task)
                    .serve(tls_app.into_make_service())
                    .await
                    .context("HTTPS listener terminated with error")
            });
            (Some(h), Some(task))
        }
        None => (None, None),
    };

    let trigger_drain = |h_http: &Handle<SocketAddr>, h_tls: Option<&Handle<SocketAddr>>| {
        h_http.graceful_shutdown(Some(shutdown_budget));
        if let Some(h) = h_tls {
            h.graceful_shutdown(Some(shutdown_budget));
        }
    };
    let trigger_force_close = |h_http: &Handle<SocketAddr>, h_tls: Option<&Handle<SocketAddr>>| {
        h_http.shutdown();
        if let Some(h) = h_tls {
            h.shutdown();
        }
    };

    tokio::pin!(shutdown_fut);

    enum ExitReason {
        Signal,
        HttpExited(std::result::Result<Result<()>, tokio::task::JoinError>),
        TlsExited(std::result::Result<Result<()>, tokio::task::JoinError>),
    }

    let reason = if let Some(ref mut tls_t) = tls_task {
        tokio::select! {
            _ = &mut shutdown_fut => ExitReason::Signal,
            res = &mut http_task => ExitReason::HttpExited(res),
            res = tls_t => ExitReason::TlsExited(res),
        }
    } else {
        tokio::select! {
            _ = &mut shutdown_fut => ExitReason::Signal,
            res = &mut http_task => ExitReason::HttpExited(res),
        }
    };

    // Persist shutdown state, wake long-polls, and cancel all registered media streams.
    state.begin_shutdown();
    trigger_drain(&http_handle, tls_handle.as_ref());

    // Bounded total deadline for remaining listener tasks to finish draining.
    let hard_deadline = shutdown_budget + Duration::from_millis(500);

    match reason {
        ExitReason::Signal => {
            let drain_all = async {
                let _ = (&mut http_task).await;
                if let Some(ref mut tls_t) = tls_task {
                    let _ = tls_t.await;
                }
            };
            if tokio::time::timeout(hard_deadline, drain_all).await.is_err() {
                trigger_force_close(&http_handle, tls_handle.as_ref());
                http_task.abort();
                if let Some(ref mut tls_t) = tls_task {
                    tls_t.abort();
                }
            }
            Ok(())
        }
        ExitReason::HttpExited(res) => {
            if let Some(ref mut tls_t) = tls_task {
                if tokio::time::timeout(hard_deadline, tls_t).await.is_err() {
                    trigger_force_close(&http_handle, tls_handle.as_ref());
                }
            }
            match res {
                Ok(Err(e)) => Err(e),
                Ok(Ok(())) => bail!("HTTP listener exited unexpectedly without shutdown signal"),
                Err(join_err) => Err(anyhow::anyhow!("HTTP listener task panicked: {join_err}")),
            }
        }
        ExitReason::TlsExited(res) => {
            if tokio::time::timeout(hard_deadline, &mut http_task)
                .await
                .is_err()
            {
                trigger_force_close(&http_handle, tls_handle.as_ref());
                http_task.abort();
            }
            match res {
                Ok(Err(e)) => Err(e),
                Ok(Ok(())) => bail!("HTTPS listener exited unexpectedly without shutdown signal"),
                Err(join_err) => Err(anyhow::anyhow!("HTTPS listener task panicked: {join_err}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{build_router, test_state_for_tls};
    use rcgen::{CertificateParams, KeyPair};
    use rustls::pki_types::{CertificateDer, ServerName};
    use std::io::Write;
    use std::net::IpAddr;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;
    use tokio_rustls::TlsConnector;

    fn write_self_signed_cert(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
        let mut params = CertificateParams::new(vec![
            "gateway.example.com".to_string(),
            "127.0.0.1".to_string(),
        ])
        .unwrap();
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            "gateway.example.com".to_string(),
        );
        let key_pair = KeyPair::generate().unwrap();
        let cert = params.self_signed(&key_pair).unwrap();
        let cert_path = dir.join("server.crt");
        let key_path = dir.join("server.key");
        std::fs::write(&cert_path, cert.pem()).unwrap();
        std::fs::write(&key_path, key_pair.serialize_pem()).unwrap();
        (cert_path, key_path, cert.der().to_vec())
    }

    #[test]
    fn tls_config_fails_fast_on_missing_empty_invalid_and_mismatched_material() {
        let dir = TempDir::new().unwrap();
        let (cert_a, key_a, _) = write_self_signed_cert(dir.path());
        assert!(build_tls_config(&cert_a, &key_a).is_ok());

        // Missing files
        assert!(build_tls_config(&dir.path().join("nope.crt"), &key_a).is_err());
        assert!(build_tls_config(&cert_a, &dir.path().join("nope.key")).is_err());

        // Empty files
        let empty = dir.path().join("empty.pem");
        std::fs::File::create(&empty).unwrap();
        assert!(build_tls_config(&empty, &key_a).is_err());
        assert!(build_tls_config(&cert_a, &empty).is_err());

        // Invalid PEM
        let garbage = dir.path().join("garbage.pem");
        let mut f = std::fs::File::create(&garbage).unwrap();
        writeln!(f, "not a pem file").unwrap();
        assert!(build_tls_config(&garbage, &key_a).is_err());
        assert!(build_tls_config(&cert_a, &garbage).is_err());

        // Mismatched cert and key
        let dir_b = TempDir::new().unwrap();
        let (_, key_b, _) = write_self_signed_cert(dir_b.path());
        assert!(build_tls_config(&cert_a, &key_b).is_err());

        // Valid leaf followed by malformed DER CERTIFICATE block must fail startup
        let bad_chain = dir.path().join("bad_chain.crt");
        let leaf_pem = std::fs::read_to_string(&cert_a).unwrap();
        let bad_chain_pem = format!(
            "{leaf_pem}\n-----BEGIN CERTIFICATE-----\nbm90LWEtdmFsaWQtZGVyLWNlcnRpZmljYXRl\n-----END CERTIFICATE-----\n"
        );
        std::fs::write(&bad_chain, bad_chain_pem).unwrap();
        assert!(build_tls_config(&bad_chain, &key_a).is_err());
    }

    #[tokio::test]
    async fn dual_listener_shares_state_serves_tls12_no_alpn_no_sni_and_drains_on_shutdown() {
        let dir = TempDir::new().unwrap();
        let (cert_path, key_path, cert_der) = write_self_signed_cert(dir.path());
        let rustls_cfg = build_tls_config(&cert_path, &key_path).unwrap();

        let http_listener = bind_std_listener("127.0.0.1:0").unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let tls_listener = bind_std_listener("127.0.0.1:0").unwrap();
        let tls_addr = tls_listener.local_addr().unwrap();

        let state = test_state_for_tls(dir.path().to_path_buf());
        let app = build_router(state.clone());

        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let server_task = tokio::spawn(serve_listeners(
            http_listener,
            Some((tls_listener, rustls_cfg)),
            app,
            state.clone(),
            async move {
                let _ = shutdown_rx.await;
            },
            Duration::from_millis(800),
        ));

        // 1. Connect over TLS 1.2 ONLY, with NO ALPN and NO SNI (IP literal 127.0.0.1).
        let mut root_store = rustls::RootCertStore::empty();
        root_store
            .add(CertificateDer::from(cert_der.clone()))
            .unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut client_cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        client_cfg.alpn_protocols.clear(); // explicitly omit ALPN like LocalTlsBridge.cpp

        let connector = TlsConnector::from(Arc::new(client_cfg));
        let tcp = TcpStream::connect(tls_addr).await.unwrap();
        let server_name = ServerName::IpAddress(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST).into());
        let mut tls_stream = connector.connect(server_name, tcp).await.unwrap();

        // Verify negotiated protocol is TLS 1.2 and configured certificate was served without SNI.
        let (_, conn) = tls_stream.get_ref();
        assert_eq!(
            conn.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_2)
        );
        let peer_certs = conn.peer_certificates().unwrap();
        assert_eq!(peer_certs[0].as_ref(), cert_der.as_slice());

        // Issue explicit_play over HTTPS.
        let play_body = r#"{"client_id":"bb10-tls","kind":"explicit_play","expected_generation":0,"idempotency_key":"tls-1","track":{"provider":"netease","track_id":"1908049566"}}"#;
        let play_req = format!(
            "POST /api/v1/playback/command HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-gateway-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            play_body.len(),
            play_body
        );
        tls_stream.write_all(play_req.as_bytes()).await.unwrap();
        let mut resp_buf = String::new();
        tls_stream.read_to_string(&mut resp_buf).await.unwrap();
        assert!(
            resp_buf.starts_with("HTTP/1.1 200 OK"),
            "unexpected HTTPS response: {resp_buf}"
        );

        // 2. Verify HTTP listener observes the exact same AppState playback generation & owner.
        let mut http_stream = TcpStream::connect(http_addr).await.unwrap();
        http_stream
            .write_all(
                b"GET /api/v1/playback/state HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-gateway-token\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut http_resp = String::new();
        http_stream.read_to_string(&mut http_resp).await.unwrap();
        assert!(http_resp.contains(r#""generation":1"#), "resp={http_resp}");
        assert!(
            http_resp.contains(r#""owner":"bb10-tls""#),
            "resp={http_resp}"
        );

        // 3. Open both a 15s event long-poll on HTTP AND a stalled raw TCP socket on HTTPS
        // (simulating a blocked TLS handshake), then trigger shutdown and assert bounded exit.
        let mut long_poll = TcpStream::connect(http_addr).await.unwrap();
        long_poll
            .write_all(
                b"GET /api/v1/playback/events?client_id=bb10-tls&known_generation=1&wait_ms=15000 HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer test-gateway-token\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let _stalled_tls_handshake = TcpStream::connect(tls_addr).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;

        let started = std::time::Instant::now();
        let _ = shutdown_tx.send(());
        let serve_res = tokio::time::timeout(Duration::from_secs(3), server_task)
            .await
            .expect("dual-listener shutdown exceeded bounded deadline")
            .expect("server task panicked");
        assert!(serve_res.is_ok(), "expected clean Ok(()) on signal shutdown");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "shutdown took too long: {:?}",
            started.elapsed()
        );
    }
}
