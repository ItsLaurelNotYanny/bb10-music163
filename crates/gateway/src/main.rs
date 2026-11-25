//! BB10 Music163 Gateway binary (CLI + authenticated HTTP API/player server).

mod backend;
mod config;
mod server;
mod tls;

use crate::config::GatewayConfig;
use crate::server::{build_router, AppState};
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use futures_lite::StreamExt;
use media_relay::{DestinationPolicy, MediaRelay, MediaSourceRequest};
use netease_adapter::{NeteaseAdapter, QrPollOutcome};
use provider_model::{AudioQuality, PlaybackResolution};
use std::sync::Arc;
use std::time::Duration;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "gateway", about = "BB10 Music163 Gateway (M1 Account & Transport)")]
struct Cli {
    /// Path to gateway TOML configuration file.
    #[arg(long, global = true, env = "GATEWAY_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Perform interactive QR login with bounded polling and atomic cookie persistence.
    QrLogin {
        /// Local QR session expiration deadline in seconds.
        #[arg(long, default_value_t = 180)]
        ttl: u64,
        /// Polling interval in seconds.
        #[arg(long, default_value_t = 2)]
        interval: u64,
        /// Maximum number of poll attempts.
        #[arg(long, default_value_t = 90)]
        max_polls: u32,
    },
    /// Request an SMS verification code (optional login path).
    SmsSend {
        #[arg(long, default_value = "86")]
        ctcode: String,
        #[arg(long)]
        phone: String,
    },
    /// Complete SMS login using a verification code (optional login path).
    SmsLogin {
        #[arg(long, default_value = "86")]
        ctcode: String,
        #[arg(long)]
        phone: String,
        #[arg(long)]
        captcha: String,
    },
    /// Restore persisted cookies and verify upstream account status.
    Status,
    /// Resolve an authorized track playback URL.
    Resolve {
        /// NetEase numeric song ID.
        song_id: u64,
        /// Requested quality tier.
        #[arg(long, value_enum, default_value_t = CliQuality::Standard)]
        quality: CliQuality,
    },
    /// Resolve an authorized track and stream a bounded byte slice through MediaRelay (M2 proof).
    RelayCheck {
        /// NetEase numeric song ID.
        song_id: u64,
        /// Optional HTTP Range header (defaults to first 64 KiB).
        #[arg(long, default_value = "bytes=0-65535")]
        range: Option<String>,
        /// Requested quality tier.
        #[arg(long, value_enum, default_value_t = CliQuality::Standard)]
        quality: CliQuality,
    },
    /// Probe proxy egress bytes and TLS certificate validation.
    ProbeProxy {
        #[arg(long, default_value = "https://music.163.com/")]
        url: String,
    },
    /// Print provider capability matrix and renewal support findings.
    Capabilities,
    /// Start the authenticated HTTP gateway server.
    Serve,
}

#[derive(Copy, Clone, ValueEnum)]
enum CliQuality {
    Standard,
    Higher,
    Extreme,
    Lossless,
    Hires,
}

impl From<CliQuality> for AudioQuality {
    fn from(q: CliQuality) -> Self {
        match q {
            CliQuality::Standard => AudioQuality::Standard,
            CliQuality::Higher => AudioQuality::Higher,
            CliQuality::Extreme => AudioQuality::Extreme,
            CliQuality::Lossless => AudioQuality::Lossless,
            CliQuality::Hires => AudioQuality::HiRes,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let cfg = GatewayConfig::load(cli.config.as_deref())?;
    let adapter = Arc::new(NeteaseAdapter::new(cfg.to_netease_config())?);

    match cli.command {
        Commands::Capabilities => {
            let caps = NeteaseAdapter::capabilities();
            println!("{}", serde_json::to_string_pretty(&caps)?);
        }
        Commands::QrLogin {
            ttl,
            interval,
            max_polls,
        } => {
            println!("Creating NetEase QR login session...");
            println!(
                "Proxy: {:?}, TLS verify: {}",
                cfg.proxy.api_proxy, cfg.proxy.tls_verify
            );
            let mut session = adapter.start_qr_login(ttl, max_polls).await?;
            println!("\nScan this QR code with the NetEase Cloud Music app:\n");
            println!("{}", session.qr_ascii);
            println!("QR URL: {}", session.qr_url);
            println!(
                "Bounded polling started (max_polls={}, ttl={}s, interval={}s)...",
                session.max_polls, ttl, interval
            );

            loop {
                let outcome = adapter.poll_qr_once(&mut session).await;
                match outcome {
                    QrPollOutcome::WaitingForScan {
                        polls_remaining, ..
                    } => {
                        println!("Waiting for scan (polls remaining: {polls_remaining})...");
                    }
                    QrPollOutcome::WaitingForConfirm {
                        polls_remaining, ..
                    } => {
                        println!("Scanned; waiting for confirmation in app (polls remaining: {polls_remaining})...");
                    }
                    QrPollOutcome::TransientPollError {
                        message,
                        polls_remaining,
                    } => {
                        eprintln!("Transient poll error ({polls_remaining} left): {message}");
                    }
                    QrPollOutcome::Confirmed { profile, cookies } => {
                        println!("\nLogin confirmed!");
                        println!("Cookie file: {}", adapter.cookie_file_path().display());
                        println!("{}", serde_json::to_string_pretty(&cookies)?);
                        if let Some(p) = profile {
                            println!("Account profile: {}", serde_json::to_string_pretty(&p)?);
                        }
                        let (supported, note) = adapter.check_renewal_support();
                        println!("Unattended renewal supported: {supported} ({note})");
                        break;
                    }
                    QrPollOutcome::Expired { message, .. } => {
                        bail!("QR session expired: {message}");
                    }
                    QrPollOutcome::PollLimitReached { reason } => {
                        bail!("QR polling stopped: {reason}");
                    }
                }
                tokio::time::sleep(Duration::from_secs(interval)).await;
            }
        }
        Commands::SmsSend { ctcode, phone } => {
            adapter.send_sms_captcha(&ctcode, &phone).await?;
            println!("SMS verification code requested.");
        }
        Commands::SmsLogin {
            ctcode,
            phone,
            captcha,
        } => {
            let profile = adapter.login_cellphone(&ctcode, &phone, &captcha).await?;
            println!("SMS login confirmed:");
            println!("{}", serde_json::to_string_pretty(&profile)?);
        }
        Commands::Status => {
            println!(
                "Restored cookies from disk: {} ({})",
                adapter.restored_from_disk(),
                adapter.cookie_file_path().display()
            );
            println!(
                "Cookie summary (secret-free): {}",
                serde_json::to_string(&adapter.cookie_summary())?
            );
            let outcome = adapter.check_status().await;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        Commands::Resolve { song_id, quality } => {
            let res = adapter.resolve_track(song_id, quality.into()).await?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Commands::RelayCheck {
            song_id,
            range,
            quality,
        } => {
            let resolution = adapter.resolve_track(song_id, quality.into()).await?;
            let source = match resolution {
                PlaybackResolution::RelayableMedia(src) => src,
                other => bail!("track resolved to non-relayable mode: {other:?}"),
            };
            let relay = MediaRelay::new(cfg.to_relay_config(), DestinationPolicy::netease_default())?;
            let mut req = MediaSourceRequest::get(&source.url);
            if let Some(ref r) = range {
                if !r.trim().is_empty() {
                    req = req.with_range(r.trim());
                }
            }
            let mut stream_res = relay.open(req).await?;
            let mut total_bytes: u64 = 0;
            while let Some(chunk) = stream_res.body.next().await {
                let bytes = chunk?;
                total_bytes += bytes.len() as u64;
            }
            let report = serde_json::json!({
                "song_id": song_id,
                "status_code": stream_res.meta.status.as_u16(),
                "content_type": stream_res.meta.headers.get(axum::http::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
                "content_length": stream_res.meta.content_length,
                "bytes_streamed": total_bytes,
                "bitrate": source.bitrate,
                "format_hint": source.format_hint,
                "api_counters": adapter.api_counters(),
                "media_counters": relay.snapshot(),
            });
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Commands::ProbeProxy { url } => {
            let report = adapter.probe_proxy_egress(&url).await?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Commands::Serve => {
            cfg.validate_for_server()?;
            let relay = Arc::new(MediaRelay::new(
                cfg.to_relay_config(),
                DestinationPolicy::netease_default(),
            )?);

            // Validate TLS material and bind both sockets before declaring readiness.
            let tls_binding = match &cfg.server.tls_listen {
                Some(tls_addr) => {
                    let cert = cfg
                        .server
                        .tls_cert
                        .as_deref()
                        .context("missing tls_cert after validation")?;
                    let key = cfg
                        .server
                        .tls_key
                        .as_deref()
                        .context("missing tls_key after validation")?;
                    let rustls_cfg = tls::build_tls_config(cert, key)?;
                    let listener = tls::bind_std_listener(tls_addr)?;
                    Some((tls_addr.clone(), listener, rustls_cfg))
                }
                None => None,
            };
            let http_listener = tls::bind_std_listener(&cfg.server.listen)?;

            let state = AppState::new(
                adapter.clone(),
                relay,
                cfg.server.gateway_token.clone(),
                cfg.state.state_dir.clone(),
            );
            let restored_gen = state.playback.lock().await.generation();

            println!("Gateway HTTP server listening on {}", cfg.server.listen);
            println!("  Player UI:       http://{}/player", cfg.server.listen);
            if let Some((ref tls_addr, _, _)) = tls_binding {
                println!("Gateway HTTPS server listening on {tls_addr}");
                println!("  Player UI (TLS): https://{tls_addr}/player");
            }
            println!("  State dir:       {}", cfg.state.state_dir.display());
            println!("  Restored cookies: {}", adapter.restored_from_disk());
            println!("  Restored gen:    {restored_gen}");
            println!("  API proxy:       {:?}", cfg.proxy.api_proxy);
            println!("  Media proxy:     {:?}", cfg.proxy.media_proxy);
            println!("  TLS verify:      {}", cfg.proxy.tls_verify);

            let app = build_router(state.clone());
            let tls_listener = tls_binding.map(|(_, l, c)| (l, c));
            tls::serve_listeners(
                http_listener,
                tls_listener,
                app,
                state,
                tls::shutdown_signal(),
                tls::DEFAULT_SHUTDOWN_BUDGET,
            )
            .await?;
        }
    }

    Ok(())
}
