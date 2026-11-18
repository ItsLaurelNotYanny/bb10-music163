//! Provider-independent playback ownership arbitration and monotonic generation state machine.
//!
//! This crate has zero HTTP, cookie, NetEase, UI, or database dependencies.
//!
//! ## Ownership Rules
//! - `ExplicitPlay` allocates a new monotonically increasing `generation`, assigns ownership
//!   to the requesting `ClientId`, and emits a [`StopInstruction`] if a different client
//!   previously owned active playback.
//! - `AutoNext`, `Pause`, `Resume`, `Seek`, `Stop`, `Heartbeat`, and `Progress` require the
//!   caller to be the current owner AND present the current `generation`. Stale generations,
//!   delayed requests, and reconnects cannot steal ownership.
//! - After a gateway restart ([`PlaybackSession::from_restart`]), the previous `generation`
//!   floor is preserved while ownership starts cleared (`None`), preventing old terminals
//!   from auto-resuming or hijacking playback.

pub mod error;
pub mod session;
pub mod types;

pub use error::SessionError;
pub use session::PlaybackSession;
pub use types::{
    ClientId, CommandOutcome, PlaybackCommand, PlaybackCommandKind, PlaybackSnapshot,
    PlaybackStatus, QueueEntry, StopAckState, StopInstruction,
};
