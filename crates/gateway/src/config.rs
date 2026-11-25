//! Gateway configuration loading and validation.

use anyhow::{bail, Context, Result};
use media_relay::RelayConfig;
use netease_adapter::NeteaseConfig;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level gateway TOML configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub server: ServerConfig,
    pub proxy: ProxyConfig,
    pub state: StateConfig,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub listen: String,
    pub gateway_token: String,
    /// Optional native HTTPS listen address (e.g. `0.0.0.0:8443`).
    #[serde(default)]
    pub tls_listen: Option<String>,
    /// PEM certificate chain path for the optional HTTPS listener.
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    /// PEM private key path for the optional HTTPS listener.
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("listen", &self.listen)
            .field("tls_listen", &self.tls_listen)
            .field("tls_cert", &self.tls_cert)
            .field("tls_key", &self.tls_key)
            .field("gateway_token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub api_proxy: Option<String>,
    pub media_proxy: Option<String>,
    pub tls_verify: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateConfig {
    pub state_dir: PathBuf,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                listen: "127.0.0.1:8090".to_string(),
                gateway_token: String::new(),
                tls_listen: None,
                tls_cert: None,
                tls_key: None,
            },
            proxy: ProxyConfig {
                api_proxy: Some("socks5h://127.0.0.1:1080".to_string()),
                media_proxy: Some("socks5h://127.0.0.1:1080".to_string()),
                tls_verify: true,
            },
            state: StateConfig {
                state_dir: PathBuf::from("./state"),
            },
        }
    }
}

impl GatewayConfig {
    /// Load configuration from an optional TOML file and apply environment overrides.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut cfg = match path {
            Some(p) => {
                let raw = fs::read_to_string(p)
                    .with_context(|| format!("failed to read config file {}", p.display()))?;
                toml::from_str::<GatewayConfig>(&raw)
                    .with_context(|| format!("failed to parse TOML config {}", p.display()))?
            }
            None => Self::default(),
        };

        if let Ok(v) = env::var("GATEWAY_LISTEN") {
            if !v.is_empty() {
                cfg.server.listen = v;
            }
        }
        if let Ok(v) = env::var("GATEWAY_TLS_LISTEN") {
            let trimmed = v.trim();
            cfg.server.tls_listen = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Ok(v) = env::var("GATEWAY_TLS_CERT") {
            let trimmed = v.trim();
            cfg.server.tls_cert = if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            };
        }
        if let Ok(v) = env::var("GATEWAY_TLS_KEY") {
            let trimmed = v.trim();
            cfg.server.tls_key = if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            };
        }
        if let Ok(v) = env::var("GATEWAY_TOKEN") {
            if !v.is_empty() {
                cfg.server.gateway_token = v;
            }
        }
        if let Ok(v) = env::var("GATEWAY_API_PROXY") {
            cfg.proxy.api_proxy = if v.trim().is_empty() { None } else { Some(v) };
        }
        if let Ok(v) = env::var("GATEWAY_MEDIA_PROXY") {
            cfg.proxy.media_proxy = if v.trim().is_empty() { None } else { Some(v) };
        }
        if let Ok(v) = env::var("GATEWAY_STATE_DIR") {
            if !v.is_empty() {
                cfg.state.state_dir = PathBuf::from(v);
            }
        }
        if let Ok(v) = env::var("GATEWAY_TLS_VERIFY") {
            cfg.proxy.tls_verify = match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" => true,
                "0" | "false" | "no" => false,
                _ => bail!("invalid boolean value for GATEWAY_TLS_VERIFY: {v}"),
            };
        }

        cfg.to_netease_config().validate()?;
        Ok(cfg)
    }

    /// Validate that HTTP/HTTPS server authentication and optional TLS paths are configured.
    pub fn validate_for_server(&self) -> Result<()> {
        let token = self.server.gateway_token.trim();
        if token.is_empty() || token == "replace-with-strong-gateway-token" {
            bail!(
                "gateway_token must be set to a non-empty secret (and not the example placeholder) before starting HTTP server"
            );
        }
        if let Some(ref tls_listen) = self.server.tls_listen {
            let tls_addr = tls_listen.trim();
            if tls_addr.is_empty() {
                bail!("tls_listen must not be blank when set");
            }
            if tls_addr == self.server.listen.trim() {
                bail!("tls_listen ({tls_addr}) must not match HTTP listen address");
            }
            let cert = self
                .server
                .tls_cert
                .as_ref()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "GATEWAY_TLS_CERT (server.tls_cert) is required when GATEWAY_TLS_LISTEN is set"
                    )
                })?;
            let key = self
                .server
                .tls_key
                .as_ref()
                .filter(|p| !p.as_os_str().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "GATEWAY_TLS_KEY (server.tls_key) is required when GATEWAY_TLS_LISTEN is set"
                    )
                })?;
            if !cert.is_file() {
                bail!("TLS certificate file not found: {}", cert.display());
            }
            if !key.is_file() {
                bail!("TLS private key file not found: {}", key.display());
            }
        }
        Ok(())
    }

    /// Convert to `NeteaseConfig`.
    pub fn to_netease_config(&self) -> NeteaseConfig {
        NeteaseConfig {
            api_proxy: self.proxy.api_proxy.clone(),
            media_proxy: self.proxy.media_proxy.clone(),
            tls_verify: self.proxy.tls_verify,
            state_dir: self.state.state_dir.clone(),
            max_connections: 32,
        }
    }

    /// Convert to `RelayConfig` for media forwarding.
    pub fn to_relay_config(&self) -> RelayConfig {
        RelayConfig {
            media_proxy: self.proxy.media_proxy.clone(),
            tls_verify: self.proxy.tls_verify,
            ..RelayConfig::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_example_style_toml_and_redacts_token_in_debug() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            r#"
[server]
listen = "127.0.0.1:8095"
gateway_token = "super-secret-gateway-token"

[proxy]
api_proxy = "socks5h://127.0.0.1:1080"
media_proxy = "socks5h://127.0.0.1:1080"
tls_verify = true

[state]
state_dir = "/tmp/test-state"
"#
        )
        .unwrap();

        let cfg = GatewayConfig::load(Some(tmp.path())).unwrap();
        assert_eq!(cfg.server.listen, "127.0.0.1:8095");
        assert!(cfg.validate_for_server().is_ok());

        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret-gateway-token"));
        assert!(dbg.contains("[REDACTED]"));
    }

    #[test]
    fn rejects_placeholder_or_empty_gateway_token_for_server() {
        let mut cfg = GatewayConfig::default();
        assert!(cfg.validate_for_server().is_err());

        cfg.server.gateway_token = "replace-with-strong-gateway-token".to_string();
        assert!(cfg.validate_for_server().is_err());
    }

    #[test]
    fn validates_optional_tls_paths_and_preserves_http_only_mode() {
        let mut cfg = GatewayConfig::default();
        cfg.server.gateway_token = "strong-test-token-123".to_string();
        // HTTP-only mode without TLS settings succeeds
        assert!(cfg.validate_for_server().is_ok());

        // TLS listen without cert/key fails fast
        cfg.server.tls_listen = Some("127.0.0.1:8443".to_string());
        assert!(cfg.validate_for_server().is_err());

        // TLS listen matching HTTP listen fails fast
        cfg.server.tls_listen = Some(cfg.server.listen.clone());
        assert!(cfg.validate_for_server().is_err());

        // Non-existent cert/key paths fail fast
        cfg.server.tls_listen = Some("127.0.0.1:8443".to_string());
        cfg.server.tls_cert = Some(PathBuf::from("/nonexistent/server.crt"));
        cfg.server.tls_key = Some(PathBuf::from("/nonexistent/server.key"));
        assert!(cfg.validate_for_server().is_err());

        // Existing cert/key files pass config validation
        let cert_file = NamedTempFile::new().unwrap();
        let key_file = NamedTempFile::new().unwrap();
        cfg.server.tls_cert = Some(cert_file.path().to_path_buf());
        cfg.server.tls_key = Some(key_file.path().to_path_buf());
        assert!(cfg.validate_for_server().is_ok());
    }
}
