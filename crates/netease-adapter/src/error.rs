//! Error types and classification separating confirmed logout from transient transport failures.

use provider_model::{CapabilityError, TransientErrorKind};
use thiserror::Error;

/// Adapter-level errors with explicit distinction between transient transport issues,
/// confirmed authentication rejection, and capability constraints.
#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("cookie persistence error: {0}")]
    Storage(String),

    #[error("transient upstream/transport error ({kind:?}): {message}")]
    Transient {
        kind: TransientErrorKind,
        message: String,
    },

    #[error("confirmed unauthenticated or logged out (code {code}): {message}")]
    ConfirmedLoggedOut { code: i32, message: String },

    #[error("capability error: {0}")]
    Capability(#[from] CapabilityError),

    #[error("upstream protocol/parse error: {0}")]
    Upstream(String),
}

/// Classification outcome when analyzing an `ncm_api::MusicApi::login_status` error string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusErrorClass {
    /// Upstream returned valid JSON indicating an unauthenticated session (e.g. `profile: null`).
    ConfirmedLoggedOut { code: i32, reason: String },
    /// Transport, proxy, TLS, timeout, or non-JSON gateway failure; credentials must be preserved.
    Transient {
        kind: TransientErrorKind,
        reason: String,
    },
}

/// Classify an error string emitted by `ncm_api::MusicApi::login_status()`.
///
/// In `netease-cloud-music-api` 2.1.0:
/// - Transport/HTTP send/receive errors are mapped to `anyhow!("none")`.
/// - `to_login_info()` returns missing-key or null-type deserialization errors when
///   NetEase returns `{"code":200,"account":null,"profile":null}` for an expired/logged-out session.
/// - Non-JSON HTML responses (e.g. 502 from a proxy) fail in `serde_json::from_str`.
pub fn classify_login_status_error(err_msg: &str) -> StatusErrorClass {
    let trimmed = err_msg.trim();
    if trimmed == "none" {
        return StatusErrorClass::Transient {
            kind: TransientErrorKind::NetworkOrProxy,
            reason: "upstream HTTP request failed (transport, proxy, or timeout)".to_string(),
        };
    }

    if trimmed.contains("key 'userId' not found")
        || trimmed.contains("key 'profile' not found")
        || trimmed.contains("invalid type: null")
    {
        return StatusErrorClass::ConfirmedLoggedOut {
            code: 200,
            reason: "upstream returned unauthenticated session payload (null profile)".to_string(),
        };
    }

    if trimmed.contains("expected value")
        || trimmed.contains("EOF while parsing")
        || trimmed.contains("expected ident")
    {
        return StatusErrorClass::Transient {
            kind: TransientErrorKind::UpstreamGateway,
            reason: format!("upstream returned non-JSON response: {trimmed}"),
        };
    }

    StatusErrorClass::Transient {
        kind: TransientErrorKind::UnexpectedPayload,
        reason: format!("unclassified status check error: {trimmed}"),
    }
}

/// Classify an `isahc::Error` into our `TransientErrorKind`.
pub fn classify_isahc_error(err: &isahc::Error) -> TransientErrorKind {
    let msg = err.to_string().to_lowercase();
    if err.is_timeout() || msg.contains("timed out") || msg.contains("timeout") {
        TransientErrorKind::Timeout
    } else if err.is_tls()
        || msg.contains("tls")
        || msg.contains("ssl")
        || msg.contains("certificate")
        || msg.contains("cert")
    {
        TransientErrorKind::TlsValidation
    } else if err.is_network() || msg.contains("proxy") || msg.contains("socks") || msg.contains("connect") {
        TransientErrorKind::NetworkOrProxy
    } else {
        TransientErrorKind::UpstreamGateway
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_error_is_classified_as_transient_network_or_proxy() {
        assert_eq!(
            classify_login_status_error("none"),
            StatusErrorClass::Transient {
                kind: TransientErrorKind::NetworkOrProxy,
                reason: "upstream HTTP request failed (transport, proxy, or timeout)".to_string(),
            }
        );
    }

    #[test]
    fn null_profile_error_is_classified_as_confirmed_logged_out() {
        let err = "at src/model.rs:885\n\nCaused by:\n    invalid type: null, expected u64";
        assert_eq!(
            classify_login_status_error(err),
            StatusErrorClass::ConfirmedLoggedOut {
                code: 200,
                reason: "upstream returned unauthenticated session payload (null profile)"
                    .to_string(),
            }
        );
    }

    #[test]
    fn non_json_html_error_is_classified_as_transient_gateway() {
        let err = "expected value at line 1 column 1";
        match classify_login_status_error(err) {
            StatusErrorClass::Transient { kind, .. } => {
                assert_eq!(kind, TransientErrorKind::UpstreamGateway);
            }
            other => panic!("unexpected classification: {other:?}"),
        }
    }
}
