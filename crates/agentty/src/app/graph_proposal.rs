//! Initial graph generation through the scoped utility worker, before approval.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Mutex;

use ag_contracts::{AgentRequestKind, OneShotRequest, PermissionMode, ProviderCallBudget};
use ag_protocol::{GraphProposalOutcome, GraphProposalResponse};
use ag_session::{
    GenerateGraphProposalRequest, GraphProposalAction, GraphProposalLimits,
    GraphProposalResolution, GraphProposalState, GraphProposalVersion, GraphProposalVersionOutcome,
    SessionError, StartExecutionGraphRequest,
};
use askama::Template;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::command_gateway::GraphProposalCommand;
use crate::app::session::SessionTaskService;
use crate::domain::session::Status;

#[derive(Template)]
#[template(path = "graph_proposal_prompt.md", escape = "none")]
struct GraphProposalPrompt<'a> {
    answers: &'a str,
    catalog: &'a str,
    limits: &'a str,
    request: &'a str,
}

pub(super) struct ProposalGeneration {
    pub(super) cancellation: CancellationToken,
    pub(super) version: u32,
}

impl App {
    pub(super) async fn admit_graph_proposal_command(
        &mut self,
        command: GraphProposalCommand,
    ) -> Result<(), SessionError> {
        match command {
            GraphProposalCommand::Generate(request) => {
                self.generate_api_graph_proposal(*request).await
            }
            GraphProposalCommand::Act {
                id,
                version,
                action,
            } => self.act_on_api_graph_proposal(&id, version, action).await,
        }
    }

    /// Proposal snapshots may only launch after their own exact-version
    /// receipt. Recovery cannot recreate credits already used before the
    /// restart.
    pub(super) async fn validate_proposal_launch(
        &self,
        request: &StartExecutionGraphRequest,
    ) -> Result<(), SessionError> {
        let Some(identity) = request.graph.id.strip_prefix("proposal:") else {
            return Ok(());
        };
        let (id, version) = identity
            .rsplit_once(':')
            .ok_or_else(|| SessionError::Operation("Invalid proposal graph identity".into()))?;
        let version = version.parse::<u32>().map_err(proposal_error)?;
        let state = self
            .services
            .db()
            .graph_proposals()
            .get(id, Some(version))
            .await
            .map_err(proposal_error)?
            .ok_or_else(|| SessionError::Operation("Proposal approval is missing".into()))?;
        let GraphProposalVersionOutcome::Generated {
            launch: Some(launch),
            ..
        } = state.version.outcome
        else {
            return Err(SessionError::Operation(
                "Proposal has no resolved snapshot".into(),
            ));
        };
        if state.version.approval.is_none() || launch.as_ref() != request {
            return Err(SessionError::Operation(
                "Launch does not match exact proposal approval and limits".into(),
            ));
        }
        let retained = self
            .services
            .graph_drivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&request.graph.id);
        if !retained
            && !self
                .services
                .db()
                .graphs()
                .list_executions(&request.graph.id, request.graph.revision)
                .await
                .map_err(proposal_error)?
                .is_empty()
        {
            return Err(SessionError::Operation(
                "Restored proposal execution cannot recreate spent provider credits".into(),
            ));
        }

        Ok(())
    }

    pub(super) async fn generate_api_graph_proposal(
        &mut self,
        input: GenerateGraphProposalRequest,
    ) -> Result<(), SessionError> {
        self.require_graph_proposal_feature()?;
        if let Some(existing) = self
            .services
            .db()
            .graph_proposals()
            .get(&input.id, None)
            .await
            .map_err(proposal_error)?
        {
            if existing.input != input {
                return Err(SessionError::Operation(
                    "Proposal identity has different original input".into(),
                ));
            }
            return Ok(());
        }
        if input.request.has_attachments()
            || input.request.text.trim().is_empty()
            || input.request.text.len() > GraphProposalLimits::default().bytes
        {
            return Err(SessionError::Operation(
                "Graph requests require text within host limits; attachment paths are unsupported"
                    .into(),
            ));
        }
        self.sessions.sync_session_from_handle(&input.session_id);
        let session = self
            .sessions
            .session_for_id(&input.session_id)
            .ok_or(SessionError::NotFound)?;
        if !matches!(
            session.status,
            Status::Draft | Status::Review | Status::AgentReview
        ) || session.is_draft_session()
        {
            return Err(SessionError::Operation(
                "Graph generation requires a prepared idle session".into(),
            ));
        }
        let (agent, permission, folder) = (
            session.agent,
            session.permission_mode,
            session.folder.clone(),
        );
        let catalog = self.services.agent_profile_catalog_client();
        let mut definitions = Vec::new();
        for summary in catalog.list_summaries(folder.clone()).await {
            if let Some(definition) = catalog.resolve(folder.clone(), summary.id).await {
                definitions.push(definition);
            }
        }
        if definitions.is_empty() {
            return Err(SessionError::Operation(
                "No enabled agent definitions are available".into(),
            ));
        }
        let state = GraphProposalState {
            agent,
            definitions,
            limits: GraphProposalLimits::default(),
            permission,
            version: GraphProposalVersion {
                answers: Vec::new(),
                approval: None,
                is_cancelled: false,
                number: 1,
                outcome: GraphProposalVersionOutcome::Generating,
                parent_id: format!("graph-proposal:{}:1", input.id),
                utility_run_id: None,
            },
            input,
        };
        if self
            .services
            .db()
            .graph_proposals()
            .create(&state)
            .await
            .map_err(proposal_error)?
        {
            self.spawn_graph_proposal_generation(state, folder);
        }

        Ok(())
    }

    pub(super) async fn act_on_api_graph_proposal(
        &mut self,
        id: &str,
        version: u32,
        action: GraphProposalAction,
    ) -> Result<(), SessionError> {
        self.require_graph_proposal_feature()?;
        let state = self
            .services
            .db()
            .graph_proposals()
            .get(id, None)
            .await
            .map_err(proposal_error)?
            .ok_or_else(|| SessionError::Operation("Proposal is missing".into()))?;
        match action {
            GraphProposalAction::Cancel => {
                self.services
                    .db()
                    .graph_proposals()
                    .cancel(id, version)
                    .await
                    .map_err(proposal_error)?;
                self.cancel_proposal_generation(id);
            }
            GraphProposalAction::Approve => {
                // A duplicate receipt is never a request to restart a stopped
                // driver.
                if state.version.number == version && state.version.approval.is_some() {
                    return Ok(());
                }
                self.sessions
                    .sync_session_from_handle(&state.input.session_id);
                let session = self
                    .sessions
                    .session_for_id(&state.input.session_id)
                    .ok_or(SessionError::NotFound)?;
                let stored = self
                    .services
                    .db()
                    .sessions()
                    .load_session(&state.input.session_id)
                    .await
                    .map_err(proposal_error)?
                    .ok_or(SessionError::NotFound)?;
                if session.permission_mode != state.permission
                    || stored.permission_mode != state.permission.to_string()
                    || !matches!(
                        session.status,
                        Status::Draft | Status::Review | Status::AgentReview
                    )
                {
                    return Err(SessionError::Operation(
                        "Session permissions or readiness changed; proposal cannot be approved"
                            .into(),
                    ));
                }
                let GraphProposalVersionOutcome::Generated {
                    launch: Some(launch),
                    ..
                } = state.version.outcome
                else {
                    return Err(SessionError::Operation(
                        "Inspect a resolved preview before approval".into(),
                    ));
                };
                if self
                    .services
                    .db()
                    .graph_proposals()
                    .approve(id, version)
                    .await
                    .map_err(proposal_error)?
                {
                    // Publication and its receipt are durable even if launch
                    // fails.
                    self.start_api_execution_graph(*launch).await?;
                }
            }
            GraphProposalAction::Answer(_) | GraphProposalAction::Regenerate => {
                let folder = self
                    .sessions
                    .session_for_id(&state.input.session_id)
                    .ok_or(SessionError::NotFound)?
                    .folder
                    .clone();
                let next = self
                    .services
                    .db()
                    .graph_proposals()
                    .reserve(id, version, &action)
                    .await
                    .map_err(proposal_error)?;
                self.cancel_proposal_generation(id);
                self.spawn_graph_proposal_generation(next, folder);
            }
        }

        Ok(())
    }

    fn require_graph_proposal_feature(&self) -> Result<(), SessionError> {
        if !self.experimental_multi_agent {
            return Err(SessionError::Operation(
                "Graph proposals require --experimental-multi-agent".into(),
            ));
        }

        Ok(())
    }

    fn cancel_proposal_generation(&self, id: &str) {
        if let Some(cancellation) = self
            .services
            .proposal_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id)
        {
            cancellation.cancellation.cancel();
        }
    }

    fn spawn_graph_proposal_generation(&self, state: GraphProposalState, folder: PathBuf) {
        let cancellation = CancellationToken::new();
        self.services
            .proposal_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                state.input.id.clone(),
                ProposalGeneration {
                    cancellation: cancellation.clone(),
                    version: state.version.number,
                },
            );
        let client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                cancellation: Some(cancellation.clone()),
                parent_id: Some(state.version.parent_id.clone()),
                purpose: Some("graph-proposal".into()),
                session_id: Some(state.input.session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        );
        let services = self.services.clone();
        let handles = self
            .sessions
            .session_handles_or_err(&state.input.session_id)
            .ok()
            .map(|handles| handles.transcript.clone());
        self.services.track_cleanup_task(tokio::spawn(async move {
            let environment = services.graph_resources.environment(&ag_session::EnvId::SessionWorktree(state.input.session_id.clone()));
            let outcome = tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                lease = environment.read_owned() => {
                    ag_worker::with_execution_resource(lease, generate(&state, folder, client.as_ref())).await
                }
            };
            if services.db().graph_proposals().finish(&state.input.id, state.version.number, &outcome).await.is_ok()
                && let Some(transcript) = handles
            {
                let mut completed = state.clone();
                completed.version.outcome = outcome;
                SessionTaskService::append_workflow_notice(
                    &transcript, services.db(), &services.event_sender(), &services.session_update_versions(),
                    &state.input.session_id, &crate::app::graph_proposal_chat::proposal_preview(&completed),
                ).await;
            }
            services.wake_graphs();
            finish_proposal_generation(&services.proposal_generations, &state.input.id, state.version.number);
        }));
    }
}

/// Remove only this generation's token while holding the same map lock used
/// by regeneration. An older completion must preserve the newer worker.
fn finish_proposal_generation(
    generations: &Mutex<HashMap<String, ProposalGeneration>>,
    id: &str,
    version: u32,
) {
    let mut generations = generations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if generations
        .get(id)
        .is_some_and(|entry| entry.version == version)
    {
        generations.remove(id);
    }
}

async fn generate(
    state: &GraphProposalState,
    folder: PathBuf,
    client: &dyn ag_worker::RunClient,
) -> GraphProposalVersionOutcome {
    match generate_inner(state, folder, client).await {
        Ok(outcome) => outcome,
        Err(error) => GraphProposalVersionOutcome::Failed(error),
    }
}

async fn generate_inner(
    state: &GraphProposalState,
    folder: PathBuf,
    client: &dyn ag_worker::RunClient,
) -> Result<GraphProposalVersionOutcome, String> {
    let request = serde_json::json!(state.input.request).to_string();
    let catalog = serde_json::json!(state.definitions).to_string();
    let limits =
        serde_json::json!({"limits": state.limits, "permission": state.permission.label()})
            .to_string();
    let answers = serde_json::json!(state.version.answers).to_string();
    let prompt = GraphProposalPrompt {
        answers: &answers,
        catalog: &catalog,
        limits: &limits,
        request: &request,
    }
    .to_string();
    let submission = client
        .submit(OneShotRequest {
            activity_tx: None,
            child_pid: None,
            execution_policy: ag_contracts::ExecutionPolicy::default(),
            folder,
            harness: state.agent.kind().to_string(),
            model: state.agent.model().as_str().into(),
            permission_mode: PermissionMode::ReadOnly,
            prompt,
            provider_call_budget: Some(ProviderCallBudget::new(
                state.limits.generation_provider_calls,
            )),
            reasoning_level: ag_contracts::ReasoningLevel::default(),
            request_kind: AgentRequestKind::GraphProposal,
            speed_mode: ag_contracts::SpeedMode::Normal,
        })
        .await
        .map_err(|error| error.to_string())?;
    let answer = submission.response.to_answer_display_text();
    if answer.len() > state.limits.bytes {
        return Err("Graph proposal response exceeds captured size limit".into());
    }
    let response: GraphProposalResponse =
        serde_json::from_str(&answer).map_err(|error| error.to_string())?;
    let launch = match &response.outcome {
        GraphProposalOutcome::Clarification { questions } => {
            if questions.is_empty()
                || questions.len() > 5
                || questions
                    .iter()
                    .any(|question| question.text.trim().is_empty())
            {
                return Err("Invalid graph clarification question set".into());
            }
            None
        }
        GraphProposalOutcome::Proposal { proposal } => {
            let graph = GraphProposalResolution {
                agent: state.agent,
                answers: &state.version.answers,
                definitions: &state.definitions,
                graph_id: &format!("proposal:{}:{}", state.input.id, state.version.number),
                limits: &state.limits,
                permission: state.permission,
                request: &state.input.request,
                session_id: &state.input.session_id,
            }
            .resolve(proposal)?;
            let provider_call_limits =
                BTreeMap::from([(format!("{}:calls", graph.id), state.limits.provider_calls)]);
            Some(Box::new(StartExecutionGraphRequest {
                graph,
                provider_call_limits,
            }))
        }
    };

    Ok(GraphProposalVersionOutcome::Generated { launch, response })
}

fn proposal_error(error: impl std::fmt::Display) -> SessionError {
    SessionError::Operation(error.to_string())
}

#[cfg(test)]
#[path = "graph_proposal_test.rs"]
mod tests;
