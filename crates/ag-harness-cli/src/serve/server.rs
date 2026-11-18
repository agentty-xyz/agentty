//! Request dispatch, open sessions, and runs in flight of `ag-harness serve`.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::future::poll_fn;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use std::{env, io};

use ag_harness::catalog::{ModelCatalog, ModelId, ModelSelector, Role};
use ag_harness::event::{RunEvent, RunId};
use ag_harness::model::ReasoningEffort;
use ag_harness::provider::ModelProvider;
use ag_harness::store::{MemoryStore, SessionInfo};
use ag_harness::{
    CancelToken, ErrorKind, EventStream, Harness, HarnessBuilder, HarnessError, Repository,
    RunHandle, RunRequest, Session, SessionError, ToolSet,
};
use futures_core::Stream;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::approval::Approvals;
use super::protocol::{
    Answer, Call, Empty, Envelope, ErrorData, ErrorObject, ErrorResponse, INVALID_PARAMS,
    MethodResult, ModelChoice, Notification, PARSE_ERROR, RequestId, Response, RunStartParams,
    RunStarted, SERVER_ERROR, SessionCreateParams, SessionOpened, SessionResumeParams,
    SessionTranscriptParams, SessionUsage, StoreChoice, Transcript, line,
};
use crate::{connect_model, login, select_cataloged, select_direct, stored_model_identity_parts};

/// Event lines runs may queue before the next one waits for the server to
/// write them.
const EVENT_LINE_BUFFER: usize = 64;

/// Serves the protocol on `input` and `output` until `shutdown` or the end
/// of `input`, then cancels every run in flight and writes its remaining
/// events; a `shutdown` request is answered last.
///
/// Requests are answered in order, and a run's identifier is answered
/// before its first event. Client notifications are executed but never
/// answered. Runs execute concurrently; their events, and the
/// `approval/decide` requests that follow their `approval_requested`
/// events, are written between responses, one whole line at a time.
///
/// # Errors
/// Returns a failure to read `input` or to write to `output`.
pub(crate) async fn serve<Input, Output>(
    mut input: Input,
    output: Output,
    environment: impl FnMut(&str) -> Result<String, env::VarError>,
    settings: Settings,
) -> io::Result<()>
where
    Input: AsyncBufRead + Unpin,
    Output: AsyncWrite + Unpin,
{
    let (event_sender, event_lines) = mpsc::channel(EVENT_LINE_BUFFER);
    let mut server = Server {
        approvals: Approvals::default(),
        environment,
        event_lines,
        event_sender,
        finished_runs: HashSet::new(),
        memory: MemoryStore::new(),
        output,
        runs: HashMap::new(),
        sessions: HashMap::new(),
        settings,
    };
    let answered = server.serve_lines(&mut input).await;
    let (mut output, stopped) = server.stop().await;

    match (answered, stopped) {
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        (Ok(Some(id)), Ok(())) => {
            let response = line(&Response::new(id, MethodResult::Empty(Empty {})))?;

            write_line(&mut output, &response).await
        }
        (Ok(None), Ok(())) => Ok(()),
    }
}

/// Host settings shared by every session of one server.
pub(crate) struct Settings {
    /// Time the client has to answer each `approval/decide` request.
    pub(crate) approval_timeout: Duration,
    pub(crate) reasoning_effort: ReasoningEffort,
    /// Repository the built-in tools work in and the tools every run
    /// offers; without it, runs offer no tools.
    pub(crate) tools: Option<(Repository, ToolSet)>,
}

/// Writes one complete line and flushes it.
async fn write_line(output: &mut (impl AsyncWrite + Unpin), line: &str) -> io::Result<()> {
    output.write_all(line.as_bytes()).await?;

    output.flush().await
}

/// Returns the next event of `events`, or `None` once the run ended.
async fn next_event(events: &mut EventStream) -> Option<RunEvent> {
    poll_fn(|context| Pin::new(&mut *events).poll_next(context)).await
}

/// Encodes `event` of a run in `session` as a `run/event` notification
/// line. An event time the wire cannot carry, such as one before the Unix
/// epoch after a clock step, is sent as the epoch, so no event, and in
/// particular no `completed` event, is ever dropped.
fn event_line(session: String, event: RunEvent) -> String {
    let mut notification = Notification::run_event(session, event);
    if let Ok(line) = line(&notification) {
        return line;
    }
    notification.params.event.at = UNIX_EPOCH;

    // The event time is the only part of a notification that can fail to
    // encode.
    line(&notification).unwrap_or_default()
}

/// State of one `serve` process.
struct Server<Environment, Output> {
    /// Approval handler of every session, which asks this server's client.
    approvals: Approvals,
    environment: Environment,
    /// Serialized events and approval requests of every run, written in
    /// arrival order.
    event_lines: mpsc::Receiver<String>,
    /// Sender cloned into each run.
    event_sender: mpsc::Sender<String>,
    /// Runs this server started that finished and left `runs`, so
    /// cancelling them still succeeds.
    finished_runs: HashSet<RunId>,
    /// Store of every session opened with [`StoreChoice::Memory`].
    memory: MemoryStore,
    output: Output,
    runs: HashMap<RunId, RunTask>,
    sessions: HashMap<String, Session>,
    settings: Settings,
}

impl<Environment, Output> Server<Environment, Output>
where
    Environment: FnMut(&str) -> Result<String, env::VarError>,
    Output: AsyncWrite + Unpin,
{
    /// Answers each line of `input`, writing run events as they arrive,
    /// until the end of input or a `shutdown`, whose identifier is returned
    /// unanswered; a `shutdown` notification returns none, as the end of
    /// input does.
    async fn serve_lines(
        &mut self,
        input: &mut (impl AsyncBufRead + Unpin),
    ) -> io::Result<Option<RequestId>> {
        // Bytes a cancelled read already consumed stay here until the next
        // read completes the line.
        let mut line = Vec::new();
        loop {
            tokio::select! {
                biased;
                Some(event) = self.event_lines.recv() => write_line(&mut self.output, &event).await?,
                read = input.read_until(b'\n', &mut line) => {
                    if read? == 0 {
                        return Ok(None);
                    }
                    let next = self.answer(&line).await?;
                    line.clear();
                    if let ControlFlow::Break(shutdown) = next {
                        return Ok(shutdown);
                    }
                }
            }
        }
    }

    /// Answers one input line; breaks with the identifier of a `shutdown`
    /// request instead of answering it.
    async fn answer(&mut self, line: &[u8]) -> io::Result<ControlFlow<Option<RequestId>>> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Ok(ControlFlow::Continue(()));
        }
        let message = match serde_json::from_slice::<Value>(line) {
            Ok(message) => message,
            Err(error) => {
                let error = ErrorObject::new(PARSE_ERROR, format!("parse error: {error}"));
                self.write(&ErrorResponse::new(None, error)).await?;

                return Ok(ControlFlow::Continue(()));
            }
        };
        if let Some(answer) = Answer::parse(&message) {
            return self.decide(answer).await.map(ControlFlow::Continue);
        }
        let envelope = match Envelope::parse(message) {
            Ok(envelope) => envelope,
            Err(response) => {
                self.write(&response).await?;

                return Ok(ControlFlow::Continue(()));
            }
        };
        let id = envelope.id;
        let reply = match Call::parse(&envelope.method, envelope.params) {
            Ok(call) => self.dispatch(call).await.map_err(ServeError::into_object),
            Err(error) => Err(error),
        };
        match reply {
            Ok(Reply::Shutdown) => return Ok(ControlFlow::Break(id)),
            Ok(Reply::Result(result)) => self.respond(id, Ok(result)).await?,
            Ok(Reply::Run(run)) => {
                let started = RunStarted {
                    run: run.events.run_id().clone(),
                };
                self.respond(id, Ok(MethodResult::RunStarted(started)))
                    .await?;
                self.spawn(*run);
            }
            Err(error) => self.respond(id, Err(error)).await?,
        }

        Ok(ControlFlow::Continue(()))
    }

    /// Hands the client's `answer` to the run waiting for it. An answer
    /// whose `result` is not a decision is rejected with an error that has
    /// no `id`, and the run keeps waiting for a valid one.
    async fn decide(&mut self, answer: Answer) -> io::Result<()> {
        match answer.decision {
            Ok(decision) => {
                self.approvals.answer(&answer.id, decision);

                Ok(())
            }
            Err(reason) => {
                let error = ErrorObject::new(
                    INVALID_PARAMS,
                    format!("invalid answer to server request {}: {reason}", answer.id),
                );

                self.write(&ErrorResponse::new(None, error)).await
            }
        }
    }

    /// Answers request `id` with `reply`; a client notification, which has
    /// no `id`, is never answered, not even with an error.
    async fn respond(
        &mut self,
        id: Option<RequestId>,
        reply: Result<MethodResult, ErrorObject>,
    ) -> io::Result<()> {
        let Some(id) = id else {
            return Ok(());
        };

        match reply {
            Ok(result) => self.write(&Response::new(id, result)).await,
            Err(error) => self.write(&ErrorResponse::new(Some(id), error)).await,
        }
    }

    /// Executes one request.
    async fn dispatch(&mut self, call: Call) -> Result<Reply, ServeError> {
        let result = match call {
            Call::SessionCreate(params) => self.create(params).await?,
            Call::SessionResume(params) => self.resume(params).await?,
            Call::RunStart(params) => {
                return self.start(params).map(|run| Reply::Run(Box::new(run)));
            }
            Call::RunCancel(params) => self.cancel(&params.run)?,
            Call::SessionTranscript(params) => self.transcript(params).await?,
            Call::SessionUsage(params) => {
                MethodResult::Usage(SessionUsage(self.session(&params.session)?.usage().await?))
            }
            Call::Shutdown(_) => return Ok(Reply::Shutdown),
        };

        Ok(Reply::Result(result))
    }

    async fn create(&mut self, params: SessionCreateParams) -> Result<MethodResult, ServeError> {
        let id = params
            .session
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        self.ensure_closed(&id)?;
        let output = params.output.into_mode().map_err(ServeError::config)?;
        let harness = self.harness(&params.model, params.store)?;
        let mut builder = harness.session(id.clone(), output);
        if let Some(system_prompt) = params.system_prompt {
            builder = builder.system_prompt(system_prompt);
        }
        let session = builder.create().await?;

        Ok(self.open(id, session))
    }

    async fn resume(&mut self, params: SessionResumeParams) -> Result<MethodResult, ServeError> {
        self.ensure_closed(&params.session)?;
        let model = match params.model {
            Some(model) => model,
            None => Self::stored_model(&params.session, &params.store).await?,
        };
        let harness = self.harness(&model, params.store)?;
        let session = harness.resume(&params.session).await?;

        Ok(self.open(params.session, session))
    }

    /// Reads the model `session` was created with from `store`.
    async fn stored_model(session: &str, store: &StoreChoice) -> Result<ModelChoice, ServeError> {
        let StoreChoice::Sqlite { path } = store else {
            // A memory session lives only on the server that created it,
            // where it stays open, so there is none to resume.
            return Err(SessionError::NotFound {
                id: session.to_string(),
            }
            .into());
        };
        let info = SessionInfo::load(path, session).await?;
        let (provider, model) = stored_model_identity_parts(info.provider(), info.model())
            .map_err(ServeError::config)?;

        Ok(ModelChoice::Id(
            ModelId::cataloged(provider, &model).to_string(),
        ))
    }

    fn start(&self, params: RunStartParams) -> Result<PendingRun, ServeError> {
        let session = self.session(&params.session)?;
        let mut request = RunRequest::new(params.input)
            .generation(params.generation.into())
            .limits(params.limits.into());
        if let Some(instructions) = params.instructions {
            request = request.instructions(instructions);
        }
        if let Some(output) = params.output {
            request = request.output(output.into_mode().map_err(ServeError::config)?);
        }
        let (events, handle) = session.run(request).into_parts();

        Ok(PendingRun {
            events,
            handle,
            session: params.session,
        })
    }

    /// Cancels `run`; a run that already finished is left as it is, so the
    /// answer does not race with the run's end.
    fn cancel(&self, run: &RunId) -> Result<MethodResult, ServeError> {
        match self.runs.get(run) {
            Some(task) => task.cancel.cancel(),
            None if self.finished_runs.contains(run) => {}
            None => return Err(ServeError::UnknownRun(run.as_str().to_string())),
        }

        Ok(MethodResult::Empty(Empty {}))
    }

    async fn transcript(
        &self,
        params: SessionTranscriptParams,
    ) -> Result<MethodResult, ServeError> {
        let turns = self
            .session(&params.session)?
            .transcript(params.range)
            .await?;

        Ok(MethodResult::Transcript(Transcript { turns }))
    }

    /// Builds the harness of one session: `model` stored in `store`, with
    /// the server's tools, or none, and every call that changes state
    /// approved by the client.
    fn harness(&mut self, model: &ModelChoice, store: StoreChoice) -> Result<Harness, ServeError> {
        let mut builder = Harness::builder()
            .model_reasoning_effort(self.settings.reasoning_effort)
            .approval(Arc::new(self.approvals.clone()))
            .approval_timeout(self.settings.approval_timeout);
        if let Some((repository, tools)) = &self.settings.tools {
            builder = builder.repository(repository.clone()).tools(tools.clone());
        }
        let builder = match store {
            StoreChoice::Memory => builder.store(Arc::new(self.memory.clone())),
            StoreChoice::Sqlite { path } => builder.sqlite(path),
        };

        Ok(self.select_model(builder, model)?.build()?)
    }

    /// Selects `model` from the environment's catalog, or else as a
    /// `provider/model` identifier.
    fn select_model(
        &mut self,
        builder: HarnessBuilder,
        model: &ModelChoice,
    ) -> Result<HarnessBuilder, ServeError> {
        // The harness keeps no default sign-in location, so the server
        // passes the one the CLI shares with Agentty.
        let auth_file = login::chatgpt_auth_file(&mut self.environment);
        let catalog = ModelCatalog::from_environment_with_auth_file(
            &mut self.environment,
            auth_file.as_deref(),
        )
        .map_err(ServeError::config)?;
        let selector = match model {
            ModelChoice::Role(role) => ModelSelector::Role(Role::new(role.clone())),
            ModelChoice::Id(id) if catalog.spec(&ModelId::new(id.as_str())).is_some() => {
                ModelSelector::Id(ModelId::new(id.as_str()))
            }
            ModelChoice::Id(id) => return self.provider_model(builder, id),
        };

        select_cataloged(builder, Arc::new(catalog), selector).map_err(ServeError::config)
    }

    /// Selects the uncataloged `provider/model` `id`, with the context
    /// profile its provider declares or else the CLI's default profile.
    fn provider_model(
        &mut self,
        builder: HarnessBuilder,
        id: &str,
    ) -> Result<HarnessBuilder, ServeError> {
        let (provider, model) = id
            .split_once('/')
            .and_then(|(provider, model)| Some((provider.parse::<ModelProvider>().ok()?, model)))
            .ok_or_else(|| ServeError::UnknownModel(id.to_string()))?;
        let (client, profile) = connect_model(provider, model, None, &mut self.environment)
            .map_err(ServeError::config)?;

        select_direct(builder, Arc::new(client), profile).map_err(ServeError::config)
    }

    fn ensure_closed(&self, id: &str) -> Result<(), ServeError> {
        if self.sessions.contains_key(id) {
            return Err(ServeError::SessionOpen(id.to_string()));
        }

        Ok(())
    }

    fn open(&mut self, id: String, session: Session) -> MethodResult {
        self.sessions.insert(id.clone(), session);

        MethodResult::SessionOpened(SessionOpened { session: id })
    }

    fn session(&self, id: &str) -> Result<&Session, ServeError> {
        self.sessions
            .get(id)
            .ok_or_else(|| ServeError::UnknownSession(id.to_string()))
    }

    /// Runs `run` on its own task, which queues its events as notification
    /// lines, and moves runs that already finished to `finished_runs`.
    fn spawn(&mut self, run: PendingRun) {
        let finished_runs = &mut self.finished_runs;
        self.runs.retain(|id, run| {
            let running = !run.task.is_finished();
            if !running {
                finished_runs.insert(id.clone());
            }

            running
        });
        let PendingRun {
            mut events,
            handle,
            session,
        } = run;
        let id = events.run_id().clone();
        let cancel = handle.cancel_token();
        let sender = self.event_sender.clone();
        // The run is polled only once its task starts, so it cannot ask
        // before its approvals are routed here.
        let mut asks = self.approvals.open(id.clone());
        let approvals = self.approvals.clone();
        let run = id.clone();
        // The run's task holds `sender` until its stream ends, so `stop`
        // drains every event. Events are read first: the run emits its
        // `approval_requested` event before it asks, so the event is
        // always written before the request.
        let forward = async move {
            loop {
                let line = tokio::select! {
                    biased;
                    event = next_event(&mut events) => {
                        match event {
                            Some(event) => event_line(session.clone(), event),
                            None => break,
                        }
                    }
                    Some(ask) = asks.recv() => {
                        line(&approvals.request(session.clone(), ask)).unwrap_or_default()
                    }
                };
                let _ = sender.send(line).await;
            }
            approvals.close(&run);
        };
        let task = tokio::spawn(async move {
            let (_outcome, ()) = tokio::join!(handle.outcome(), forward);
        });
        self.runs.insert(id, RunTask { cancel, task });
    }

    /// Closes every session, cancels every run in flight, and writes the
    /// runs' remaining events, which end once every run's task finished.
    /// Returns the output and the first failure to write an event; later
    /// events are still drained so every run ends.
    async fn stop(self) -> (Output, io::Result<()>) {
        let Self {
            mut event_lines,
            event_sender,
            mut output,
            runs,
            sessions,
            ..
        } = self;
        drop(sessions);
        drop(event_sender);
        for run in runs.values() {
            run.cancel.cancel();
        }
        let mut written = Ok(());
        while let Some(event) = event_lines.recv().await {
            written = written.and(write_line(&mut output, &event).await);
        }

        (output, written)
    }

    async fn write(&mut self, message: &impl Serialize) -> io::Result<()> {
        write_line(&mut self.output, &line(message)?).await
    }
}

/// What a dispatched request answers.
enum Reply {
    /// The request's result.
    Result(MethodResult),
    /// A run to announce and then start.
    Run(Box<PendingRun>),
    /// `shutdown`, answered once every run stopped.
    Shutdown,
}

/// A run whose identifier is not yet announced.
struct PendingRun {
    events: EventStream,
    handle: RunHandle,
    session: String,
}

/// A run in flight.
struct RunTask {
    cancel: CancelToken,
    task: JoinHandle<()>,
}

/// Failure of one request, answered with a [`SERVER_ERROR`].
#[derive(Debug, Error)]
enum ServeError {
    /// The harness classified the failure.
    #[error(transparent)]
    Harness(#[from] HarnessError),
    /// The request's model, output, or provider configuration is unusable.
    #[error(transparent)]
    Config(Box<dyn Error + Send + Sync>),
    /// The session is already open on this server.
    #[error("session `{0}` is already open")]
    SessionOpen(String),
    /// The model is neither cataloged nor a known provider's model.
    #[error("model `{0}` is not cataloged and does not name a known provider as `provider/model`")]
    UnknownModel(String),
    /// The run was not started by this server.
    #[error("run `{0}` was not started by this server")]
    UnknownRun(String),
    /// The session is not open on this server.
    #[error("session `{0}` is not open; create or resume it first")]
    UnknownSession(String),
}

impl ServeError {
    fn config(error: impl Error + Send + Sync + 'static) -> Self {
        Self::Config(Box::new(error))
    }

    /// Converts to a JSON-RPC error object classified by [`ErrorKind`].
    fn into_object(self) -> ErrorObject {
        let (kind, retryable) = match &self {
            Self::Harness(error) => (error.kind(), error.is_retryable()),
            Self::Config(_) | Self::UnknownModel(_) => (ErrorKind::Config, false),
            Self::SessionOpen(_) => (ErrorKind::Conflict, false),
            Self::UnknownRun(_) | Self::UnknownSession(_) => (ErrorKind::NotFound, false),
        };

        ErrorObject {
            code: SERVER_ERROR,
            data: Some(ErrorData::new(kind, retryable)),
            message: self.to_string(),
        }
    }
}

impl From<SessionError> for ServeError {
    fn from(error: SessionError) -> Self {
        Self::Harness(error.into())
    }
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
