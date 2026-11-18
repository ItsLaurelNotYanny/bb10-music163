use playback_session::{
    ClientId, PlaybackCommand, PlaybackCommandKind, PlaybackSession, PlaybackStatus, QueueEntry,
    SessionError, StopAckState,
};
use provider_model::{AudioQuality, ProviderId, TrackRef};

fn sample_track(id: &str) -> QueueEntry {
    QueueEntry {
        track: TrackRef::new(ProviderId::Netease, id),
        title: Some(format!("Track {id}")),
        artist: Some("Artist".to_string()),
        duration_ms: Some(180_000),
        quality: AudioQuality::Standard,
        pic_url: None,
    }
}

fn explicit_play(client: &str, track_id: &str, key: &str) -> PlaybackCommand {
    PlaybackCommand {
        client_id: ClientId::new(client).unwrap(),
        expected_generation: 0,
        idempotency_key: key.to_string(),
        kind: PlaybackCommandKind::ExplicitPlay {
            track: sample_track(track_id),
            queue: Some(vec![sample_track(track_id), sample_track("200")]),
            queue_index: Some(0),
        },
    }
}

#[test]
fn desktop_to_phone_and_phone_to_desktop_transfers_require_stop_ack() {
    let mut session = PlaybackSession::new();
    let desktop = ClientId::new("desktop-browser").unwrap();
    let bb10 = ClientId::new("bb10-passport").unwrap();

    // 1. Desktop starts playback -> gen 1, no prior owner so exclusivity confirmed.
    let out1 = session
        .apply(explicit_play("desktop-browser", "100", "k1"), 1000)
        .unwrap();
    assert!(out1.accepted);
    assert_eq!(out1.generation, 1);
    assert_eq!(out1.owner.as_ref(), Some(&desktop));
    assert!(out1.stop_instruction.is_none());
    assert!(out1.exclusivity_confirmed);

    // 2. BB10 takes ownership -> gen 2, StopInstruction targeting desktop gen 1.
    let out2 = session
        .apply(explicit_play("bb10-passport", "300", "k2"), 1005)
        .unwrap();
    assert!(out2.accepted);
    assert_eq!(out2.generation, 2);
    assert_eq!(out2.owner.as_ref(), Some(&bb10));
    assert!(!out2.exclusivity_confirmed);
    let stop_instr = out2.stop_instruction.expect("must instruct desktop to stop");
    assert_eq!(stop_instr.target_client, desktop);
    assert_eq!(stop_instr.revoked_generation, 1);
    assert_eq!(stop_instr.new_generation, 2);
    assert!(session.pending_stop_for(&desktop).is_some());
    assert!(session.validate_media_generation(1).is_err());
    assert!(session.validate_media_generation(2).is_ok());

    // 3. Desktop acknowledges stop -> exclusivity confirmed.
    session.ack_stop(&desktop, 1, 1006).unwrap();
    assert!(session.snapshot().exclusivity_confirmed);
    assert!(session.pending_stop_for(&desktop).is_none());

    // 4. Desktop reclaims ownership -> gen 3, StopInstruction targeting BB10 gen 2.
    let out3 = session
        .apply(explicit_play("desktop-browser", "400", "k3"), 1010)
        .unwrap();
    assert_eq!(out3.generation, 3);
    assert!(!out3.exclusivity_confirmed);
    let stop_bb10 = out3.stop_instruction.expect("must instruct BB10 to stop");
    assert_eq!(stop_bb10.target_client, bb10);
    assert_eq!(stop_bb10.revoked_generation, 2);
}

#[test]
fn delayed_auto_next_heartbeat_progress_and_reconnect_cannot_steal_ownership() {
    let mut session = PlaybackSession::new();
    let bb10 = ClientId::new("bb10-passport").unwrap();
    let desktop = ClientId::new("desktop-browser").unwrap();

    session
        .apply(explicit_play("bb10-passport", "100", "p1"), 100)
        .unwrap();
    session
        .apply(explicit_play("desktop-browser", "200", "p2"), 101)
        .unwrap();

    // BB10 sends delayed AutoNext, Heartbeat, Progress, Resume with its old generation 1
    for (idx, kind) in [
        PlaybackCommandKind::AutoNext,
        PlaybackCommandKind::Heartbeat {
            position_ms: Some(45_000),
        },
        PlaybackCommandKind::Progress { position_ms: 46_000 },
        PlaybackCommandKind::Resume,
        PlaybackCommandKind::Pause,
        PlaybackCommandKind::Seek { position_ms: 10_000 },
    ]
    .into_iter()
    .enumerate()
    {
        let res = session
            .apply(
                PlaybackCommand {
                    client_id: bb10.clone(),
                    expected_generation: 1,
                    idempotency_key: format!("stale-{idx}"),
                    kind,
                },
                102,
            )
            .unwrap();
        assert!(!res.accepted);
        assert_eq!(res.generation, 2);
        assert_eq!(res.owner.as_ref(), Some(&desktop));
    }

    // Even if BB10 guesses generation 2 on a reconnect, it is NOT the active owner
    let stolen_attempt = session
        .apply(
            PlaybackCommand {
                client_id: bb10,
                expected_generation: 2,
                idempotency_key: "guess-gen".to_string(),
                kind: PlaybackCommandKind::AutoNext,
            },
            103,
        )
        .unwrap();
    assert!(!stolen_attempt.accepted);
    assert_eq!(stolen_attempt.owner.as_ref(), Some(&desktop));
}

#[test]
fn duplicate_idempotency_keys_do_not_advance_generation() {
    let mut session = PlaybackSession::new();
    let cmd = explicit_play("bb10-passport", "100", "dup-key");

    let first = session.apply(cmd.clone(), 500).unwrap();
    assert!(first.accepted);
    assert!(!first.deduplicated);
    assert_eq!(first.generation, 1);

    let replay = session.apply(cmd, 501).unwrap();
    assert!(replay.accepted);
    assert!(replay.deduplicated);
    assert_eq!(replay.generation, 1);
    assert_eq!(session.generation(), 1);
}

#[test]
fn unreachable_stop_target_never_reports_confirmed_exclusivity() {
    let mut session = PlaybackSession::new();
    let phone = ClientId::new("bb10-passport").unwrap();

    session
        .apply(explicit_play("bb10-passport", "100", "k1"), 10)
        .unwrap();
    let transfer = session
        .apply(explicit_play("desktop-browser", "200", "k2"), 11)
        .unwrap();
    assert!(!transfer.exclusivity_confirmed);

    session
        .mark_stop_unreachable(&phone, 1, "client offline")
        .unwrap();
    let snap = session.snapshot();
    assert!(!snap.exclusivity_confirmed);
    assert!(matches!(snap.stop_ack, StopAckState::Unreachable { .. }));

    // Stale ack for wrong generation is rejected.
    assert!(matches!(
        session.ack_stop(&phone, 99, 12),
        Err(SessionError::StaleStopAck { .. })
    ));

    // Late real ack promotes unreachable -> acknowledged.
    session.ack_stop(&phone, 1, 13).unwrap();
    assert!(session.snapshot().exclusivity_confirmed);
}

#[test]
fn gateway_restart_preserves_generation_floor_without_auto_resuming_old_owner() {
    let mut session = PlaybackSession::from_restart(7, 2000);
    let bb10 = ClientId::new("bb10-passport").unwrap();

    assert_eq!(session.generation(), 7);
    assert_eq!(session.snapshot().status, PlaybackStatus::Stopped);
    assert!(session.owner().is_none());
    assert!(session.validate_media_generation(7).is_err());

    // Old client trying to heartbeat/resume with gen 7 after restart cannot hijack playback
    let resume_old = session
        .apply(
            PlaybackCommand {
                client_id: bb10.clone(),
                expected_generation: 7,
                idempotency_key: "post-restart-resume".to_string(),
                kind: PlaybackCommandKind::Resume,
            },
            2001,
        )
        .unwrap();
    assert!(!resume_old.accepted);

    // New explicit play starts strictly above the restored floor (gen 8)
    let fresh = session
        .apply(explicit_play("bb10-passport", "555", "post-restart-play"), 2002)
        .unwrap();
    assert!(fresh.accepted);
    assert_eq!(fresh.generation, 8);
    assert_eq!(fresh.owner.as_ref(), Some(&bb10));
}
