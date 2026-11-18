use thiserror::Error;

/// Errors returned by the media relay client.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// Configuration parameter is invalid (e.g., disallowed proxy scheme, invalid timeouts).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Request or redirect target violated destination policy (e.g., private IP, disallowed scheme/host).
    #[error("policy violation: {0}")]
    PolicyViolation(String),

    /// Request specification was invalid (e.g., malformed URL, illegal headers).
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Upstream HTTP server returned an error status code before response headers were yielded to caller.
    #[error("upstream HTTP error before headers (status {status}): {message}")]
    UpstreamPreHeader { status: u16, message: String },

    /// Stream transmission failed or stalled mid-stream after response headers were already yielded.
    #[error("mid-stream failure after {bytes_sent} bytes (expected {expected_bytes:?}): {reason}")]
    MidStreamFailure {
        bytes_sent: u64,
        expected_bytes: Option<u64>,
        reason: String,
    },

    /// Network or transport level failure when communicating with upstream server.
    #[error("transport error: {0}")]
    Transport(String),

    /// Operation timed out during connection or reading.
    #[error("timeout: {0}")]
    Timeout(String),

    /// Operation or stream was cancelled externally.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<isahc::Error> for RelayError {
    fn from(err: isahc::Error) -> Self {
        if err.kind() == isahc::error::ErrorKind::Timeout {
            RelayError::Timeout(err.to_string())
        } else {
            RelayError::Transport(err.to_string())
        }
    }
}
