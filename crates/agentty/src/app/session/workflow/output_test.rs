//! Streamed-output accumulation tests covering fragment replacement, final
//! answers, discarded items, bounded history, and per-frame notification.

use ag_contracts::{OutputEvent, OutputKind};
use tokio::sync::mpsc;

use super::{LIVE_OUTPUT_ITEM_LIMIT, LiveOutput};
use crate::app::AppEvent;
use crate::domain::session::{SessionId, Status};
use crate::domain::transient_message::TransientMessageSlot;

/// Live output with a notification channel for one session.
struct Fixture {
    output: LiveOutput,
    receiver: mpsc::UnboundedReceiver<AppEvent>,
    sender: mpsc::UnboundedSender<AppEvent>,
    session_id: SessionId,
}

impl Fixture {
    fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();

        Self {
            output: LiveOutput::default(),
            receiver,
            sender,
            session_id: SessionId::from("session"),
        }
    }

    /// Applies `events` in order.
    fn observe(&self, events: impl IntoIterator<Item = OutputEvent>) {
        for event in events {
            self.output.observe(event, &self.sender, &self.session_id);
        }
    }

    /// Returns how many notifications were queued since the last call.
    fn notifications(&mut self) -> usize {
        std::iter::from_fn(|| self.receiver.try_recv().ok()).count()
    }
}

/// Returns a `Started` event.
fn started(id: u64, kind: OutputKind) -> OutputEvent {
    OutputEvent::Started { id, kind }
}

/// Returns a `Delta` event.
fn delta(id: u64, text: &str) -> OutputEvent {
    OutputEvent::Delta {
        id,
        text: text.to_string(),
    }
}

/// Returns a `Completed` event.
fn completed(id: u64, text: &str) -> OutputEvent {
    OutputEvent::Completed {
        id,
        text: text.to_string(),
    }
}

#[test]
fn fragments_accumulate_and_the_completed_item_replaces_them() {
    // Arrange
    let fixture = Fixture::new();
    fixture.observe([
        started(0, OutputKind::Reasoning),
        delta(0, "Check the\nnotes"),
        started(1, OutputKind::Message),
        delta(1, "Hel"),
        delta(1, "lo"),
        started(2, OutputKind::Message),
    ]);
    let streamed = fixture.output.snapshot();

    // Act
    fixture.observe([
        completed(0, "Checked."),
        completed(1, "Hello, world."),
        delta(1, " late"),
        delta(7, "unknown"),
        completed(7, "unknown"),
    ]);

    // Assert
    assert_eq!(streamed, "> Check the\n> notes\n\nHello");
    assert_eq!(fixture.output.snapshot(), "> Checked.\n\nHello, world.");
}

#[test]
fn tool_calls_show_a_preparation_line_until_they_run() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.observe([started(2, OutputKind::ToolCall)]);
    let preparing = fixture.output.snapshot();
    fixture.observe([started(3, OutputKind::ToolCall), completed(3, "")]);
    let completed_removed = fixture.output.snapshot();
    fixture.notifications();
    // Nothing changed since the snapshot, so this releases the notification.
    fixture.output.refresh();

    // Act
    fixture
        .output
        .settle_tool_calls(&fixture.sender, &fixture.session_id);
    let settled = fixture.notifications();
    fixture
        .output
        .settle_tool_calls(&fixture.sender, &fixture.session_id);

    // Assert
    assert_eq!(preparing, "_Preparing a tool call…_");
    assert_eq!(completed_removed, preparing);
    assert_eq!(settled, 1);
    assert_eq!(fixture.notifications(), 0);
    assert_eq!(fixture.output.snapshot(), "");
}

#[test]
fn final_answer_replaces_the_newest_streamed_message() {
    // Arrange
    let replaced = Fixture::new();
    let appended = Fixture::new();
    replaced.observe([
        started(0, OutputKind::Message),
        delta(0, "{\"answer\""),
        started(1, OutputKind::Message),
        completed(1, "Draft answer"),
    ]);
    appended.observe([started(0, OutputKind::Reasoning), completed(0, "Thought.")]);

    // Act
    replaced.observe([OutputEvent::Answer("Final answer".to_string())]);
    appended.observe([OutputEvent::Answer("Unstreamed answer".to_string())]);

    // Assert
    assert_eq!(replaced.output.snapshot(), "{\"answer\"\n\nFinal answer");
    assert_eq!(
        appended.output.snapshot(),
        "> Thought.\n\nUnstreamed answer"
    );
}

#[test]
fn discarding_open_items_keeps_completed_output() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.observe([
        started(0, OutputKind::Message),
        completed(0, "Kept."),
        started(1, OutputKind::Message),
        delta(1, "Partial"),
        started(2, OutputKind::ToolCall),
        delta(2, "{}"),
    ]);
    fixture.output.snapshot();
    fixture.notifications();
    fixture.output.refresh();

    // Act
    fixture.observe([OutputEvent::DiscardOpen]);
    let discarded = fixture.notifications();
    fixture.output.refresh();
    fixture.output.refresh();
    fixture.observe([OutputEvent::DiscardOpen]);

    // Assert
    assert_eq!(discarded, 1);
    assert_eq!(fixture.notifications(), 0);
    assert_eq!(fixture.output.snapshot(), "Kept.");
}

#[test]
fn history_keeps_the_most_recent_items() {
    // Arrange
    let fixture = Fixture::new();
    let limit = u64::try_from(LIVE_OUTPUT_ITEM_LIMIT).expect("item limit");
    fixture.observe((0..limit).flat_map(|id| {
        [
            started(id, OutputKind::Reasoning),
            completed(id, &format!("r{id}")),
        ]
    }));

    // Act
    fixture.observe([started(limit, OutputKind::Message), completed(limit, "m")]);
    fixture.observe([
        started(limit + 1, OutputKind::Reasoning),
        completed(limit + 1, "last"),
        OutputEvent::Answer("answer".to_string()),
    ]);

    // Assert
    let snapshot = fixture.output.snapshot();
    assert!(!snapshot.contains("> r0\n"), "{snapshot}");
    assert!(!snapshot.contains("> r1\n"), "{snapshot}");
    assert!(!snapshot.contains("> r2\n"), "{snapshot}");
    assert!(snapshot.starts_with("> r3\n"), "{snapshot}");
    assert!(snapshot.ends_with("m\n\n> last\n\nanswer"), "{snapshot}");
}

#[test]
fn foreground_keeps_one_notification_armed_while_output_changes() {
    // Arrange
    let mut fixture = Fixture::new();
    let streamed_before = fixture.output.has_streamed();

    // Act
    fixture.observe([started(0, OutputKind::Message), delta(0, "a")]);
    let first = fixture.notifications();
    let snapshot = fixture.output.snapshot();
    fixture.observe([delta(0, "b")]);
    let while_armed = fixture.notifications();
    let refreshed = fixture.output.refresh();
    let unchanged = fixture.output.refresh();
    fixture.observe([delta(0, "c")]);
    let after_release = fixture.notifications();

    // Assert
    assert!(!streamed_before);
    assert!(fixture.output.has_streamed());
    assert_eq!(first, 1);
    assert_eq!(snapshot, "a");
    assert_eq!(while_armed, 0);
    assert_eq!(refreshed.as_deref(), Some("ab"));
    assert_eq!(unchanged, None);
    assert_eq!(after_release, 1);
    assert_eq!(fixture.output, fixture.output.clone());
    assert_ne!(fixture.output, LiveOutput::default());
}

/// Returns the streamed-output message body of the app's only session.
fn shown_output(app: &crate::app::App) -> Option<String> {
    app.sessions.sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::StreamedOutput)
        .map(|message| message.body.text().to_string())
}

#[tokio::test]
async fn foreground_shows_streamed_output_once_per_frame_below_activity_rows() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session")
            .status(Status::InProgress)
            .build(),
    );
    let fixture = Fixture::new();
    fixture.observe([started(0, OutputKind::Message), delta(0, "Hello")]);

    // Act
    app.apply_app_events(AppEvent::SessionOutputReady {
        output: fixture.output.clone(),
        session_id: fixture.session_id.clone(),
    })
    .await;
    let first = shown_output(&app);
    app.sessions
        .update_live_activity("session", Some("Tools: read ×1".to_string()));
    fixture.observe([delta(0, " world")]);
    app.clear_redraw();
    app.process_pending_app_events().await;
    let refreshed = (shown_output(&app), app.needs_redraw());
    app.clear_redraw();
    app.process_pending_app_events().await;
    let unchanged = app.needs_redraw();
    let slots = app.sessions.sessions()[0]
        .transient_messages
        .messages()
        .iter()
        .map(|message| message.slot)
        .collect::<Vec<_>>();
    app.sessions
        .track_live_output(SessionId::from("missing"), Some(LiveOutput::default()));
    app.apply_app_events(AppEvent::SessionOutputCleared {
        session_id: fixture.session_id.clone(),
    })
    .await;
    let cleared = shown_output(&app);

    // Assert
    assert_eq!(first.as_deref(), Some("Hello"));
    assert_eq!(refreshed, (Some("Hello world".to_string()), true));
    assert!(!unchanged);
    assert_eq!(
        slots,
        [
            TransientMessageSlot::Activity,
            TransientMessageSlot::StreamedOutput
        ]
    );
    assert_eq!(cleared, None);
}

#[tokio::test]
async fn empty_streamed_output_shows_nothing() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session")
            .status(Status::InProgress)
            .build(),
    );
    app.sessions
        .update_live_activity("session", Some("Tools: read ×1".to_string()));

    // Act
    app.sessions
        .track_live_output(SessionId::from("session"), Some(LiveOutput::default()));

    // Assert
    assert_eq!(shown_output(&app), None);
}

#[test]
fn notification_names_the_session_and_its_output() {
    // Arrange
    let mut fixture = Fixture::new();

    // Act
    fixture.observe([started(0, OutputKind::Message), delta(0, "Hi")]);

    // Assert
    assert_eq!(
        fixture.receiver.try_recv().ok(),
        Some(AppEvent::SessionOutputReady {
            output: fixture.output.clone(),
            session_id: fixture.session_id.clone(),
        })
    );
}
