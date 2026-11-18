//! Secret-free append-only JSONL session lifecycle log for M1 and M6 longevity tracking.

use crate::cookie_store::now_epoch_secs;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const SESSION_LOG_FILE: &str = "session_events.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionEvent {
    LoginConfirmed {
        timestamp_epoch_secs: u64,
        method: String,
        uid: Option<u64>,
        cookie_count: usize,
        has_music_u: bool,
    },
    StatusChecked {
        timestamp_epoch_secs: u64,
        outcome: String,
        uid: Option<u64>,
        detail: String,
    },
    TrackResolved {
        timestamp_epoch_secs: u64,
        song_id: u64,
        bitrate: u32,
        ok: bool,
        detail: String,
    },
    RenewalCapabilityChecked {
        timestamp_epoch_secs: u64,
        supported: bool,
        note: String,
    },
    ProxyEgressProbed {
        timestamp_epoch_secs: u64,
        proxy_used: Option<String>,
        tls_verify: bool,
        status_code: Option<u16>,
        bytes_uploaded: u64,
        bytes_downloaded: u64,
        ok: bool,
    },
}

#[derive(Debug, Clone)]
pub struct SessionLogger {
    path: PathBuf,
}

impl SessionLogger {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(SESSION_LOG_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a secret-free event line. Failures are logged via `tracing` without crashing callers.
    pub fn record(&self, event: SessionEvent) {
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                let _ = fs::create_dir_all(parent);
                #[cfg(unix)]
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }

        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };

        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        opts.mode(0o600);

        if let Ok(mut file) = opts.open(&self.path) {
            let _ = writeln!(file, "{line}");
        }
    }

    pub fn now() -> u64 {
        now_epoch_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn records_secret_free_jsonl_events() {
        let dir = TempDir::new().unwrap();
        let logger = SessionLogger::new(dir.path());
        logger.record(SessionEvent::LoginConfirmed {
            timestamp_epoch_secs: 1_700_000_000,
            method: "qr".to_string(),
            uid: Some(123456),
            cookie_count: 5,
            has_music_u: true,
        });
        let content = fs::read_to_string(logger.path()).unwrap();
        assert!(content.contains("\"event\":\"login_confirmed\""));
        assert!(content.contains("123456"));
    }
}
