//! Session transcripts and forks: typed records of settled turns for every
//! range, through SQLite resume, and sessions forked at a settled turn that
//! continue independently under their own prompt-cache key.

use std::error::Error;
use std::future::IntoFuture;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use ag_harness::bash::{CommandCleanupScope, CommandIntent, CommandOutcome, CommandTermination};
use ag_harness::event::{SessionId, TurnIndex};
use ag_harness::model::{
    CompletionMetadata, CompletionUsage, ModelCompletion, ModelMessage, ModelRequest, ModelResponse,
};
use ag_harness::recovery::ExecutionIdentity;
use ag_harness::store::{
    AcquiredTurn, HistoryProjection, MemoryStore, NewSession, SessionCheckpoint, SessionStore,
    SqliteStore,
};
use ag_harness::tool::ToolCall;
use ag_harness::transcript::{TranscriptInput, TranscriptItem, TranscriptToolCall, TurnStatus};
use ag_harness::{
    ErrorKind, Harness, Model, ModelError, OutputMode, OutputSchema, RunLimits, RunOutput,
    RunRequest, Session, SessionError, TurnInput, TurnRange, TurnRecord,
};
use async_trait::async_trait;
use serde_json::json;
use tempfile::tempdir;
use tokio::sync::Notify;

use crate::context_budget_fixture::unbounded_context_budget;
use crate::store_fixture::{ExternalStore, options, schema};

/// Assistant text sent with every tool-call response.
const CALL_TEXT: &str = "Reading";

/// Answers JSON requests with `{"answer": <prompt>}` and text requests with
/// `plain <prompt>`, reporting 10 input and 5 output tokens per request. A
/// `batch` prompt first calls `read` twice; a `hang` prompt never answers.
#[derive(Clone, Default)]
struct ScriptedModel {
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

fn json_output(answer: &str) -> TranscriptItem {
    TranscriptItem::Output {
        output: RunOutput::Json(json!({"answer": answer})),
    }
}

/// Returns the identifier, name, and result of each tool call of `record`.
fn tool_calls(record: &TurnRecord) -> Vec<(&str, &str, Option<&str>)> {
    record
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ToolCall(TranscriptToolCall {
                call_id,
                name,
                result,
                ..
            }) => Some((call_id.as_str(), name.as_str(), result.as_deref())),
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
        assert_eq!(all[0].error_type, None);
        assert!(all[0].started_at <= all[0].finished_at);
        assert!(all[0].finished_at <= all[3].started_at);
        assert_eq!(all[1].instructions, None);
        assert_eq!(all[1].error_type.as_deref(), Some("RunLimit(ToolCalls)"));
        assert_eq!(
            all[1].items.first(),
            Some(&TranscriptItem::Message {
                text: CALL_TEXT.to_string()
            })
        );
        assert_eq!(
            tool_calls(&all[1]),
            [("call-0", "read", None), ("call-1", "read", None)],
            "the refused batch is recorded unanswered"
        );
        assert_eq!(all[2].input, text("hang"));
        assert_eq!(all[2].items, Vec::new());
        assert!(all[2].error_type.is_some());
        assert_eq!(
            all[3].items,
            [TranscriptItem::Output {
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
        calls.iter().all(|(_, _, result)| result.is_some()),
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
    }

    Ok(())
}

#[tokio::test]
async fn stores_fork_settled_turns_with_their_records_and_covered_checkpoint()
-> Result<(), Box<dyn Error>> {
    for store in journaled_stores().await? {
        // Arrange
        store
            .create_session(&NewSession::new("source", schema()), None, 4096)
            .await?;
        complete_turn(&store, "zero", true).await?;
        complete_turn(&store, "one", false).await?;
        let checkpoint = SessionCheckpoint::new(
            1,
            0,
            None,
            None,
            json!({"context": "c", "decisions": [], "state": "s"}),
        )?;
        store.publish_checkpoint("source", &checkpoint).await?;
        let running = AcquiredTurn::begin(
            Arc::clone(&store),
            "source",
            &TurnInput::from("two"),
            &options(),
            0,
        )
        .await?;

        // Act
        store.fork_session("source", 1, "whole").await?;
        store.fork_session("source", 0, "early").await?;
        let running_turn = store.fork_session("source", 2, "late").await;
        let missing = store.fork_session("missing", 0, "other").await;
        let taken = store.fork_session("source", 0, "whole").await;
        let missing_transcript = store.load_transcript("missing", &TurnRange::All).await;

        // Assert
        assert!(matches!(
            running_turn,
            Err(SessionError::TurnNotFound { turn: 2, .. })
        ));
        assert!(matches!(missing, Err(SessionError::NotFound { .. })));
        assert!(matches!(taken, Err(SessionError::AlreadyExists { .. })));
        assert!(matches!(
            missing_transcript,
            Err(SessionError::NotFound { .. })
        ));
        let whole = store.load_session("whole").await?;
        assert_eq!(whole.checkpoint, Some(checkpoint));
        assert_eq!(store.load_session("early").await?.checkpoint, None);
        let writes = store.load_writes("whole").await?;
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, "a.txt");
        let commands = store.load_commands("whole").await?;
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].owner().session_id(), "whole");
        assert_eq!(commands[0].outcome, Some(completed_command()));
        let copied = store.load_transcript("whole", &TurnRange::All).await?;
        assert_eq!(copied.len(), 2);
        assert_eq!(copied[1].messages[0], ModelMessage::User("one".to_string()));
        assert_eq!(
            store.load_transcript("early", &TurnRange::All).await?.len(),
            1
        );
        store.interrupt(running.owner()).await?;
        assert_eq!(
            store
                .load_transcript("source", &TurnRange::All)
                .await?
                .len(),
            3,
            "the source is unchanged"
        );
    }

    Ok(())
}

#[tokio::test]
async fn stores_without_transcript_support_fail_closed() -> Result<(), Box<dyn Error>> {
    // Arrange
    let model = ScriptedModel::default();
    let harness = harness(&model, Arc::new(ExternalStore::new()))?;
    let session = harness
        .session("external", answer_schema()?)
        .create()
        .await?;

    // Act
    let transcript = session.transcript(TurnRange::All).await;
    let fork = harness
        .fork(
            &SessionId::new("external"),
            TurnIndex::new(0),
            SessionId::new("copy"),
        )
        .await;

    // Assert
    assert_eq!(
        transcript.err().map(|error| error.kind()),
        Some(ErrorKind::Store)
    );
    assert_eq!(fork.err().map(|error| error.kind()), Some(ErrorKind::Store));

    Ok(())
}

/// Completes a turn of session `source` for `input`, first recording an
/// applied write and a completed command when `with_effects`.
async fn complete_turn(
    store: &Arc<dyn SessionStore>,
    input: &str,
    with_effects: bool,
) -> Result<(), Box<dyn Error>> {
    let turn = AcquiredTurn::begin(
        Arc::clone(store),
        "source",
        &TurnInput::from(input),
        &options(),
        0,
    )
    .await?;
    if with_effects {
        let write = store
            .write_intent(
                turn.owner(),
                "call",
                Path::new("/repo"),
                "a.txt",
                None,
                b"new",
            )
            .await?;
        store.finish_write(turn.owner(), write, true).await?;
        let intent = CommandIntent {
            call_id: "call".into(),
            command: "true".into(),
            policy: json!({}),
            workspace: "/repo".into(),
        };
        let command = store.command_intent(turn.owner(), &intent).await?;
        store
            .finish_command(turn.owner(), command, &completed_command())
            .await?;
    }
    let output = json!({"answer": input}).to_string();
    store
        .complete_turn(
            turn.owner(),
            &[ModelMessage::Assistant(output)],
            HistoryProjection::default(),
        )
        .await?;

    Ok(())
}

fn completed_command() -> CommandOutcome {
    CommandOutcome {
        cleanup_failed: false,
        cleanup_scope: CommandCleanupScope::PidNamespace,
        execution_failure: None,
        exit_code: Some(0),
        signal: None,
        stderr: String::new(),
        stderr_omitted_bytes: 0,
        stderr_tail: String::new(),
        stdout: String::new(),
        stdout_omitted_bytes: 0,
        stdout_tail: String::new(),
        termination: CommandTermination::Completed,
        truncated: false,
    }
}
