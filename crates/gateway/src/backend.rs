//! Gateway backend abstraction and atomic playback generation persistence.

use netease_adapter::{
    AdapterError, ApiTrafficSnapshot, CookieSummary, NeteaseAdapter, PlaylistDetailSummary,
    PlaylistSummary, ProxyProbeReport, QrPollOutcome, QrSession, TrackSummary,
};
use provider_model::{AudioQuality, PlaybackResolution, ProviderCapabilities, SessionCheckOutcome};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const GENERATION_FILE_NAME: &str = "playback_generation.json";
static GEN_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Boxed future helper avoiding extra proc-macro dependencies.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Backend provider abstraction used by HTTP handlers so tests can supply fixture resolvers.
pub trait MusicBackend: Send + Sync {
    fn capabilities(&self) -> ProviderCapabilities;
    fn restored_from_disk(&self) -> bool;
    fn cookie_summary(&self) -> CookieSummary;
    fn last_known_uid(&self) -> Option<u64>;
    fn api_counters(&self) -> ApiTrafficSnapshot;

    fn check_status(&self) -> BoxFuture<'_, SessionCheckOutcome>;
    fn start_qr_login(&self, ttl_secs: u64, max_polls: u32)
        -> BoxFuture<'_, Result<QrSession, AdapterError>>;
    fn poll_qr_once<'a>(&'a self, session: &'a mut QrSession) -> BoxFuture<'a, QrPollOutcome>;
    fn resolve_track(
        &self,
        song_id: u64,
        quality: AudioQuality,
    ) -> BoxFuture<'_, Result<PlaybackResolution, AdapterError>>;
    fn search_songs(
        &self,
        keywords: &str,
        offset: u16,
        limit: u16,
    ) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>>;
    fn user_playlists(
        &self,
        uid: u64,
        offset: u16,
        limit: u16,
    ) -> BoxFuture<'_, Result<Vec<PlaylistSummary>, AdapterError>>;
    fn playlist_detail(&self, playlist_id: u64)
        -> BoxFuture<'_, Result<PlaylistDetailSummary, AdapterError>>;
    fn recommend_songs(&self) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>>;
    fn personal_fm(&self) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>>;
    fn intelligence_list(
        &self,
        song_id: u64,
        playlist_id: u64,
    ) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>>;
    fn user_liked_song_ids(&self, uid: u64) -> BoxFuture<'_, Result<Vec<u64>, AdapterError>>;
    fn songs_detail<'a>(&'a self, ids: &'a [u64]) -> BoxFuture<'a, Result<Vec<TrackSummary>, AdapterError>>;
    fn like_track(&self, song_id: u64, like: bool) -> BoxFuture<'_, Result<bool, AdapterError>>;
    fn probe_proxy_egress(&self, target_url: &str)
        -> BoxFuture<'_, Result<ProxyProbeReport, AdapterError>>;
}

impl MusicBackend for NeteaseAdapter {
    fn capabilities(&self) -> ProviderCapabilities {
        NeteaseAdapter::capabilities()
    }

    fn restored_from_disk(&self) -> bool {
        NeteaseAdapter::restored_from_disk(self)
    }

    fn cookie_summary(&self) -> CookieSummary {
        NeteaseAdapter::cookie_summary(self)
    }

    fn last_known_uid(&self) -> Option<u64> {
        NeteaseAdapter::last_known_uid(self)
    }

    fn api_counters(&self) -> ApiTrafficSnapshot {
        NeteaseAdapter::api_counters(self)
    }

    fn check_status(&self) -> BoxFuture<'_, SessionCheckOutcome> {
        Box::pin(NeteaseAdapter::check_status(self))
    }

    fn start_qr_login(
        &self,
        ttl_secs: u64,
        max_polls: u32,
    ) -> BoxFuture<'_, Result<QrSession, AdapterError>> {
        Box::pin(NeteaseAdapter::start_qr_login(self, ttl_secs, max_polls))
    }

    fn poll_qr_once<'a>(&'a self, session: &'a mut QrSession) -> BoxFuture<'a, QrPollOutcome> {
        Box::pin(NeteaseAdapter::poll_qr_once(self, session))
    }

    fn resolve_track(
        &self,
        song_id: u64,
        quality: AudioQuality,
    ) -> BoxFuture<'_, Result<PlaybackResolution, AdapterError>> {
        Box::pin(NeteaseAdapter::resolve_track(self, song_id, quality))
    }

    fn search_songs(
        &self,
        keywords: &str,
        offset: u16,
        limit: u16,
    ) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
        let kw = keywords.to_string();
        Box::pin(async move { NeteaseAdapter::search_songs(self, &kw, offset, limit).await })
    }

    fn user_playlists(
        &self,
        uid: u64,
        offset: u16,
        limit: u16,
    ) -> BoxFuture<'_, Result<Vec<PlaylistSummary>, AdapterError>> {
        Box::pin(NeteaseAdapter::user_playlists(self, uid, offset, limit))
    }

    fn playlist_detail(
        &self,
        playlist_id: u64,
    ) -> BoxFuture<'_, Result<PlaylistDetailSummary, AdapterError>> {
        Box::pin(NeteaseAdapter::playlist_detail(self, playlist_id))
    }

    fn recommend_songs(&self) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
        Box::pin(NeteaseAdapter::recommend_songs(self))
    }

    fn personal_fm(&self) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
        Box::pin(NeteaseAdapter::personal_fm(self))
    }

    fn intelligence_list(
        &self,
        song_id: u64,
        playlist_id: u64,
    ) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
        Box::pin(NeteaseAdapter::intelligence_list(self, song_id, playlist_id))
    }

    fn user_liked_song_ids(&self, uid: u64) -> BoxFuture<'_, Result<Vec<u64>, AdapterError>> {
        Box::pin(NeteaseAdapter::user_liked_song_ids(self, uid))
    }

    fn songs_detail<'a>(
        &'a self,
        ids: &'a [u64],
    ) -> BoxFuture<'a, Result<Vec<TrackSummary>, AdapterError>> {
        Box::pin(NeteaseAdapter::songs_detail(self, ids))
    }

    fn like_track(&self, song_id: u64, like: bool) -> BoxFuture<'_, Result<bool, AdapterError>> {
        Box::pin(NeteaseAdapter::like_track(self, song_id, like))
    }

    fn probe_proxy_egress(
        &self,
        target_url: &str,
    ) -> BoxFuture<'_, Result<ProxyProbeReport, AdapterError>> {
        let url = target_url.to_string();
        Box::pin(async move { NeteaseAdapter::probe_proxy_egress(self, &url).await })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedGeneration {
    generation: u64,
}

/// Persists the monotonic playback generation floor across gateway restarts.
#[derive(Debug, Clone)]
pub struct GenerationStore {
    path: PathBuf,
}

impl GenerationStore {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(GENERATION_FILE_NAME),
        }
    }

    pub fn load(&self) -> u64 {
        let Ok(raw) = fs::read_to_string(&self.path) else {
            return 0;
        };
        serde_json::from_str::<PersistedGeneration>(&raw)
            .map(|p| p.generation)
            .unwrap_or(0)
    }

    pub fn save(&self, generation: u64) {
        let Some(dir) = self.path.parent() else {
            return;
        };
        if !dir.exists() && fs::create_dir_all(dir).is_err() {
            return;
        }
        #[cfg(unix)]
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));

        let Ok(encoded) = serde_json::to_string_pretty(&PersistedGeneration { generation }) else {
            return;
        };

        let seq = GEN_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(
            ".{GENERATION_FILE_NAME}.tmp.{}.{seq}",
            std::process::id()
        ));
        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        opts.mode(0o600);

        if let Ok(mut f) = opts.open(&tmp) {
            if f.write_all(encoded.as_bytes()).is_ok() && f.sync_all().is_ok() {
                let _ = fs::rename(&tmp, &self.path);
            } else {
                let _ = fs::remove_file(&tmp);
            }
        }
    }
}
