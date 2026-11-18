use crate::error::SessionError;
use crate::types::{
    ClientId, CommandOutcome, PlaybackCommand, PlaybackCommandKind, PlaybackSnapshot,
    PlaybackStatus, QueueEntry, StopAckState, StopInstruction,
};
use std::collections::VecDeque;

const MAX_IDEMPOTENCY_ENTRIES: usize = 256;

/// Authoritative single-owner playback state machine with monotonic generations.
#[derive(Debug, Clone)]
pub struct PlaybackSession {
    generation: u64,
    owner: Option<ClientId>,
    status: PlaybackStatus,
    current_track: Option<QueueEntry>,
    queue: Vec<QueueEntry>,
    queue_index: Option<usize>,
    position_ms: u64,
    stop_ack: StopAckState,
    last_activity_epoch_secs: u64,
    idempotency_cache: VecDeque<(String, String, CommandOutcome)>,
}

impl Default for PlaybackSession {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackSession {
    /// Create a fresh playback session starting at generation `0` with no owner.
    pub fn new() -> Self {
        Self {
            generation: 0,
            owner: None,
            status: PlaybackStatus::Idle,
            current_track: None,
            queue: Vec::new(),
            queue_index: None,
            position_ms: 0,
            stop_ack: StopAckState::None,
            last_activity_epoch_secs: 0,
            idempotency_cache: VecDeque::with_capacity(MAX_IDEMPOTENCY_ENTRIES),
        }
    }

    /// Restore a session after a gateway restart, preserving the monotonic generation floor
    /// while clearing ownership and stopping playback so old clients cannot auto-resume.
    pub fn from_restart(previous_generation: u64, now_epoch_secs: u64) -> Self {
        Self {
            generation: previous_generation,
            owner: None,
            status: PlaybackStatus::Stopped,
            current_track: None,
            queue: Vec::new(),
            queue_index: None,
            position_ms: 0,
            stop_ack: StopAckState::None,
            last_activity_epoch_secs: now_epoch_secs,
            idempotency_cache: VecDeque::with_capacity(MAX_IDEMPOTENCY_ENTRIES),
        }
    }

    /// Return the current monotonic generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Return the active owning terminal, if any.
    pub fn owner(&self) -> Option<&ClientId> {
        self.owner.as_ref()
    }

    /// Return a full read-only snapshot of session state.
    pub fn snapshot(&self) -> PlaybackSnapshot {
        PlaybackSnapshot {
            generation: self.generation,
            owner: self.owner.clone(),
            status: self.status,
            current_track: self.current_track.clone(),
            queue: self.queue.clone(),
            queue_index: self.queue_index,
            position_ms: self.position_ms,
            stop_ack: self.stop_ack.clone(),
            exclusivity_confirmed: self.stop_ack.is_exclusivity_confirmed(),
            last_activity_epoch_secs: self.last_activity_epoch_secs,
        }
    }

    /// Check whether a media ticket's generation is still authorized to stream audio.
    pub fn validate_media_generation(&self, ticket_generation: u64) -> Result<(), SessionError> {
        if self.owner.is_none()
            || !matches!(self.status, PlaybackStatus::Playing | PlaybackStatus::Paused)
            || ticket_generation != self.generation
        {
            return Err(SessionError::RevokedGeneration {
                ticket_generation,
                active_generation: self.generation,
            });
        }
        Ok(())
    }

    /// Update the active track for the current owner (used when client-side `AutoNext` supplies
    /// the next queued entry outside a server-stored full queue).
    pub fn set_current_track_for_owner(&mut self, entry: QueueEntry) {
        if let Some(pos) = self.queue.iter().position(|q| q.track == entry.track) {
            self.queue_index = Some(pos);
        }
        self.current_track = Some(entry);
        self.position_ms = 0;
        self.status = PlaybackStatus::Playing;
    }

    /// Return an outstanding [`StopInstruction`] if `client_id` has a pending stop acknowledgement.
    pub fn pending_stop_for(&self, client_id: &ClientId) -> Option<StopInstruction> {
        match &self.stop_ack {
            StopAckState::Pending {
                client_id: target,
                revoked_generation,
                new_generation,
                new_owner,
                ..
            } if target == client_id => Some(StopInstruction {
                target_client: target.clone(),
                revoked_generation: *revoked_generation,
                new_generation: *new_generation,
                new_owner: new_owner.clone(),
            }),
            _ => None,
        }
    }

    /// Apply a client playback command under single-owner generation arbitration.
    pub fn apply(
        &mut self,
        cmd: PlaybackCommand,
        now_epoch_secs: u64,
    ) -> Result<CommandOutcome, SessionError> {
        let key = cmd.idempotency_key.trim();
        if key.is_empty() {
            return Err(SessionError::EmptyIdempotencyKey);
        }

        if let Some((_, _, cached)) = self
            .idempotency_cache
            .iter()
            .rev()
            .find(|(cid, k, _)| cid == cmd.client_id.as_str() && k == key)
        {
            let mut replay = cached.clone();
            replay.deduplicated = true;
            return Ok(replay);
        }

        let outcome = match cmd.kind {
            PlaybackCommandKind::ExplicitPlay {
                track,
                queue,
                queue_index,
            } => self.handle_explicit_play(
                &cmd.client_id,
                track,
                queue,
                queue_index,
                now_epoch_secs,
            ),
            other => self.handle_owner_command(
                &cmd.client_id,
                cmd.expected_generation,
                other,
                now_epoch_secs,
            ),
        };

        if self.idempotency_cache.len() >= MAX_IDEMPOTENCY_ENTRIES {
            self.idempotency_cache.pop_front();
        }
        self.idempotency_cache.push_back((
            cmd.client_id.as_str().to_string(),
            key.to_string(),
            outcome.clone(),
        ));

        Ok(outcome)
    }

    /// Record acknowledgement from a superseded terminal confirming local playback stopped and buffers cleared.
    pub fn ack_stop(
        &mut self,
        client_id: &ClientId,
        revoked_generation: u64,
        now_epoch_secs: u64,
    ) -> Result<(), SessionError> {
        match &self.stop_ack {
            StopAckState::Pending {
                client_id: expected_client,
                revoked_generation: expected_gen,
                ..
            } if expected_client == client_id && *expected_gen == revoked_generation => {
                self.stop_ack = StopAckState::Acknowledged {
                    client_id: client_id.clone(),
                    revoked_generation,
                    acknowledged_at_epoch_secs: now_epoch_secs,
                };
                Ok(())
            }
            StopAckState::Unreachable {
                client_id: expected_client,
                revoked_generation: expected_gen,
                ..
            } if expected_client == client_id && *expected_gen == revoked_generation => {
                // Late acknowledgement arriving after an unreachable mark promotes to Acknowledged.
                self.stop_ack = StopAckState::Acknowledged {
                    client_id: client_id.clone(),
                    revoked_generation,
                    acknowledged_at_epoch_secs: now_epoch_secs,
                };
                Ok(())
            }
            StopAckState::Pending {
                client_id: expected_client,
                revoked_generation: expected_gen,
                ..
            } => Err(SessionError::StaleStopAck {
                expected_client: Some(expected_client.as_str().to_string()),
                expected_gen: Some(*expected_gen),
                got_client: client_id.as_str().to_string(),
                got_gen: revoked_generation,
            }),
            _ => Err(SessionError::StaleStopAck {
                expected_client: None,
                expected_gen: None,
                got_client: client_id.as_str().to_string(),
                got_gen: revoked_generation,
            }),
        }
    }

    /// Mark a pending stop target as unreachable so callers never misreport confirmed exclusivity.
    pub fn mark_stop_unreachable(
        &mut self,
        client_id: &ClientId,
        revoked_generation: u64,
        reason: impl Into<String>,
    ) -> Result<(), SessionError> {
        match &self.stop_ack {
            StopAckState::Pending {
                client_id: expected_client,
                revoked_generation: expected_gen,
                ..
            } if expected_client == client_id && *expected_gen == revoked_generation => {
                self.stop_ack = StopAckState::Unreachable {
                    client_id: client_id.clone(),
                    revoked_generation,
                    reason: reason.into(),
                };
                Ok(())
            }
            StopAckState::Pending {
                client_id: expected_client,
                revoked_generation: expected_gen,
                ..
            } => Err(SessionError::StaleStopAck {
                expected_client: Some(expected_client.as_str().to_string()),
                expected_gen: Some(*expected_gen),
                got_client: client_id.as_str().to_string(),
                got_gen: revoked_generation,
            }),
            _ => Err(SessionError::StaleStopAck {
                expected_client: None,
                expected_gen: None,
                got_client: client_id.as_str().to_string(),
                got_gen: revoked_generation,
            }),
        }
    }

    fn handle_explicit_play(
        &mut self,
        client_id: &ClientId,
        track: QueueEntry,
        queue: Option<Vec<QueueEntry>>,
        queue_index: Option<usize>,
        now_epoch_secs: u64,
    ) -> CommandOutcome {
        let old_gen = self.generation;
        let old_owner = self.owner.clone();
        let was_active = matches!(
            self.status,
            PlaybackStatus::Playing | PlaybackStatus::Paused
        );

        self.generation = self.generation.saturating_add(1);
        self.owner = Some(client_id.clone());
        self.status = PlaybackStatus::Playing;
        self.position_ms = 0;
        self.last_activity_epoch_secs = now_epoch_secs;

        if let Some(new_queue) = queue {
            self.queue = new_queue;
            self.queue_index = queue_index.or_else(|| {
                self.queue
                    .iter()
                    .position(|entry| entry.track == track.track)
            });
        } else if self.queue.is_empty() {
            self.queue = vec![track.clone()];
            self.queue_index = Some(0);
        } else if let Some(idx) = queue_index {
            self.queue_index = if idx < self.queue.len() {
                Some(idx)
            } else {
                None
            };
        } else if let Some(pos) = self.queue.iter().position(|entry| entry.track == track.track) {
            self.queue_index = Some(pos);
        }

        self.current_track = Some(track);

        let stop_instruction = match old_owner {
            Some(prev) if was_active && prev != *client_id => {
                let instr = StopInstruction {
                    target_client: prev.clone(),
                    revoked_generation: old_gen,
                    new_generation: self.generation,
                    new_owner: client_id.clone(),
                };
                self.stop_ack = StopAckState::Pending {
                    client_id: prev,
                    revoked_generation: old_gen,
                    new_generation: self.generation,
                    new_owner: client_id.clone(),
                    requested_at_epoch_secs: now_epoch_secs,
                };
                Some(instr)
            }
            _ => {
                // Keep any unacknowledged stop pending for a previously displaced third client;
                // otherwise clear stop_ack.
                if !matches!(self.stop_ack, StopAckState::Pending { .. }) {
                    self.stop_ack = StopAckState::None;
                }
                None
            }
        };

        CommandOutcome {
            accepted: true,
            deduplicated: false,
            generation: self.generation,
            owner: self.owner.clone(),
            status: self.status,
            current_track: self.current_track.clone(),
            queue_index: self.queue_index,
            position_ms: self.position_ms,
            stop_instruction,
            exclusivity_confirmed: self.stop_ack.is_exclusivity_confirmed(),
            rejection_reason: None,
        }
    }

    fn handle_owner_command(
        &mut self,
        client_id: &ClientId,
        expected_generation: u64,
        kind: PlaybackCommandKind,
        now_epoch_secs: u64,
    ) -> CommandOutcome {
        if let Some(reason) = self.check_ownership(client_id, expected_generation) {
            return CommandOutcome {
                accepted: false,
                deduplicated: false,
                generation: self.generation,
                owner: self.owner.clone(),
                status: self.status,
                current_track: self.current_track.clone(),
                queue_index: self.queue_index,
                position_ms: self.position_ms,
                stop_instruction: None,
                exclusivity_confirmed: self.stop_ack.is_exclusivity_confirmed(),
                rejection_reason: Some(reason),
            };
        }

        self.last_activity_epoch_secs = now_epoch_secs;

        match kind {
            PlaybackCommandKind::AutoNext => {
                if self.queue.is_empty() {
                    return CommandOutcome {
                        accepted: false,
                        deduplicated: false,
                        generation: self.generation,
                        owner: self.owner.clone(),
                        status: self.status,
                        current_track: self.current_track.clone(),
                        queue_index: self.queue_index,
                        position_ms: self.position_ms,
                        stop_instruction: None,
                        exclusivity_confirmed: self.stop_ack.is_exclusivity_confirmed(),
                        rejection_reason: Some("queue is empty for auto_next".to_string()),
                    };
                }
                let next_idx = match self.queue_index {
                    Some(idx) => (idx + 1) % self.queue.len(),
                    None => 0,
                };
                self.queue_index = Some(next_idx);
                self.current_track = Some(self.queue[next_idx].clone());
                self.position_ms = 0;
                self.status = PlaybackStatus::Playing;
            }
            PlaybackCommandKind::Pause => {
                self.status = PlaybackStatus::Paused;
            }
            PlaybackCommandKind::Resume => {
                self.status = PlaybackStatus::Playing;
            }
            PlaybackCommandKind::Seek { position_ms } => {
                self.position_ms = position_ms;
            }
            PlaybackCommandKind::Stop => {
                self.status = PlaybackStatus::Stopped;
                self.position_ms = 0;
                self.owner = None;
            }
            PlaybackCommandKind::Heartbeat { position_ms } => {
                if let Some(pos) = position_ms {
                    self.position_ms = pos;
                }
            }
            PlaybackCommandKind::Progress { position_ms } => {
                self.position_ms = position_ms;
            }
            PlaybackCommandKind::ExplicitPlay { .. } => unreachable!(),
        }

        CommandOutcome {
            accepted: true,
            deduplicated: false,
            generation: self.generation,
            owner: self.owner.clone(),
            status: self.status,
            current_track: self.current_track.clone(),
            queue_index: self.queue_index,
            position_ms: self.position_ms,
            stop_instruction: None,
            exclusivity_confirmed: self.stop_ack.is_exclusivity_confirmed(),
            rejection_reason: None,
        }
    }

    fn check_ownership(&self, client_id: &ClientId, expected_generation: u64) -> Option<String> {
        let Some(ref active_owner) = self.owner else {
            return Some("no active playback owner".to_string());
        };
        if !matches!(self.status, PlaybackStatus::Playing | PlaybackStatus::Paused) {
            return Some("playback is stopped or idle".to_string());
        }
        if active_owner != client_id {
            return Some(format!(
                "client '{}' does not own playback (owned by '{}')",
                client_id, active_owner
            ));
        }
        if expected_generation != self.generation {
            return Some(format!(
                "stale generation {} (active generation is {})",
                expected_generation, self.generation
            ));
        }
        None
    }
}
