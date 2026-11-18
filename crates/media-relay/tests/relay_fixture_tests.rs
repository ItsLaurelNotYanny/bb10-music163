use futures_lite::StreamExt;
use http::StatusCode;
use media_relay::{DestinationPolicy, MediaRelay, MediaSourceRequest, RelayConfig, RelayError};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Generate synthetic sample audio bytes for fixture tests.
fn sample_data(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 251) as u8).collect()
}

/// Start an in-process HTTP test fixture server.
async fn start_fixture_server() -> (SocketAddr, tokio::sync::broadcast::Sender<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let mut shutdown_rx = shutdown_tx.subscribe();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                accept_res = listener.accept() => {
                    if let Ok((socket, _)) = accept_res {
                        tokio::spawn(handle_connection(socket));
                    }
                }
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }
    });

    (addr, shutdown_tx)
}

async fn handle_connection(mut socket: TcpStream) {
    let mut buf = vec![0u8; 8192];
    let n = match socket.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let req_str = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req_str.lines();
    let req_line = match lines.next() {
        Some(l) => l,
        None => return,
    };

    let mut parts = req_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_uppercase();
    let path = parts.next().unwrap_or("");

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() || line == "\r" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let full_data = sample_data(1000);

    match path {
        "/audio.mp3" => {
            if method == "HEAD" {
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    full_data.len()
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            } else if let Some(range_val) = headers.get("range") {
                // Parse Range: bytes=100-299
                if range_val == "bytes=100-299" {
                    let slice = &full_data[100..300];
                    let resp_header = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mpeg\r\nContent-Length: 200\r\nContent-Range: bytes 100-299/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                        full_data.len()
                    );
                    let _ = socket.write_all(resp_header.as_bytes()).await;
                    let _ = socket.write_all(slice).await;
                } else {
                    let resp = "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(resp.as_bytes()).await;
                }
            } else {
                let resp_header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                    full_data.len()
                );
                let _ = socket.write_all(resp_header.as_bytes()).await;
                let _ = socket.write_all(&full_data).await;
            }
        }
        "/range_unsatisfiable" => {
            let resp = "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */1000\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        "/ignore_range" => {
            // Upstream ignores Range header and responds with 200 OK + full body
            let resp_header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                full_data.len()
            );
            let _ = socket.write_all(resp_header.as_bytes()).await;
            let _ = socket.write_all(&full_data).await;
        }
        "/expired" => {
            let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 15\r\nConnection: close\r\n\r\nToken expired";
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        "/valid" => {
            let data = sample_data(500);
            let resp_header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                data.len()
            );
            let _ = socket.write_all(resp_header.as_bytes()).await;
            let _ = socket.write_all(&data).await;
        }
        "/abrupt_disconnect" => {
            // Sends headers claiming 1000 bytes, sends 300 bytes, then abruptly terminates TCP connection
            let resp_header = "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(resp_header.as_bytes()).await;
            let partial = &full_data[..300];
            let _ = socket.write_all(partial).await;
            let _ = socket.flush().await;
            // Drop socket immediately without sending the remaining 700 bytes
        }
        "/large_stream" => {
            let total = 200_000usize;
            let resp_header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                total
            );
            let _ = socket.write_all(resp_header.as_bytes()).await;
            let chunk = vec![0x42u8; 1024];
            for _ in 0..(total / chunk.len()) {
                if socket.write_all(&chunk).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        "/redirect_permitted" => {
            let resp = "HTTP/1.1 302 Found\r\nLocation: /audio.mp3\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        "/redirect_untrusted" => {
            let resp = "HTTP/1.1 302 Found\r\nLocation: http://evil.com/audio.mp3\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        "/check_headers" => {
            let has_forbidden = headers.contains_key("cookie")
                || headers.contains_key("set-cookie")
                || headers.contains_key("authorization")
                || headers.contains_key("proxy-authorization");
            if has_forbidden {
                let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 24\r\nConnection: close\r\n\r\nForbidden header present";
                let _ = socket.write_all(resp.as_bytes()).await;
            } else {
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                let _ = socket.write_all(resp.as_bytes()).await;
            }
        }
        _ => {
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    }
}

// -----------------------------------------------------------------------------
// Scenario 1: Normal GET and HEAD
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_1_normal_get_and_head() {
    let (addr, _shutdown) = start_fixture_server().await;
    let policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let relay = MediaRelay::new(RelayConfig::default(), policy).unwrap();

    // 1. Normal GET
    let req = MediaSourceRequest::get(format!("http://{addr}/audio.mp3"));
    let mut resp = relay.open(req).await.unwrap();
    assert_eq!(resp.meta.status, StatusCode::OK);
    assert_eq!(resp.meta.content_length, Some(1000));
    assert_eq!(
        resp.meta.headers.get("content-type").unwrap(),
        "audio/mpeg"
    );

    let mut body_bytes = Vec::new();
    while let Some(chunk_res) = resp.body.next().await {
        let chunk = chunk_res.unwrap();
        body_bytes.extend_from_slice(&chunk);
    }
    assert_eq!(body_bytes.len(), 1000);
    assert_eq!(body_bytes, sample_data(1000));

    // 2. HEAD request
    let head_req = MediaSourceRequest::head(format!("http://{addr}/audio.mp3"));
    let mut head_resp = relay.open(head_req).await.unwrap();
    assert_eq!(head_resp.meta.status, StatusCode::OK);
    assert_eq!(head_resp.meta.content_length, Some(1000));

    let first_chunk = head_resp.body.next().await;
    assert!(first_chunk.is_none(), "HEAD must yield zero body bytes");
}

// -----------------------------------------------------------------------------
// Scenario 2: Valid Range -> 206 Partial Content
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_2_valid_range_partial_content() {
    let (addr, _shutdown) = start_fixture_server().await;
    let policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let relay = MediaRelay::new(RelayConfig::default(), policy).unwrap();

    let req = MediaSourceRequest::get(format!("http://{addr}/audio.mp3"))
        .with_range("bytes=100-299");

    let mut resp = relay.open(req).await.unwrap();
    assert_eq!(resp.meta.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(resp.meta.content_length, Some(200));
    assert_eq!(
        resp.meta.headers.get("content-range").unwrap(),
        "bytes 100-299/1000"
    );

    let mut body_bytes = Vec::new();
    while let Some(chunk_res) = resp.body.next().await {
        let chunk = chunk_res.unwrap();
        body_bytes.extend_from_slice(&chunk);
    }
    assert_eq!(body_bytes.len(), 200);
    assert_eq!(body_bytes, sample_data(1000)[100..300]);
}

// -----------------------------------------------------------------------------
// Scenario 3: Unsatisfiable Range -> 416 Range Not Satisfiable
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_3_unsatisfiable_range_416() {
    let (addr, _shutdown) = start_fixture_server().await;
    let policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let relay = MediaRelay::new(RelayConfig::default(), policy).unwrap();

    let req = MediaSourceRequest::get(format!("http://{addr}/range_unsatisfiable"))
        .with_range("bytes=999999-1000000");

    let resp = relay.open(req).await.unwrap();
    assert_eq!(resp.meta.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        resp.meta.headers.get("content-range").unwrap(),
        "bytes */1000"
    );
}

// -----------------------------------------------------------------------------
// Scenario 4: Upstream ignoring Range preserves 200 OK
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_4_upstream_ignoring_range_preserves_200() {
    let (addr, _shutdown) = start_fixture_server().await;
    let policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let relay = MediaRelay::new(RelayConfig::default(), policy).unwrap();

    let req = MediaSourceRequest::get(format!("http://{addr}/ignore_range"))
        .with_range("bytes=100-299");

    let mut resp = relay.open(req).await.unwrap();
    // Upstream ignored range; relay must preserve 200 OK and NEVER fabricate 206!
    assert_eq!(resp.meta.status, StatusCode::OK);
    assert_eq!(resp.meta.content_length, Some(1000));

    let mut body_bytes = Vec::new();
    while let Some(chunk_res) = resp.body.next().await {
        let chunk = chunk_res.unwrap();
        body_bytes.extend_from_slice(&chunk);
    }
    assert_eq!(body_bytes.len(), 1000);
    assert_eq!(body_bytes, sample_data(1000));
}

// -----------------------------------------------------------------------------
// Scenario 5: Expired URL refresh before headers & Mid-stream disconnect
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_5_expired_url_refresh_and_midstream_failure() {
    let (addr, _shutdown) = start_fixture_server().await;
    let policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let relay = MediaRelay::new(RelayConfig::default(), policy).unwrap();

    // Part A: 403 Forbidden before headers triggers refresh_url once and succeeds
    let req = MediaSourceRequest::get(format!("http://{addr}/expired"));
    let refreshed_url = format!("http://{addr}/valid");

    let mut resp = relay
        .open_with_refresh(req, || async { Ok(refreshed_url) })
        .await
        .unwrap();

    assert_eq!(resp.meta.status, StatusCode::OK);
    assert_eq!(resp.meta.content_length, Some(500));

    let mut body_bytes = Vec::new();
    while let Some(chunk_res) = resp.body.next().await {
        let chunk = chunk_res.unwrap();
        body_bytes.extend_from_slice(&chunk);
    }
    assert_eq!(body_bytes.len(), 500);

    let snap = relay.snapshot();
    assert_eq!(snap.pre_header_refreshes, 1);
    assert_eq!(snap.requests_succeeded, 1);

    // Part B: Mid-stream disconnect emits MidStreamFailure and NEVER restarts at 0
    let config = RelayConfig {
        chunk_size: 128, // Smaller chunks to observe partial delivery
        ..Default::default()
    };
    let relay_b = MediaRelay::new(config, DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()])).unwrap();

    let req_b = MediaSourceRequest::get(format!("http://{addr}/abrupt_disconnect"));
    let mut resp_b = relay_b.open(req_b).await.unwrap();
    assert_eq!(resp_b.meta.status, StatusCode::OK);
    assert_eq!(resp_b.meta.content_length, Some(1000));

    let mut received_bytes = 0usize;
    let mut midstream_err = None;

    while let Some(item) = resp_b.body.next().await {
        match item {
            Ok(chunk) => {
                received_bytes += chunk.len();
            }
            Err(e) => {
                midstream_err = Some(e);
                break;
            }
        }
    }

    assert_eq!(received_bytes, 300);
    match midstream_err {
        Some(RelayError::MidStreamFailure { bytes_sent, expected_bytes, reason }) => {
            assert_eq!(bytes_sent, 300);
            assert_eq!(expected_bytes, Some(1000));
            assert!(reason.contains("prematurely") || reason.contains("I/O error"));
        }
        other => panic!("Expected MidStreamFailure, got {other:?}"),
    }

    let snap_b = relay_b.snapshot();
    assert_eq!(snap_b.mid_stream_failures, 1);
}

// -----------------------------------------------------------------------------
// Scenario 6: Slow client backpressure & stream cancellation
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_6_backpressure_and_stream_cancellation() {
    let (addr, _shutdown) = start_fixture_server().await;
    let policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let config = RelayConfig {
        chunk_size: 1024,
        max_buffered_chunks: 2, // Bounded buffer
        ..Default::default()
    };

    let relay = MediaRelay::new(config, policy).unwrap();
    let req = MediaSourceRequest::get(format!("http://{addr}/large_stream"));

    let mut resp = relay.open(req).await.unwrap();
    assert_eq!(relay.snapshot().active_streams, 1);

    // Read only 1 chunk
    let first = resp.body.next().await.unwrap().unwrap();
    assert!(!first.is_empty());

    // Drop the body stream prematurely (simulating caller closing player / skipping track)
    drop(resp.body);

    // Give background task a moment to notice cancellation and clean up
    tokio::time::sleep(Duration::from_millis(50)).await;

    let snap = relay.snapshot();
    assert_eq!(snap.active_streams, 0, "active_streams must return to 0 after drop");
    assert_eq!(snap.streams_cancelled, 1, "streams_cancelled must increment on premature drop");
}

// -----------------------------------------------------------------------------
// Scenario 7: Destination policy & redirect validation
// -----------------------------------------------------------------------------
#[tokio::test]
async fn test_scenario_7_destination_policy_and_redirect_validation() {
    let (addr, _shutdown) = start_fixture_server().await;

    // 1. netease_default() policy checks
    let netease = DestinationPolicy::netease_default();

    // Blocks loopback and RFC 1918 private IPs
    assert!(netease.validate_url("http://127.0.0.1/song.mp3").is_err());
    assert!(netease.validate_url("http://10.0.0.1/song.mp3").is_err());
    assert!(netease.validate_url("http://192.168.1.1/song.mp3").is_err());
    assert!(netease.validate_url("http://[::1]/song.mp3").is_err());
    assert!(netease.validate_url("http://100.64.0.1/song.mp3").is_err());
    assert!(netease.validate_url("http://localhost/song.mp3").is_err());
    assert!(netease.validate_url("http://test.local/song.mp3").is_err());

    // Blocks non-allowed host suffixes
    assert!(netease.validate_url("http://evil.com/song.mp3").is_err());
    assert!(netease.validate_url("http://evil126.net/song.mp3").is_err());

    // Blocks URL credentials
    assert!(netease.validate_url("http://user:pass@music.126.net/song.mp3").is_err());

    // Allows official NetEase CDN domains
    assert!(netease.validate_url("http://m7.music.126.net/song.mp3").is_ok());
    assert!(netease.validate_url("https://music.126.net/song.mp3").is_ok());
    assert!(netease.validate_url("http://vod.163.com/song.mp3").is_ok());

    // 2. Redirect validation via relay
    let test_policy = DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]);
    let relay = MediaRelay::new(RelayConfig::default(), test_policy).unwrap();

    // Permitted redirect is followed successfully
    let req_ok = MediaSourceRequest::get(format!("http://{addr}/redirect_permitted"));
    let resp_ok = relay.open(req_ok).await.unwrap();
    assert_eq!(resp_ok.meta.status, StatusCode::OK);
    assert_eq!(resp_ok.meta.content_length, Some(1000));

    // Redirect to untrusted/external domain is blocked with PolicyViolation
    let req_bad = MediaSourceRequest::get(format!("http://{addr}/redirect_untrusted"));
    let err_bad = relay.open(req_bad).await.unwrap_err();
    match err_bad {
        RelayError::PolicyViolation(msg) => {
            assert!(msg.contains("evil.com") || msg.contains("not match allowed"));
        }
        other => panic!("Expected PolicyViolation, got {other:?}"),
    }

    // 3. Header hygiene: Cookie and Authorization headers are never forwarded upstream
    let req_hygiene = MediaSourceRequest::get(format!("http://{addr}/check_headers"));
    let resp_hygiene = relay.open(req_hygiene).await.unwrap();
    assert_eq!(resp_hygiene.meta.status, StatusCode::OK);
}
