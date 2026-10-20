//! Terminal projection of the shared planning coordinator's programmatic API.

use std::fmt::Write as _;

use ag_protocol::AgentResponse;
use ag_session::{
    AcceptTaskChainPlanRequest, AgentSelection, BuiltinAgentDefinition, ContinueTaskChainRequest,
    SessionService, StartPlanThenImplementRequest, TaskAgentHistory, TaskChain, TaskChainPhase,
};
use uuid::Uuid;

use crate::app::{App, AppEvent};
use crate::domain::session::SessionId;
use crate::presentation::app_mode::AppMode;
use crate::presentation::plan_workflow::{PlanEditorKind, PlanWorkflowIntent, PlanWorkflowItem};

/// One asynchronously loaded snapshot, applied only to its originating panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlanWorkflowCompletion {
    pub(crate) clear_editor: bool,
    pub(crate) error: Option<String>,
    pub(crate) request_id: String,
    pub(crate) result: Result<Vec<PlanWorkflowItem>, String>,
    pub(crate) select_chain_id: Option<String>,
    pub(crate) session_id: SessionId,
}

enum PlanWorkflowRequest {
    Load,
    Start {
        agent: AgentSelection,
        request: String,
    },
    Accept {
        chain_id: String,
        plan_attempt_id: String,
    },
    Continue {
        chain_id: String,
        clarification: String,
        expected_attempt_id: String,
    },
    Cancel {
        chain_id: String,
    },
}

impl App {
    /// Opens isolated task output without modifying the ordinary transcript.
    pub(crate) fn open_plan_workflow(&mut self, session_id: SessionId) {
        if !self.experimental_multi_agent {
            return;
        }

        self.mode = AppMode::PlanWorkflow {
            session_id,
            state: Box::default(),
        };
        self.request_plan_workflow(PlanWorkflowRequest::Load);
    }

    /// Refreshes only after a reducer-observed worker update, retaining drafts.
    pub(crate) fn refresh_plan_workflow_if_needed(&mut self) {
        let AppMode::PlanWorkflow { session_id, state } = &self.mode else {
            return;
        };
        if !state.is_loading
            && state.update_version != self.session_update_version(session_id.as_str())
        {
            self.request_plan_workflow(PlanWorkflowRequest::Load);
        }
    }

    pub(crate) fn handle_plan_workflow_intent(&mut self, intent: PlanWorkflowIntent) {
        let AppMode::PlanWorkflow { session_id, state } = &mut self.mode else {
            return;
        };
        let request = match intent {
            PlanWorkflowIntent::Close => {
                self.mode = AppMode::View {
                    session_id: session_id.clone(),
                    scroll_offset: None,
                };

                return;
            }
            PlanWorkflowIntent::Refresh => PlanWorkflowRequest::Load,
            PlanWorkflowIntent::Accept => {
                let Some(chain) = state.selected() else {
                    return;
                };
                let Some((_, plan_attempt_id)) = &chain.accept_plan else {
                    return;
                };

                PlanWorkflowRequest::Accept {
                    chain_id: chain.id.clone(),
                    plan_attempt_id: plan_attempt_id.clone(),
                }
            }
            PlanWorkflowIntent::Cancel => {
                let Some(chain) = state.selected().filter(|chain| chain.can_cancel) else {
                    return;
                };

                PlanWorkflowRequest::Cancel {
                    chain_id: chain.id.clone(),
                }
            }
            PlanWorkflowIntent::Submit => {
                let Some(editor) = &state.editor else {
                    return;
                };
                if editor.kind == PlanEditorKind::New {
                    let Some(session) = self.sessions.session_for_id(session_id.as_str()) else {
                        return;
                    };

                    PlanWorkflowRequest::Start {
                        agent: session.agent,
                        request: editor.input.text().to_string(),
                    }
                } else {
                    let Some(chain) = state.selected() else {
                        return;
                    };
                    if editor.chain_id.as_deref() != Some(chain.id.as_str())
                        || chain.continuation != Some(editor.kind)
                        || editor.expected_attempt_id != chain.current_attempt_id
                    {
                        state.error = Some(
                            "Workflow changed. Discard this draft and inspect the current stage."
                                .into(),
                        );

                        return;
                    }

                    PlanWorkflowRequest::Continue {
                        chain_id: chain.id.clone(),
                        clarification: editor.input.text().to_string(),
                        expected_attempt_id: editor.expected_attempt_id.clone(),
                    }
                }
            }
        };
        self.request_plan_workflow(request);
    }

    fn request_plan_workflow(&mut self, request: PlanWorkflowRequest) {
        let AppMode::PlanWorkflow { session_id, state } = &mut self.mode else {
            return;
        };
        let version = self
            .last_seen_session_update_versions
            .get(session_id.as_str())
            .copied()
            .unwrap_or_default();
        if state.is_loading {
            return;
        }
        let request_id = Uuid::new_v4().to_string();
        state.request_id.clone_from(&request_id);
        state.update_version = version;
        state.is_loading = true;
        state.is_submitting = !matches!(&request, PlanWorkflowRequest::Load);
        if !matches!(&request, PlanWorkflowRequest::Load) {
            state.error = None;
        }
        let session_id = session_id.clone();
        let service = self.session_view_service();
        let services = self.services.clone();
        tokio::spawn(async move {
            let is_mutation = !matches!(&request, PlanWorkflowRequest::Load);
            let action = apply_request(&service, &session_id, request).await;
            let (select_chain_id, error) = match action {
                Ok(select_id) => (select_id, None),
                Err(error) => (None, Some(error)),
            };
            let clear_editor = is_mutation && error.is_none();
            let result = load_items(&service, &session_id).await;
            services.emit_app_event(AppEvent::PlanWorkflowLoaded {
                completion: PlanWorkflowCompletion {
                    clear_editor,
                    error,
                    request_id,
                    result,
                    select_chain_id,
                    session_id,
                },
            });
        });
        self.mark_dirty();
    }

    pub(crate) fn complete_plan_workflow(&mut self, completion: PlanWorkflowCompletion) {
        let AppMode::PlanWorkflow { session_id, state } = &mut self.mode else {
            return;
        };
        if *session_id != completion.session_id || state.request_id != completion.request_id {
            return;
        }
        state.is_loading = false;
        state.is_submitting = false;
        if completion.error.is_some() {
            state.error = completion.error;
        }
        if completion.clear_editor {
            state.editor = None;
        }
        match completion.result {
            Ok(chains) => {
                state.replace_chains(chains);
                if let Some(id) = completion.select_chain_id {
                    state.scroll_offset = 0;
                    state.selected_index = state
                        .chains
                        .iter()
                        .position(|chain| chain.id == id)
                        .unwrap_or_default();
                }
                if state.chains.is_empty() && state.editor.is_none() && state.error.is_none() {
                    state.begin_editing(PlanEditorKind::New);
                }
            }
            Err(error) => state.error = Some(error),
        }
    }
}

async fn apply_request(
    service: &SessionService,
    session_id: &SessionId,
    request: PlanWorkflowRequest,
) -> Result<Option<String>, String> {
    let select_id = match request {
        PlanWorkflowRequest::Load => None,
        PlanWorkflowRequest::Start { agent, request } => {
            let chain_id = Uuid::new_v4().to_string();
            service
                .start_plan_then_implement(
                    session_id,
                    StartPlanThenImplementRequest {
                        agent,
                        chain_id: chain_id.clone(),
                        planner_attempt_id: Uuid::new_v4().to_string(),
                        user_request: request,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;

            Some(chain_id)
        }
        PlanWorkflowRequest::Accept {
            chain_id,
            plan_attempt_id,
        } => {
            service
                .accept_task_chain_plan(
                    &chain_id,
                    AcceptTaskChainPlanRequest {
                        plan_attempt_id,
                        attempt_id: Uuid::new_v4().to_string(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;

            Some(chain_id)
        }
        PlanWorkflowRequest::Continue {
            chain_id,
            clarification,
            expected_attempt_id,
        } => {
            service
                .continue_task_chain(
                    &chain_id,
                    ContinueTaskChainRequest {
                        attempt_id: Uuid::new_v4().to_string(),
                        clarification: (!clarification.trim().is_empty()).then_some(clarification),
                        expected_attempt_id,
                    },
                )
                .await
                .map_err(|error| error.to_string())?;

            Some(chain_id)
        }
        PlanWorkflowRequest::Cancel { chain_id } => {
            service
                .cancel_task_chain(&chain_id)
                .await
                .map_err(|error| error.to_string())?;

            Some(chain_id)
        }
    };

    Ok(select_id)
}

async fn load_items(
    service: &SessionService,
    session_id: &SessionId,
) -> Result<Vec<PlanWorkflowItem>, String> {
    let chains = service
        .list_task_chains(session_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut items = Vec::with_capacity(chains.len());
    for chain in chains {
        let planner = service
            .get_agent_task_history(&chain.planner_task_id)
            .await
            .map_err(|error| error.to_string())?;
        let implementer = service
            .get_agent_task_history(&chain.implementer_task_id)
            .await
            .map_err(|error| error.to_string())?;
        items.push(project_chain(
            chain,
            planner.as_ref(),
            implementer.as_ref(),
        )?);
    }

    Ok(items)
}

fn project_chain(
    chain: TaskChain,
    planner: Option<&TaskAgentHistory>,
    implementer: Option<&TaskAgentHistory>,
) -> Result<PlanWorkflowItem, String> {
    let (status, continuation) = match chain.phase {
        TaskChainPhase::Planning => ("Planner running", None),
        TaskChainPhase::AwaitingPlanAcceptance => {
            ("Awaiting plan acceptance", Some(PlanEditorKind::RevisePlan))
        }
        TaskChainPhase::PlannerQuestion => (
            "Planner needs clarification",
            Some(PlanEditorKind::AnswerPlanner),
        ),
        TaskChainPhase::Implementing => ("Implementer running", None),
        TaskChainPhase::ImplementerQuestion => (
            "Implementer needs clarification",
            Some(PlanEditorKind::AnswerImplementer),
        ),
        TaskChainPhase::Failed | TaskChainPhase::Canceled => {
            let role = chain.current_request.assignment.definition;
            let retry = if role == BuiltinAgentDefinition::Planner {
                PlanEditorKind::RetryPlanner
            } else {
                PlanEditorKind::RetryImplementer
            };
            (
                if chain.phase == TaskChainPhase::Failed {
                    "Failed · explicit retry required"
                } else {
                    "Canceled · retry after the worker stops"
                },
                Some(retry),
            )
        }
        TaskChainPhase::Succeeded => ("Implementation succeeded", None),
    };
    let mut body = String::new();
    let planner_label = if chain.accepted_plan.is_some() {
        "Planner · accepted plan"
    } else {
        "Planner"
    };
    match chain.current_request.assignment.definition {
        BuiltinAgentDefinition::Planner => {
            append_history(&mut body, planner_label, planner)?;
            append_history(&mut body, "Implementer", implementer)?;
        }
        BuiltinAgentDefinition::Implementer => {
            append_history(&mut body, "Implementer", implementer)?;
            append_history(&mut body, planner_label, planner)?;
        }
    }
    let _ = writeln!(body, "# Original request\n\n{}\n", chain.user_request);
    let accept_plan = (chain.phase == TaskChainPhase::AwaitingPlanAcceptance).then(|| {
        (
            chain.planner_task_id,
            chain.current_request.attempt_id.clone(),
        )
    });

    Ok(PlanWorkflowItem {
        accept_plan,
        agent_label: chain.agent.model().as_str().to_string(),
        body,
        can_cancel: matches!(
            chain.phase,
            TaskChainPhase::Planning | TaskChainPhase::Implementing
        ),
        continuation,
        current_attempt_id: chain.current_request.attempt_id.clone(),
        id: chain.id,
        status: status.into(),
        user_request: chain.user_request,
    })
}

fn append_history(
    body: &mut String,
    role: &str,
    history: Option<&TaskAgentHistory>,
) -> Result<(), String> {
    let _ = writeln!(body, "# {role}\n");
    let Some(history) = history else {
        body.push_str("No response yet.\n\n");

        return Ok(());
    };
    for (index, turn) in history.turns.iter().enumerate().rev() {
        let response = serde_json::from_str::<AgentResponse>(&turn.assistant_message)
            .map_err(|error| format!("Cannot read {role} response: {error}"))?;
        let _ = writeln!(body, "## Response {}\n", index + 1);
        let _ = writeln!(body, "{}\n", response.to_display_text());
        if let Some(clarification) = &turn.input.clarification {
            let _ = writeln!(body, "Clarification: {clarification}\n");
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "plan_workflow_test.rs"]
mod tests;
