//! Session transcripts and forks through the harness: typed records of
//! settled turns for every range, through SQLite resume, and sessions forked
//! at a settled turn that continue independently under their own
//! prompt-cache key. Store-level fork and transcript contracts live in the
//! store conformance suite.

use std::error::Error;
use std::future::IntoFuture;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use ag_harness::event::{Item, SessionId, ToolCallItem, ToolCallStatus, TurnIndex};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCapabilities, ModelCompletion, ModelMessage,
    ModelMetadata, ModelRegistry, ModelRequest, ModelResponse,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::{MemoryStore, SessionStore, SqliteStore};
use ag_harness::tool::ToolCall;
use ag_harness::transcript::{TranscriptInput, TurnStatus};
use ag_harness::{
    ErrorKind, Harness, LimitKind, Model, ModelError, OutputMode, OutputSchema, RunLimits,
    RunOutput, RunRequest, Session, TurnRange, TurnRecord,
};
use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::Notify;

use crate::context_budget_fixture::unbounded_context_budget;
use crate::model_profile_fixture::unbounded_model_profile;

/// Assistant text sent with every tool-call response.
const CALL_TEXT: &str = "Reading";

/// Answers JSON requests with `{"answer": <prompt>}` and text requests with
/// `plain <prompt>`, reporting 10 input and 5 output tokens per request. A
/// `batch` prompt first calls `read` twice; a `hang` prompt never answers.
/// A named model reports itself as provider and model `name`.
#[derive(Clone, Default)]
struct ScriptedModel {
    name: Option<&'static str>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
    started: Arc<Notify>,
}

impl ScriptedModel {
    fn requests(&self) -> Vec<ModelRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl Model for ScriptedModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        self.name
            .and_then(|name| ModelMetadata::new(name, name).ok())
    }

    fn supports_text_output(&self) -> bool {
        true
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let answered = matches!(
            request.messages().last(),
            Some(ModelMessage::ToolResult { .. })
        );
        let response = match (request.prompt(), request.output()) {
            ("hang", _) => {
                self.started.notify_one();

                return std::future::pending().await;
            }
            ("batch", _) if !answered => ModelResponse::ToolCalls(vec![
                read_call("call-0", true)?,
                read_call("call-1", false)?,
            ]),
            (prompt, OutputMode::Text) => ModelResponse::Text(format!("plain {prompt}")),
            (prompt, _) => ModelResponse::Output(json!({"answer": prompt})),
        };
        let usage = CompletionUsage::new(None, None, Some(10), Some(5), None, None);

        Ok(ModelCompletion::new(
            CompletionMetadata::new("stop".into(), None, None, None, Some(usage)),
            response,
        ))
    }
}

/// Returns `read` call `id`, carrying [`CALL_TEXT`] when `first`.
fn read_call(id: &str, first: bool) -> Result<ToolCall, ModelError> {
    ToolCall::from_json(id.to_string(), "read", r#"{"path":"a.txt"}"#, None)?
        .with_assistant_text(first.then(|| CALL_TEXT.to_string()))
}

/// Returns the two durable backends.
async fn journaled_stores() -> Result<Vec<Arc<dyn SessionStore>>, Box<dyn Error>> {
    Ok(vec![
        Arc::new(MemoryStore::new()),
        Arc::new(SqliteStore::open(Path::new(":memory:")).await?),
    ])
}

fn harness(model: &ScriptedModel, store: Arc<dyn SessionStore>) -> Result<Harness, Box<dyn Error>> {
    Ok(Harness::new(model.clone(), unbounded_context_budget())
        .execution_identity(ExecutionIdentity::new("scripted", "1")?)
        .store(store))
}

fn answer_schema() -> Result<OutputSchema, Box<dyn Error>> {
    Ok(OutputSchema::new(json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"]
    }))?)
}

/// Runs `hang` and cancels it once the model received it, returning the
/// transcript read while it was still running.
async fn cancelled_run(
    session: &Session,
    model: &ScriptedModel,
) -> Result<Vec<TurnRecord>, Box<dyn Error>> {
    let run = session.run(RunRequest::new("hang"));
    let cancel = run.cancel_token();
    let (outcome, transcript) = tokio::join!(run.into_future(), async {
        model.started.notified().await;
        let transcript = session.transcript(TurnRange::All).await;
        cancel.cancel();

        transcript
    });
    assert_eq!(
        outcome.result.err().map(|error| error.kind()),
        Some(ErrorKind::Cancelled)
    );

    Ok(transcript?)
}

fn text(text: &str) -> Vec<TranscriptInput> {
    vec![TranscriptInput::Text {
        text: text.to_string(),
    }]
}

fn json_output(answer: &str) -> Item {
    Item::Output {
        output: RunOutput::Json(json!({"answer": answer})),
    }
}

/// Returns the identifier, name, status, and output of each tool call of
/// `record`.
fn tool_calls(record: &TurnRecord) -> Vec<(&str, &str, ToolCallStatus, &str)> {
    record
        .items
        .iter()
        .filter_map(|item| match item {
            Item::ToolCall(ToolCallItem {
                call_id,
                name,
                output,
                status,
                ..
            }) => Some((call_id.as_str(), name.as_str(), *status, output.as_str())),
            _ => None,
        })
        .collect()
}

fn indices(records: &[TurnRecord]) -> Vec<u64> {
    records.iter().map(|record| record.turn.get()).collect()
}

/// Returns the prompts of the user inputs `request` replays and sends.
fn sent_inputs(request: &ModelRequest) -> Vec<String> {
    request
        .messages()
        .iter()
        .filter_map(|message| match message {
            ModelMessage::User(text) if !text.starts_with("[harness]") => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Returns a registry of the named scripted models `a` and `b`, sharing
/// `model`'s request log.
fn registry(model: &ScriptedModel) -> Result<ModelRegistry, Box<dyn Error>> {
    let mut registry = ModelRegistry::new();
    for name in ["a", "b"] {
        registry.register(
            ExecutionIdentity::new(name, "1")?,
            ScriptedModel {
                name: Some(name),
                ..model.clone()
            },
            ModelCapabilities {
                image_input: false,
                profile: unbounded_model_profile(),
                tool_calls: true,
            },
        )?;
    }

    Ok(registry)
}

#[tokio::test]
async fn transcript_records_completed_failed_and_interrupted_turns() -> Result<(), Box<dyn Error>> {
    for store in journaled_stores().await? {
        // Arrange
        let model = ScriptedModel::default();
        let session = harness(&model, store)?
            .session("transcript", answer_schema()?)
            .create()
            .await?;
        let first = session
            .run(RunRequest::new("first").instructions("be brief"))
            .await;
        assert!(first.result.is_ok());
        let limits = RunLimits::default().with_max_tool_calls(NonZeroU32::new(1).ok_or("one")?);
        let limited = session.run(RunRequest::new("batch").limits(limits)).await;
        assert!(limited.result.is_err());
        let while_running = cancelled_run(&session, &model).await?;
        let plain = session
            .run(RunRequest::new("last").output(OutputMode::Text))
            .await;
        assert!(plain.result.is_ok());

        // Act
        let all = session.transcript(TurnRange::All).await?;
        let last = session.transcript(TurnRange::Last(2)).await?;
        let span = session
            .transcript(TurnRange::Span(TurnIndex::new(1)..TurnIndex::new(3)))
            .await?;
        let beyond = session
            .transcript(TurnRange::Span(TurnIndex::new(9)..TurnIndex::new(12)))
            .await?;

        // Assert
        assert_eq!(
            indices(&while_running),
            [0, 1],
            "a running turn is left out"
        );
        assert_eq!(indices(&all), [0, 1, 2, 3]);
        assert_eq!(indices(&last), [2, 3]);
        assert_eq!(indices(&span), [1, 2]);
        assert_eq!(indices(&beyond), Vec::<u64>::new());
        let statuses: Vec<_> = all.iter().map(|record| record.status).collect();
        assert_eq!(
            statuses,
            [
                TurnStatus::Completed,
                TurnStatus::Failed,
                TurnStatus::Interrupted,
                TurnStatus::Completed,
            ]
        );
        assert_eq!(all[0].input, text("first"));
        assert_eq!(all[0].instructions.as_deref(), Some("be brief"));
        assert_eq!(all[0].items, [json_output("first")]);
        assert_eq!(all[0].error, None);
        assert!(all[0].started_at <= all[0].finished_at);
        assert!(all[0].finished_at <= all[3].started_at);
        assert_eq!(all[1].instructions, None);
        assert_eq!(
            all[1].error.map(|error| error.kind),
            Some(ErrorKind::BudgetExhausted(LimitKind::ToolCalls)),
            "a limit stop reads as the live run reported it"
        );
        assert_eq!(
            all[1].items.first(),
            Some(&Item::Message {
                text: CALL_TEXT.to_string()
            })
        );
        assert_eq!(
            tool_calls(&all[1]),
            [
                ("call-0", "read", ToolCallStatus::Unanswered, ""),
                ("call-1", "read", ToolCallStatus::Unanswered, ""),
            ],
            "the refused batch is recorded unanswered"
        );
        assert_eq!(all[2].input, text("hang"));
        assert_eq!(all[2].items, Vec::new());
        assert_eq!(
            all[2].error.map(|error| error.kind),
            Some(ErrorKind::Cancelled)
        );
        assert_eq!(
            all[3].items,
            [Item::Output {
                output: RunOutput::Text("plain last".to_string())
            }]
        );
        assert_eq!(all[3].usage, None, "only host-request turns keep usage");
    }

    Ok(())
}

#[tokio::test]
async fn transcript_survives_sqlite_resume_with_answered_calls_and_usage()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempdir()?;
    let database = directory.path().join("sessions.sqlite");
    let model = ScriptedModel::default();
    let build = || -> Result<Harness, Box<dyn Error>> {
        Ok(Harness::new(model.clone(), unbounded_context_budget())
            .execution_identity(ExecutionIdentity::new("scripted", "1")?)
            .database(&database))
    };
    let mut session = build()?
        .session("durable", answer_schema()?)
        .create()
        .await?;
    session.turn("batch").host_id("request").await?;
    let before = session.transcript(TurnRange::All).await?;
    drop(session);

    // Act
    let resumed = build()?.resume("durable").await?;
    let after = resumed.transcript(TurnRange::All).await?;

    // Assert
    assert_eq!(after, before);
    assert_eq!(after.len(), 1);
    let usage = after[0]
        .usage
        .ok_or("a host-request turn keeps its usage")?;
    assert_eq!((usage.input, usage.output), (20, 10));
    let calls = tool_calls(&after[0]);
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|(_, _, status, output)| *status == ToolCallStatus::Answered
                && !output.is_empty()),
        "both calls keep their results"
    );
    assert_eq!(after[0].items.last(), Some(&json_output("batch")));

    Ok(())
}

#[tokio::test]
async fn forked_sessions_continue_independently_under_their_own_cache_keys()
-> Result<(), Box<dyn Error>> {
    for store in journaled_stores().await? {
        // Arrange
        let model = ScriptedModel::default();
        let harness = harness(&model, store)?;
        let source = harness
            .session("source", answer_schema()?)
            .system_prompt("policy")
            .create()
            .await?;
        for prompt in ["one", "two", "three"] {
            assert!(source.run(RunRequest::new(prompt)).await.result.is_ok());
        }
        let source_id = SessionId::new("source");

        // Act
        let fork = harness
            .fork(&source_id, TurnIndex::new(1), SessionId::new("fork"))
            .await?;
        let fork_outcome = fork.run(RunRequest::new("four")).await;
        let source_outcome = source.run(RunRequest::new("five")).await;

        // Assert
        assert_eq!(
            fork_outcome.result?,
            RunOutput::Json(json!({"answer": "four"})),
            "the fork keeps the source's output mode"
        );
        assert!(source_outcome.result.is_ok());
        let requests = model.requests();
        let [.., fork_request, source_request] = requests.as_slice() else {
            return Err("both sessions must send a request".into());
        };
        assert_eq!(sent_inputs(fork_request), ["one", "two", "four"]);
        assert_eq!(sent_inputs(source_request), ["one", "two", "three", "five"]);
        assert!(matches!(
            fork_request.messages().first(),
            Some(ModelMessage::System(prompt)) if prompt == "policy"
        ));
        assert_ne!(fork_request.cache_key(), source_request.cache_key());
        assert_eq!(
            source_request.cache_key(),
            requests[0].cache_key(),
            "the source keeps its cache key"
        );
        let fork_inputs: Vec<_> = fork
            .transcript(TurnRange::All)
            .await?
            .into_iter()
            .map(|record| record.input)
            .collect();
        assert_eq!(fork_inputs, [text("one"), text("two"), text("four")]);
        assert_eq!(source.transcript(TurnRange::All).await?.len(), 4);
    }

    Ok(())
}

#[tokio::test]
async fn forks_replay_host_requests_the_source_recorded() -> Result<(), Box<dyn Error>> {
    for store in journaled_stores().await? {
        // Arrange
        let model = ScriptedModel::default();
        let harness = harness(&model, store)?;
        let mut source = harness.session("source", answer_schema()?).create().await?;
        let recorded = source.turn("one").host_id("request").await?;
        let mut fork = harness
            .fork(
                &SessionId::new("source"),
                TurnIndex::new(0),
                SessionId::new("fork"),
            )
            .await?;
        let sent = model.requests().len();

        // Act
        let replayed = fork.turn("one").host_id("request").await?;
        let fresh = fork.turn("two").host_id("fresh").await?;

        // Assert
        assert_eq!(replayed.output(), recorded.output());
        assert_eq!(fresh.output(), &json!({"answer": "two"}));
        assert_eq!(
            model.requests().len(),
            sent + 1,
            "only the new host request reaches the model"
        );
        assert_eq!(indices(&fork.transcript(TurnRange::All).await?), [0, 1]);
    }

    Ok(())
}

#[tokio::test]
async fn forks_keep_the_switched_model_and_reject_another() -> Result<(), Box<dyn Error>> {
    for store in journaled_stores().await? {
        // Arrange
        let model = ScriptedModel::default();
        let registry = registry(&model)?;
        let on_a = Harness::from_registry(&registry, "a")?.store(Arc::clone(&store));
        let on_b = Harness::from_registry(&registry, "b")?.store(Arc::clone(&store));
        let mut source = on_a.session("source", answer_schema()?).create().await?;
        assert!(source.run(RunRequest::new("one")).await.result.is_ok());
        source.switch_model(&registry, "b").await?;
        let source_id = SessionId::new("source");

        // Act
        let mismatched = on_a
            .fork(&source_id, TurnIndex::new(0), SessionId::new("on-a"))
            .await;
        let fork = on_b
            .fork(&source_id, TurnIndex::new(0), SessionId::new("on-b"))
            .await?;
        let outcome = fork.run(RunRequest::new("two")).await;

        // Assert
        assert_eq!(
            mismatched.err().map(|error| error.kind()),
            Some(ErrorKind::Config)
        );
        assert!(
            on_b.resume("on-a").await.is_ok(),
            "a fork whose resume failed stays stored"
        );
        assert!(outcome.result.is_ok());
        let requests = model.requests();
        let last = requests.last().ok_or("the fork sends a request")?;
        assert_eq!(last.prompt(), "two");
        let source_record = store.load_session("source").await?;
        let fork_record = store.load_session("on-b").await?;
        assert_eq!(fork_record.recorded_model(), source_record.recorded_model());
        assert_eq!(
            fork_record
                .registration_identity
                .as_ref()
                .map(ExecutionIdentity::key),
            Some("b")
        );
        assert_eq!(fork_record.model.as_deref(), Some("b"));
    }

    Ok(())
}

#[tokio::test]
async fn fork_rejects_missing_sources_turns_and_existing_or_blank_targets()
-> Result<(), Box<dyn Error>> {
    for store in journaled_stores().await? {
        // Arrange
        let model = ScriptedModel::default();
        let harness = harness(&model, store)?;
        let source = harness.session("source", answer_schema()?).create().await?;
        assert!(source.run(RunRequest::new("one")).await.result.is_ok());
        harness.session("taken", answer_schema()?).create().await?;
        let fork = |from: &str, at: u64, new: &str| {
            let harness = harness.clone();
            let (from, new) = (SessionId::new(from), SessionId::new(new));

            async move {
                harness
                    .fork(&from, TurnIndex::new(at), new)
                    .await
                    .err()
                    .map(|error| error.kind())
            }
        };

        // Act
        let missing_source = fork("missing", 0, "copy").await;
        let missing_turn = fork("source", 1, "copy").await;
        let out_of_range = fork("source", u64::MAX, "copy").await;
        let taken = fork("source", 0, "taken").await;
        let blank = fork("source", 0, " ").await;

        // Assert
        assert_eq!(missing_source, Some(ErrorKind::NotFound));
        assert_eq!(missing_turn, Some(ErrorKind::NotFound));
        assert_eq!(out_of_range, Some(ErrorKind::NotFound));
        assert_eq!(taken, Some(ErrorKind::Conflict));
        assert_eq!(blank, Some(ErrorKind::Config));
        assert_eq!(source.transcript(TurnRange::All).await?.len(), 1);
        assert!(
            harness.resume("copy").await.is_err(),
            "no rejected fork is created"
        );
    }

    Ok(())
}
