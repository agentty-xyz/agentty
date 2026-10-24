//! Cross-turn history projection: each turn's first request continues the
//! history the previous turn last sent, and older turns are re-projected
//! only once that history outgrows its limit.

use std::collections::HashMap;
use std::io::Cursor;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tempfile::tempdir;

use super::support::{model, object_schema, response_without_metadata};
use crate::context::ContextBudget;
use crate::context_budget_fixture::unbounded_context_budget;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::model::{MockModel, ModelMessage, ModelRequest, ModelResponse};
use crate::repository::Repository;
use crate::store::MemoryStore;
use crate::tool::{Tool, ToolCall};

/// Turns each scenario runs; enough for history to outgrow its limit
/// several times.
const TURNS: usize = 30;

/// Scripts `turns` turns that each read one new file and then answer, and
/// records every request.
fn scripted_turns(turns: usize) -> (MockModel, Arc<Mutex<Vec<ModelRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let responses = Mutex::new((0..turns).flat_map(|turn| {
        let read = ToolCall::from_json(
            format!("read_{turn}"),
            "read",
            &json!({ "path": format!("notes_{turn}.txt") }).to_string(),
            Some(format!("Turn {turn} needs the notes; I read them next.")),
        )
        .expect("read call");

        [
            ModelResponse::ToolCall(read),
            ModelResponse::Output(json!({ "summary": format!("notes {turn} read") })),
        ]
    }));
    let mut model = model();
    model.expect_complete().returning(move |request| {
        recorded.lock().expect("requests lock").push(request);
        let response = responses
            .lock()
            .expect("responses lock")
            .next()
            .expect("scripted response");

        Ok(response_without_metadata(response))
    });

    (model, requests)
}

/// Serves about one kilobyte of notes for every file.
fn notes_file_system() -> MockFileSystem {
    let content: Vec<u8> = (0..40)
        .flat_map(|line| format!("line {line} of these notes\n").into_bytes())
        .collect();
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .returning(|path| Ok(path.to_path_buf()));
    file_system
        .expect_open_beneath()
        .returning(move |_, _| Ok(Box::new(Cursor::new(content.clone()))));

    file_system
}

/// Runs the scripted turns in one session, resuming it before every turn
/// as a host process would.
async fn run_turns(harness: Harness) {
    harness
        .session("session-a", object_schema())
        .create()
        .await
        .expect("session is created");
    for turn in 0..TURNS {
        let mut session = harness.resume("session-a").await.expect("session resumes");
        session
            .send(format!("read notes_{turn}.txt"))
            .await
            .expect("turn succeeds");
    }
}

/// Returns the indexes of the requests that did not keep every message the
/// request before them sent.
fn rewriting_requests(requests: &[ModelRequest]) -> Vec<usize> {
    (1..requests.len())
        .filter(|&index| {
            let previous = requests[index - 1].messages();

            requests[index].messages().get(..previous.len()) != Some(previous)
        })
        .collect()
}

/// Asserts that every tool result was sent whole and then in at most one
/// other form, so no message was rewritten twice.
fn assert_results_rewritten_at_most_once(requests: &[ModelRequest]) {
    let mut forms: HashMap<&str, Vec<&str>> = HashMap::new();
    for message in requests.iter().flat_map(ModelRequest::messages) {
        if let ModelMessage::ToolResult {
            call_id, content, ..
        } = message
        {
            let seen = forms.entry(call_id.as_str()).or_default();
            if !seen.contains(&content.as_str()) {
                seen.push(content.as_str());
            }
        }
    }
    assert_eq!(forms.len(), TURNS);
    assert!(forms.values().all(|seen| seen.len() <= 2), "{forms:?}");
}

/// Asserts that each turn's first request replays the previous turn's
/// result whole.
fn assert_previous_turn_replayed_whole(requests: &[ModelRequest]) {
    for turn in 1..TURNS {
        let previous_result = requests[2 * turn - 1]
            .messages()
            .last()
            .expect("previous result");
        assert!(
            requests[2 * turn].messages().contains(previous_result),
            "turn {turn} shortened the previous turn"
        );
    }
}

#[tokio::test]
async fn turns_append_to_the_history_earlier_turns_sent_within_the_byte_budget() {
    // Arrange
    let (model, requests) = scripted_turns(TURNS);
    let directory = tempdir().expect("temporary directory");
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(notes_file_system())
        .database(directory.path().join("harness.db"))
        .max_history_bytes(NonZeroUsize::new(16 * 1024).expect("nonzero"));

    // Act
    run_turns(harness).await;

    // Assert
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 2 * TURNS);
    let rewriting = rewriting_requests(&requests);
    // Only a turn's first request re-projects history, and only rarely.
    assert!(
        rewriting.iter().all(|index| index % 2 == 0),
        "{rewriting:?}"
    );
    assert!(rewriting.len() >= 2, "history never outgrew its budget");
    assert!(rewriting.len() * 6 <= TURNS, "{rewriting:?}");
    assert_results_rewritten_at_most_once(&requests);
    assert_previous_turn_replayed_whole(&requests);
}

#[tokio::test]
async fn turns_append_to_the_history_earlier_turns_sent_within_the_context_budget() {
    // Arrange
    let (model, requests) = scripted_turns(TURNS);
    let harness = Harness::new(
        model,
        ContextBudget::new(NonZeroU64::new(6_000).expect("nonzero")),
    )
    .repository(Repository::fixture("repo"))
    .allow(Tool::Read)
    .file_system(notes_file_system())
    .store(Arc::new(MemoryStore::new()));

    // Act
    run_turns(harness).await;

    // Assert
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 2 * TURNS);
    let rewriting = rewriting_requests(&requests);
    assert!(rewriting.len() >= 2, "history never outgrew its budget");
    assert!(rewriting.len() * 6 <= 2 * TURNS, "{rewriting:?}");
    assert_results_rewritten_at_most_once(&requests);
    assert_previous_turn_replayed_whole(&requests);
}
