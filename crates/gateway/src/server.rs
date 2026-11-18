//! Gateway HTTP API server for M1–M4:
//! - Account status, bounded QR login, catalogue search/playlists (M1/M3)
//! - Authenticated incremental media relay with Range & pre-header refresh (M2)
//! - Single-owner monotonic generation playback arbitration & stop notifications (M4)
//! - Lightweight ES5 web player UI served at `/` and `/player` (M3/M4)

use crate::backend::{GenerationStore, MusicBackend};
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use media_relay::{
    MediaRelay, MediaSourceRequest, MediaTrafficSnapshot, RelayCancellationToken, RelayError,
    RelayMethod,
};
use netease_adapter::{
    AdapterError, ApiTrafficSnapshot, PlaylistDetailSummary, PlaylistSummary, ProxyProbeReport,
    QrPollOutcome, QrSession, TrackSummary,
};
use playback_session::{
    ClientId, CommandOutcome, PlaybackCommand, PlaybackCommandKind, PlaybackSession,
    PlaybackSnapshot, PlaybackStatus, QueueEntry, SessionError, StopInstruction,
};
use provider_model::{
    AudioQuality, PlaybackResolution, ProviderCapabilities, ProviderId, SessionCheckOutcome,
    TrackRef,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, Notify};

const PLAYER_HTML: &str = include_str!("../../../web/player.html");
const DEFAULT_TICKET_TTL_SECS: u64 = 900;
const PLAYLIST_CACHE_TTL_SECS: u64 = 600;
const MAX_EVENT_WAIT_MS: u64 = 15_000;

static TICKET_SEQ: AtomicU64 = AtomicU64::new(1);

/// Short-lived generation-bound media access ticket.
#[derive(Debug, Clone)]
pub struct MediaTicket {
    pub song_id: u64,
    pub quality: AudioQuality,
    pub generation: u64,
    pub expires_at_epoch_secs: u64,
    pub cached_url: String,
    pub format_hint: Option<String>,
}

/// Shared application state for gateway HTTP endpoints.
#[derive(Clone)]
pub struct AppState {
    pub backend: Arc<dyn MusicBackend>,
    pub media_relay: Arc<MediaRelay>,
    pub gateway_token: Arc<String>,
    pub qr_sessions: Arc<Mutex<HashMap<String, QrSession>>>,
    pub playback: Arc<Mutex<PlaybackSession>>,
    pub tickets: Arc<Mutex<HashMap<String, MediaTicket>>>,
    pub active_streams: Arc<Mutex<Vec<(u64, RelayCancellationToken)>>>,
    pub events_notify: Arc<Notify>,
    pub generation_store: GenerationStore,
    pub playlist_cache: Arc<Mutex<HashMap<u64, (u64, PlaylistDetailSummary)>>>,
    pub liked_playlist_id: Arc<Mutex<Option<u64>>>,
}

impl AppState {
    /// Construct gateway application state, restoring the playback generation floor from `state_dir`.
    pub fn new(
        backend: Arc<dyn MusicBackend>,
        media_relay: Arc<MediaRelay>,
        gateway_token: String,
        state_dir: PathBuf,
    ) -> Self {
        let generation_store = GenerationStore::new(&state_dir);
        let prev_gen = generation_store.load();
        let playback = PlaybackSession::from_restart(prev_gen, now_epoch_secs());

        Self {
            backend,
            media_relay,
            gateway_token: Arc::new(gateway_token),
            qr_sessions: Arc::new(Mutex::new(HashMap::new())),
            playback: Arc::new(Mutex::new(playback)),
            tickets: Arc::new(Mutex::new(HashMap::new())),
            active_streams: Arc::new(Mutex::new(Vec::new())),
            events_notify: Arc::new(Notify::new()),
            generation_store,
            playlist_cache: Arc::new(Mutex::new(HashMap::new())),
            liked_playlist_id: Arc::new(Mutex::new(None)),
        }
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    restored_from_disk: bool,
    cookie_summary: netease_adapter::CookieSummary,
    playback_generation: u64,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

#[derive(Debug, Deserialize)]
pub struct QrPollRequest {
    pub unikey: String,
}

#[derive(Debug, Deserialize)]
pub struct ResolveQuery {
    #[serde(default)]
    pub quality: Option<AudioQuality>,
}

#[derive(Debug, Deserialize)]
pub struct CoverQuery {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default)]
    pub offset: Option<u16>,
    #[serde(default)]
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub tracks: Vec<TrackSummary>,
}

#[derive(Debug, Deserialize)]
pub struct PlaylistsQuery {
    #[serde(default)]
    pub uid: Option<u64>,
    #[serde(default)]
    pub offset: Option<u16>,
    #[serde(default)]
    pub limit: Option<u16>,
}

#[derive(Debug, Serialize)]
pub struct PlaylistsResponse {
    pub uid: u64,
    pub playlists: Vec<PlaylistSummary>,
}

#[derive(Debug, Deserialize)]
pub struct IntelligenceQuery {
    pub song_id: u64,
    pub playlist_id: u64,
}

#[derive(Debug, Deserialize)]
pub struct LikelistQuery {
    #[serde(default)]
    pub uid: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct LikelistResponse {
    pub uid: u64,
    pub song_ids: Vec<u64>,
}

#[derive(Debug, Deserialize)]
pub struct LikeTrackRequest {
    pub like: bool,
}

#[derive(Debug, Serialize)]
pub struct LikeTrackResponse {
    pub song_id: u64,
    pub like: bool,
    pub ok: bool,
}

#[derive(Debug, Serialize)]
pub struct QrStartResponse {
    pub unikey: String,
    pub qr_url: String,
    pub qr_svg: String,
    pub expires_at_epoch_secs: u64,
    pub max_polls: u32,
}

#[derive(Debug, Serialize)]
pub struct MediaTicketResponse {
    pub ticket: String,
    pub media_url: String,
    pub generation: u64,
    pub expires_at_epoch_secs: u64,
    pub bitrate: u32,
    pub format_hint: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TicketBody {
    #[serde(default)]
    pub quality: Option<AudioQuality>,
}

#[derive(Debug, Deserialize)]
pub struct PlaybackCommandRequest {
    pub client_id: String,
    pub kind: String,
    #[serde(default)]
    pub expected_generation: Option<u64>,
    pub idempotency_key: String,
    #[serde(default)]
    pub track: Option<TrackRef>,
    #[serde(default)]
    pub track_title: Option<String>,
    #[serde(default)]
    pub track_artist: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub pic_url: Option<String>,
    #[serde(default)]
    pub quality: Option<AudioQuality>,
    #[serde(default)]
    pub queue: Option<Vec<QueueEntry>>,
    #[serde(default)]
    pub queue_index: Option<usize>,
    #[serde(default)]
    pub position_ms: Option<u64>,
    #[serde(default)]
    pub position_secs: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct PlaybackCommandResponse {
    pub accepted: bool,
    pub deduplicated: bool,
    pub generation: u64,
    pub owner: Option<String>,
    pub status: PlaybackStatus,
    pub current_track: Option<QueueEntry>,
    pub queue_index: Option<usize>,
    pub position_ms: u64,
    pub stop_instruction: Option<StopInstruction>,
    pub exclusivity_confirmed: bool,
    pub rejection_reason: Option<String>,
    pub ticket: Option<String>,
    pub media_url: Option<String>,
    pub media_error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AckStopRequest {
    pub client_id: String,
    pub revoked_generation: u64,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AckStopResponse {
    pub acknowledged: bool,
    pub exclusivity_confirmed: bool,
    pub generation: u64,
}

#[derive(Debug, Deserialize)]
pub struct PlaybackEventsQuery {
    pub client_id: String,
    #[serde(default)]
    pub known_generation: Option<u64>,
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct PlaybackEventResponse {
    pub stop_required: bool,
    pub revoked_generation: Option<u64>,
    pub generation: u64,
    pub owner_client_id: Option<String>,
    pub status: PlaybackStatus,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsCountersResponse {
    pub api_counters: ApiTrafficSnapshot,
    pub media_counters: MediaTrafficSnapshot,
}

/// Build the Axum router with open `/health`, `/player`, ticket-bound `/api/v1/media/{ticket}`,
/// and Bearer-protected `/api/v1/*` routes.
pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/v1/capabilities", get(get_capabilities))
        .route("/api/v1/auth/status", get(get_auth_status))
        .route("/api/v1/auth/qr/start", post(post_qr_start))
        .route("/api/v1/auth/qr/poll", post(post_qr_poll))
        .route("/api/v1/search", get(get_search))
        .route("/api/v1/playlists", get(get_playlists))
        .route("/api/v1/playlists/{id}", get(get_playlist_detail))
        .route("/api/v1/recommend/songs", get(get_recommend_songs))
        .route("/api/v1/personal-fm", get(get_personal_fm))
        .route("/api/v1/intelligence", get(get_intelligence_list))
        .route("/api/v1/likelist", get(get_likelist))
        .route("/api/v1/tracks/{id}/like", post(post_like_track))
        .route("/api/v1/tracks/{id}/resolve", get(get_resolve_track))
        .route("/api/v1/tracks/{id}/ticket", post(post_track_ticket))
        .route("/api/v1/tracks/{id}/stream", get(stream_track_direct))
        .route("/api/v1/playback/state", get(get_playback_state))
        .route("/api/v1/playback/command", post(post_playback_command))
        .route("/api/v1/playback/ack-stop", post(post_playback_ack_stop))
        .route("/api/v1/playback/events", get(get_playback_events))
        .route("/api/v1/diagnostics/counters", get(get_diagnostics_counters))
        .route("/api/v1/diagnostics/probe-proxy", post(post_probe_proxy))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_gateway_auth,
        ));

    Router::new()
        .route("/", get(get_player_page))
        .route("/player", get(get_player_page))
        .route("/health", get(get_health))
        .route("/api/v1/cover", get(proxy_cover_image))
        .route("/api/v1/media/{ticket}", get(stream_by_ticket))
        .route(
            "/api/v1/media/{ticket}/{filename}",
            get(stream_by_ticket_with_filename),
        )
        .merge(protected)
        .with_state(state)
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn generate_ticket_token() -> String {
    let seq = TICKET_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut h1 = RandomState::new().build_hasher();
    h1.write_u64(seq);
    h1.write_u64(now_epoch_secs());
    let part1 = h1.finish();

    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(part1 ^ 0x9e37_79b9_7f4a_7c15);
    h2.write_u64(seq.wrapping_mul(6364136223846793005));
    let part2 = h2.finish();

    format!("{part1:016x}{part2:016x}")
}

/// Constant-time token comparison to avoid timing side channels.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn require_gateway_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let auth_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|token| constant_time_eq(token.trim().as_bytes(), state.gateway_token.as_bytes()))
        .unwrap_or(false);

    if !auth_ok {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "unauthorized",
                message: "missing or invalid gateway bearer token".to_string(),
            }),
        )
            .into_response();
    }

    next.run(req).await
}

async fn get_player_page() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        PLAYER_HTML,
    )
        .into_response()
}

async fn get_health(State(state): State<AppState>) -> Json<HealthResponse> {
    let gen = state.playback.lock().await.generation();
    Json(HealthResponse {
        status: "ok",
        service: "bb10-music163-gateway",
        restored_from_disk: state.backend.restored_from_disk(),
        cookie_summary: state.backend.cookie_summary(),
        playback_generation: gen,
    })
}

async fn get_capabilities(State(state): State<AppState>) -> Json<ProviderCapabilities> {
    Json(state.backend.capabilities())
}

async fn get_auth_status(State(state): State<AppState>) -> Json<SessionCheckOutcome> {
    Json(state.backend.check_status().await)
}

async fn post_qr_start(
    State(state): State<AppState>,
) -> Result<Json<QrStartResponse>, (StatusCode, Json<ErrorBody>)> {
    let session = state
        .backend
        .start_qr_login(180, 90)
        .await
        .map_err(map_adapter_error)?;

    let resp = QrStartResponse {
        unikey: session.unikey.clone(),
        qr_url: session.qr_url.clone(),
        qr_svg: session.qr_svg.clone(),
        expires_at_epoch_secs: session.expires_at_epoch_secs,
        max_polls: session.max_polls,
    };

    let mut sessions = state.qr_sessions.lock().await;
    if sessions.len() >= 16 {
        sessions.clear();
    }
    sessions.insert(session.unikey.clone(), session);

    Ok(Json(resp))
}

async fn post_qr_poll(
    State(state): State<AppState>,
    Json(body): Json<QrPollRequest>,
) -> Result<Json<QrPollOutcome>, (StatusCode, Json<ErrorBody>)> {
    let mut session = {
        let sessions = state.qr_sessions.lock().await;
        sessions.get(&body.unikey).cloned().ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorBody {
                    error: "qr_session_not_found",
                    message: "no active QR login session for the provided unikey".to_string(),
                }),
            )
        })?
    };

    let outcome = state.backend.poll_qr_once(&mut session).await;
    let is_terminal = matches!(
        outcome,
        QrPollOutcome::Confirmed { .. }
            | QrPollOutcome::Expired { .. }
            | QrPollOutcome::PollLimitReached { .. }
    );

    let mut sessions = state.qr_sessions.lock().await;
    if is_terminal {
        sessions.remove(&body.unikey);
    } else {
        sessions.insert(body.unikey, session);
    }

    Ok(Json(outcome))
}

async fn get_search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchResponse>, (StatusCode, Json<ErrorBody>)> {
    let q = query.q.trim();
    if q.is_empty() {
        return Err(bad_request("invalid_query", "search query 'q' must not be empty"));
    }
    let tracks = state
        .backend
        .search_songs(q, query.offset.unwrap_or(0), query.limit.unwrap_or(20).min(50))
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(SearchResponse { tracks }))
}

async fn resolve_session_uid(
    state: &AppState,
    explicit_uid: Option<u64>,
) -> Result<u64, (StatusCode, Json<ErrorBody>)> {
    if let Some(id) = explicit_uid.or_else(|| state.backend.last_known_uid()) {
        return Ok(id);
    }
    match state.backend.check_status().await {
        SessionCheckOutcome::Authenticated { profile, .. } => Ok(profile.uid),
        SessionCheckOutcome::ConfirmedLoggedOut { reason, .. } => Err((
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "upstream_unauthenticated",
                message: reason,
            }),
        )),
        SessionCheckOutcome::TransientError { message, .. } => Err((
            StatusCode::BAD_GATEWAY,
            Json(ErrorBody {
                error: "transient_upstream_error",
                message,
            }),
        )),
    }
}

async fn get_playlists(
    State(state): State<AppState>,
    Query(query): Query<PlaylistsQuery>,
) -> Result<Json<PlaylistsResponse>, (StatusCode, Json<ErrorBody>)> {
    let uid = resolve_session_uid(&state, query.uid).await?;
    let playlists = state
        .backend
        .user_playlists(uid, query.offset.unwrap_or(0), query.limit.unwrap_or(30).min(100))
        .await
        .map_err(map_adapter_error)?;
    if let Some(fav) = playlists.iter().find(|p| p.special_type == 5) {
        let mut guard = state.liked_playlist_id.lock().await;
        *guard = Some(fav.id);
    }
    Ok(Json(PlaylistsResponse { uid, playlists }))
}

async fn get_recommend_songs(
    State(state): State<AppState>,
) -> Result<Json<SearchResponse>, (StatusCode, Json<ErrorBody>)> {
    let tracks = state
        .backend
        .recommend_songs()
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(SearchResponse { tracks }))
}

async fn get_personal_fm(
    State(state): State<AppState>,
) -> Result<Json<SearchResponse>, (StatusCode, Json<ErrorBody>)> {
    let tracks = state
        .backend
        .personal_fm()
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(SearchResponse { tracks }))
}

async fn get_intelligence_list(
    State(state): State<AppState>,
    Query(query): Query<IntelligenceQuery>,
) -> Result<Json<SearchResponse>, (StatusCode, Json<ErrorBody>)> {
    let tracks = state
        .backend
        .intelligence_list(query.song_id, query.playlist_id)
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(SearchResponse { tracks }))
}

async fn get_likelist(
    State(state): State<AppState>,
    Query(query): Query<LikelistQuery>,
) -> Result<Json<LikelistResponse>, (StatusCode, Json<ErrorBody>)> {
    let uid = resolve_session_uid(&state, query.uid).await?;
    let song_ids = state
        .backend
        .user_liked_song_ids(uid)
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(LikelistResponse { uid, song_ids }))
}

async fn post_like_track(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Json(req): Json<LikeTrackRequest>,
) -> Result<Json<LikeTrackResponse>, (StatusCode, Json<ErrorBody>)> {
    let ok = state
        .backend
        .like_track(id, req.like)
        .await
        .map_err(map_adapter_error)?;
    if ok {
        let mut cache = state.playlist_cache.lock().await;
        cache.clear();
    }
    Ok(Json(LikeTrackResponse {
        song_id: id,
        like: req.like,
        ok,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PlaylistDetailQuery {
    #[serde(default)]
    pub refresh: Option<bool>,
}

const PLAYLIST_DETAIL_UPSTREAM_CAP: usize = 1000;

async fn is_favorites_playlist(state: &AppState, playlist_id: u64) -> bool {
    if let Some(cached_id) = *state.liked_playlist_id.lock().await {
        return cached_id == playlist_id;
    }

    let Ok(uid) = resolve_session_uid(state, None).await else {
        return false;
    };
    let Ok(playlists) = state.backend.user_playlists(uid, 0, 50).await else {
        return false;
    };
    if let Some(fav) = playlists.iter().find(|p| p.special_type == 5) {
        let mut guard = state.liked_playlist_id.lock().await;
        *guard = Some(fav.id);
        return fav.id == playlist_id;
    }
    false
}

/// Hydrate liked tracks truncated past NetEase's 1,000-track playlist detail cap.
async fn hydrate_favorites_overflow(state: &AppState, detail: &mut PlaylistDetailSummary) {
    let Ok(uid) = resolve_session_uid(state, None).await else {
        return;
    };
    let liked_ids = match state.backend.user_liked_song_ids(uid).await {
        Ok(ids) => ids,
        Err(err) => {
            tracing::warn!(
                playlist_id = detail.id,
                error = %err,
                "failed to fetch liked song ids for favorites overflow hydration"
            );
            return;
        }
    };

    let mut seen: HashSet<u64> = detail.songs.iter().map(|s| s.id).collect();
    let missing_ids: Vec<u64> = liked_ids
        .into_iter()
        .filter(|song_id| seen.insert(*song_id))
        .collect();
    if missing_ids.is_empty() {
        return;
    }

    let base_count = detail.songs.len();
    match state.backend.songs_detail(&missing_ids).await {
        Ok(extra_tracks) => {
            tracing::info!(
                playlist_id = detail.id,
                base_count,
                extra_count = extra_tracks.len(),
                "hydrated favorites playlist overflow tracks"
            );
            detail.songs.extend(extra_tracks);
        }
        Err(err) => {
            tracing::warn!(
                playlist_id = detail.id,
                missing_count = missing_ids.len(),
                error = %err,
                "failed to hydrate favorites overflow tracks; returning base playlist"
            );
        }
    }
}

async fn get_playlist_detail(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<PlaylistDetailQuery>,
) -> Result<Json<PlaylistDetailSummary>, (StatusCode, Json<ErrorBody>)> {
    let now = now_epoch_secs();
    let force_refresh = query.refresh.unwrap_or(false);
    if !force_refresh {
        let cache = state.playlist_cache.lock().await;
        if let Some((expires_at, cached)) = cache.get(&id) {
            if *expires_at > now {
                return Ok(Json(cached.clone()));
            }
        }
    }

    let mut detail = state
        .backend
        .playlist_detail(id)
        .await
        .map_err(map_adapter_error)?;

    if detail.songs.len() >= PLAYLIST_DETAIL_UPSTREAM_CAP && is_favorites_playlist(&state, id).await
    {
        hydrate_favorites_overflow(&state, &mut detail).await;
    }

    {
        let mut cache = state.playlist_cache.lock().await;
        cache.retain(|_, (exp, _)| *exp > now);
        cache.insert(id, (now.saturating_add(PLAYLIST_CACHE_TTL_SECS), detail.clone()));
    }

    Ok(Json(detail))
}

async fn get_resolve_track(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<ResolveQuery>,
) -> Result<Json<PlaybackResolution>, (StatusCode, Json<ErrorBody>)> {
    let quality = query.quality.unwrap_or_default();
    let resolution = state
        .backend
        .resolve_track(id, quality)
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(resolution))
}

async fn post_track_ticket(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<ResolveQuery>,
    body: Option<Json<TicketBody>>,
) -> Result<Json<MediaTicketResponse>, (StatusCode, Json<ErrorBody>)> {
    let quality = body
        .and_then(|b| b.0.quality)
        .or(query.quality)
        .unwrap_or_default();
    let generation = {
        let pb = state.playback.lock().await;
        if pb.owner().is_some() {
            pb.generation()
        } else {
            // Standalone M2 ticket (generation 0) while no playback session owns playback.
            pb.generation()
        }
    };

    let resp = mint_media_ticket(&state, id, quality, generation).await?;
    Ok(Json(resp))
}

async fn mint_media_ticket(
    state: &AppState,
    song_id: u64,
    quality: AudioQuality,
    generation: u64,
) -> Result<MediaTicketResponse, (StatusCode, Json<ErrorBody>)> {
    let resolution = state
        .backend
        .resolve_track(song_id, quality)
        .await
        .map_err(map_adapter_error)?;

    let source = match resolution {
        PlaybackResolution::RelayableMedia(src) => src,
        other => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorBody {
                    error: "unsupported_playback_mode",
                    message: format!("track requires non-relayable playback mode: {other:?}"),
                }),
            ));
        }
    };

    let ticket = generate_ticket_token();
    let now = now_epoch_secs();
    let expires_at = now.saturating_add(DEFAULT_TICKET_TTL_SECS);
    let fmt_hint = source.format_hint.clone();
    let media_url = match fmt_hint.as_deref() {
        Some("aac") | Some("m4a") | Some("mp4") => format!("/api/v1/media/{ticket}/stream.m4a"),
        Some("mp3") => format!("/api/v1/media/{ticket}/stream.mp3"),
        Some("flac") => format!("/api/v1/media/{ticket}/stream.flac"),
        _ => format!("/api/v1/media/{ticket}"),
    };
    let entry = MediaTicket {
        song_id,
        quality,
        generation,
        expires_at_epoch_secs: expires_at,
        cached_url: source.url,
        format_hint: fmt_hint.clone(),
    };

    {
        let mut tickets = state.tickets.lock().await;
        tickets.retain(|_, v| v.expires_at_epoch_secs > now);
        if tickets.len() >= 128 {
            tickets.clear();
        }
        tickets.insert(ticket.clone(), entry);
    }

    Ok(MediaTicketResponse {
        media_url,
        ticket,
        generation,
        expires_at_epoch_secs: expires_at,
        bitrate: source.bitrate,
        format_hint: fmt_hint,
    })
}

async fn stream_by_ticket(
    State(state): State<AppState>,
    Path(ticket): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    stream_by_ticket_inner(&state, &ticket, method, &headers).await
}

async fn stream_by_ticket_with_filename(
    State(state): State<AppState>,
    Path((ticket, _filename)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    stream_by_ticket_inner(&state, &ticket, method, &headers).await
}

async fn stream_by_ticket_inner(
    state: &AppState,
    ticket: &str,
    method: Method,
    headers: &HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let entry = {
        let mut tickets = state.tickets.lock().await;
        let now = now_epoch_secs();
        match tickets.get(ticket).cloned() {
            Some(e) if e.expires_at_epoch_secs > now => e,
            Some(_) => {
                tickets.remove(ticket);
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(ErrorBody {
                        error: "ticket_expired",
                        message: "media ticket has expired".to_string(),
                    }),
                ));
            }
            None => {
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(ErrorBody {
                        error: "invalid_ticket",
                        message: "unknown media ticket".to_string(),
                    }),
                ));
            }
        }
    };

    {
        let pb = state.playback.lock().await;
        if pb.generation() > 0 || pb.owner().is_some() {
            if let Err(err) = pb.validate_media_generation(entry.generation) {
                return Err((
                    StatusCode::FORBIDDEN,
                    Json(ErrorBody {
                        error: "revoked_generation",
                        message: err.to_string(),
                    }),
                ));
            }
        }
    }

    relay_stream_response(
        state,
        Some(ticket),
        entry.song_id,
        entry.quality,
        entry.generation,
        entry.cached_url,
        entry.format_hint.as_deref(),
        method,
        headers,
    )
    .await
}

/// Proxy album cover images through the gateway on the same origin to avoid HTTPS mixed-content issues.
async fn proxy_cover_image(
    State(state): State<AppState>,
    Query(query): Query<CoverQuery>,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let raw_url = query.url.trim();
    if raw_url.is_empty() || raw_url.len() > 1024 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorBody {
                error: "invalid_cover_url",
                message: "cover url is empty or exceeds maximum length".to_string(),
            }),
        ));
    }

    let stream_res = state
        .media_relay
        .open(MediaSourceRequest::get(raw_url.to_string()))
        .await
        .map_err(map_relay_error)?;

    let mut response_builder = Response::builder()
        .status(stream_res.meta.status)
        .header(header::CACHE_CONTROL, "public, max-age=86400");
    if let Some(ct) = stream_res.meta.headers.get(header::CONTENT_TYPE) {
        response_builder = response_builder.header(header::CONTENT_TYPE, ct);
    }

    response_builder
        .body(Body::from_stream(stream_res.body))
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorBody {
                    error: "response_build_error",
                    message: e.to_string(),
                }),
            )
        })
}

async fn stream_track_direct(
    State(state): State<AppState>,
    Path(id): Path<u64>,
    Query(query): Query<ResolveQuery>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let quality = query.quality.unwrap_or_default();
    let resolution = state
        .backend
        .resolve_track(id, quality)
        .await
        .map_err(map_adapter_error)?;
    let source = match resolution {
        PlaybackResolution::RelayableMedia(src) => src,
        other => {
            return Err((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(ErrorBody {
                    error: "unsupported_playback_mode",
                    message: format!("track requires non-relayable playback mode: {other:?}"),
                }),
            ));
        }
    };
    let gen = state.playback.lock().await.generation();
    relay_stream_response(
        &state,
        None,
        id,
        quality,
        gen,
        source.url,
        source.format_hint.as_deref(),
        method,
        &headers,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn relay_stream_response(
    state: &AppState,
    ticket_key: Option<&str>,
    song_id: u64,
    quality: AudioQuality,
    generation: u64,
    initial_url: String,
    format_hint: Option<&str>,
    method: Method,
    headers: &HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorBody>)> {
    let relay_method = if method == Method::HEAD {
        RelayMethod::Head
    } else {
        RelayMethod::Get
    };

    let mut req = match relay_method {
        RelayMethod::Head => MediaSourceRequest::head(initial_url),
        RelayMethod::Get => MediaSourceRequest::get(initial_url),
    };
    if let Some(range) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        req = req.with_range(range);
    }
    if let Some(if_range) = headers
        .get(header::IF_RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        req = req.with_if_range(if_range);
    }

    let backend_clone = Arc::clone(&state.backend);
    let tickets_clone = Arc::clone(&state.tickets);
    let ticket_owned = ticket_key.map(ToString::to_string);

    let stream_res = state
        .media_relay
        .open_with_refresh(req, || async move {
            let refreshed = backend_clone
                .resolve_track(song_id, quality)
                .await
                .map_err(|e| RelayError::Transport(format!("url refresh failed: {e}")))?;
            let fresh_url = match refreshed {
                PlaybackResolution::RelayableMedia(src) => src.url,
                other => {
                    return Err(RelayError::PolicyViolation(format!(
                        "refreshed track returned non-relayable mode: {other:?}"
                    )));
                }
            };
            if let Some(ref t) = ticket_owned {
                let mut guard = tickets_clone.lock().await;
                if let Some(entry) = guard.get_mut(t) {
                    entry.cached_url = fresh_url.clone();
                }
            }
            Ok(fresh_url)
        })
        .await
        .map_err(map_relay_error)?;

    {
        let mut active = state.active_streams.lock().await;
        active.retain(|(_, tok)| !tok.is_cancelled());
        active.push((generation, stream_res.cancel_token.clone()));
    }

    let mut response_builder = Response::builder().status(stream_res.meta.status);
    let override_ct = match format_hint {
        Some("aac") | Some("m4a") | Some("mp4") => Some("audio/mp4"),
        Some("mp3") => Some("audio/mpeg"),
        Some("flac") => Some("audio/flac"),
        _ => None,
    };
    for (name, val) in stream_res.meta.headers.iter() {
        if override_ct.is_some() && *name == header::CONTENT_TYPE {
            continue;
        }
        response_builder = response_builder.header(name, val);
    }
    if let Some(ct) = override_ct {
        response_builder = response_builder.header(header::CONTENT_TYPE, ct);
    }

    let body = if relay_method == RelayMethod::Head {
        Body::empty()
    } else {
        Body::from_stream(stream_res.body)
    };

    response_builder.body(body).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: "response_build_error",
                message: e.to_string(),
            }),
        )
    })
}

async fn get_playback_state(State(state): State<AppState>) -> Json<PlaybackSnapshot> {
    Json(state.playback.lock().await.snapshot())
}

async fn post_playback_command(
    State(state): State<AppState>,
    Json(req): Json<PlaybackCommandRequest>,
) -> Result<Json<PlaybackCommandResponse>, (StatusCode, Json<ErrorBody>)> {
    let client_id = ClientId::new(req.client_id)
        .map_err(|e| bad_request("invalid_client_id", e.to_string()))?;

    let pos_ms = req.position_ms.unwrap_or_else(|| {
        req.position_secs
            .map(|s| if s > 0.0 { (s * 1000.0) as u64 } else { 0 })
            .unwrap_or(0)
    });

    let mut auto_next_track_override: Option<QueueEntry> = None;

    let kind = match req.kind.as_str() {
        "explicit_play" => {
            let track_ref = req.track.ok_or_else(|| {
                bad_request("missing_track", "explicit_play requires 'track' field")
            })?;
            let quality = req.quality.unwrap_or_default();
            let entry = QueueEntry {
                track: track_ref,
                title: req.track_title,
                artist: req.track_artist,
                duration_ms: req.duration_ms,
                quality,
                pic_url: req.pic_url,
            };
            PlaybackCommandKind::ExplicitPlay {
                track: entry,
                queue: req.queue,
                queue_index: req.queue_index,
            }
        }
        "auto_next" => {
            if let Some(track_ref) = req.track {
                auto_next_track_override = Some(QueueEntry {
                    track: track_ref,
                    title: req.track_title,
                    artist: req.track_artist,
                    duration_ms: req.duration_ms,
                    quality: req.quality.unwrap_or_default(),
                    pic_url: req.pic_url,
                });
            }
            PlaybackCommandKind::AutoNext
        }
        "pause" => PlaybackCommandKind::Pause,
        "resume" => PlaybackCommandKind::Resume,
        "seek" => PlaybackCommandKind::Seek {
            position_ms: pos_ms,
        },
        "stop" => PlaybackCommandKind::Stop,
        "heartbeat" => PlaybackCommandKind::Heartbeat {
            position_ms: if req.position_ms.is_some() || req.position_secs.is_some() {
                Some(pos_ms)
            } else {
                None
            },
        },
        "progress" => PlaybackCommandKind::Progress {
            position_ms: pos_ms,
        },
        other => {
            return Err(bad_request(
                "unsupported_command_kind",
                format!("unknown playback command kind '{other}'"),
            ));
        }
    };

    let cmd = PlaybackCommand {
        client_id,
        expected_generation: req.expected_generation.unwrap_or(0),
        idempotency_key: req.idempotency_key,
        kind,
    };

    let is_stop_cmd = matches!(cmd.kind, PlaybackCommandKind::Stop);
    let is_auto_next_cmd = matches!(cmd.kind, PlaybackCommandKind::AutoNext);
    let needs_ticket_resolution = matches!(
        cmd.kind,
        PlaybackCommandKind::ExplicitPlay { .. } | PlaybackCommandKind::AutoNext
    );

    let (outcome, prev_gen): (CommandOutcome, u64) = {
        let mut pb = state.playback.lock().await;
        let old_gen = pb.generation();
        let mut outcome = pb
            .apply(cmd, now_epoch_secs())
            .map_err(|e| bad_request("invalid_playback_command", e.to_string()))?;
        if outcome.accepted && !outcome.deduplicated && is_auto_next_cmd {
            if let Some(entry) = auto_next_track_override {
                pb.set_current_track_for_owner(entry.clone());
                outcome.current_track = Some(entry);
            }
        }
        (outcome, old_gen)
    };

    if outcome.accepted && (outcome.generation > prev_gen || is_stop_cmd) {
        let cutoff = if is_stop_cmd {
            outcome.generation
        } else {
            outcome.generation.saturating_sub(1)
        };

        {
            let mut active = state.active_streams.lock().await;
            for (g, tok) in active.iter() {
                if *g <= cutoff {
                    tok.cancel();
                }
            }
            active.retain(|(g, tok)| *g > cutoff && !tok.is_cancelled());
        }
        {
            let now = now_epoch_secs();
            let mut tickets = state.tickets.lock().await;
            tickets.retain(|_, v| v.expires_at_epoch_secs > now);
        }

        if outcome.generation > prev_gen {
            state.generation_store.save(outcome.generation);
        }
        state.events_notify.notify_waiters();
    }

    let mut ticket = None;
    let mut media_url = None;
    let mut media_error = None;

    if outcome.accepted && needs_ticket_resolution {
        if let Some(ref entry) = outcome.current_track {
            if entry.track.provider == ProviderId::Netease {
                match entry.track.track_id.parse::<u64>() {
                    Ok(song_id) => {
                        match mint_media_ticket(&state, song_id, entry.quality, outcome.generation)
                            .await
                        {
                            Ok(t) => {
                                ticket = Some(t.ticket);
                                media_url = Some(t.media_url);
                            }
                            Err((_, err_json)) => {
                                media_error = Some(err_json.0.message);
                            }
                        }
                    }
                    Err(e) => {
                        media_error = Some(format!("invalid NetEase song id: {e}"));
                    }
                }
            } else {
                media_error = Some(format!(
                    "provider '{}' is not relayable in current gateway release",
                    entry.track.provider
                ));
            }
        }
    }

    Ok(Json(PlaybackCommandResponse {
        accepted: outcome.accepted,
        deduplicated: outcome.deduplicated,
        generation: outcome.generation,
        owner: outcome.owner.map(|c| c.as_str().to_string()),
        status: outcome.status,
        current_track: outcome.current_track,
        queue_index: outcome.queue_index,
        position_ms: outcome.position_ms,
        stop_instruction: outcome.stop_instruction,
        exclusivity_confirmed: outcome.exclusivity_confirmed,
        rejection_reason: outcome.rejection_reason,
        ticket,
        media_url,
        media_error,
    }))
}

async fn post_playback_ack_stop(
    State(state): State<AppState>,
    Json(req): Json<AckStopRequest>,
) -> Result<Json<AckStopResponse>, (StatusCode, Json<ErrorBody>)> {
    if let Some(ref key) = req.idempotency_key {
        if key.trim().is_empty() {
            return Err(bad_request(
                "invalid_idempotency_key",
                "idempotency_key must not be blank when provided",
            ));
        }
    }
    let client_id = ClientId::new(req.client_id)
        .map_err(|e| bad_request("invalid_client_id", e.to_string()))?;

    let mut pb = state.playback.lock().await;
    match pb.ack_stop(&client_id, req.revoked_generation, now_epoch_secs()) {
        Ok(()) => {
            let snap = pb.snapshot();
            state.events_notify.notify_waiters();
            Ok(Json(AckStopResponse {
                acknowledged: true,
                exclusivity_confirmed: snap.exclusivity_confirmed,
                generation: snap.generation,
            }))
        }
        Err(SessionError::StaleStopAck { .. }) => {
            let snap = pb.snapshot();
            Ok(Json(AckStopResponse {
                acknowledged: false,
                exclusivity_confirmed: snap.exclusivity_confirmed,
                generation: snap.generation,
            }))
        }
        Err(err) => Err(bad_request("ack_stop_error", err.to_string())),
    }
}

async fn get_playback_events(
    State(state): State<AppState>,
    Query(query): Query<PlaybackEventsQuery>,
) -> Result<Json<PlaybackEventResponse>, (StatusCode, Json<ErrorBody>)> {
    let client_id = ClientId::new(query.client_id)
        .map_err(|e| bad_request("invalid_client_id", e.to_string()))?;
    let known_gen = query.known_generation.unwrap_or(0);
    let wait_ms = query.wait_ms.unwrap_or(0).min(MAX_EVENT_WAIT_MS);

    let check_snapshot = |pb: &PlaybackSession| -> Option<PlaybackEventResponse> {
        let snap = pb.snapshot();
        if let Some(instr) = pb.pending_stop_for(&client_id) {
            return Some(PlaybackEventResponse {
                stop_required: true,
                revoked_generation: Some(instr.revoked_generation),
                generation: snap.generation,
                owner_client_id: snap.owner.map(|o| o.as_str().to_string()),
                status: snap.status,
            });
        }
        if known_gen > 0
            && known_gen < snap.generation
            && snap.owner.as_ref() != Some(&client_id)
        {
            return Some(PlaybackEventResponse {
                stop_required: true,
                revoked_generation: Some(known_gen),
                generation: snap.generation,
                owner_client_id: snap.owner.map(|o| o.as_str().to_string()),
                status: snap.status,
            });
        }
        None
    };

    let notified = {
        let pb = state.playback.lock().await;
        if let Some(ev) = check_snapshot(&pb) {
            return Ok(Json(ev));
        }
        if wait_ms == 0 {
            let snap = pb.snapshot();
            return Ok(Json(PlaybackEventResponse {
                stop_required: false,
                revoked_generation: None,
                generation: snap.generation,
                owner_client_id: snap.owner.map(|o| o.as_str().to_string()),
                status: snap.status,
            }));
        }
        state.events_notify.notified()
    };

    tokio::select! {
        _ = notified => {}
        _ = tokio::time::sleep(Duration::from_millis(wait_ms)) => {}
    }

    let pb = state.playback.lock().await;
    if let Some(ev) = check_snapshot(&pb) {
        return Ok(Json(ev));
    }
    let snap = pb.snapshot();
    Ok(Json(PlaybackEventResponse {
        stop_required: false,
        revoked_generation: None,
        generation: snap.generation,
        owner_client_id: snap.owner.map(|o| o.as_str().to_string()),
        status: snap.status,
    }))
}

async fn get_diagnostics_counters(
    State(state): State<AppState>,
) -> Json<DiagnosticsCountersResponse> {
    Json(DiagnosticsCountersResponse {
        api_counters: state.backend.api_counters(),
        media_counters: state.media_relay.snapshot(),
    })
}

async fn post_probe_proxy(
    State(state): State<AppState>,
) -> Result<Json<ProxyProbeReport>, (StatusCode, Json<ErrorBody>)> {
    let report = state
        .backend
        .probe_proxy_egress("https://music.163.com/")
        .await
        .map_err(map_adapter_error)?;
    Ok(Json(report))
}

fn bad_request(code: &'static str, message: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            error: code,
            message: message.into(),
        }),
    )
}

fn map_adapter_error(err: AdapterError) -> (StatusCode, Json<ErrorBody>) {
    match err {
        AdapterError::Capability(cap) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorBody {
                error: "capability_error",
                message: cap.to_string(),
            }),
        ),
        AdapterError::ConfirmedLoggedOut { message, .. } => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorBody {
                error: "upstream_unauthenticated",
                message,
            }),
        ),
        AdapterError::Transient { message, .. } => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorBody {
                error: "transient_upstream_error",
                message,
            }),
        ),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: "internal_error",
                message: other.to_string(),
            }),
        ),
    }
}

fn map_relay_error(err: RelayError) -> (StatusCode, Json<ErrorBody>) {
    match err {
        RelayError::PolicyViolation(msg) => (
            StatusCode::FORBIDDEN,
            Json(ErrorBody {
                error: "relay_policy_violation",
                message: msg,
            }),
        ),
        RelayError::InvalidRequest(msg) => bad_request("invalid_media_request", msg),
        RelayError::UpstreamPreHeader { status, message } => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            Json(ErrorBody {
                error: "upstream_media_error",
                message,
            }),
        ),
        RelayError::Timeout(msg) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(ErrorBody {
                error: "upstream_timeout",
                message: msg,
            }),
        ),
        other => (
            StatusCode::BAD_GATEWAY,
            Json(ErrorBody {
                error: "media_relay_error",
                message: other.to_string(),
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::BoxFuture;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt;
    use media_relay::{DestinationPolicy, RelayConfig};
    use netease_adapter::{CookieSummary, NeteaseAdapter, NeteaseConfig};
    use provider_model::{AccountProfile, RelayableMediaSource};
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    struct MockBackend {
        fixture_port: u16,
        resolve_calls: AtomicUsize,
        api_calls: AtomicU64,
        favorites_base_count: usize,
        extra_liked_ids: Vec<u64>,
    }

    impl MockBackend {
        fn new(fixture_port: u16) -> Self {
            Self {
                fixture_port,
                resolve_calls: AtomicUsize::new(0),
                api_calls: AtomicU64::new(0),
                favorites_base_count: 1,
                extra_liked_ids: vec![1908049566, 29999478],
            }
        }
    }

    impl MusicBackend for MockBackend {
        fn capabilities(&self) -> ProviderCapabilities {
            NeteaseAdapter::capabilities()
        }

        fn restored_from_disk(&self) -> bool {
            true
        }

        fn cookie_summary(&self) -> CookieSummary {
            CookieSummary {
                total_cookies: 2,
                has_music_u: true,
                has_csrf: true,
                cookie_names: vec!["MUSIC_U".into(), "__csrf".into()],
            }
        }

        fn last_known_uid(&self) -> Option<u64> {
            Some(42)
        }

        fn api_counters(&self) -> ApiTrafficSnapshot {
            ApiTrafficSnapshot {
                api_requests_total: self.api_calls.load(Ordering::Relaxed),
                api_requests_failed: 0,
                probe_bytes_uploaded: 0,
                probe_bytes_downloaded: 0,
            }
        }

        fn check_status(&self) -> BoxFuture<'_, SessionCheckOutcome> {
            Box::pin(async {
                SessionCheckOutcome::Authenticated {
                    profile: AccountProfile {
                        uid: 42,
                        nickname: "Tester".into(),
                        vip_type: 11,
                    },
                    cookie_count: 2,
                    checked_at_epoch_secs: 1_700_000_000,
                }
            })
        }

        fn start_qr_login(
            &self,
            _ttl_secs: u64,
            _max_polls: u32,
        ) -> BoxFuture<'_, Result<QrSession, AdapterError>> {
            Box::pin(async { Err(AdapterError::Upstream("unused in mock".into())) })
        }

        fn poll_qr_once<'a>(&'a self, _session: &'a mut QrSession) -> BoxFuture<'a, QrPollOutcome> {
            Box::pin(async {
                QrPollOutcome::Expired {
                    code: 800,
                    message: "unused".into(),
                }
            })
        }

        fn resolve_track(
            &self,
            song_id: u64,
            quality: AudioQuality,
        ) -> BoxFuture<'_, Result<PlaybackResolution, AdapterError>> {
            let count = self.resolve_calls.fetch_add(1, Ordering::SeqCst);
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            let port = self.fixture_port;
            Box::pin(async move {
                // First resolution returns /expired; second returns /fresh.mp3
                let path = if count == 0 { "expired" } else { "fresh.mp3" };
                Ok(PlaybackResolution::RelayableMedia(RelayableMediaSource {
                    track: TrackRef::new(ProviderId::Netease, song_id.to_string()),
                    url: format!("http://127.0.0.1:{port}/{path}"),
                    bitrate: 128_000,
                    format_hint: Some("mp3".into()),
                    quality,
                    expires_hint_secs: Some(1200),
                }))
            })
        }

        fn search_songs(
            &self,
            keywords: &str,
            _offset: u16,
            _limit: u16,
        ) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            let kw = keywords.to_string();
            Box::pin(async move {
                Ok(vec![TrackSummary {
                    id: 1908049566,
                    track_ref: TrackRef::new(ProviderId::Netease, "1908049566"),
                    name: format!("Result for {kw}"),
                    singer: "Mock Artist".into(),
                    album: "Mock Album".into(),
                    duration_ms: 180_000,
                    pic_url: "https://p1.music.126.net/mock.jpg".into(),
                }])
            })
        }

        fn user_playlists(
            &self,
            _uid: u64,
            _offset: u16,
            _limit: u16,
        ) -> BoxFuture<'_, Result<Vec<PlaylistSummary>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(vec![
                    PlaylistSummary {
                        id: 777,
                        name: "Mock Favorites".into(),
                        author: "Tester".into(),
                        special_type: 5,
                        cover_img_url: "https://p1.music.126.net/mock_pl.jpg".into(),
                    },
                    PlaylistSummary {
                        id: 888,
                        name: "Regular Playlist".into(),
                        author: "Tester".into(),
                        special_type: 0,
                        cover_img_url: "https://p1.music.126.net/mock_pl.jpg".into(),
                    },
                ])
            })
        }

        fn playlist_detail(
            &self,
            playlist_id: u64,
        ) -> BoxFuture<'_, Result<PlaylistDetailSummary, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            let base_count = self.favorites_base_count;
            Box::pin(async move {
                let songs = if base_count <= 1 {
                    vec![TrackSummary {
                        id: 1908049566,
                        track_ref: TrackRef::new(ProviderId::Netease, "1908049566"),
                        name: "Mock Song".into(),
                        singer: "Mock Artist".into(),
                        album: "Mock Album".into(),
                        duration_ms: 180_000,
                        pic_url: "https://p1.music.126.net/mock.jpg".into(),
                    }]
                } else {
                    (1..=base_count as u64)
                        .map(|song_id| TrackSummary {
                            id: song_id,
                            track_ref: TrackRef::new(ProviderId::Netease, song_id.to_string()),
                            name: format!("Mock Song {song_id}"),
                            singer: "Mock Artist".into(),
                            album: "Mock Album".into(),
                            duration_ms: 180_000,
                            pic_url: "https://p1.music.126.net/mock.jpg".into(),
                        })
                        .collect()
                };
                Ok(PlaylistDetailSummary {
                    id: playlist_id,
                    name: if playlist_id == 777 {
                        "Mock Favorites".into()
                    } else {
                        "Regular Playlist".into()
                    },
                    description: "Mock".into(),
                    cover_img_url: "https://p1.music.126.net/mock_pl.jpg".into(),
                    songs,
                })
            })
        }

        fn recommend_songs(&self) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(vec![TrackSummary {
                    id: 433223079,
                    track_ref: TrackRef::new(ProviderId::Netease, "433223079"),
                    name: "Million Reasons".into(),
                    singer: "Lady Gaga".into(),
                    album: "Joanne".into(),
                    duration_ms: 205_000,
                    pic_url: "https://p1.music.126.net/daily.jpg".into(),
                }])
            })
        }

        fn personal_fm(&self) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(vec![TrackSummary {
                    id: 186272,
                    track_ref: TrackRef::new(ProviderId::Netease, "186272"),
                    name: "FM Song".into(),
                    singer: "FM Artist".into(),
                    album: "FM Album".into(),
                    duration_ms: 200_000,
                    pic_url: "https://p1.music.126.net/fm.jpg".into(),
                }])
            })
        }

        fn intelligence_list(
            &self,
            _song_id: u64,
            _playlist_id: u64,
        ) -> BoxFuture<'_, Result<Vec<TrackSummary>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async {
                Ok(vec![TrackSummary {
                    id: 29999478,
                    track_ref: TrackRef::new(ProviderId::Netease, "29999478"),
                    name: "Heart Track".into(),
                    singer: "Heart Artist".into(),
                    album: "Heart Album".into(),
                    duration_ms: 210_000,
                    pic_url: "https://p1.music.126.net/heart.jpg".into(),
                }])
            })
        }

        fn user_liked_song_ids(&self, _uid: u64) -> BoxFuture<'_, Result<Vec<u64>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            let ids = self.extra_liked_ids.clone();
            Box::pin(async move { Ok(ids) })
        }

        fn songs_detail<'a>(
            &'a self,
            ids: &'a [u64],
        ) -> BoxFuture<'a, Result<Vec<TrackSummary>, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                Ok(ids
                    .iter()
                    .map(|&song_id| TrackSummary {
                        id: song_id,
                        track_ref: TrackRef::new(ProviderId::Netease, song_id.to_string()),
                        name: format!("Hydrated Song {song_id}"),
                        singer: "Mock Artist".into(),
                        album: "Mock Album".into(),
                        duration_ms: 180_000,
                        pic_url: "https://p1.music.126.net/mock.jpg".into(),
                    })
                    .collect())
            })
        }

        fn like_track(
            &self,
            _song_id: u64,
            _like: bool,
        ) -> BoxFuture<'_, Result<bool, AdapterError>> {
            self.api_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(true) })
        }

        fn probe_proxy_egress(
            &self,
            _target_url: &str,
        ) -> BoxFuture<'_, Result<ProxyProbeReport, AdapterError>> {
            Box::pin(async { Err(AdapterError::Upstream("unused".into())) })
        }
    }

    async fn spawn_fixture_audio_server() -> (u16, Vec<u8>) {
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let server_payload = payload.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let data = server_payload.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    let req_str = String::from_utf8_lossy(&buf[..n]);
                    let first_line = req_str.lines().next().unwrap_or("");
                    let mut parts = first_line.split_whitespace();
                    let method = parts.next().unwrap_or("GET");
                    let path = parts.next().unwrap_or("/");

                    if path == "/expired" {
                        let _ = sock
                            .write_all(
                                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await;
                        return;
                    }

                    let range_hdr = req_str.lines().find_map(|l| {
                        let lower = l.to_ascii_lowercase();
                        if lower.starts_with("range:") {
                            Some(l[6..].trim().to_string())
                        } else {
                            None
                        }
                    });

                    if let Some(r) = range_hdr {
                        if let Some(spec) = r.strip_prefix("bytes=") {
                            let bounds: Vec<&str> = spec.split('-').collect();
                            let start: usize = bounds.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                            let end: usize = bounds
                                .get(1)
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(data.len() - 1)
                                .min(data.len() - 1);
                            if start <= end && start < data.len() {
                                let slice = &data[start..=end];
                                let hdr = format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Type: audio/mpeg\r\nAccept-Ranges: bytes\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    data.len(),
                                    slice.len()
                                );
                                let _ = sock.write_all(hdr.as_bytes()).await;
                                if method != "HEAD" {
                                    let _ = sock.write_all(slice).await;
                                }
                                return;
                            }
                        }
                    }

                    let hdr = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len()
                    );
                    let _ = sock.write_all(hdr.as_bytes()).await;
                    if method != "HEAD" {
                        let _ = sock.write_all(&data).await;
                    }
                });
            }
        });

        (port, payload)
    }

    fn test_state() -> (TempDir, AppState) {
        let dir = TempDir::new().unwrap();
        let cfg = NeteaseConfig {
            api_proxy: None,
            media_proxy: None,
            tls_verify: true,
            state_dir: dir.path().to_path_buf(),
            max_connections: 4,
        };
        let adapter = Arc::new(NeteaseAdapter::new(cfg).unwrap());
        let relay = Arc::new(
            MediaRelay::new(RelayConfig::default(), DestinationPolicy::netease_default()).unwrap(),
        );
        let state = AppState::new(
            adapter,
            relay,
            "test-gateway-token".to_string(),
            dir.path().to_path_buf(),
        );
        (dir, state)
    }

    #[tokio::test]
    async fn health_player_and_bearer_auth_work() {
        let (_dir, state) = test_state();
        let app = build_router(state);

        let res = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(HttpRequest::builder().uri("/player").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/capabilities")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/capabilities")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn media_ticket_range_refresh_and_ownership_revocation_work() {
        let (port, expected_bytes) = spawn_fixture_audio_server().await;
        let dir = TempDir::new().unwrap();
        let backend: Arc<dyn MusicBackend> = Arc::new(MockBackend::new(port));
        let relay = Arc::new(
            MediaRelay::new(
                RelayConfig::default(),
                DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]),
            )
            .unwrap(),
        );
        let state = AppState::new(
            backend,
            relay,
            "test-gateway-token".to_string(),
            dir.path().to_path_buf(),
        );
        let app = build_router(state.clone());

        // 1. Standalone M2 ticket issuance -> initial URL is /expired (403), gateway refreshes to /fresh.mp3 and serves Range 206.
        let ticket_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/tracks/1908049566/ticket")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ticket_res.status(), StatusCode::OK);
        let ticket_json: serde_json::Value =
            serde_json::from_slice(&ticket_res.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let media_url_0 = ticket_json["media_url"].as_str().unwrap().to_string();

        let range_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(&media_url_0)
                    .header(header::RANGE, "bytes=10-109")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(range_res.status(), StatusCode::PARTIAL_CONTENT);
        let slice = range_res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&slice[..], &expected_bytes[10..=109]);

        let head_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::HEAD)
                    .uri(&media_url_0)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(head_res.status(), StatusCode::OK);
        assert_eq!(
            head_res.headers().get(header::CONTENT_LENGTH).unwrap(),
            "4096"
        );
        let head_body = head_res.into_body().collect().await.unwrap().to_bytes();
        assert!(head_body.is_empty());

        // 2. ExplicitPlay by BB10 creates gen=1 and supersedes standalone gen=0 ticket (403 Forbidden).
        let play_bb10 = serde_json::json!({
            "client_id": "bb10-passport",
            "kind": "explicit_play",
            "idempotency_key": "k1",
            "track": { "provider": "netease", "track_id": "1908049566" },
            "quality": "standard"
        });
        let p1_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/playback/command")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(play_bb10.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(p1_res.status(), StatusCode::OK);
        let p1_json: serde_json::Value =
            serde_json::from_slice(&p1_res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(p1_json["generation"], 1);
        let bb10_media_url = p1_json["media_url"].as_str().unwrap().to_string();

        let old_ticket_blocked = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(&media_url_0)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(old_ticket_blocked.status(), StatusCode::FORBIDDEN);

        // 3. Desktop ExplicitPlay takes ownership (gen=2), revokes BB10's media URL, and emits stop event for BB10.
        let play_desktop = serde_json::json!({
            "client_id": "desktop-browser",
            "kind": "explicit_play",
            "idempotency_key": "k2",
            "track": { "provider": "netease", "track_id": "1908049566" },
            "quality": "higher"
        });
        let p2_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/playback/command")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(play_desktop.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(p2_res.status(), StatusCode::OK);

        let bb10_revoked = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(&bb10_media_url)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            bb10_revoked.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ));

        let ev_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/playback/events?client_id=bb10-passport&known_generation=1")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let ev_json: serde_json::Value =
            serde_json::from_slice(&ev_res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(ev_json["stop_required"], true);
        assert_eq!(ev_json["revoked_generation"], 1);

        let ack_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/playback/ack-stop")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "client_id": "bb10-passport",
                            "revoked_generation": 1,
                            "idempotency_key": "ack-1"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let ack_json: serde_json::Value =
            serde_json::from_slice(&ack_res.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(ack_json["acknowledged"], true);
        assert_eq!(ack_json["exclusivity_confirmed"], true);

        // 3a. AutoNext supplying next client track updates current_track and issues extension-suffixed media URL.
        let auto_next = serde_json::json!({
            "client_id": "desktop-browser",
            "kind": "auto_next",
            "expected_generation": 2,
            "idempotency_key": "an-1",
            "track": { "provider": "netease", "track_id": "29999478" },
            "quality": "standard"
        });
        let an_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/playback/command")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(auto_next.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let an_json: serde_json::Value =
            serde_json::from_slice(&an_res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(an_json["accepted"], true);
        assert_eq!(an_json["current_track"]["track"]["track_id"], "29999478");
        assert!(an_json["media_url"].as_str().unwrap().ends_with("/stream.mp3"));

        // 3b. Search & playlist endpoints return catalogue results.
        let search_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/search?q=hello")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(search_res.status(), StatusCode::OK);

        let pl_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/playlists")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pl_res.status(), StatusCode::OK);

        let pl_det = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/playlists/777")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pl_det.status(), StatusCode::OK);

        // 3c. Recommend, Personal FM, Intelligence, Likelist, and Like endpoints work.
        for path in [
            "/api/v1/recommend/songs",
            "/api/v1/personal-fm",
            "/api/v1/intelligence?song_id=1908049566&playlist_id=777",
            "/api/v1/likelist",
        ] {
            let r = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK, "expected OK for {path}");
        }
        let like_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/v1/tracks/1908049566/like")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"like":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(like_res.status(), StatusCode::OK);

        // 4. Diagnostics counters track API vs media traffic independently.
        let cnt_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/diagnostics/counters")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let cnt_json: serde_json::Value =
            serde_json::from_slice(&cnt_res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert!(cnt_json["api_counters"]["api_requests_total"].as_u64().unwrap() >= 3);
        assert_eq!(cnt_json["media_counters"]["bytes_forwarded"], 100);
        assert_eq!(cnt_json["media_counters"]["pre_header_refreshes"], 1);

        // 5. Generation floor persisted to disk prevents restart from resetting generation back to 0.
        let restarted = AppState::new(
            state.backend.clone(),
            state.media_relay.clone(),
            "test-gateway-token".to_string(),
            dir.path().to_path_buf(),
        );
        let snap = restarted.playback.lock().await.snapshot();
        assert_eq!(snap.generation, 2);
        assert!(snap.owner.is_none());
        assert_eq!(snap.status, PlaybackStatus::Stopped);
        // 6. Cover proxy rejects disallowed hosts via DestinationPolicy.
        let cover_bad = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/cover?url=http://evil.example/cover.jpg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cover_bad.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn favorites_playlist_hydrates_beyond_1000_limit() {
        let dir = TempDir::new().unwrap();
        let backend: Arc<dyn MusicBackend> = Arc::new(MockBackend {
            fixture_port: 0,
            resolve_calls: AtomicUsize::new(0),
            api_calls: AtomicU64::new(0),
            favorites_base_count: 1000,
            extra_liked_ids: (1..=1412).collect(),
        });
        let relay = Arc::new(
            MediaRelay::new(
                RelayConfig::default(),
                DestinationPolicy::local_fixture_for_test(vec!["127.0.0.1".into()]),
            )
            .unwrap(),
        );
        let state = AppState::new(
            backend,
            relay,
            "test-gateway-token".to_string(),
            dir.path().to_path_buf(),
        );
        let app = build_router(state);

        // Cold favorites detail request (before /api/v1/playlists) detects special_type == 5
        // and hydrates the 412 liked songs truncated past NetEase's 1,000-track ceiling.
        let fav_res = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/playlists/777")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fav_res.status(), StatusCode::OK);
        let fav_bytes = fav_res.into_body().collect().await.unwrap().to_bytes();
        let fav_json: PlaylistDetailSummary = serde_json::from_slice(&fav_bytes).unwrap();
        assert_eq!(fav_json.songs.len(), 1412);
        let unique_ids: HashSet<u64> = fav_json.songs.iter().map(|s| s.id).collect();
        assert_eq!(unique_ids.len(), 1412);
        assert_eq!(fav_json.songs[0].id, 1);
        assert_eq!(fav_json.songs[1411].id, 1412);

        // Non-favorites playlist remains untouched at 1,000 tracks.
        let reg_res = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/playlists/888")
                    .header(header::AUTHORIZATION, "Bearer test-gateway-token")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(reg_res.status(), StatusCode::OK);
        let reg_bytes = reg_res.into_body().collect().await.unwrap().to_bytes();
        let reg_json: PlaylistDetailSummary = serde_json::from_slice(&reg_bytes).unwrap();
        assert_eq!(reg_json.songs.len(), 1000);
    }
}
