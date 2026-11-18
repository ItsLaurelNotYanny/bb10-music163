//! Configuration and validation for the NetEase adapter.

use crate::error::AdapterError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;

/// Configuration for the NetEase upstream adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeteaseConfig {
    /// Proxy URL for NetEase API requests (e.g. `socks5h://127.0.0.1:1080`).
    /// Use `socks5h://` so DNS resolution runs on the proxy side.
    pub api_proxy: Option<String>,
    /// Separate proxy setting reserved for media relay (M2 split-routing).
    pub media_proxy: Option<String>,
    /// Enforce TLS certificate and hostname verification (must remain true).
    pub tls_verify: bool,
    /// Directory for atomic cookie persistence and secret-free session logs.
    pub state_dir: PathBuf,
    /// Maximum concurrent upstream connections in `ncm-api`.
    pub max_connections: usize,
}

impl Default for NeteaseConfig {
    fn default() -> Self {
        Self {
            api_proxy: Some("socks5h://127.0.0.1:1080".to_string()),
            media_proxy: Some("socks5h://127.0.0.1:1080".to_string()),
            tls_verify: true,
            state_dir: PathBuf::from("./state"),
            max_connections: 32,
        }
    }
}

impl NeteaseConfig {
    /// Validate configuration invariants before constructing network clients.
    pub fn validate(&self) -> Result<(), AdapterError> {
        if !self.tls_verify {
            return Err(AdapterError::Config(
                "tls_verify cannot be disabled; TLS validation must remain enabled".to_string(),
            ));
        }

        if let Some(ref proxy) = self.api_proxy {
            validate_proxy_url(proxy)?;
        }
        if let Some(ref proxy) = self.media_proxy {
            validate_proxy_url(proxy)?;
        }

        Ok(())
    }
}

/// Validate a proxy URL and verify it uses a supported scheme and valid host/port.
pub fn validate_proxy_url(raw: &str) -> Result<Url, AdapterError> {
    let parsed = Url::parse(raw)
        .map_err(|e| AdapterError::Config(format!("invalid proxy URL '{raw}': {e}")))?;

    match parsed.scheme() {
        "socks5h" | "socks5" | "http" | "https" => {}
        other => {
            return Err(AdapterError::Config(format!(
                "unsupported proxy scheme '{other}'; expected socks5h, socks5, http, or https"
            )));
        }
    }

    if parsed.host_str().is_none() || parsed.port().is_none() {
        return Err(AdapterError::Config(format!(
            "proxy URL '{raw}' must include both host and explicit port"
        )));
    }

    // Verify `http::Uri` compatibility since `ncm-api` / `isahc` parses proxy via `http::Uri`.
    let _: isahc::http::Uri = raw.parse().map_err(|e| {
        AdapterError::Config(format!("proxy URL '{raw}' is not a valid HTTP URI: {e}"))
    })?;

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_socks5h_remote_dns_proxy() {
        let cfg = NeteaseConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn rejects_disabled_tls_validation() {
        let cfg = NeteaseConfig {
            tls_verify: false,
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("tls_verify"));
    }

    #[test]
    fn rejects_proxy_missing_port_or_bad_scheme() {
        assert!(validate_proxy_url("socks5h://127.0.0.1").is_err());
        assert!(validate_proxy_url("ftp://127.0.0.1:1080").is_err());
        assert!(validate_proxy_url("not a url").is_err());
    }
}
