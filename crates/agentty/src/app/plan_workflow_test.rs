use std::sync::Arc;

use ag_protocol::{AgentResponse, QuestionItem};
use ag_session::{
    AcceptTaskChainPlanRequest, AcceptedTaskPlan, AgentKind, AgentModel, AgentSelection,
    AnswerQuestionsRequest, BuiltinAgentDefinition, ContinueTaskChainRequest, CreateSessionRequest,
    DurableMessageRequest, ReviewRequest, Session, SessionBackend, SessionError, SessionId,
    SessionService, StartPlanThenImplementRequest, TaskAgentContext, TaskAgentHistory,
    TaskAgentTurn, TaskChain, TaskChainPhase,
};
use async_trait::async_trait;
use mockall::mock;

use crate::app::plan_workflow::{
    PlanWorkflowCompletion, PlanWorkflowRequest, apply_request, load_items, project_chain,
};
use crate::app::{AppEvent, AppRuntimeEvent};
use crate::presentation::app_mode::AppMode;
use crate::presentation::plan_workflow::{PlanEditorKind, PlanWorkflowIntent, PlanWorkflowState};
use crate::test_support;

mock! {
    ChainBackend {}
    #[async_trait]
    impl SessionBackend for ChainBackend {
        async fn create_session(&self, request: CreateSessionRequest) -> Result<SessionId, SessionError>;
        async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError>;
        async fn send_message(&self, session_id: &SessionId, message: String) -> Result<(), SessionError>;
        async fn submit_durable_message(&self, session_id: &SessionId, request: DurableMessageRequest) -> Result<(), SessionError>;
        async fn answer_questions(&self, session_id: &SessionId, request: AnswerQuestionsRequest) -> Result<(), SessionError>;
        async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError>;
        async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError>;
        async fn create_review_request(&self, session_id: &SessionId) -> Result<ReviewRequest, SessionError>;
        async fn start_plan_then_implement(&self, session_id: &SessionId, request: StartPlanThenImplementRequest) -> Result<(), SessionError>;
        async fn list_task_chains(&self, session_id: &SessionId) -> Result<Vec<TaskChain>, SessionError>;
        async fn get_agent_task_history(&self, task_id: &str) -> Result<Option<TaskAgentHistory>, SessionError>;
        async fn accept_task_chain_plan(&self, chain_id: &str, request: AcceptTaskChainPlanRequest) -> Result<(), SessionError>;
        async fn continue_task_chain(&self, chain_id: &str, request: ContinueTaskChainRequest) -> Result<(), SessionError>;
        async fn cancel_task_chain(&self, chain_id: &str) -> Result<(), SessionError>;
    }
}

fn chain(phase: TaskChainPhase) -> TaskChain {
    let start = StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        chain_id: "chain".into(),
        planner_attempt_id: "plan-v2".into(),
        user_request: "  Original request.\n".into(),
    };
    let planner = start.planner_request();

    TaskChain {
        accepted_plan: None,
        agent: start.agent,
        current_request: planner.clone(),
        id: start.chain_id.clone(),
        implementer_task_id: start.implementer_task_id(),
        phase,
        planner_task_id: planner.task_id,
        session_id: "session".into(),
        user_request: start.user_request,
    }
}

fn history(chain: &TaskChain) -> TaskAgentHistory {
    TaskAgentHistory {
        context: TaskAgentContext {
            assignment: chain.current_request.assignment,
            id: "context".into(),
            session_id: chain.session_id.clone(),
            task_id: chain.planner_task_id.clone(),
        },
        input: chain.current_request.input.clone(),
        turns: vec![TaskAgentTurn {
            assistant_message: serde_json::to_string(&AgentResponse {
                answer: "  Plan text.\n".into(),
                questions: vec![QuestionItem::new("Which scope?")],
                review_comment_outcomes: Vec::new(),
            })
            .expect("protocol"),
            attempt_id: "plan-v2".into(),
            input: ag_session::TaskAgentInput {
                clarification: Some("Narrow scope".into()),
                ..chain.current_request.input.clone()
            },
            input_tokens: 5,
            output_tokens: 2,
        }],
    }
}

#[test]
fn projection_labels_roles_and_distinguishes_every_explicit_continuation() {
    // Arrange
    let cases = [
        (TaskChainPhase::Planning, "Planner running", None, true),
        (
            TaskChainPhase::AwaitingPlanAcceptance,
            "Awaiting plan acceptance",
            Some(PlanEditorKind::RevisePlan),
            false,
        ),
        (
            TaskChainPhase::PlannerQuestion,
            "Planner needs clarification",
            Some(PlanEditorKind::AnswerPlanner),
            false,
        ),
        (
            TaskChainPhase::Implementing,
            "Implementer running",
            None,
            true,
        ),
        (
            TaskChainPhase::ImplementerQuestion,
            "Implementer needs clarification",
            Some(PlanEditorKind::AnswerImplementer),
            false,
        ),
        (
            TaskChainPhase::Failed,
            "Failed · explicit retry available",
            Some(PlanEditorKind::RetryPlanner),
            false,
        ),
        (
            TaskChainPhase::Canceled,
            "Canceled · explicit retry available",
            Some(PlanEditorKind::RetryPlanner),
            false,
        ),
        (
            TaskChainPhase::Succeeded,
            "Implementation succeeded",
            None,
            false,
        ),
    ];
    for (phase, status, continuation, can_cancel) in cases {
        let chain = chain(phase);
        let history = history(&chain);

        // Act
        let item = project_chain(chain, Some(&history), None).expect("projection");

        // Assert
        assert_eq!(item.status, status);
        assert_eq!(item.continuation, continuation);
        assert_eq!(item.can_cancel, can_cancel);
        assert_eq!(item.agent_label, "gpt-6.1-sol");
        assert_eq!(item.user_request, "  Original request.\n");
        assert!(item.body.contains("# Planner"));
        assert!(item.body.contains("Plan text."));
        assert!(item.body.contains("Which scope?"));
        assert!(item.body.contains("Clarification: Narrow scope"));
        assert!(item.body.contains("# Implementer\n\nNo response yet."));
        assert_eq!(
            item.accept_plan.is_some(),
            phase == TaskChainPhase::AwaitingPlanAcceptance
        );
    }
    for phase in [TaskChainPhase::Canceled, TaskChainPhase::Failed] {
        let mut chain = chain(phase);
        chain.accepted_plan = Some(AcceptedTaskPlan {
            attempt_id: "accepted-version".into(),
            task_id: chain.planner_task_id.clone(),
            text: "Accepted body".into(),
        });
        chain.current_request.assignment.definition = BuiltinAgentDefinition::Implementer;
        let item = project_chain(chain, None, None).expect("projection");
        assert_eq!(item.continuation, Some(PlanEditorKind::RetryImplementer));
        assert!(
            item.body
                .contains("Accepted plan version: accepted-version")
        );
    }
}

#[test]
fn projection_rejects_corrupt_responses_and_shows_latest_version_first() {
    // Arrange
    let chain = chain(TaskChainPhase::AwaitingPlanAcceptance);
    let mut history = history(&chain);
    history.turns[0].input.clarification = None;
    let mut newest = history.turns[0].clone();
    newest.attempt_id = "newest".into();
    newest.assistant_message =
        serde_json::to_string(&AgentResponse::plain("Latest plan")).expect("protocol");
    history.turns.push(newest);

    // Act
    let item = project_chain(chain.clone(), Some(&history), None).expect("projection");

    // Assert
    assert!(
        item.body.find("Latest plan").expect("new") < item.body.find("Plan text").expect("old")
    );
    history.turns[0].assistant_message = "broken JSON".into();
    assert!(
        project_chain(chain, Some(&history), None)
            .expect_err("corrupt")
            .contains("Cannot read Planner response")
    );
}

#[tokio::test]
async fn reducer_applies_only_matching_panel_completions_and_preserves_drafts_on_errors() {
    // Arrange: this tests reducer integration, not a state transformation
    // fixture.
    let (mut app, _root) = test_support::new_test_app().await;
    let item =
        project_chain(chain(TaskChainPhase::AwaitingPlanAcceptance), None, None).expect("item");
    let mut state = PlanWorkflowState {
        request_id: "request".into(),
        is_loading: true,
        chains: vec![item.clone()],
        ..PlanWorkflowState::default()
    };
    state.begin_editing(PlanEditorKind::RevisePlan);
    app.mode = AppMode::PlanWorkflow {
        session_id: "session".into(),
        state: Box::new(state),
    };
    let completion = PlanWorkflowCompletion {
        clear_editor: false,
        error: None,
        request_id: "request".into(),
        result: Ok(vec![item.clone()]),
        select_chain_id: Some(item.id.clone()),
        session_id: "session".into(),
    };

    // Act, Assert
    for (session_id, request_id) in [("other", "request"), ("session", "old")] {
        app.complete_plan_workflow(PlanWorkflowCompletion {
            session_id: session_id.into(),
            request_id: request_id.into(),
            ..completion.clone()
        });
        assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.is_loading));
    }
    app.apply_app_events(AppEvent::PlanWorkflowLoaded {
        completion: PlanWorkflowCompletion {
            result: Err("Admission failed".into()),
            ..completion.clone()
        },
    })
    .await;
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if !state.is_loading && state.error.as_deref() == Some("Admission failed") && state.editor.is_some())
    );
    app.complete_plan_workflow(completion.clone());
    assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.is_some()));
    app.complete_plan_workflow(PlanWorkflowCompletion {
        error: Some("Worker admission failed; persisted stage reloaded.".into()),
        ..completion.clone()
    });
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.error.as_deref() == Some("Worker admission failed; persisted stage reloaded."))
    );
    app.complete_plan_workflow(PlanWorkflowCompletion {
        clear_editor: true,
        select_chain_id: Some("removed".into()),
        ..completion.clone()
    });
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.is_none() && state.selected_index == 0)
    );
    if let AppMode::PlanWorkflow { state, .. } = &mut app.mode {
        state.begin_editing(PlanEditorKind::New);
    }
    app.complete_plan_workflow(PlanWorkflowCompletion {
        clear_editor: true,
        result: Err("Admitted, but refresh failed".into()),
        ..completion.clone()
    });
    assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.is_none()));
    app.complete_plan_workflow(PlanWorkflowCompletion {
        clear_editor: true,
        result: Ok(Vec::new()),
        ..completion
    });
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.as_ref().is_some_and(|editor| editor.kind == PlanEditorKind::New))
    );
    app.mode = AppMode::List;
    app.complete_plan_workflow(PlanWorkflowCompletion {
        clear_editor: true,
        error: None,
        request_id: "request".into(),
        result: Ok(Vec::new()),
        select_chain_id: None,
        session_id: "session".into(),
    });
    assert!(matches!(app.mode, AppMode::List));
}

async fn drive_panel(app: &mut crate::app::App) {
    while matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.is_loading) {
        match app.next_runtime_event().await {
            AppRuntimeEvent::App(event) => app.apply_app_events(*event).await,
            AppRuntimeEvent::Session(command) => app.apply_session_runtime_command(command).await,
        }
    }
}

#[tokio::test]
async fn empty_panel_queries_use_mailbox_and_reload_without_discarding_a_draft() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.open_plan_workflow(SessionId::from("missing"));

    // Act
    drive_panel(&mut app).await;
    if let AppMode::PlanWorkflow { state, .. } = &mut app.mode {
        state
            .editor
            .as_mut()
            .expect("editor")
            .input
            .insert_text("Unsaved request");
    }
    app.last_seen_session_update_versions
        .insert("missing".into(), 1);
    app.refresh_plan_workflow_if_needed();
    app.refresh_plan_workflow_if_needed();
    drive_panel(&mut app).await;

    // Assert
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.as_ref().is_some_and(|editor| editor.input.text() == "Unsaved request"))
    );
    app.refresh_plan_workflow_if_needed();
    assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if !state.is_loading));
    app.handle_plan_workflow_intent(PlanWorkflowIntent::Submit);
    assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if !state.is_loading));
    app.handle_plan_workflow_intent(PlanWorkflowIntent::Close);
    app.refresh_plan_workflow_if_needed();
    app.handle_plan_workflow_intent(PlanWorkflowIntent::Refresh);
    assert!(matches!(app.mode, AppMode::View { .. }));
}

#[tokio::test]
async fn action_guards_keep_missing_or_replaced_stage_inputs_out_of_admission() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    app.mode = AppMode::List;
    app.request_plan_workflow(PlanWorkflowRequest::Load);
    app.handle_plan_workflow_intent(PlanWorkflowIntent::Submit);
    let mut item =
        project_chain(chain(TaskChainPhase::AwaitingPlanAcceptance), None, None).expect("item");
    item.accept_plan = None;
    let mut state = PlanWorkflowState {
        chains: vec![item],
        ..PlanWorkflowState::default()
    };
    app.mode = AppMode::PlanWorkflow {
        session_id: "missing".into(),
        state: Box::new(state.clone()),
    };

    // Act, Assert
    for intent in [
        PlanWorkflowIntent::Accept,
        PlanWorkflowIntent::Cancel,
        PlanWorkflowIntent::Submit,
    ] {
        app.handle_plan_workflow_intent(intent);
        assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if !state.is_loading));
    }
    state.chains.clear();
    state.begin_editing(PlanEditorKind::RetryPlanner);
    app.mode = AppMode::PlanWorkflow {
        session_id: "missing".into(),
        state: Box::new(state.clone()),
    };
    for intent in [
        PlanWorkflowIntent::Accept,
        PlanWorkflowIntent::Cancel,
        PlanWorkflowIntent::Submit,
    ] {
        app.handle_plan_workflow_intent(intent);
    }
    state.chains.push(
        project_chain(chain(TaskChainPhase::AwaitingPlanAcceptance), None, None).expect("item"),
    );
    state.begin_editing(PlanEditorKind::RevisePlan);
    state.editor.as_mut().expect("editor").chain_id = Some("replaced".into());
    app.mode = AppMode::PlanWorkflow {
        session_id: "missing".into(),
        state: Box::new(state.clone()),
    };
    app.handle_plan_workflow_intent(PlanWorkflowIntent::Submit);
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.error.as_deref().is_some_and(|error| error.contains("Workflow changed")))
    );
    state.editor.as_mut().expect("editor").chain_id = Some("chain".into());
    state.chains[0].continuation = None;
    app.mode = AppMode::PlanWorkflow {
        session_id: "missing".into(),
        state: Box::new(state.clone()),
    };
    app.handle_plan_workflow_intent(PlanWorkflowIntent::Submit);
    assert!(matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.error.is_some()));
    state.is_loading = true;
    app.mode = AppMode::PlanWorkflow {
        session_id: "missing".into(),
        state: Box::new(state),
    };
    app.request_plan_workflow(PlanWorkflowRequest::Load);
    assert!(
        matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.request_id.is_empty())
    );
}

#[tokio::test]
async fn terminal_admission_failure_retains_composer_and_reloads_durable_phase() {
    // Arrange
    let (mut app, _root) = test_support::new_test_app().await;
    let session = test_support::SessionFixtureBuilder::new().build();
    let session_id = session.id.clone();
    app.sessions.push_session(session);
    for intent in [
        PlanWorkflowIntent::Submit,
        PlanWorkflowIntent::Accept,
        PlanWorkflowIntent::Cancel,
    ] {
        let mut item =
            project_chain(chain(TaskChainPhase::AwaitingPlanAcceptance), None, None).expect("item");
        item.can_cancel = true;
        let mut state = PlanWorkflowState {
            chains: vec![item],
            ..PlanWorkflowState::default()
        };
        if intent == PlanWorkflowIntent::Submit {
            state.begin_editing(PlanEditorKind::New);
            state
                .editor
                .as_mut()
                .expect("editor")
                .input
                .insert_text("Preserve this failed request");
        }
        app.mode = AppMode::PlanWorkflow {
            session_id: session_id.clone(),
            state: Box::new(state),
        };

        // Act
        app.handle_plan_workflow_intent(intent);
        drive_panel(&mut app).await;

        // Assert
        assert!(
            matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if !state.is_loading && state.error.is_some())
        );
        if intent == PlanWorkflowIntent::Submit {
            assert!(
                matches!(&app.mode, AppMode::PlanWorkflow { state, .. } if state.editor.as_ref().is_some_and(|editor| editor.input.text() == "Preserve this failed request"))
            );
        }
    }
}

#[tokio::test]
async fn terminal_requests_preserve_inputs_and_surface_each_admission_failure() {
    // Arrange
    for succeeds in [false, true] {
        let mut backend = MockChainBackend::new();
        let response = move || {
            if succeeds {
                Ok(())
            } else {
                Err(SessionError::Operation("Rejected".into()))
            }
        };
        backend
            .expect_start_plan_then_implement()
            .once()
            .returning(move |session_id, request| {
                assert_eq!(session_id.as_str(), "session");
                assert_eq!(request.user_request, "  Original request.\n");
                assert!(uuid::Uuid::parse_str(&request.chain_id).is_ok());
                assert!(uuid::Uuid::parse_str(&request.planner_attempt_id).is_ok());
                response()
            });
        backend
            .expect_accept_task_chain_plan()
            .once()
            .returning(move |chain_id, request| {
                assert_eq!(chain_id, "chain");
                assert_eq!(request.plan_attempt_id, "exact-version");
                assert!(uuid::Uuid::parse_str(&request.attempt_id).is_ok());
                response()
            });
        backend
            .expect_continue_task_chain()
            .times(2)
            .returning(move |chain_id, request| {
                assert_eq!(chain_id, "chain");
                assert!(uuid::Uuid::parse_str(&request.attempt_id).is_ok());
                if let Some(clarification) = request.clarification {
                    assert_eq!(clarification, "  Answer.\n");
                }
                response()
            });
        backend
            .expect_cancel_task_chain()
            .once()
            .returning(move |chain_id| {
                assert_eq!(chain_id, "chain");
                response()
            });
        let service = SessionService::new(Arc::new(backend));
        let requests = [
            PlanWorkflowRequest::Start {
                agent: chain(TaskChainPhase::Planning).agent,
                request: "  Original request.\n".into(),
            },
            PlanWorkflowRequest::Accept {
                chain_id: "chain".into(),
                plan_attempt_id: "exact-version".into(),
            },
            PlanWorkflowRequest::Continue {
                chain_id: "chain".into(),
                clarification: "  Answer.\n".into(),
            },
            PlanWorkflowRequest::Continue {
                chain_id: "chain".into(),
                clarification: " \n".into(),
            },
            PlanWorkflowRequest::Cancel {
                chain_id: "chain".into(),
            },
        ];

        for request in requests {
            // Act
            let result = apply_request(&service, &SessionId::from("session"), request).await;

            // Assert
            if succeeds {
                assert!(result.expect("admitted").is_some());
            } else {
                assert!(result.expect_err("failure").contains("Rejected"));
            }
        }
        assert_eq!(
            apply_request(
                &service,
                &SessionId::from("session"),
                PlanWorkflowRequest::Load
            )
            .await,
            Ok(None)
        );
    }
}

#[tokio::test]
async fn snapshot_loading_propagates_list_and_role_history_failures() {
    // Arrange
    for failure_at in 0..4 {
        let mut backend = MockChainBackend::new();
        backend
            .expect_list_task_chains()
            .once()
            .returning(move |_| {
                if failure_at == 0 {
                    Err(SessionError::Operation("List failed".into()))
                } else {
                    Ok(vec![chain(TaskChainPhase::AwaitingPlanAcceptance)])
                }
            });
        if failure_at > 0 {
            backend
                .expect_get_agent_task_history()
                .times(if failure_at == 1 { 1 } else { 2 })
                .returning(move |task_id| {
                    if failure_at == 1 || (failure_at == 2 && task_id.ends_with("implementer")) {
                        Err(SessionError::Operation("History failed".into()))
                    } else {
                        Ok(None)
                    }
                });
        }
        let service = SessionService::new(Arc::new(backend));

        // Act
        let result = load_items(&service, &SessionId::from("session")).await;

        // Assert
        if failure_at == 3 {
            assert_eq!(result.expect("snapshot").len(), 1);
        } else {
            assert!(result.expect_err("failure").contains("failed"));
        }
    }
}
