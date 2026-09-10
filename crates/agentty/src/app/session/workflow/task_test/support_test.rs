use std::sync::{Mutex, Mutex as StdMutex};
use std::time::{Instant, SystemTime};

use ag_agent as agent;

use crate::app::session::Clock;
use crate::db::AppRepositories;
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};

/// Mutable test clock used to drive deterministic status-transition timing
/// assertions.
pub(super) struct StaticClock {
    pub(super) now_system_time: StdMutex<SystemTime>,
}

impl StaticClock {
    /// Creates a test clock seeded with one wall-clock timestamp.
    pub(super) fn new(now_system_time: SystemTime) -> Self {
        Self {
            now_system_time: StdMutex::new(now_system_time),
        }
    }

    /// Replaces the current wall-clock timestamp returned by the clock.
    pub(super) fn set_now_system_time(&self, now_system_time: SystemTime) {
        *self
            .now_system_time
            .lock()
            .expect("static clock lock should not be poisoned") = now_system_time;
    }
}

impl Clock for StaticClock {
    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        *self
            .now_system_time
            .lock()
            .expect("static clock lock should not be poisoned")
    }
}

/// Builds one deterministic one-shot result for app workflow tests.
pub(super) fn one_shot_submission(
    answer: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> agent::OneShotSubmission {
    agent::OneShotSubmission {
        response: ag_protocol::AgentResponse::plain(answer),
        stats: agent::SessionStats {
            added_lines: 0,
            deleted_lines: 0,
            diff_state: agent::SessionDiffState::Unknown,
            input_tokens,
            output_tokens,
        },
    }
}

/// Inserts one review session used by assist-task tests.
pub(super) async fn insert_review_session(database: &AppRepositories, model: &str) {
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session("session-id", model, "main", "Review", project_id)
        .await
        .expect("failed to insert session");
}

/// Supplies user/assistant chat alongside internal context excluded from
/// fallback.
pub(super) fn commit_fallback_transcript() -> Mutex<SessionTranscript> {
    let mut transcript = SessionTranscript::default();
    transcript.append_message(
        SessionMessageKind::UserPrompt,
        "Please recover oversized commits",
    );
    transcript.append_message(
        SessionMessageKind::AssistantAnswer,
        "Implemented commit fallback",
    );
    transcript.append_message(SessionMessageKind::WorkflowNotice, "INTERNAL_NOTICE");
    transcript.append_message(
        SessionMessageKind::AgentPrompt,
        "INTERNAL_NOTICE generated context",
    );

    Mutex::new(transcript)
}
