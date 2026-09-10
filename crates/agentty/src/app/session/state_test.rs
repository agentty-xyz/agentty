use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use super::{SessionGitStatus, SessionState};
use crate::app::session::Clock;
use crate::domain::agent::{AgentKind, AgentSelection};
use crate::domain::selection::SelectionState;
use crate::domain::session::{
    Session, SessionDiffState, SessionDiffStats, SessionHandles, SessionId, SessionSize, Status,
};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::test_support::SessionFixtureBuilder;

struct FixedClock {
    instant: Instant,
    system_time: SystemTime,
}

impl FixedClock {
    fn new() -> Self {
        Self {
            instant: Instant::now(),
            system_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        }
    }
}

impl Clock for FixedClock {
    fn now_instant(&self) -> Instant {
        self.instant
    }

    fn now_system_time(&self) -> SystemTime {
        self.system_time
    }
}

fn session_replay_text(session: &Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

/// Builds the common session snapshot used by state-focused tests.
fn state_session_fixture(session_id: impl Into<SessionId>, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .agent(AgentSelection::new(
            AgentKind::Antigravity,
            AgentKind::Antigravity.default_model(),
        ))
        .folder(std::env::temp_dir())
        .id(session_id)
        .prompt("prompt")
        .status(status)
        .build()
}

#[test]
/// Verifies handle transcript replaces the session transcript snapshot.
fn sync_from_handles_updates_transcript_snapshot() {
    // Arrange
    let session_id = "sess-1".to_string();
    let mut session = state_session_fixture(session_id.clone(), Status::Review);
    session.transcript = Some(crate::test_support::assistant_transcript("old"));
    let handles: HashMap<SessionId, SessionHandles> = HashMap::from([(
        session_id.into(),
        SessionHandles::new_with_transcript(
            Status::Review,
            crate::test_support::assistant_transcript("new"),
        ),
    )]);
    let mut state = SessionState::new(
        handles,
        vec![session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    state.sync_from_handles();

    // Assert
    assert_eq!(session_replay_text(&state.sessions[0]), "new\n\n");
    assert_eq!(state.sessions[0].status, Status::Review);
}

#[test]
/// Applies optimistic status transitions through the runtime-state
/// boundary without exposing the handle map.
fn transition_status_if_current_updates_snapshot_and_live_handle() {
    // Arrange
    let session_id = SessionId::from("session-status-transition");
    let session = SessionFixtureBuilder::new()
        .id(session_id.as_str())
        .status(Status::Review)
        .build();
    let handles = HashMap::from([(session_id.clone(), SessionHandles::new(Status::Review))]);
    let mut state = SessionState::new(
        handles,
        vec![session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    state.transition_status_if_current(session_id.as_str(), Status::Review, Status::AgentReview);
    state.transition_status_if_current(session_id.as_str(), Status::Question, Status::Done);

    // Assert
    assert_eq!(state.sessions()[0].status, Status::AgentReview);
    assert_eq!(
        state.handle(session_id.as_str()).and_then(|handles| handles
            .status
            .lock()
            .ok()
            .map(|status| *status)),
        Some(Status::AgentReview)
    );
}

#[test]
/// Verifies direct single-session sync updates transcript and status.
fn sync_session_with_handles_updates_transcript_and_status() {
    // Arrange
    let mut session = state_session_fixture("session-2", Status::Draft);
    session.transcript = Some(crate::test_support::assistant_transcript("Old"));
    let handles = SessionHandles::new_with_transcript(
        Status::InProgress,
        crate::test_support::assistant_transcript("New"),
    );

    // Act
    SessionState::sync_session_with_handles(&mut session, &handles);

    // Assert
    assert_eq!(session_replay_text(&session), "New\n\n");
    assert_eq!(session.status, Status::InProgress);
}

#[test]
/// Verifies a failed turn's status sync clears its review-resolution
/// loader.
fn sync_from_handles_clears_review_resolution_loader_after_failed_turn() {
    // Arrange
    let session_id = SessionId::from("failed-review-resolution");
    let mut session = SessionFixtureBuilder::new()
        .id(session_id.as_str())
        .status(Status::InProgress)
        .build();
    session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Resolving 2 review comments...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::ReviewCommentResolution,
        turn_position: None,
    });
    let handles = HashMap::from([(session_id, SessionHandles::new(Status::Review))]);
    let mut state = SessionState::new(
        handles,
        vec![session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    state.sync_from_handles();

    // Assert
    assert_eq!(state.sessions[0].status, Status::Review);
    assert!(
        state.sessions[0]
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
}

#[test]
/// Verifies extended handle transcripts replace the session snapshot.
fn sync_session_with_handles_replaces_with_extended_transcript() {
    // Arrange
    let mut session = state_session_fixture("session-3", Status::InProgress);
    session.transcript = Some(crate::test_support::assistant_transcript("first line\n"));
    let handles = SessionHandles::new_with_transcript(
        Status::InProgress,
        crate::test_support::assistant_transcript("first line\nsecond line\n"),
    );

    // Act
    SessionState::sync_session_with_handles(&mut session, &handles);

    // Assert
    assert_eq!(session_replay_text(&session), "first line\nsecond line\n\n");
    assert_eq!(session.status, Status::InProgress);
}

#[test]
/// Verifies handle transcript changes replace stale typed snapshots.
fn sync_session_with_handles_replaces_stale_transcript() {
    // Arrange
    let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::UserPrompt,
        "old prompt",
    )]);
    let mut session = SessionFixtureBuilder::new().status(Status::Review).build();
    session.transcript = Some(transcript);
    let handles = SessionHandles::new_with_transcript(
        Status::Review,
        crate::test_support::assistant_transcript("new output"),
    );

    // Act
    SessionState::sync_session_with_handles(&mut session, &handles);

    // Assert
    assert_eq!(session_replay_text(&session), "new output\n\n");
}

#[test]
/// Verifies known and unknown diff updates patch the in-memory snapshot
/// without discarding the last known line totals.
fn apply_session_diff_stats_updated_updates_matching_session() {
    // Arrange
    let session_id = "session-3".to_string();
    let session = state_session_fixture(session_id.clone(), Status::Review);
    let mut state = SessionState::new(
        HashMap::new(),
        vec![session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    state.apply_session_diff_stats_updated(
        &session_id,
        SessionDiffStats::Known {
            added_lines: 12,
            deleted_lines: 4,
            has_diff: true,
            session_size: SessionSize::S,
        },
    );
    state.apply_session_diff_stats_updated(&session_id, SessionDiffStats::Unknown);

    // Assert
    assert_eq!(state.sessions[0].stats.added_lines, 12);
    assert_eq!(state.sessions[0].stats.deleted_lines, 4);
    assert_eq!(
        state.sessions[0].stats.diff_state,
        SessionDiffState::Unknown
    );
    assert_eq!(state.sessions[0].size, SessionSize::S);
}

#[test]
/// Verifies replacing the session list rebuilds identifier lookups.
fn replace_sessions_rebuilds_session_id_index() {
    // Arrange
    let initial_session = state_session_fixture("session-1", Status::Review);
    let replacement_session = state_session_fixture("session-2", Status::Review);
    let mut state = SessionState::new(
        HashMap::new(),
        vec![initial_session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    state.replace_sessions(vec![replacement_session]);

    // Assert
    assert_eq!(state.session_index_for_id("session-1"), None);
    assert_eq!(state.session_index_for_id("session-2"), Some(0));
}

#[test]
/// Verifies persisted refreshes retain active workflow loaders.
fn replace_sessions_preserves_active_transient_messages() {
    // Arrange
    let mut initial_session = SessionFixtureBuilder::new()
        .id("session-1")
        .status(Status::Review)
        .build();
    initial_session.transient_messages.upsert(TransientMessage {
        anchor: TransientMessageAnchor::Tail,
        body: TransientMessageBody::Loading("Publishing review request...".to_string()),
        lifecycle: TransientMessageLifecycle::UntilResolved,
        slot: TransientMessageSlot::BranchPublish,
        turn_position: None,
    });
    let mut refreshed_session = SessionFixtureBuilder::new()
        .id("session-1")
        .status(Status::Review)
        .build();
    refreshed_session.published_upstream_ref = Some("origin/wt/session-1".to_string());
    let mut state = SessionState::new(
        HashMap::new(),
        vec![initial_session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    state.replace_sessions(vec![refreshed_session]);

    // Assert
    let refreshed_session = &state.sessions[0];
    assert_eq!(
        refreshed_session.published_upstream_ref.as_deref(),
        Some("origin/wt/session-1")
    );
    assert_eq!(
        refreshed_session
            .transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .map(|message| message.body.text()),
        Some("Publishing review request...")
    );
}

#[test]
/// Verifies removing a session keeps identifier lookups aligned with the
/// remaining list order.
fn remove_session_at_rebuilds_session_id_index() {
    // Arrange
    let first_session = state_session_fixture("session-1", Status::Review);
    let second_session = state_session_fixture("session-2", Status::Review);
    let mut state = SessionState::new(
        HashMap::new(),
        vec![first_session, second_session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );

    // Act
    let removed_session = state.remove_session_at(0);

    // Assert
    assert_eq!(
        removed_session.map(|session| session.id),
        Some("session-1".into())
    );
    assert_eq!(state.session_index_for_id("session-1"), None);
    assert_eq!(state.session_index_for_id("session-2"), Some(0));
}

#[test]
/// Verifies non-prefix transcript changes still replace the snapshot.
fn sync_session_with_handles_replaces_transcript_when_prefix_changes() {
    // Arrange
    let mut session = state_session_fixture("session-4", Status::InProgress);
    session.transcript = Some(crate::test_support::assistant_transcript("abc"));
    let handles = SessionHandles::new_with_transcript(
        Status::Review,
        crate::test_support::assistant_transcript("xyzq"),
    );

    // Act
    SessionState::sync_session_with_handles(&mut session, &handles);

    // Assert
    assert_eq!(session_replay_text(&session), "xyzq\n\n");
    assert_eq!(session.status, Status::Review);
}

#[test]
/// Verifies session git-status caching keeps only entries for active
/// sessions after refresh.
fn retain_session_git_statuses_drops_removed_sessions() {
    // Arrange
    let mut state = SessionState::new(
        HashMap::new(),
        Vec::new(),
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );
    state.replace_session_git_statuses(HashMap::from([
        (
            "session-1".into(),
            SessionGitStatus {
                base_status: Some((1, 0)),
                has_merge_conflict: Some(false),
                remote_status: Some((0, 1)),
            },
        ),
        (
            "session-2".into(),
            SessionGitStatus {
                base_status: Some((0, 2)),
                has_merge_conflict: Some(false),
                remote_status: None,
            },
        ),
    ]));
    let active_session_ids = HashSet::from(["session-2".into()]);

    // Act
    state.retain_session_git_statuses(&active_session_ids);

    // Assert
    assert_eq!(state.session_git_statuses.get("session-1"), None);
    assert_eq!(
        state.session_git_statuses.get("session-2"),
        Some(&SessionGitStatus {
            base_status: Some((0, 2)),
            has_merge_conflict: Some(false),
            remote_status: None,
        })
    );
}

#[test]
/// Verifies cached follow-up-task selections are clamped for surviving
/// sessions and dropped for removed or taskless sessions in one refresh
/// pass.
fn retain_follow_up_task_positions_clamps_and_drops_invalid_entries() {
    // Arrange
    let mut surviving_session = state_session_fixture("session-1", Status::Done);
    surviving_session.follow_up_tasks = vec![crate::domain::session::SessionFollowUpTask {
        id: 1,
        launched_session_id: None,
        position: 0,
        text: "Document the behavior.".to_string(),
    }];
    surviving_session
        .follow_up_tasks
        .push(crate::domain::session::SessionFollowUpTask {
            id: 2,
            launched_session_id: None,
            position: 1,
            text: "Add the regression test.".to_string(),
        });
    let taskless_session = state_session_fixture("session-2", Status::Done);
    let mut state = SessionState::new(
        HashMap::new(),
        vec![surviving_session, taskless_session],
        SelectionState::default(),
        Arc::new(FixedClock::new()),
        0,
        0,
    );
    state.follow_up_task_positions.insert("session-1".into(), 9);
    state.follow_up_task_positions.insert("session-2".into(), 4);
    state.follow_up_task_positions.insert("session-3".into(), 2);
    let active_session_ids = HashSet::from(["session-1".into(), "session-2".into()]);

    // Act
    state.retain_follow_up_task_positions(&active_session_ids);

    // Assert
    assert_eq!(state.follow_up_task_positions.get("session-1"), Some(&1));
    assert_eq!(state.follow_up_task_positions.get("session-2"), None);
    assert_eq!(state.follow_up_task_positions.get("session-3"), None);
}
