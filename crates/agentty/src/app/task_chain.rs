//! Foreground admission for static, explicitly accepted task workflows.

use ag_session::{
    AcceptTaskChainPlanRequest, BuiltinAgentDefinition, ContinueTaskChainRequest, SessionError,
    SessionId, StartPlanThenImplementRequest, TaskChain,
};
use tracing::warn;

use crate::app::App;
use crate::infra::db::DbError;

impl App {
    /// Starts only newly admitted work; matching launch redelivery is a
    /// receipt.
    pub(super) async fn start_api_task_chain(
        &mut self,
        session_id: &SessionId,
        request: StartPlanThenImplementRequest,
    ) -> Result<(), SessionError> {
        if self
            .services
            .db()
            .tasks()
            .load_task_chain(&request.chain_id)
            .await
            .map_err(|error| api_error(&error))?
            .is_none()
        {
            self.require_task_chain_session_ready(session_id, BuiltinAgentDefinition::Planner)?;
        }
        let admitted = self
            .services
            .db()
            .tasks()
            .start_task_chain(session_id, &request, "agent_task")
            .await
            .map_err(|error| api_error(&error))?;
        if admitted {
            self.dispatch_task_chain(&request.chain_id, &request.planner_attempt_id)
                .await?;
        }

        Ok(())
    }

    /// Accepts and dispatches the exact version recorded by the transaction.
    pub(super) async fn accept_api_task_chain_plan(
        &mut self,
        chain_id: &str,
        request: AcceptTaskChainPlanRequest,
    ) -> Result<(), SessionError> {
        let chain = self.require_task_chain(chain_id).await?;
        if !self
            .task_chain_has_receipt(&chain, &request.attempt_id)
            .await?
        {
            self.require_task_chain_session_ready(
                &chain.session_id,
                BuiltinAgentDefinition::Implementer,
            )?;
        }
        let admitted = self
            .services
            .db()
            .tasks()
            .accept_task_chain_plan(chain_id, &request, "agent_task")
            .await
            .map_err(|error| api_error(&error))?;
        if admitted {
            self.dispatch_task_chain(chain_id, &request.attempt_id)
                .await?;
        }

        Ok(())
    }

    /// Retries or clarifies only on a new, explicit caller delivery.
    pub(super) async fn continue_api_task_chain(
        &mut self,
        chain_id: &str,
        request: ContinueTaskChainRequest,
    ) -> Result<(), SessionError> {
        let chain = self.require_task_chain(chain_id).await?;
        if !self
            .task_chain_has_receipt(&chain, &request.attempt_id)
            .await?
        {
            self.require_task_chain_session_ready(
                &chain.session_id,
                chain.current_request.assignment.definition,
            )?;
        }
        let admitted = self
            .services
            .db()
            .tasks()
            .continue_task_chain(chain_id, &request, "agent_task")
            .await
            .map_err(|error| api_error(&error))?;
        if admitted {
            self.dispatch_task_chain(chain_id, &request.attempt_id)
                .await?;
        }

        Ok(())
    }

    /// Persists cancellation before interrupting the exact attempt token.
    pub(super) async fn cancel_api_task_chain(
        &mut self,
        chain_id: &str,
    ) -> Result<(), SessionError> {
        let chain = self.require_task_chain(chain_id).await?;
        self.services
            .db()
            .tasks()
            .cancel_task_chain(chain_id)
            .await
            .map_err(|error| api_error(&error))?;
        self.sessions.cancel_agent_task_attempt(&format!(
            "task-attempt:{}",
            chain.current_request.attempt_id
        ));

        Ok(())
    }

    fn require_task_chain_session_ready(
        &mut self,
        session_id: &SessionId,
        definition: BuiltinAgentDefinition,
    ) -> Result<(), SessionError> {
        self.sessions
            .validate_agent_task_admission(session_id, definition)
            .map_err(|error| match error {
                crate::app::SessionError::NotFound => SessionError::NotFound,
                other => SessionError::Operation(other.to_string()),
            })
    }

    async fn require_task_chain(&self, chain_id: &str) -> Result<TaskChain, SessionError> {
        self.services
            .db()
            .tasks()
            .load_task_chain(chain_id)
            .await
            .map_err(|error| api_error(&error))?
            .ok_or(SessionError::NotFound)
    }

    async fn task_chain_has_receipt(
        &self,
        chain: &TaskChain,
        attempt_id: &str,
    ) -> Result<bool, SessionError> {
        for task_id in [&chain.planner_task_id, &chain.implementer_task_id] {
            if self
                .services
                .db()
                .tasks()
                .load_task(task_id)
                .await
                .map_err(|error| api_error(&error))?
                .is_some_and(|task| task.attempts.iter().any(|attempt| attempt.id == attempt_id))
            {
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn dispatch_task_chain(
        &mut self,
        chain_id: &str,
        attempt_id: &str,
    ) -> Result<(), SessionError> {
        let result = async {
            let chain = self.require_task_chain(chain_id).await?;
            if chain.current_request.attempt_id != attempt_id {
                return Err(SessionError::Operation(
                    "Workflow attempt changed before delivery".to_string(),
                ));
            }
            self.suppress_agent_task_automation(&chain.session_id, attempt_id)
                .await?;
            self.sessions
                .enqueue_claimed_agent_task(
                    &self.services,
                    &chain.session_id,
                    chain.current_request,
                )
                .await
                .map_err(|error| SessionError::Operation(error.to_string()))
        }
        .await;
        if let Err(error) = &result {
            let operation_id = format!("task-attempt:{attempt_id}");
            if let Err(store_error) = self
                .services
                .db()
                .operations()
                .mark_session_operation_failed(&operation_id, &error.to_string())
                .await
            {
                warn!(%operation_id, error = %store_error, "failed to persist workflow delivery failure");
            }
        }

        result
    }
}

fn api_error(error: &DbError) -> SessionError {
    SessionError::Operation(error.to_string())
}

#[cfg(test)]
#[path = "task_chain_test.rs"]
mod tests;
