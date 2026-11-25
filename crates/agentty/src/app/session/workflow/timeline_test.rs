//! Live-timeline mailbox tests covering per-frame notification, clearing,
//! and how the foreground shows and removes the timeline.

use ag_contracts::{OutputEvent, OutputKind};
use tokio::sync::mpsc;

use super::LiveTurn;
use crate::app::AppEvent;
use crate::domain::session::{SessionId, Status};
use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};
use crate::test_support::activity_event;

/// A live timeline with a notification channel for one session.
struct Fixture {
    receiver: mpsc::UnboundedReceiver<AppEvent>,
    sender: mpsc::UnboundedSender<AppEvent>,
    session_id: SessionId,
    turn: LiveTurn,
}

impl Fixture {
    fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();

        Self {
            receiver,
            sender,
            session_id: SessionId::from("session"),
            turn: LiveTurn::default(),
        }
    }

    /// Streams the start of message item `id`.
    fn start_message(&self, id: u64) {
        self.turn.observe_output(
            OutputEvent::Started {
                id,
                kind: OutputKind::Message,
            },
            0,
            &self.sender,
            &self.session_id,
        );
    }

    /// Returns the notifications queued since the last call.
    fn notifications(&mut self) -> Vec<AppEvent> {
        std::iter::from_fn(|| self.receiver.try_recv().ok()).collect()
    }
}

#[test]
fn one_notification_stays_armed_until_a_frame_finds_nothing_new() {
    // Arrange
    let mut fixture = Fixture::new();

    // Act
    fixture.turn.observe_progress(
        "Waiting for model".into(),
        0,
        &fixture.sender,
        &fixture.session_id,
    );
    let before_entries = fixture.notifications();
    fixture.start_message(0);
    fixture.start_message(1);
    let first = fixture.notifications();
    let snapshot = fixture.turn.snapshot();
    fixture.turn.observe_activity(
        activity_event("call"),
        0,
        &fixture.sender,
        &fixture.session_id,
    );
    fixture.turn.observe_progress(
        "Waiting again".into(),
        0,
        &fixture.sender,
        &fixture.session_id,
    );
    fixture
        .turn
        .interrupt_running(0, &fixture.sender, &fixture.session_id);
    let while_armed = fixture.notifications();
    let refreshed = fixture.turn.refresh();
    let unchanged = fixture.turn.refresh();
    fixture.start_message(2);
    let after_release = fixture.notifications();

    // Assert
    assert_eq!(before_entries, []);
    assert_eq!(
        first,
        [AppEvent::SessionTurnReady {
            session_id: fixture.session_id.clone(),
            turn: fixture.turn.clone(),
        }]
    );
    assert_eq!(snapshot.step_count, 2);
    assert_eq!(while_armed, []);
    assert_eq!(
        refreshed.map(|snapshot| (
            snapshot
                .steps
                .iter()
                .map(|step| step.rows.len())
                .sum::<usize>(),
            snapshot
                .current
                .as_ref()
                .map(|current| current.text.clone())
        )),
        Some((1, Some("Waiting again".to_string())))
    );
    assert_eq!(unchanged, None);
    assert_eq!(after_release.len(), 1);
    assert_ne!(fixture.turn, LiveTurn::default());
}

#[test]
fn calls_past_the_row_limit_change_nothing() {
    // Arrange
    let fixture = Fixture::new();
    for index in 0..2_048 {
        fixture.turn.observe_activity(
            activity_event(&format!("call-{index}")),
            0,
            &fixture.sender,
            &fixture.session_id,
        );
    }
    fixture.turn.snapshot();

    // Act
    fixture.turn.observe_activity(
        activity_event("one-more"),
        0,
        &fixture.sender,
        &fixture.session_id,
    );

    // Assert
    assert_eq!(fixture.turn.refresh(), None);
}

#[test]
fn clearing_sends_one_notice_only_after_the_timeline_showed_something() {
    // Arrange
    let mut fixture = Fixture::new();
    let empty = LiveTurn::default();

    // Act
    empty.clear(&fixture.sender, &fixture.session_id);
    let unshown = fixture.notifications();
    let empty_action = empty.current_action_text();
    let empty_snapshot = empty.snapshot();
    fixture.start_message(0);
    fixture.notifications();
    fixture.turn.clear(&fixture.sender, &fixture.session_id);
    fixture.turn.clear(&fixture.sender, &fixture.session_id);
    let cleared = fixture.notifications();

    // Assert
    assert_eq!(unshown, []);
    assert_eq!(empty_action, None);
    assert_eq!(empty_snapshot.step_count, 0);
    assert_eq!(
        cleared,
        [AppEvent::SessionTurnCleared {
            session_id: fixture.session_id.clone(),
        }]
    );
}

/// Returns whether the app's only session shows a live timeline.
fn shown_steps(app: &crate::app::App) -> Option<usize> {
    app.sessions.sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::Turn)
        .and_then(|message| match &message.body {
            TransientMessageBody::Timeline(snapshot) => Some(snapshot.step_count),
            _ => None,
        })
}

#[tokio::test]
async fn foreground_shows_the_timeline_once_per_frame_and_removes_it() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session")
            .status(Status::InProgress)
            .build(),
    );
    let fixture = Fixture::new();
    fixture.start_message(0);

    // Act
    app.apply_app_events(AppEvent::SessionTurnReady {
        session_id: fixture.session_id.clone(),
        turn: fixture.turn.clone(),
    })
    .await;
    let first = shown_steps(&app);
    let live_timeline = app.sessions.sessions()[0].live_timeline().is_some();
    fixture.start_message(1);
    app.clear_redraw();
    app.process_pending_app_events().await;
    let refreshed = (shown_steps(&app), app.needs_redraw());
    app.clear_redraw();
    app.process_pending_app_events().await;
    let unchanged = app.needs_redraw();
    app.apply_app_events(AppEvent::SessionTurnCleared {
        session_id: fixture.session_id.clone(),
    })
    .await;
    let cleared = shown_steps(&app);

    // Assert
    assert_eq!(first, Some(1));
    assert!(live_timeline);
    assert_eq!(refreshed, (Some(2), true));
    assert!(!unchanged);
    assert_eq!(cleared, None);
}

#[test]
fn empty_or_unknown_session_timelines_show_nothing() {
    // Arrange
    let mut manager = crate::test_support::session_manager_with_sessions(vec![
        crate::test_support::SessionFixtureBuilder::new()
            .id("session")
            .status(Status::InProgress)
            .build(),
    ]);

    // Act
    manager.track_live_turn(SessionId::from("session"), Some(LiveTurn::default()));
    manager.track_live_turn(SessionId::from("missing"), Some(LiveTurn::default()));

    // Assert
    assert_eq!(
        manager.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::Turn),
        None
    );
}
