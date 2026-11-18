//! Durable, atomic, and private cookie storage for NetEase upstream sessions.
//!
//! Cookie values are never emitted in `Debug` output, logs, URLs, or API responses.

use crate::error::AdapterError;
use isahc::cookies::{CookieBuilder, CookieJar};
use isahc::http::Uri;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const COOKIE_FILE_NAME: &str = "netease_cookies.json";
const COOKIE_SCHEMA_VERSION: u32 = 1;

static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// URIs inspected when harvesting cookies from `isahc::cookies::CookieJar`.
/// Includes `interface.music.163.com` (used by EAPI) and the subpaths observed by GTK 2.5.4.
pub const HARVEST_URIS: [&str; 13] = [
    "https://music.163.com/",
    "https://interface.music.163.com/",
    "https://music.163.com/eapi/clientlog",
    "https://music.163.com/eapi/feedback",
    "https://music.163.com/api/clientlog",
    "https://music.163.com/api/feedback",
    "https://music.163.com/neapi/clientlog",
    "https://music.163.com/neapi/feedback",
    "https://music.163.com/weapi/clientlog",
    "https://music.163.com/weapi/feedback",
    "https://music.163.com/wapi/clientlog",
    "https://music.163.com/wapi/feedback",
    "https://music.163.com/openapi/clientlog",
];

/// Secret-free summary of cookies currently in a `CookieJar` or loaded from disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieSummary {
    pub total_cookies: usize,
    pub has_music_u: bool,
    pub has_csrf: bool,
    pub cookie_names: Vec<String>,
}

/// Serializable representation of a single HTTP cookie.
#[derive(Clone, Serialize, Deserialize)]
pub struct PersistedCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub expires_epoch_secs: Option<u64>,
}

impl fmt::Debug for PersistedCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedCookie")
            .field("name", &self.name)
            .field("value", &"[REDACTED]")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("expires_epoch_secs", &self.expires_epoch_secs)
            .finish()
    }
}

/// On-disk session cookie jar payload.
#[derive(Clone, Serialize, Deserialize)]
pub struct PersistedCookieFile {
    pub version: u32,
    pub saved_at_epoch_secs: u64,
    pub login_created_at_epoch_secs: Option<u64>,
    pub last_verified_at_epoch_secs: Option<u64>,
    pub last_uid: Option<u64>,
    pub cookies: Vec<PersistedCookie>,
}

impl fmt::Debug for PersistedCookieFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedCookieFile")
            .field("version", &self.version)
            .field("saved_at_epoch_secs", &self.saved_at_epoch_secs)
            .field("login_created_at_epoch_secs", &self.login_created_at_epoch_secs)
            .field("last_verified_at_epoch_secs", &self.last_verified_at_epoch_secs)
            .field("last_uid", &self.last_uid)
            .field("cookie_count", &self.cookies.len())
            .finish()
    }
}

/// Manages private, atomic persistence of the NetEase cookie jar.
#[derive(Debug, Clone)]
pub struct CookieStorage {
    path: PathBuf,
}

impl CookieStorage {
    /// Create a storage manager targeting `<state_dir>/netease_cookies.json`.
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(COOKIE_FILE_NAME),
        }
    }

    /// Return the path to the cookie file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Ensure the parent state directory exists with private `0700` permissions on Unix.
    fn ensure_state_dir(&self) -> Result<(), AdapterError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| AdapterError::Storage("cookie path has no parent directory".into()))?;
        if !dir.exists() {
            fs::create_dir_all(dir)
                .map_err(|e| AdapterError::Storage(format!("failed to create state dir: {e}")))?;
        }
        #[cfg(unix)]
        {
            let perms = fs::Permissions::from_mode(0o700);
            fs::set_permissions(dir, perms).map_err(|e| {
                AdapterError::Storage(format!("failed to chmod 0700 state dir: {e}"))
            })?;
        }
        Ok(())
    }

    /// Load existing metadata + cookies from disk if present.
    pub fn load_file(&self) -> Result<Option<PersistedCookieFile>, AdapterError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&self.path)
            .map_err(|e| AdapterError::Storage(format!("failed to read cookie file: {e}")))?;
        let parsed: PersistedCookieFile = serde_json::from_str(&raw)
            .map_err(|e| AdapterError::Storage(format!("corrupt cookie JSON: {e}")))?;
        Ok(Some(parsed))
    }

    /// Reconstruct an `isahc::cookies::CookieJar` from disk, returning `(jar, metadata)` if found.
    pub fn load_cookie_jar(&self) -> Result<Option<(CookieJar, PersistedCookieFile)>, AdapterError> {
        let Some(file) = self.load_file()? else {
            return Ok(None);
        };

        let jar = CookieJar::default();
        let now = SystemTime::now();

        for item in &file.cookies {
            let domain = item.domain.trim_start_matches('.');
            let domain = if domain.is_empty() {
                "music.163.com"
            } else {
                domain
            };
            let path = if item.path.is_empty() {
                "/"
            } else {
                &item.path
            };
            let uri_str = format!("https://{domain}{path}");
            let Ok(uri) = uri_str.parse::<Uri>() else {
                continue;
            };

            let mut builder = CookieBuilder::new(&item.name, &item.value)
                .domain(domain)
                .path(path)
                .secure(item.secure);

            if let Some(exp_secs) = item.expires_epoch_secs {
                let exp_time = UNIX_EPOCH + Duration::from_secs(exp_secs);
                if exp_time > now {
                    builder = builder.expiration(exp_time);
                } else {
                    // Expired cookie in file; skip inserting into active jar.
                    continue;
                }
            }

            if let Ok(cookie) = builder.build() {
                let _ = jar.set(cookie, &uri);
            }
        }

        Ok(Some((jar, file)))
    }

    /// Harvest cookies from `jar` and save them atomically with `0600` permissions.
    pub fn save_cookie_jar(
        &self,
        jar: &CookieJar,
        login_created_at_epoch_secs: Option<u64>,
        last_verified_at_epoch_secs: Option<u64>,
        last_uid: Option<u64>,
    ) -> Result<CookieSummary, AdapterError> {
        self.ensure_state_dir()?;

        // Preserve existing login_created_at / last_uid if caller did not supply overrides.
        let existing = self.load_file().ok().flatten();
        let created = login_created_at_epoch_secs
            .or_else(|| existing.as_ref().and_then(|e| e.login_created_at_epoch_secs));
        let verified = last_verified_at_epoch_secs
            .or_else(|| existing.as_ref().and_then(|e| e.last_verified_at_epoch_secs));
        let uid = last_uid.or_else(|| existing.as_ref().and_then(|e| e.last_uid));

        let cookies = extract_cookies_from_jar(jar);
        let summary = summarize_persisted_cookies(&cookies);
        let payload = PersistedCookieFile {
            version: COOKIE_SCHEMA_VERSION,
            saved_at_epoch_secs: now_epoch_secs(),
            login_created_at_epoch_secs: created,
            last_verified_at_epoch_secs: verified,
            last_uid: uid,
            cookies,
        };

        let encoded = serde_json::to_string_pretty(&payload)
            .map_err(|e| AdapterError::Storage(format!("failed to serialize cookies: {e}")))?;

        self.write_atomic_private(encoded.as_bytes())?;
        Ok(summary)
    }

    /// Atomically write bytes to the target file with `0600` permissions.
    fn write_atomic_private(&self, bytes: &[u8]) -> Result<(), AdapterError> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| AdapterError::Storage("cookie path has no parent directory".into()))?;
        let seq = TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_name = format!(".{COOKIE_FILE_NAME}.tmp.{}.{seq}", std::process::id());
        let tmp_path = dir.join(tmp_name);

        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        opts.mode(0o600);

        let write_result = (|| -> std::io::Result<()> {
            let mut file = opts.open(&tmp_path)?;
            #[cfg(unix)]
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(())
        })();

        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(AdapterError::Storage(format!(
                "failed writing temporary cookie file: {err}"
            )));
        }

        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(AdapterError::Storage(format!(
                "failed atomically renaming cookie file: {err}"
            )));
        }

        Ok(())
    }
}

/// Extract deduplicated cookies from an `isahc::cookies::CookieJar`.
pub fn extract_cookies_from_jar(jar: &CookieJar) -> Vec<PersistedCookie> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out = Vec::new();

    for raw_uri in HARVEST_URIS {
        let Ok(uri) = raw_uri.parse::<Uri>() else {
            continue;
        };
        let default_domain = uri.host().unwrap_or("music.163.com");
        let default_path = uri.path();

        for cookie in jar.get_for_uri(&uri) {
            let name = cookie.name().to_string();
            let value = cookie.value().to_string();
            let key = (name.clone(), value.clone());

            if seen.insert(key) {
                out.push(PersistedCookie {
                    name,
                    value,
                    domain: default_domain.to_string(),
                    path: default_path.to_string(),
                    secure: uri.scheme_str() == Some("https"),
                    expires_epoch_secs: None,
                });
            }
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
    out
}

/// Build a secret-free summary from a `CookieJar`.
pub fn summarize_jar(jar: &CookieJar) -> CookieSummary {
    let cookies = extract_cookies_from_jar(jar);
    summarize_persisted_cookies(&cookies)
}

fn summarize_persisted_cookies(cookies: &[PersistedCookie]) -> CookieSummary {
    let mut names: Vec<String> = cookies.iter().map(|c| c.name.clone()).collect();
    names.sort();
    names.dedup();
    let has_music_u = names.iter().any(|n| n == "MUSIC_U");
    let has_csrf = names.iter().any(|n| n == "__csrf");
    CookieSummary {
        total_cookies: cookies.len(),
        has_music_u,
        has_csrf,
        cookie_names: names,
    }
}

pub fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn atomic_save_and_reload_preserves_cookies_and_0600_mode() {
        let dir = TempDir::new().unwrap();
        let storage = CookieStorage::new(dir.path());

        let jar = CookieJar::default();
        let uri: Uri = "https://music.163.com/".parse().unwrap();
        let c1 = CookieBuilder::new("MUSIC_U", "super-secret-session-value")
            .domain("music.163.com")
            .path("/")
            .secure(true)
            .expiration(SystemTime::now() + Duration::from_secs(3600))
            .build()
            .unwrap();
        let c2 = CookieBuilder::new("__csrf", "csrf-secret-token")
            .domain("music.163.com")
            .path("/")
            .build()
            .unwrap();
        jar.set(c1, &uri).unwrap();
        jar.set(c2, &uri).unwrap();

        let summary = storage
            .save_cookie_jar(&jar, Some(1_700_000_000), Some(1_700_000_010), Some(42))
            .unwrap();
        assert_eq!(summary.total_cookies, 2);
        assert!(summary.has_music_u);
        assert!(summary.has_csrf);

        // Verify no leftover temp files exist
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec![COOKIE_FILE_NAME.to_string()]);

        // Verify strict 0600 file mode and 0700 directory mode on Unix
        #[cfg(unix)]
        {
            let file_mode = fs::metadata(storage.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
        }

        // Reload and verify cookies match
        let (reloaded_jar, meta) = storage.load_cookie_jar().unwrap().unwrap();
        assert_eq!(meta.login_created_at_epoch_secs, Some(1_700_000_000));
        assert_eq!(meta.last_uid, Some(42));
        assert_eq!(
            reloaded_jar.get_by_name(&uri, "MUSIC_U").unwrap().value(),
            "super-secret-session-value"
        );
        assert_eq!(
            reloaded_jar.get_by_name(&uri, "__csrf").unwrap().value(),
            "csrf-secret-token"
        );

        // Verify Debug redacts secret cookie values
        let debug_str = format!("{meta:?}");
        assert!(!debug_str.contains("super-secret-session-value"));
        assert!(!debug_str.contains("csrf-secret-token"));
    }

    #[test]
    fn expired_cookies_are_skipped_on_reload() {
        let dir = TempDir::new().unwrap();
        let storage = CookieStorage::new(dir.path());
        let file = PersistedCookieFile {
            version: COOKIE_SCHEMA_VERSION,
            saved_at_epoch_secs: 100,
            login_created_at_epoch_secs: Some(100),
            last_verified_at_epoch_secs: None,
            last_uid: None,
            cookies: vec![PersistedCookie {
                name: "MUSIC_U".into(),
                value: "expired-token".into(),
                domain: "music.163.com".into(),
                path: "/".into(),
                secure: true,
                expires_epoch_secs: Some(1), // year 1970
            }],
        };
        storage
            .write_atomic_private(serde_json::to_string(&file).unwrap().as_bytes())
            .unwrap();
        let (jar, _) = storage.load_cookie_jar().unwrap().unwrap();
        let uri: Uri = "https://music.163.com/".parse().unwrap();
        assert!(jar.get_by_name(&uri, "MUSIC_U").is_none());
    }
}
