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
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("listen", &self.listen)
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

    /// Validate that HTTP server authentication is configured with a non-placeholder token.
    pub fn validate_for_server(&self) -> Result<()> {
        let token = self.server.gateway_token.trim();
        if token.is_empty() || token == "replace-with-strong-gateway-token" {
            bail!(
                "gateway_token must be set to a non-empty secret (and not the example placeholder) before starting HTTP server"
            );
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
}
