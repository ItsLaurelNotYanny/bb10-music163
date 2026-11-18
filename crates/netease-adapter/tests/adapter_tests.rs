//! Independent integration tests for `netease-adapter` (offline fixtures only).

use isahc::cookies::{CookieBuilder, CookieJar};
use isahc::http::Uri;
use netease_adapter::adapter::{NeteaseAdapter, QrPollOutcome, QrSession};
use netease_adapter::config::NeteaseConfig;
use netease_adapter::cookie_store::CookieStorage;
use netease_adapter::error::AdapterError;
use openssl::asn1::Asn1Time;
use openssl::bn::{BigNum, MsbOption};
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::rsa::Rsa;
use openssl::ssl::{SslAcceptor, SslMethod};
use openssl::x509::extension::{BasicConstraints, SubjectAlternativeName};
use openssl::x509::{X509Builder, X509NameBuilder};
use provider_model::{SessionCheckOutcome, TransientErrorKind};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn unused_local_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test]
async fn network_error_simulation_preserves_stored_credentials() {
    let dir = TempDir::new().unwrap();
    let storage = CookieStorage::new(dir.path());

    // Seed valid cookie jar on disk
    let jar = CookieJar::default();
    let uri: Uri = "https://music.163.com/".parse().unwrap();
    let cookie = CookieBuilder::new("MUSIC_U", "persisted-real-like-token")
        .domain("music.163.com")
        .path("/")
        .secure(true)
        .expiration(SystemTime::now() + Duration::from_secs(3600))
        .build()
        .unwrap();
    jar.set(cookie, &uri).unwrap();
    storage
        .save_cookie_jar(&jar, Some(1_700_000_000), Some(1_700_000_010), Some(999))
        .unwrap();

    let original_bytes = fs::read(storage.path()).unwrap();

    // Point api_proxy to an unreachable SOCKS5 port
    let dead_port = unused_local_port();
    let cfg = NeteaseConfig {
        api_proxy: Some(format!("socks5h://127.0.0.1:{dead_port}")),
        media_proxy: None,
        tls_verify: true,
        state_dir: dir.path().to_path_buf(),
        max_connections: 8,
    };

    let adapter = NeteaseAdapter::new(cfg.clone()).unwrap();
    assert!(adapter.restored_from_disk());
    assert!(adapter.cookie_summary().has_music_u);

    // Status check must classify unreachable proxy as TransientError and preserve credentials
    let outcome = adapter.check_status().await;
    match outcome {
        SessionCheckOutcome::TransientError {
            kind,
            credentials_preserved,
            ..
        } => {
            assert_eq!(kind, TransientErrorKind::NetworkOrProxy);
            assert!(credentials_preserved);
        }
        other => panic!("expected TransientError, got {other:?}"),
    }

    // Verify cookie file on disk is completely unchanged
    let after_bytes = fs::read(storage.path()).unwrap();
    assert_eq!(original_bytes, after_bytes);

    // Restarting adapter restores the exact same session cookies
    let restarted = NeteaseAdapter::new(cfg).unwrap();
    assert!(restarted.restored_from_disk());
    assert!(restarted.cookie_summary().has_music_u);
}

#[tokio::test]
async fn tls_validation_rejects_untrusted_self_signed_certificate() {
    let dir = TempDir::new().unwrap();

    // Generate an ephemeral self-signed certificate for 127.0.0.1
    let rsa = Rsa::generate(2048).unwrap();
    let pkey = PKey::from_rsa(rsa).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", "127.0.0.1").unwrap();
    let name = name.build();

    let mut serial = BigNum::new().unwrap();
    serial.rand(64, MsbOption::MAYBE_ZERO, false).unwrap();
    let serial = serial.to_asn1_integer().unwrap();

    let mut builder = X509Builder::new().unwrap();
    builder.set_version(2).unwrap();
    builder.set_serial_number(&serial).unwrap();
    builder.set_subject_name(&name).unwrap();
    builder.set_issuer_name(&name).unwrap();
    builder.set_pubkey(&pkey).unwrap();
    let not_before = Asn1Time::days_from_now(0).unwrap();
    let not_after = Asn1Time::days_from_now(1).unwrap();
    builder.set_not_before(&not_before).unwrap();
    builder.set_not_after(&not_after).unwrap();
    builder
        .append_extension(BasicConstraints::new().critical().ca().build().unwrap())
        .unwrap();
    let san = SubjectAlternativeName::new()
        .ip("127.0.0.1")
        .build(&builder.x509v3_context(None, None))
        .unwrap();
    builder.append_extension(san).unwrap();
    builder.sign(&pkey, MessageDigest::sha256()).unwrap();
    let cert = builder.build();

    let mut acceptor = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    acceptor.set_private_key(&pkey).unwrap();
    acceptor.set_certificate(&cert).unwrap();
    let acceptor = acceptor.build();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server_handle = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = acceptor.accept(stream);
        }
    });

    let cfg = NeteaseConfig {
        api_proxy: None,
        media_proxy: None,
        tls_verify: true,
        state_dir: dir.path().to_path_buf(),
        max_connections: 4,
    };
    let adapter = NeteaseAdapter::new(cfg).unwrap();

    let err = adapter
        .probe_proxy_egress(&format!("https://127.0.0.1:{port}/"))
        .await
        .unwrap_err();
    match err {
        AdapterError::Transient { kind, .. } => {
            assert_eq!(kind, TransientErrorKind::TlsValidation);
        }
        other => panic!("expected TlsValidation error, got {other:?}"),
    }

    let _ = server_handle.join();
}

#[tokio::test]
async fn probe_measures_local_http_egress() {
    let dir = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let server_handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });

    let cfg = NeteaseConfig {
        api_proxy: None,
        media_proxy: None,
        tls_verify: true,
        state_dir: dir.path().to_path_buf(),
        max_connections: 4,
    };
    let adapter = NeteaseAdapter::new(cfg).unwrap();
    let report = adapter
        .probe_proxy_egress(&format!("http://127.0.0.1:{port}/health"))
        .await
        .unwrap();

    assert_eq!(report.status_code, 200);
    assert!(report.tls_verify_enabled);
    assert!(report.bytes_uploaded > 0);
    assert!(report.bytes_downloaded > 0);
    let snap = adapter.api_counters();
    assert_eq!(snap.api_requests_total, 1);
    assert_eq!(snap.api_requests_failed, 0);
    assert_eq!(snap.probe_bytes_uploaded, report.bytes_uploaded);
    assert_eq!(snap.probe_bytes_downloaded, report.bytes_downloaded);
    let _ = server_handle.join();
}

#[tokio::test]
async fn qr_polling_enforces_poll_limit_and_ttl_bounds() {
    let dir = TempDir::new().unwrap();
    let cfg = NeteaseConfig {
        api_proxy: None,
        media_proxy: None,
        tls_verify: true,
        state_dir: dir.path().to_path_buf(),
        max_connections: 4,
    };
    let adapter = NeteaseAdapter::new(cfg).unwrap();

    let mut capped = QrSession {
        unikey: "test-key".into(),
        qr_url: "https://music.163.com/login?codekey=test-key".into(),
        qr_ascii: String::new(),
        qr_svg: String::new(),
        created_at_epoch_secs: 1_700_000_000,
        expires_at_epoch_secs: u64::MAX,
        max_polls: 3,
        polls_used: 3,
    };
    assert!(matches!(
        adapter.poll_qr_once(&mut capped).await,
        QrPollOutcome::PollLimitReached { .. }
    ));

    let mut expired = QrSession {
        unikey: "test-key".into(),
        qr_url: "https://music.163.com/login?codekey=test-key".into(),
        qr_ascii: String::new(),
        qr_svg: String::new(),
        created_at_epoch_secs: 100,
        expires_at_epoch_secs: 200, // already expired
        max_polls: 10,
        polls_used: 0,
    };
    assert!(matches!(
        adapter.poll_qr_once(&mut expired).await,
        QrPollOutcome::Expired { .. }
    ));
}
