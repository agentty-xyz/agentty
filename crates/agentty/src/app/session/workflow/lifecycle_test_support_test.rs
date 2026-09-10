use std::sync::Arc;

use ag_agent as agent;

use super::ReplyOptions;
use crate::app::{AppServices, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::turn_prompt::TurnPrompt;

impl SessionManager {
    /// Submits a follow-up prompt using a pre-built backend for
    /// deterministic test execution.
    ///
    /// Creates a test CLI channel backed by the given
    /// [`agent::AgentBackend`] and registers it in the session-local
    /// channel map so the worker uses it instead of the default factory.
    /// This allows tests to control spawned process commands without
    /// relying on a real provider binary.
    pub(crate) async fn reply_with_backend(
        &mut self,
        services: &AppServices,
        session_id: &str,
        prompt: impl Into<TurnPrompt>,
        backend: Arc<dyn agent::AgentBackend>,
        session_model: AgentModel,
    ) {
        let prompt = prompt.into();
        let session_agent = self.session_or_err(session_id).map_or(
            AgentSelection::new(AgentKind::Antigravity, session_model),
            |session| session.agent,
        );
        let channel =
            ag_agent::create_cli_agent_channel_with_backend(backend, session_agent.kind());
        self.worker_service
            .test_agent_channels
            .insert(session_id.to_string().into(), channel);
        self.reply_impl(
            services,
            session_id,
            prompt,
            session_agent,
            ReplyOptions::standard(Vec::new()),
        )
        .await;
    }
}
