//! Shared task-chain fixture with scripted graph role turns on a real session.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{MockAgentChannel, TurnRequest, TurnResult};
use ag_protocol::AgentResponse;
use ag_session::{
    AgentAnswerContent, AgentKind, AgentModel, AgentSelection, PermissionMode, SessionId,
    SessionMessageKind, SessionService, SessionStatus, StartPlanThenImplementRequest,
    TaskAgentRole, TaskChain, TaskChainPhase,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::test_support::TestSessionRunFactory;
use crate::infra::db::{AppRepositories, DbError};
use crate::test_support::{new_git_test_app_with_pool, set_session_status_for_test};

/// Injected repository failure shared by workflow admission tests.
pub(in crate::app) fn graph_unavailable() -> DbError {
    DbError::InvalidData {
        entity: "execution graph",
        reason: "injected graph outage".into(),
    }
}

/// Launch input for the fixture workflow `workflow`.
pub(in crate::app) fn launch_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        agents: Box::new(ag_session::PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: Vec::new(),
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "workflow".into(),
        user_request: "Implement a scoped change".into(),
    }
}

/// Builds the persisted transcript entry for one role-attributed answer.
pub(in crate::app) fn agent_answer(
    agent: TaskAgentRole,
    text: &str,
) -> (SessionMessageKind, String) {
    (
        SessionMessageKind::AgentAnswer,
        AgentAnswerContent {
            agent,
            text: text.to_string(),
        }
        .encode(),
    )
}

/// A review-ready session whose graph role turns are answered by scripts.
pub(in crate::app) struct ChainFixture {
    pub(in crate::app) app: App,
    pub(in crate::app) pool: SqlitePool,
    pub(in crate::app) service: SessionService,
    pub(in crate::app) session_id: SessionId,
    factory: Arc<TestSessionRunFactory>,
    _directory: TempDir,
}

impl ChainFixture {
    /// Creates a read-only, review-ready session with scripted role turns.
    pub(in crate::app) async fn new() -> Self {
        let mut fixture = Self::new_untitled().await;
        fixture
            .app
            .services
            .db()
            .sessions()
            .update_session_title(&fixture.session_id, "Scoped change")
            .await
            .expect("existing title keeps role tests independent of title utilities");
        let index = fixture
            .app
            .sessions
            .session_index_or_err(&fixture.session_id)
            .expect("session index");
        fixture
            .app
            .sessions
            .session_at_mut(index)
            .expect("session")
            .title = Some("Scoped change".into());

        fixture
    }

    /// Creates a blank session for first-message metadata and title tests.
    pub(in crate::app) async fn new_untitled() -> Self {
        let (mut app, directory, pool) = new_git_test_app_with_pool().await;
        let session_id = SessionId::from(app.create_session().await.expect("session"));
        set_session_status_for_test(&mut app, &session_id, SessionStatus::Review);
        app.services
            .db()
            .sessions()
            .update_session_permission_mode(&session_id, PermissionMode::ReadOnly)
            .await
            .expect("read-only workflow fixture");
        let factory = TestSessionRunFactory::install(&mut app.services);
        let service = app.session_service();

        Self {
            app,
            factory,
            pool,
            service,
            session_id,
            _directory: directory,
        }
    }

    /// Queues the next graph role turn; a blocked turn waits for
    /// `Script::release`. Role turns run in queue order.
    pub(in crate::app) fn register_turn(&self, answer: &str, blocked: bool) -> Script {
        self.register_response(AgentResponse::plain(answer), blocked)
    }

    /// Queues the next graph role turn with an explicit structured response.
    pub(in crate::app) fn register_response(
        &self,
        response: AgentResponse,
        blocked: bool,
    ) -> Script {
        let script = Script {
            entered: CancellationToken::new(),
            prompt: Arc::new(Mutex::new(None)),
            release: CancellationToken::new(),
        };
        if !blocked {
            script.release.cancel();
        }
        let entered = script.entered.clone();
        let prompt = Arc::clone(&script.prompt);
        let release = script.release.clone();
        let mut channel = MockAgentChannel::new();
        channel
            .expect_run_turn()
            .once()
            .returning(move |_, request, _| {
                *prompt.lock().expect("observed prompt") = Some(request);
                let entered = entered.clone();
                let release = release.clone();
                let response = response.clone();

                Box::pin(async move {
                    entered.cancel();
                    release.cancelled().await;
                    Ok(TurnResult {
                        assistant_message: response,
                        context_reset: false,
                        harness_report: None,
                        input_tokens: 1,
                        output_tokens: 1,
                        provider_conversation_id: Some("task-thread".into()),
                    })
                })
            });
        channel
            .expect_shutdown_session()
            .times(1..)
            .returning(|_| Box::pin(async { Ok(()) }));
        self.factory.register_graph_task(Arc::new(channel));

        script
    }

    /// Drives the runtime until the scripted turn starts and the workflow
    /// leaves its working phase.
    pub(in crate::app) async fn settle(&mut self, script: &Script) {
        self.settle_chain(script, "workflow").await;
    }

    /// Settles a named workflow when a session owns several chains.
    pub(in crate::app) async fn settle_chain(&mut self, script: &Script, chain_id: &str) {
        let db = self.app.services.db().clone();
        let chain_id = chain_id.to_string();
        self.app.drive_session_request(script.started()).await;
        // Run database polling independently: suspending a transaction in
        // drive_session_request can starve a foreground command on the
        // fixture's single-connection pool.
        let waiter = tokio::spawn(async move {
            wait_for_chain(&db, &chain_id, |chain| {
                !matches!(
                    chain.phase,
                    TaskChainPhase::Planning | TaskChainPhase::Implementing
                )
            })
            .await
        });
        self.app
            .drive_session_request(waiter)
            .await
            .expect("workflow waiter");
        self.drain_app_events().await;
    }

    /// Drives the runtime until the workflow satisfies `condition`.
    pub(in crate::app) async fn wait_for(
        &mut self,
        condition: impl Fn(&TaskChain) -> bool + Send + Sync + 'static,
    ) -> TaskChain {
        let db = self.app.services.db().clone();
        let waiter = tokio::spawn(async move { wait_for_chain(&db, "workflow", condition).await });
        let chain = self
            .app
            .drive_session_request(waiter)
            .await
            .expect("workflow waiter");
        self.drain_app_events().await;

        chain
    }

    /// Applies already queued reducer events and syncs the live session.
    pub(in crate::app) async fn drain_app_events(&mut self) {
        while let Ok(Some(event)) =
            tokio::time::timeout(Duration::from_millis(20), self.app.next_app_event()).await
        {
            self.app.apply_app_events(event).await;
        }
        self.app.sessions.sync_session_from_handle(&self.session_id);
    }

    /// Reads the persisted session transcript as kind and content pairs.
    pub(in crate::app) async fn persisted_transcript(&self) -> Vec<(SessionMessageKind, String)> {
        self.app
            .services
            .db()
            .sessions()
            .load_session_messages(&self.session_id)
            .await
            .expect("session messages")
            .into_iter()
            .map(|row| (row.kind.parse().expect("message kind"), row.content))
            .collect()
    }

    /// Reads the reducer-synced live transcript as kind and content pairs.
    pub(in crate::app) fn live_transcript(&self) -> Vec<(SessionMessageKind, String)> {
        self.app
            .sessions
            .session_for_id(&self.session_id)
            .and_then(|session| session.transcript.as_ref())
            .expect("live transcript")
            .messages()
            .iter()
            .map(|message| (message.kind, message.content.clone()))
            .collect()
    }

    /// Reads the fixture workflow through the public API.
    pub(in crate::app) async fn snapshot(&mut self) -> TaskChain {
        self.app
            .drive_session_request(self.service.get_task_chain("workflow"))
            .await
            .expect("read workflow")
            .expect("workflow exists")
    }
}

/// Polls durable workflow state; graph settlement completes in the
/// background driver after the worker's own status updates.
async fn wait_for_chain(
    db: &AppRepositories,
    chain_id: &str,
    condition: impl Fn(&TaskChain) -> bool,
) -> TaskChain {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(chain) = db
                .graphs()
                .load_task_chain(chain_id)
                .await
                .expect("workflow state")
                .filter(|chain| condition(chain))
            {
                return chain;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let diagnostic = if result.is_err() {
        format!(
            "workflow settle guard; progress: {:?}",
            db.graphs().progress(chain_id).await
        )
    } else {
        String::new()
    };

    result.expect(&diagnostic)
}

/// Observes and releases one scripted role turn.
pub(in crate::app) struct Script {
    pub(in crate::app) entered: CancellationToken,
    pub(in crate::app) prompt: Arc<Mutex<Option<TurnRequest>>>,
    pub(in crate::app) release: CancellationToken,
}

impl Script {
    /// Waits until the scripted turn starts executing.
    pub(in crate::app) async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.cancelled())
            .await
            .expect("execution start guard");
    }

    /// Returns the replayed history and prompt sent to the provider.
    pub(in crate::app) fn prompt(&self) -> String {
        let request = self.request();

        format!(
            "{}\n{}",
            request.continuation.replay_transcript().unwrap_or_default(),
            request.prompt.agent_text()
        )
    }

    /// Captured provider request, including structured images and settings.
    pub(in crate::app) fn request(&self) -> TurnRequest {
        self.prompt
            .lock()
            .expect("observed prompt")
            .clone()
            .expect("turn ran")
    }
}
