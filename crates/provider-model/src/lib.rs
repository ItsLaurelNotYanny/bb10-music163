//! Provider-independent domain identifiers, capability models, and session check outcomes.
//!
//! This crate contains zero HTTP, cookie, database, or provider-specific protocol dependencies.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Supported or evaluated music service providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    Netease,
    AppleMusic,
    Spotify,
    Tidal,
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Netease => write!(f, "netease"),
            Self::AppleMusic => write!(f, "apple_music"),
            Self::Spotify => write!(f, "spotify"),
            Self::Tidal => write!(f, "tidal"),
        }
    }
}

/// Provider-independent track reference carrying both provider and provider-native track ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TrackRef {
    pub provider: ProviderId,
    pub track_id: String,
}

impl TrackRef {
    /// Construct a new provider-independent track reference.
    pub fn new(provider: ProviderId, track_id: impl Into<String>) -> Self {
        Self {
            provider,
            track_id: track_id.into(),
        }
    }
}

/// Normalized audio quality tier requested by clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioQuality {
    #[default]
    Standard,
    Higher,
    Extreme,
    Lossless,
    HiRes,
}

/// Catalogue and metadata capabilities for a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueCapabilities {
    pub search: bool,
    pub playlists: bool,
    pub track_details: bool,
}

/// Account authentication and session lifecycle capabilities for a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCapabilities {
    pub qr_login: bool,
    pub sms_login: bool,
    pub unattended_renewal: bool,
    pub renewal_note: String,
}

/// Distinct playback delivery architectures across providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// Gateway fetches and relays encoded audio bytes directly to the terminal.
    RelayableMedia,
    /// Client terminal must initialize an official provider SDK (e.g. MusicKit, Spotify Web Playback).
    ProviderSdkPlayback,
    /// Control commands target a remote official player without direct audio relay.
    RemoteDeviceControl,
}

/// Playback capabilities for a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackCapabilities {
    pub supported_modes: Vec<PlaybackMode>,
    pub supports_range_seek: bool,
}

/// Complete separated capability profile for a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    pub provider: ProviderId,
    pub catalogue: CatalogueCapabilities,
    pub account: AccountCapabilities,
    pub playback: PlaybackCapabilities,
}

/// Resolved media source suitable for gateway HTTP relay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayableMediaSource {
    pub track: TrackRef,
    pub url: String,
    pub bitrate: u32,
    pub format_hint: Option<String>,
    pub quality: AudioQuality,
    pub expires_hint_secs: Option<u64>,
}

/// Result of resolving playback for a track; distinguishes relayable streams from SDK/remote modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PlaybackResolution {
    RelayableMedia(RelayableMediaSource),
    ProviderSdkPlayback {
        track: TrackRef,
        sdk_uri: String,
    },
    RemoteDeviceControl {
        track: TrackRef,
        target_device_required: bool,
    },
}

/// Explicit capability or entitlement error returned instead of faking unsupported playback.
#[derive(Debug, Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityError {
    #[error("operation '{operation}' is not supported by provider '{provider}'")]
    UnsupportedOperation {
        provider: ProviderId,
        operation: String,
    },
    #[error("playback mode '{mode:?}' is not supported by provider '{provider}'")]
    UnsupportedPlaybackMode {
        provider: ProviderId,
        mode: PlaybackMode,
    },
    #[error("track '{track_id}' on '{provider}' is restricted: {reason}")]
    RegionOrCopyrightRestricted {
        provider: ProviderId,
        track_id: String,
        reason: String,
    },
    #[error("track '{track_id}' on '{provider}' requires higher account tier: {reason}")]
    AccountTierRequired {
        provider: ProviderId,
        track_id: String,
        reason: String,
    },
}

/// Sanitized account profile (strictly free of cookies or auth secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub uid: u64,
    pub nickname: String,
    pub vip_type: i32,
}

/// Classification of transient upstream/network errors that must NOT discard stored credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransientErrorKind {
    NetworkOrProxy,
    TlsValidation,
    Timeout,
    UpstreamGateway,
    UnexpectedPayload,
}

/// Outcome of an account session status verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionCheckOutcome {
    Authenticated {
        profile: AccountProfile,
        cookie_count: usize,
        checked_at_epoch_secs: u64,
    },
    ConfirmedLoggedOut {
        upstream_code: i32,
        reason: String,
        checked_at_epoch_secs: u64,
    },
    TransientError {
        kind: TransientErrorKind,
        message: String,
        credentials_preserved: bool,
        checked_at_epoch_secs: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_ref_preserves_provider_and_id() {
        let track = TrackRef::new(ProviderId::Netease, "1908049566");
        assert_eq!(track.provider, ProviderId::Netease);
        assert_eq!(track.track_id, "1908049566");
    }

    #[test]
    fn playback_resolution_serializes_with_explicit_mode_tag() {
        let source = RelayableMediaSource {
            track: TrackRef::new(ProviderId::Netease, "12345"),
            url: "https://example.invalid/track.mp3".to_string(),
            bitrate: 128000,
            format_hint: Some("mp3".to_string()),
            quality: AudioQuality::Standard,
            expires_hint_secs: Some(1200),
        };
        let res = PlaybackResolution::RelayableMedia(source);
        let json = serde_json::to_string(&res).unwrap();
        assert!(json.contains("\"mode\":\"relayable_media\""));
    }

    #[test]
    fn capability_error_formats_explicitly() {
        let err = CapabilityError::UnsupportedPlaybackMode {
            provider: ProviderId::Spotify,
            mode: PlaybackMode::RelayableMedia,
        };
        let msg = err.to_string();
        assert!(msg.contains("spotify"));
        assert!(msg.contains("RelayableMedia"));
    }
}
