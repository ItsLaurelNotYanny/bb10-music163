//! NetEase Cloud Music adapter wrapping `ncm-api` 2.1.0.

use crate::catalogue::{PlaylistDetailSummary, PlaylistSummary, TrackSummary};
use crate::config::NeteaseConfig;
use crate::cookie_store::{now_epoch_secs, summarize_jar, CookieStorage, CookieSummary};
use crate::error::{
    classify_isahc_error, classify_login_status_error, AdapterError, StatusErrorClass,
};
use crate::session_log::{SessionEvent, SessionLogger};
use futures_lite::io::AsyncReadExt;
use isahc::config::Configurable;
use isahc::cookies::CookieJar;
use isahc::{HttpClient, Request, ResponseExt};
use ncm_api::{MusicApi, SongQuality};
use provider_model::{
    AccountCapabilities, AccountProfile, AudioQuality, CapabilityError, CatalogueCapabilities,
    PlaybackCapabilities, PlaybackMode, PlaybackResolution, ProviderCapabilities, ProviderId,
    RelayableMediaSource, SessionCheckOutcome, TrackRef, TransientErrorKind,
};
use qrcode::render::{svg, unicode};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const PROBE_TIMEOUT_SECS: u64 = 15;
const PROBE_MAX_BODY_BYTES: usize = 4096;

/// Active QR login session bounded by wall-clock expiry and maximum poll count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrSession {
    pub unikey: String,
    pub qr_url: String,
    pub qr_ascii: String,
    pub qr_svg: String,
    pub created_at_epoch_secs: u64,
    pub expires_at_epoch_secs: u64,
    pub max_polls: u32,
    pub polls_used: u32,
}

/// Outcome of a single bounded QR poll step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QrPollOutcome {
    WaitingForScan {
        code: i32,
        message: String,
        polls_remaining: u32,
    },
    WaitingForConfirm {
        code: i32,
        message: String,
        polls_remaining: u32,
    },
    Expired {
        code: i32,
        message: String,
    },
    Confirmed {
        profile: Option<AccountProfile>,
        cookies: CookieSummary,
    },
    PollLimitReached {
        reason: String,
    },
    TransientPollError {
        message: String,
        polls_remaining: u32,
    },
}

/// Measured proxy and TLS egress probe report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyProbeReport {
    pub target_url: String,
    pub proxy_configured: Option<String>,
    pub tls_verify_enabled: bool,
    pub status_code: u16,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    pub total_time_ms: u64,
}

/// Atomic traffic and request counters for upstream NetEase API operations.
#[derive(Debug, Default)]
pub struct ApiTrafficCounters {
    pub api_requests_total: AtomicU64,
    pub api_requests_failed: AtomicU64,
    pub probe_bytes_uploaded: AtomicU64,
    pub probe_bytes_downloaded: AtomicU64,
}

impl ApiTrafficCounters {
    pub fn snapshot(&self) -> ApiTrafficSnapshot {
        ApiTrafficSnapshot {
            api_requests_total: self.api_requests_total.load(Ordering::Relaxed),
            api_requests_failed: self.api_requests_failed.load(Ordering::Relaxed),
            probe_bytes_uploaded: self.probe_bytes_uploaded.load(Ordering::Relaxed),
            probe_bytes_downloaded: self.probe_bytes_downloaded.load(Ordering::Relaxed),
        }
    }
}

/// Serializable point-in-time snapshot of API request and probe byte counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiTrafficSnapshot {
    pub api_requests_total: u64,
    pub api_requests_failed: u64,
    pub probe_bytes_uploaded: u64,
    pub probe_bytes_downloaded: u64,
}

/// NetEase provider adapter managing upstream API calls, proxy routing, and durable cookies.
pub struct NeteaseAdapter {
    config: NeteaseConfig,
    api: MusicApi,
    probe_client: HttpClient,
    storage: CookieStorage,
    logger: SessionLogger,
    restored_from_disk: bool,
    counters: Arc<ApiTrafficCounters>,
}

impl NeteaseAdapter {
    /// Construct the adapter, restoring persisted cookies from `config.state_dir` if present.
    pub fn new(config: NeteaseConfig) -> Result<Self, AdapterError> {
        config.validate()?;

        let storage = CookieStorage::new(&config.state_dir);
        let logger = SessionLogger::new(&config.state_dir);

        let (mut api, restored_from_disk) = match storage.load_cookie_jar()? {
            Some((jar, _meta)) => (MusicApi::from_cookie_jar(jar, config.max_connections), true),
            None => (MusicApi::new(config.max_connections), false),
        };

        if let Some(ref proxy) = config.api_proxy {
            api.set_proxy(proxy)
                .map_err(|e| AdapterError::Config(format!("failed to apply api_proxy: {e}")))?;
        }

        let mut probe_builder = HttpClient::builder()
            .timeout(Duration::from_secs(PROBE_TIMEOUT_SECS))
            .metrics(true);
        if let Some(ref proxy) = config.api_proxy {
            let uri: isahc::http::Uri = proxy
                .parse()
                .map_err(|e| AdapterError::Config(format!("invalid probe proxy URI: {e}")))?;
            probe_builder = probe_builder.proxy(Some(uri));
        }
        let probe_client = probe_builder
            .build()
            .map_err(|e| AdapterError::Config(format!("failed building probe HTTP client: {e}")))?;

        Ok(Self {
            config,
            api,
            probe_client,
            storage,
            logger,
            restored_from_disk,
            counters: Arc::new(ApiTrafficCounters::default()),
        })
    }

    /// Return a point-in-time snapshot of upstream API request and probe counters.
    pub fn api_counters(&self) -> ApiTrafficSnapshot {
        self.counters.snapshot()
    }

    /// Return the last authenticated user ID persisted on disk, if known.
    pub fn last_known_uid(&self) -> Option<u64> {
        self.storage
            .load_file()
            .ok()
            .flatten()
            .and_then(|f| f.last_uid)
    }

    /// Return whether a persisted cookie file was loaded during startup.
    pub fn restored_from_disk(&self) -> bool {
        self.restored_from_disk
    }

    /// Return secret-free summary of cookies currently held in memory.
    pub fn cookie_summary(&self) -> CookieSummary {
        match self.api.cookie_jar() {
            Some(jar) => summarize_jar(jar),
            None => CookieSummary {
                total_cookies: 0,
                has_music_u: false,
                has_csrf: false,
                cookie_names: Vec::new(),
            },
        }
    }

    /// Return the path to the private cookie storage file.
    pub fn cookie_file_path(&self) -> &std::path::Path {
        self.storage.path()
    }

    /// Return the path to the secret-free session event log.
    pub fn session_log_path(&self) -> &std::path::Path {
        self.logger.path()
    }

    /// Return a reference to the adapter configuration.
    pub fn config(&self) -> &NeteaseConfig {
        &self.config
    }

    /// Describe NetEase's catalogue, account, and playback capabilities.
    pub fn capabilities() -> ProviderCapabilities {
        ProviderCapabilities {
            provider: ProviderId::Netease,
            catalogue: CatalogueCapabilities {
                search: true,
                playlists: true,
                track_details: true,
            },
            account: AccountCapabilities {
                qr_login: true,
                sms_login: true,
                unattended_renewal: false,
                renewal_note: "Pinned netease-cloud-music-api 2.1.0 has no token refresh endpoint; QR session duration depends on server cookie lifetime and valid-response Set-Cookie updates.".to_string(),
            },
            playback: PlaybackCapabilities {
                supported_modes: vec![PlaybackMode::RelayableMedia],
                supports_range_seek: true,
            },
        }
    }

    /// Check renewal support explicitly and record the capability observation.
    pub fn check_renewal_support(&self) -> (bool, String) {
        let caps = Self::capabilities();
        self.logger.record(SessionEvent::RenewalCapabilityChecked {
            timestamp_epoch_secs: SessionLogger::now(),
            supported: caps.account.unattended_renewal,
            note: caps.account.renewal_note.clone(),
        });
        (caps.account.unattended_renewal, caps.account.renewal_note)
    }

    /// Start a bounded QR login session.
    pub async fn start_qr_login(
        &self,
        ttl_secs: u64,
        max_polls: u32,
    ) -> Result<QrSession, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let (qr_url, unikey) = match self.api.login_qr_create().await {
            Ok(pair) => pair,
            Err(e) => {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                return Err(AdapterError::Transient {
                    kind: TransientErrorKind::NetworkOrProxy,
                    message: format!("failed to create QR login key: {e}"),
                });
            }
        };

        let code = QrCode::new(qr_url.as_bytes())
            .map_err(|e| AdapterError::Upstream(format!("failed to encode QR code: {e}")))?;
        let qr_ascii = code
            .render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .build();
        let qr_svg = code
            .render::<svg::Color>()
            .min_dimensions(220, 220)
            .build();

        let now = now_epoch_secs();
        Ok(QrSession {
            unikey,
            qr_url,
            qr_ascii,
            qr_svg,
            created_at_epoch_secs: now,
            expires_at_epoch_secs: now.saturating_add(ttl_secs),
            max_polls,
            polls_used: 0,
        })
    }

    /// Perform one bounded QR poll attempt.
    pub async fn poll_qr_once(&self, session: &mut QrSession) -> QrPollOutcome {
        if session.polls_used >= session.max_polls {
            return QrPollOutcome::PollLimitReached {
                reason: format!("reached maximum poll count ({})", session.max_polls),
            };
        }

        let now = now_epoch_secs();
        if now >= session.expires_at_epoch_secs {
            return QrPollOutcome::Expired {
                code: 800,
                message: "QR session reached local TTL deadline".to_string(),
            };
        }

        session.polls_used += 1;
        let polls_remaining = session.max_polls.saturating_sub(session.polls_used);

        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        match self.api.login_qr_check(session.unikey.clone()).await {
            Ok(msg) => match msg.code {
                801 => QrPollOutcome::WaitingForScan {
                    code: msg.code,
                    message: msg.msg,
                    polls_remaining,
                },
                802 => QrPollOutcome::WaitingForConfirm {
                    code: msg.code,
                    message: msg.msg,
                    polls_remaining,
                },
                800 => QrPollOutcome::Expired {
                    code: msg.code,
                    message: msg.msg,
                },
                803 => {
                    // Persist cookies immediately upon 803 authorization.
                    let mut summary = self
                        .persist_current_cookies(Some(now), Some(now), None)
                        .unwrap_or_else(|_| self.cookie_summary());

                    // Fetch profile via login_status to populate UID and verify session.
                    let profile = match self.check_status().await {
                        SessionCheckOutcome::Authenticated {
                            profile,
                            cookie_count,
                            ..
                        } => {
                            summary.total_cookies = cookie_count;
                            Some(profile)
                        }
                        _ => None,
                    };

                    self.logger.record(SessionEvent::LoginConfirmed {
                        timestamp_epoch_secs: now,
                        method: "qr".to_string(),
                        uid: profile.as_ref().map(|p| p.uid),
                        cookie_count: summary.total_cookies,
                        has_music_u: summary.has_music_u,
                    });

                    QrPollOutcome::Confirmed {
                        profile,
                        cookies: summary,
                    }
                }
                other => QrPollOutcome::TransientPollError {
                    message: format!("unexpected QR poll code {other}: {}", msg.msg),
                    polls_remaining,
                },
            },
            Err(err) => {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                QrPollOutcome::TransientPollError {
                    message: format!("QR poll transport/parse error: {err}"),
                    polls_remaining,
                }
            }
        }
    }

    /// Optional SMS verification code request.
    pub async fn send_sms_captcha(&self, ctcode: &str, phone: &str) -> Result<(), AdapterError> {
        self.api
            .captcha(ctcode.to_string(), phone.to_string())
            .await
            .map_err(|e| AdapterError::Upstream(format!("SMS captcha send failed: {e}")))
    }

    /// Optional SMS login. Atomically saves cookies if login succeeds.
    pub async fn login_cellphone(
        &self,
        ctcode: &str,
        phone: &str,
        captcha: &str,
    ) -> Result<AccountProfile, AdapterError> {
        let info = self
            .api
            .login_cellphone(ctcode.to_string(), phone.to_string(), captcha.to_string())
            .await
            .map_err(|e| AdapterError::Upstream(format!("SMS login failed: {e}")))?;

        if info.code == 200 && info.uid > 0 {
            let now = now_epoch_secs();
            let summary = self.persist_current_cookies(Some(now), Some(now), Some(info.uid))?;
            self.logger.record(SessionEvent::LoginConfirmed {
                timestamp_epoch_secs: now,
                method: "sms".to_string(),
                uid: Some(info.uid),
                cookie_count: summary.total_cookies,
                has_music_u: summary.has_music_u,
            });
            Ok(AccountProfile {
                uid: info.uid,
                nickname: info.nickname,
                vip_type: info.vip_type,
            })
        } else {
            Err(AdapterError::ConfirmedLoggedOut {
                code: info.code,
                message: info.msg,
            })
        }
    }

    /// Check account login status, classifying confirmed logout vs transient errors.
    ///
    /// On valid authenticated responses, updates persisted cookies atomically.
    /// On transient network/proxy/TLS errors, preserves stored credentials untouched.
    pub async fn check_status(&self) -> SessionCheckOutcome {
        let now = now_epoch_secs();
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        match self.api.login_status().await {
            Ok(info) if info.code == 200 && info.uid > 0 => {
                let profile = AccountProfile {
                    uid: info.uid,
                    nickname: info.nickname,
                    vip_type: info.vip_type,
                };
                let cookie_count = self
                    .persist_current_cookies(None, Some(now), Some(profile.uid))
                    .map(|s| s.total_cookies)
                    .unwrap_or_else(|_| self.cookie_summary().total_cookies);

                self.logger.record(SessionEvent::StatusChecked {
                    timestamp_epoch_secs: now,
                    outcome: "authenticated".to_string(),
                    uid: Some(profile.uid),
                    detail: format!("cookies={cookie_count}"),
                });

                SessionCheckOutcome::Authenticated {
                    profile,
                    cookie_count,
                    checked_at_epoch_secs: now,
                }
            }
            Ok(info) => {
                self.logger.record(SessionEvent::StatusChecked {
                    timestamp_epoch_secs: now,
                    outcome: "confirmed_logged_out".to_string(),
                    uid: None,
                    detail: format!("upstream_code={}", info.code),
                });
                SessionCheckOutcome::ConfirmedLoggedOut {
                    upstream_code: info.code,
                    reason: if info.msg.is_empty() {
                        "upstream indicated unauthenticated status".to_string()
                    } else {
                        info.msg
                    },
                    checked_at_epoch_secs: now,
                }
            }
            Err(err) => match classify_login_status_error(&err.to_string()) {
                StatusErrorClass::ConfirmedLoggedOut { code, reason } => {
                    self.logger.record(SessionEvent::StatusChecked {
                        timestamp_epoch_secs: now,
                        outcome: "confirmed_logged_out".to_string(),
                        uid: None,
                        detail: reason.clone(),
                    });
                    SessionCheckOutcome::ConfirmedLoggedOut {
                        upstream_code: code,
                        reason,
                        checked_at_epoch_secs: now,
                    }
                }
                StatusErrorClass::Transient { kind, reason } => {
                    self.counters
                        .api_requests_failed
                        .fetch_add(1, Ordering::Relaxed);
                    // Credentials on disk MUST NOT be modified or cleared on transient failure.
                    self.logger.record(SessionEvent::StatusChecked {
                        timestamp_epoch_secs: now,
                        outcome: "transient_error".to_string(),
                        uid: None,
                        detail: format!("{kind:?}: {reason}"),
                    });
                    SessionCheckOutcome::TransientError {
                        kind,
                        message: reason,
                        credentials_preserved: true,
                        checked_at_epoch_secs: now,
                    }
                }
            },
        }
    }

    /// Resolve an authorized track URL for relayable playback.
    pub async fn resolve_track(
        &self,
        song_id: u64,
        quality: AudioQuality,
    ) -> Result<PlaybackResolution, AdapterError> {
        let ncm_quality = map_audio_quality(quality);
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let urls = self
            .api
            .songs_url_v1(&[song_id], ncm_quality)
            .await
            .map_err(|e| {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let msg = e.to_string();
                if msg.trim() == "none" {
                    AdapterError::Transient {
                        kind: TransientErrorKind::NetworkOrProxy,
                        message: "track resolution request failed (transport, proxy, or non-200 code)".to_string(),
                    }
                } else {
                    AdapterError::Upstream(format!("failed to resolve track {song_id}: {msg}"))
                }
            })?;

        // Update cookies from valid upstream response.
        let _ = self.persist_current_cookies(None, None, None);

        let now = SessionLogger::now();
        let Some(song_url) = urls.into_iter().find(|u| !u.url.is_empty()) else {
            self.logger.record(SessionEvent::TrackResolved {
                timestamp_epoch_secs: now,
                song_id,
                bitrate: 0,
                ok: false,
                detail: "no playable URL returned (region/VIP/copyright restriction)".to_string(),
            });
            return Err(AdapterError::Capability(
                CapabilityError::RegionOrCopyrightRestricted {
                    provider: ProviderId::Netease,
                    track_id: song_id.to_string(),
                    reason: "upstream returned no playable URL for current account/region"
                        .to_string(),
                },
            ));
        };

        let format_hint = infer_audio_format(&song_url.url);
        let actual_quality = map_song_quality_back(song_url.quality);

        self.logger.record(SessionEvent::TrackResolved {
            timestamp_epoch_secs: now,
            song_id,
            bitrate: song_url.rate,
            ok: true,
            detail: format!("format={}", format_hint.as_deref().unwrap_or("unknown")),
        });

        Ok(PlaybackResolution::RelayableMedia(RelayableMediaSource {
            track: TrackRef::new(ProviderId::Netease, song_id.to_string()),
            url: song_url.url,
            bitrate: song_url.rate,
            format_hint,
            quality: actual_quality,
            // NetEase CDN signed URLs typically expire within ~20 minutes; M2 handles re-resolution.
            expires_hint_secs: Some(1200),
        }))
    }

    /// Search tracks by keyword on NetEase Cloud Music.
    pub async fn search_songs(
        &self,
        keywords: &str,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<TrackSummary>, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let songs = self
            .api
            .search_song(keywords.to_string(), offset, limit)
            .await
            .map_err(|e| {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let msg = e.to_string();
                if msg.trim() == "none" {
                    AdapterError::Transient {
                        kind: TransientErrorKind::NetworkOrProxy,
                        message: "song search request failed (transport or proxy)".to_string(),
                    }
                } else {
                    AdapterError::Upstream(format!("song search failed: {msg}"))
                }
            })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(songs.into_iter().map(TrackSummary::from_ncm).collect())
    }

    /// List playlists for a given NetEase user ID.
    pub async fn user_playlists(
        &self,
        uid: u64,
        offset: u16,
        limit: u16,
    ) -> Result<Vec<PlaylistSummary>, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let lists = self
            .api
            .user_song_list(uid, offset, limit)
            .await
            .map_err(|e| {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let msg = e.to_string();
                if msg.trim() == "none" {
                    AdapterError::Transient {
                        kind: TransientErrorKind::NetworkOrProxy,
                        message: "playlist list request failed (transport or proxy)".to_string(),
                    }
                } else {
                    AdapterError::Upstream(format!("user playlists fetch failed: {msg}"))
                }
            })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(lists.into_iter().map(PlaylistSummary::from_ncm).collect())
    }

    /// Fetch playlist tracks and metadata by playlist ID.
    pub async fn playlist_detail(
        &self,
        playlist_id: u64,
    ) -> Result<PlaylistDetailSummary, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let detail = self
            .api
            .song_list_detail(playlist_id)
            .await
            .map_err(|e| {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let msg = e.to_string();
                if msg.trim() == "none" {
                    AdapterError::Transient {
                        kind: TransientErrorKind::NetworkOrProxy,
                        message: "playlist detail request failed (transport or proxy)".to_string(),
                    }
                } else {
                    AdapterError::Upstream(format!("playlist detail fetch failed: {msg}"))
                }
            })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(PlaylistDetailSummary::from_ncm(detail))
    }

    /// Fetch daily recommended tracks for the authenticated account.
    pub async fn recommend_songs(&self) -> Result<Vec<TrackSummary>, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let songs = self.api.recommend_songs().await.map_err(|e| {
            self.counters
                .api_requests_failed
                .fetch_add(1, Ordering::Relaxed);
            let msg = e.to_string();
            if msg.trim() == "none" {
                AdapterError::Transient {
                    kind: TransientErrorKind::NetworkOrProxy,
                    message: "daily recommend songs request failed (transport or proxy)".to_string(),
                }
            } else {
                AdapterError::Upstream(format!("recommend songs failed: {msg}"))
            }
        })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(songs.into_iter().map(TrackSummary::from_ncm).collect())
    }

    /// Fetch Personal FM tracks for the authenticated account.
    pub async fn personal_fm(&self) -> Result<Vec<TrackSummary>, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let songs = self.api.personal_fm().await.map_err(|e| {
            self.counters
                .api_requests_failed
                .fetch_add(1, Ordering::Relaxed);
            let msg = e.to_string();
            if msg.trim() == "none" {
                AdapterError::Transient {
                    kind: TransientErrorKind::NetworkOrProxy,
                    message: "personal FM request failed (transport or proxy)".to_string(),
                }
            } else {
                AdapterError::Upstream(format!("personal FM failed: {msg}"))
            }
        })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(songs.into_iter().map(TrackSummary::from_ncm).collect())
    }

    /// Fetch Heart / Intelligence mode tracks seeded by `song_id` and `playlist_id`.
    pub async fn intelligence_list(
        &self,
        song_id: u64,
        playlist_id: u64,
    ) -> Result<Vec<TrackSummary>, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let songs = self
            .api
            .playmode_intelligence_list(song_id, playlist_id)
            .await
            .map_err(|e| {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let msg = e.to_string();
                if msg.trim() == "none" {
                    AdapterError::Transient {
                        kind: TransientErrorKind::NetworkOrProxy,
                        message: "intelligence mode request failed (transport or proxy)".to_string(),
                    }
                } else {
                    AdapterError::Upstream(format!("intelligence mode failed: {msg}"))
                }
            })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(songs.into_iter().map(TrackSummary::from_ncm).collect())
    }

    /// Fetch all liked song IDs for the given user ID.
    pub async fn user_liked_song_ids(&self, uid: u64) -> Result<Vec<u64>, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let ids = self.api.user_song_id_list(uid).await.map_err(|e| {
            self.counters
                .api_requests_failed
                .fetch_add(1, Ordering::Relaxed);
            let msg = e.to_string();
            if msg.trim() == "none" {
                AdapterError::Transient {
                    kind: TransientErrorKind::NetworkOrProxy,
                    message: "liked song IDs request failed (transport or proxy)".to_string(),
                }
            } else {
                AdapterError::Upstream(format!("liked song IDs failed: {msg}"))
            }
        })?;
        let _ = self.persist_current_cookies(None, None, None);
        Ok(ids)
    }

    /// Fetch full track metadata in batches for the specified numeric song IDs.
    ///
    /// Queries NetEase's `/api/v3/song/detail` directly rather than `ncm_api::MusicApi::songs_detail`
    /// because pinned `ncm-api` 2.1.0 builds malformed double-escaped JSON in its `c` parameter.
    pub async fn songs_detail(&self, ids: &[u64]) -> Result<Vec<TrackSummary>, AdapterError> {
        const BATCH_SIZE: usize = 400;
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        #[derive(Serialize)]
        struct SongIdEntry {
            id: u64,
        }

        #[derive(Deserialize)]
        struct V3SongDetailResponse {
            code: i64,
            #[serde(default)]
            songs: Vec<V3SongItem>,
        }

        #[derive(Deserialize)]
        struct V3SongItem {
            id: u64,
            #[serde(default)]
            name: String,
            #[serde(default)]
            ar: Vec<V3ArtistItem>,
            #[serde(default)]
            al: Option<V3AlbumItem>,
            #[serde(default)]
            dt: u64,
        }

        #[derive(Deserialize)]
        struct V3ArtistItem {
            #[serde(default)]
            name: String,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct V3AlbumItem {
            #[serde(default)]
            name: String,
            #[serde(default)]
            pic_url: String,
        }

        let mut tracks = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(BATCH_SIZE) {
            self.counters
                .api_requests_total
                .fetch_add(1, Ordering::Relaxed);

            let entries: Vec<SongIdEntry> = chunk.iter().map(|&id| SongIdEntry { id }).collect();
            let c_json = serde_json::to_string(&entries)
                .map_err(|e| AdapterError::Upstream(format!("failed serializing song ids: {e}")))?;
            let form_body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("c", &c_json)
                .finish();

            let req = Request::post("https://music.163.com/api/v3/song/detail")
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("Referer", "https://music.163.com/")
                .body(form_body)
                .map_err(|e| {
                    AdapterError::Config(format!("failed building songs detail request: {e}"))
                })?;

            let mut resp = self.probe_client.send_async(req).await.map_err(|err| {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let kind = classify_isahc_error(&err);
                AdapterError::Transient {
                    kind,
                    message: format!("songs detail request failed: {err}"),
                }
            })?;

            let status = resp.status();
            let mut body_text = String::new();
            resp.body_mut()
                .read_to_string(&mut body_text)
                .await
                .map_err(|e| {
                    self.counters
                        .api_requests_failed
                        .fetch_add(1, Ordering::Relaxed);
                    AdapterError::Upstream(format!("failed reading songs detail response: {e}"))
                })?;

            if !status.is_success() {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                return Err(AdapterError::Upstream(format!(
                    "songs detail HTTP error {status}"
                )));
            }

            let parsed: V3SongDetailResponse =
                serde_json::from_str(&body_text).map_err(|e| {
                    self.counters
                        .api_requests_failed
                        .fetch_add(1, Ordering::Relaxed);
                    AdapterError::Upstream(format!("invalid songs detail JSON: {e}"))
                })?;

            if parsed.code != 200 {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                return Err(AdapterError::Upstream(format!(
                    "songs detail API code {}",
                    parsed.code
                )));
            }

            for item in parsed.songs {
                let singer = {
                    let joined: Vec<&str> = item
                        .ar
                        .iter()
                        .map(|a| a.name.trim())
                        .filter(|s| !s.is_empty())
                        .collect();
                    if joined.is_empty() {
                        "unknown".to_string()
                    } else {
                        joined.join(" / ")
                    }
                };
                let album = item
                    .al
                    .as_ref()
                    .map(|a| a.name.trim())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("unknown")
                    .to_string();
                let pic_url = item.al.map(|a| a.pic_url).unwrap_or_default();

                tracks.push(TrackSummary {
                    id: item.id,
                    track_ref: TrackRef::new(ProviderId::Netease, item.id.to_string()),
                    name: item.name,
                    singer,
                    album,
                    duration_ms: item.dt,
                    pic_url,
                });
            }
        }
        Ok(tracks)
    }

    /// Like or unlike a track (`like = true` adds to favorites; `false` removes).
    pub async fn like_track(&self, song_id: u64, like: bool) -> Result<bool, AdapterError> {
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        let ok = self.api.like(like, song_id).await;
        if !ok {
            self.counters
                .api_requests_failed
                .fetch_add(1, Ordering::Relaxed);
        } else {
            let _ = self.persist_current_cookies(None, None, None);
        }
        Ok(ok)
    }

    /// Measure proxy egress bytes and verify TLS certificate validation using `probe_client`.
    pub async fn probe_proxy_egress(
        &self,
        target_url: &str,
    ) -> Result<ProxyProbeReport, AdapterError> {
        let parsed = Url::parse(target_url)
            .map_err(|e| AdapterError::Config(format!("invalid probe target URL: {e}")))?;
        let req_path = match parsed.query() {
            Some(q) => format!("{}?{q}", parsed.path()),
            None => parsed.path().to_string(),
        };
        let host_hdr = parsed.host_str().unwrap_or_default();
        let est_req_bytes = format!(
            "GET {req_path} HTTP/1.1\r\nHost: {host_hdr}\r\nRange: bytes=0-1023\r\n\r\n"
        )
        .len() as u64;

        let req = Request::get(parsed.as_str())
            .header("Range", "bytes=0-1023")
            .body(())
            .map_err(|e| AdapterError::Config(format!("failed building probe request: {e}")))?;

        let now = SessionLogger::now();
        self.counters
            .api_requests_total
            .fetch_add(1, Ordering::Relaxed);
        match self.probe_client.send_async(req).await {
            Ok(mut resp) => {
                let status_code = resp.status().as_u16();
                let mut est_resp_header_bytes =
                    format!("HTTP/1.1 {status_code}\r\n").len() as u64 + 2;
                for (name, val) in resp.headers() {
                    est_resp_header_bytes +=
                        name.as_str().len() as u64 + 2 + val.as_bytes().len() as u64 + 2;
                }

                // Read bounded body slice so curl transfer metrics reflect downloaded payload.
                let mut body_read: usize = 0;
                let mut buf = [0u8; 1024];
                while body_read < PROBE_MAX_BODY_BYTES {
                    let max_slice = buf.len().min(PROBE_MAX_BODY_BYTES - body_read);
                    match resp.body_mut().read(&mut buf[..max_slice]).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body_read += n,
                    }
                }

                let (bytes_uploaded, bytes_downloaded, total_time_ms) =
                    if let Some(metrics) = resp.metrics() {
                        let (_, upload_total) = metrics.upload_progress();
                        let (_, download_total) = metrics.download_progress();
                        let up = if upload_total > 0 {
                            upload_total
                        } else {
                            est_req_bytes
                        };
                        let down = if download_total > 0 {
                            download_total.max(body_read as u64)
                        } else {
                            (body_read as u64).max(est_resp_header_bytes)
                        };
                        (up, down, metrics.total_time().as_millis() as u64)
                    } else {
                        (
                            est_req_bytes,
                            (body_read as u64).max(est_resp_header_bytes),
                            0,
                        )
                    };

                self.counters
                    .probe_bytes_uploaded
                    .fetch_add(bytes_uploaded, Ordering::Relaxed);
                self.counters
                    .probe_bytes_downloaded
                    .fetch_add(bytes_downloaded, Ordering::Relaxed);

                self.logger.record(SessionEvent::ProxyEgressProbed {
                    timestamp_epoch_secs: now,
                    proxy_used: self.config.api_proxy.clone(),
                    tls_verify: self.config.tls_verify,
                    status_code: Some(status_code),
                    bytes_uploaded,
                    bytes_downloaded,
                    ok: true,
                });

                Ok(ProxyProbeReport {
                    target_url: target_url.to_string(),
                    proxy_configured: self.config.api_proxy.clone(),
                    tls_verify_enabled: self.config.tls_verify,
                    status_code,
                    bytes_uploaded,
                    bytes_downloaded,
                    total_time_ms,
                })
            }
            Err(err) => {
                self.counters
                    .api_requests_failed
                    .fetch_add(1, Ordering::Relaxed);
                let kind = classify_isahc_error(&err);
                self.logger.record(SessionEvent::ProxyEgressProbed {
                    timestamp_epoch_secs: now,
                    proxy_used: self.config.api_proxy.clone(),
                    tls_verify: self.config.tls_verify,
                    status_code: None,
                    bytes_uploaded: 0,
                    bytes_downloaded: 0,
                    ok: false,
                });
                Err(AdapterError::Transient {
                    kind,
                    message: format!("proxy/TLS probe failed: {err}"),
                })
            }
        }
    }

    /// Persist current in-memory cookies atomically to disk.
    fn persist_current_cookies(
        &self,
        login_created_at: Option<u64>,
        last_verified_at: Option<u64>,
        last_uid: Option<u64>,
    ) -> Result<CookieSummary, AdapterError> {
        let jar = self
            .api
            .cookie_jar()
            .cloned()
            .unwrap_or_else(CookieJar::default);
        self.storage
            .save_cookie_jar(&jar, login_created_at, last_verified_at, last_uid)
    }
}

fn map_audio_quality(quality: AudioQuality) -> SongQuality {
    match quality {
        AudioQuality::Standard => SongQuality::Standard,
        AudioQuality::Higher => SongQuality::Higher,
        AudioQuality::Extreme => SongQuality::Extreme,
        AudioQuality::Lossless => SongQuality::Lossless,
        AudioQuality::HiRes => SongQuality::HiRes,
    }
}

fn map_song_quality_back(quality: SongQuality) -> AudioQuality {
    match quality {
        SongQuality::Standard => AudioQuality::Standard,
        SongQuality::Higher => AudioQuality::Higher,
        SongQuality::Extreme => AudioQuality::Extreme,
        SongQuality::Lossless => AudioQuality::Lossless,
        SongQuality::HiRes | SongQuality::Surround | SongQuality::AudioVivid | SongQuality::Master => {
            AudioQuality::HiRes
        }
    }
}

fn infer_audio_format(raw_url: &str) -> Option<String> {
    let parsed = Url::parse(raw_url).ok()?;
    let path = parsed.path().to_lowercase();
    if path.ends_with(".mp3") {
        Some("mp3".to_string())
    } else if path.ends_with(".m4a") || path.ends_with(".aac") {
        Some("aac".to_string())
    } else if path.ends_with(".flac") {
        Some("flac".to_string())
    } else {
        None
    }
}
