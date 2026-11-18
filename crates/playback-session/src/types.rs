use crate::error::SessionError;
use provider_model::{AudioQuality, TrackRef};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Non-empty identifier of a playback terminal (e.g. `bb10-passport-1` or `desktop-chrome-1`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientId(String);

impl ClientId {
    /// Validate and construct a non-empty [`ClientId`].
    pub fn new(raw: impl Into<String>) -> Result<Self, SessionError> {
        let trimmed = raw.into().trim().to_string();
        if trimmed.is_empty() {
            return Err(SessionError::EmptyClientId);
        }
        Ok(Self(trimmed))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Queue item carrying provider-independent track identification and display metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub track: TrackRef,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub quality: AudioQuality,
    #[serde(default)]
    pub pic_url: Option<String>,
}

/// Logical playback state of the active owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackStatus {
    Idle,
    Playing,
    Paused,
    Stopped,
}

/// Instruction emitted when a new explicit play revokes an older owner's generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopInstruction {
    pub target_client: ClientId,
    pub revoked_generation: u64,
    pub new_generation: u64,
    pub new_owner: ClientId,
}

/// Delivery / acknowledgement state for stopping a superseded terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StopAckState {
    None,
    Pending {
        client_id: ClientId,
        revoked_generation: u64,
        new_generation: u64,
        new_owner: ClientId,
        requested_at_epoch_secs: u64,
    },
    Acknowledged {
        client_id: ClientId,
        revoked_generation: u64,
        acknowledged_at_epoch_secs: u64,
    },
    Unreachable {
        client_id: ClientId,
        revoked_generation: u64,
        reason: String,
    },
}

impl StopAckState {
    /// Returns `true` only when no prior terminal needed stopping or its stop was acknowledged.
    pub fn is_exclusivity_confirmed(&self) -> bool {
        matches!(self, Self::None | Self::Acknowledged { .. })
    }
}

/// Playback commands accepted by [`crate::PlaybackSession`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlaybackCommandKind {
    ExplicitPlay {
        track: QueueEntry,
        queue: Option<Vec<QueueEntry>>,
        queue_index: Option<usize>,
    },
    AutoNext,
    Pause,
    Resume,
    Seek {
        position_ms: u64,
    },
    Stop,
    Heartbeat {
        position_ms: Option<u64>,
    },
    Progress {
        position_ms: u64,
    },
}

/// Ownership-checked playback command envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackCommand {
    pub client_id: ClientId,
    /// Required to match active generation for all commands except `ExplicitPlay`.
    pub expected_generation: u64,
    /// Client-generated key ensuring network retries do not double-advance state.
    pub idempotency_key: String,
    #[serde(flatten)]
    pub kind: PlaybackCommandKind,
}

/// Result of applying a [`PlaybackCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcome {
    pub accepted: bool,
    pub deduplicated: bool,
    pub generation: u64,
    pub owner: Option<ClientId>,
    pub status: PlaybackStatus,
    pub current_track: Option<QueueEntry>,
    pub queue_index: Option<usize>,
    pub position_ms: u64,
    pub stop_instruction: Option<StopInstruction>,
    pub exclusivity_confirmed: bool,
    pub rejection_reason: Option<String>,
}

/// Read-only snapshot of current playback session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackSnapshot {
    pub generation: u64,
    pub owner: Option<ClientId>,
    pub status: PlaybackStatus,
    pub current_track: Option<QueueEntry>,
    pub queue: Vec<QueueEntry>,
    pub queue_index: Option<usize>,
    pub position_ms: u64,
    pub stop_ack: StopAckState,
    pub exclusivity_confirmed: bool,
    pub last_activity_epoch_secs: u64,
}
