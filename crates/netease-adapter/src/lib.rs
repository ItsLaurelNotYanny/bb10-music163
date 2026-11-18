//! NetEase Cloud Music adapter crate.
//!
//! Wraps pinned `netease-cloud-music-api` (`2.1.0`, GPLv3) with durable atomic
//! private cookie persistence, SOCKS5 remote DNS configuration, TLS validation
//! enforcement, bounded QR polling, and explicit error classification.

pub mod adapter;
pub mod catalogue;
pub mod config;
pub mod cookie_store;
pub mod error;
pub mod session_log;

pub use adapter::{
    ApiTrafficCounters, ApiTrafficSnapshot, NeteaseAdapter, ProxyProbeReport, QrPollOutcome,
    QrSession,
};
pub use catalogue::{PlaylistDetailSummary, PlaylistSummary, TrackSummary};
pub use config::{validate_proxy_url, NeteaseConfig};
pub use cookie_store::{CookieStorage, CookieSummary};
pub use error::{classify_login_status_error, AdapterError, StatusErrorClass};
pub use session_log::{SessionEvent, SessionLogger};
