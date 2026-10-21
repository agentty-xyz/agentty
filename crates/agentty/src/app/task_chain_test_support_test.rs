//! Shared task-chain fixture with scripted role turns on a real session.

use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{MockAgentChannel, TurnResult};
use ag_protocol::AgentResponse;
use ag_session::{
    AgentAnswerContent, AgentKind, AgentModel, AgentSelection, PermissionMode, SessionId,
    SessionMessageKind, SessionService, SessionStatus, StartPlanThenImplementRequest,
    TaskAgentRole, TaskChain,
};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::test_support::TestSessionRunFactory;
use crate::test_support::{new_git_test_app_with_pool, set_session_status_for_test};

/// Launch input for the fixture workflow `workflow`.
pub(in crate::app) fn launch_request() -> StartPlanThenImplementRequest {
    StartPlanThenImplementRequest {
        agents: Box::new(ag_session::PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: Vec::new(),
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "workflow".into(),
        planner_attempt_id: "initial".into(),
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

/// A review-ready session whose role turns are answered by registered
/// scripts.
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

    /// Registers the next turn of `task_id`; a blocked turn waits for
    /// `Script::release`.
    pub(in crate::app) fn register_result(
        &self,
        task_id: &str,
        answer: &str,
        blocked: bool,
    ) -> Script {
        let script = Script {
            entered: CancellationToken::new(),
            release: CancellationToken::new(),
        };
        if !blocked {
            script.release.cancel();
        }
        let entered = script.entered.clone();
        let release = script.release.clone();
        let response = AgentResponse::plain(answer);
        let mut channel = MockAgentChannel::new();
        channel.expect_run_turn().once().returning(move |_, _, _| {
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
        self.factory.register(task_id, Arc::new(channel));

        script
    }

    /// Drives reducer events until the current attempt's operation settles.
    pub(in crate::app) async fn settle(&mut self, script: &Script) {
        self.settle_chain(script, "workflow").await;
    }

    /// Settles a named workflow when a session owns several chains.
    pub(in crate::app) async fn settle_chain(&mut self, script: &Script, chain_id: &str) {
        script.started().await;
        let request = self
            .app
            .drive_session_request(self.service.get_task_chain(chain_id))
            .await
            .expect("read workflow")
            .expect("workflow exists")
            .current_request;
        let operation_id = format!("task-attempt:{}", request.attempt_id);
        tokio::time::timeout(Duration::from_secs(10), async {
            while self
                .app
                .services
                .db()
                .operations()
                .is_session_operation_unfinished(&operation_id)
                .await
                .expect("operation state")
            {
                let event = self.app.next_app_event().await.expect("worker event");
                self.app.apply_app_events(event).await;
            }
        })
        .await
        .expect("settle guard");
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

/// Observes and releases one scripted role turn.
pub(in crate::app) struct Script {
    pub(in crate::app) entered: CancellationToken,
    pub(in crate::app) release: CancellationToken,
}

impl Script {
    /// Waits until the scripted turn starts executing.
    pub(in crate::app) async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.entered.cancelled())
            .await
            .expect("execution start guard");
    }
}
