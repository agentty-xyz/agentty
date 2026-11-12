use std::sync::Arc;

use ag_contracts::{
    AgentRequestKind, OneShotError, OneShotSubmission, PermissionMode, SessionStats,
};
use ag_protocol::{AgentResponse, GraphProposalOutcome, GraphProposalResponse};
use ag_session::test_support::{graph_proposal, graph_proposal_state};
use ag_session::{GraphProposalState, GraphProposalVersionOutcome, SessionId};
use ag_worker::MockRunClient;
use ag_worker::test_support::{AppServerTurnResponse, MockAppServerClient};
use tokio::sync::watch;

use super::generate;
use crate::app::execution_graph::tests::seed_session;
use crate::infra::db::AppRepositories;
use crate::test_support::{new_test_app, new_test_app_with_clients, test_app_clients};

#[path = "graph_proposal_boundary_test.rs"]
mod boundary;

#[tokio::test]
async fn generation_uses_read_only_worker_contract_and_captured_inputs() {
    // Arrange
    let state = graph_proposal_state("preview", "session");
    let mut client = MockRunClient::new();
    let response = GraphProposalResponse {
        outcome: GraphProposalOutcome::Proposal {
            proposal: graph_proposal(),
        },
    };
    client
        .expect_submit()
        .once()
        .withf(|request| {
            request.request_kind == AgentRequestKind::GraphProposal
                && request.permission_mode == PermissionMode::ReadOnly
                && request
                    .prompt
                    .contains("Available agent definitions (JSON data)")
                && request.prompt.contains("inspector")
                && request.prompt.contains("Inspect the requested change")
                && request
                    .provider_call_budget
                    .as_ref()
                    .is_some_and(|budget| budget.remaining() == 3)
        })
        .return_once(move |_| {
            Ok(OneShotSubmission {
                response: AgentResponse::plain(serde_json::json!(response).to_string()),
                stats: SessionStats::default(),
            })
        });

    // Act
    let outcome = generate(&state, "workspace".into(), &client).await;

    // Assert
    let launch = match outcome {
        GraphProposalVersionOutcome::Generated { launch, .. } => launch,
        _ => None,
    }
    .expect("Expected resolved graph");
    assert_eq!(launch.graph.id, "proposal:preview:1");
    assert_eq!(launch.graph.nodes[0].agent, state.agent);
    assert_eq!(launch.provider_call_limits["proposal:preview:1:calls"], 48);
}

#[tokio::test]
async fn graph_proposal_generation_rejects_worker_wire_and_resolution_failures() {
    for case in [
        "worker",
        "json",
        "bytes",
        "empty-questions",
        "blank-question",
        "many-questions",
        "clarification",
        "unknown-agent",
    ] {
        // Arrange
        let mut state = graph_proposal_state("preview", "session");
        let mut client = MockRunClient::new();
        let answer = match case {
            "json" => "not JSON".to_string(),
            "bytes" => {
                state.limits.bytes = 1;
                "oversized".into()
            }
            "unknown-agent" => {
                let mut proposal = graph_proposal();
                proposal.nodes[0].agent_id = "missing".into();
                serde_json::json!(GraphProposalResponse {
                    outcome: GraphProposalOutcome::Proposal { proposal }
                })
                .to_string()
            }
            _ => {
                let questions = match case {
                    "empty-questions" => Vec::new(),
                    "blank-question" => vec![ag_protocol::QuestionItem::new(" ")],
                    "many-questions" => vec![ag_protocol::QuestionItem::new("Target?"); 6],
                    _ => vec![ag_protocol::QuestionItem::new("Target?")],
                };
                serde_json::json!(GraphProposalResponse {
                    outcome: GraphProposalOutcome::Clarification { questions }
                })
                .to_string()
            }
        };
        client.expect_submit().once().return_once(move |_| {
            if case == "worker" {
                Err(OneShotError::new("unavailable"))
            } else {
                Ok(OneShotSubmission {
                    response: AgentResponse::plain(answer),
                    stats: SessionStats::default(),
                })
            }
        });

        // Act
        let outcome = generate(&state, "workspace".into(), &client).await;

        // Assert
        assert_eq!(
            matches!(
                outcome,
                GraphProposalVersionOutcome::Generated { launch: None, .. }
            ),
            case == "clarification",
            "{case}"
        );
        assert_eq!(
            matches!(outcome, GraphProposalVersionOutcome::Failed(_)),
            case != "clarification",
            "{case}"
        );
    }
}

fn proposal_provider() -> MockAppServerClient {
    let mut provider = MockAppServerClient::new();
    provider
        .expect_run_isolated_turn()
        .times(3)
        .returning(|request, _| {
            assert_eq!(request.permission_mode, PermissionMode::ReadOnly);
            let answer = if request.request_kind == AgentRequestKind::GraphProposal {
                assert!(
                    request
                        .prompt
                        .contains("Propose an initial execution graph")
                );
                let outcome = if request.prompt.contains("Local  only") {
                    let mut proposal = graph_proposal();
                    proposal.nodes[0].agent_id = "planner".into();
                    GraphProposalOutcome::Proposal { proposal }
                } else {
                    GraphProposalOutcome::Clarification {
                        questions: vec![ag_protocol::QuestionItem::new("Which execution target?")],
                    }
                };
                serde_json::json!(GraphProposalResponse { outcome }).to_string()
            } else {
                assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
                assert!(request.prompt.contains("Which execution target?"));
                assert!(request.prompt.contains("Local  only"));
                assert!(
                    request
                        .prompt
                        .contains("Complete clarification answer history (JSON data)")
                );
                r#"{"answer":"Graph node finished"}"#.into()
            };
            Box::pin(async move {
                Ok(AppServerTurnResponse {
                    assistant_message: answer,
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 1,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
    provider
        .expect_shutdown_session()
        .returning(|_| Box::pin(async {}));

    provider
}

#[tokio::test]
async fn graph_proposal_public_api_uses_worker_then_launches_only_after_approval() {
    // Arrange
    let clients = test_app_clients().with_app_server_client_override(Arc::new(proposal_provider()));
    let (mut app, _directory) = new_test_app_with_clients(clients).await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_multi_agent = true;
    let service = app.session_service();
    let input = graph_proposal_state("preview", "sess").input;
    let wake = app.services.graph_wake.subscribe();

    // Act
    app.drive_session_request(service.generate_graph_proposal(input.clone()))
        .await
        .expect("generate");
    let clarification = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_proposal(app.services.db(), wake.clone(), "preview"),
    )
    .await
    .expect("generation deadline");
    assert!(matches!(
        clarification.version.outcome,
        GraphProposalVersionOutcome::Generated { launch: None, .. }
    ));
    app.drive_session_request(service.answer_graph_proposal(
        "preview",
        1,
        vec!["Local  only".into()],
    ))
    .await
    .expect("answer clarification");
    let state = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_proposal(app.services.db(), wake.clone(), "preview"),
    )
    .await
    .expect("resolved preview deadline");
    let queried = app
        .drive_session_request(service.get_graph_proposal("preview"))
        .await
        .expect("query")
        .expect("proposal");
    assert_eq!(queried.version, state.version);
    assert!(state.version.utility_run_id.is_some());
    assert_unapproved_launches_rejected(&app, &state).await;
    assert_eq!(
        app.services
            .db()
            .graphs()
            .list_graphs("sess")
            .await
            .expect("no graphs"),
        []
    );
    app.drive_session_request(service.generate_graph_proposal(input))
        .await
        .expect("duplicate generation");
    assert_changed_permission_rejected(&mut app).await;
    app.drive_session_request(service.approve_graph_proposal("preview", 2))
        .await
        .expect("approval");
    let graph_id = "proposal:preview:2";
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_graph(app.services.db(), wake, graph_id),
    )
    .await
    .expect("execution deadline");
    app.drive_session_request(service.approve_graph_proposal("preview", 2))
        .await
        .expect("duplicate approval");

    // Assert
    assert_eq!(
        app.services
            .db()
            .graphs()
            .list_executions(graph_id, std::num::NonZeroU64::MIN)
            .await
            .expect("executions")
            .len(),
        1
    );
    assert!(
        app.drive_session_request(service.regenerate_graph_proposal("preview", 2))
            .await
            .is_err()
    );
    assert!(
        app.drive_session_request(service.cancel_graph_proposal("preview", 2))
            .await
            .is_err()
    );
    app.services.stop_graph_drivers(None);
    if let GraphProposalVersionOutcome::Generated {
        launch: Some(launch),
        ..
    } = &state.version.outcome
    {
        assert!(app.validate_proposal_launch(launch).await.is_err());
    }
    app.services.wait_for_cleanup_tasks(None).await;
}

async fn assert_unapproved_launches_rejected(app: &crate::app::App, state: &GraphProposalState) {
    let launch = match &state.version.outcome {
        GraphProposalVersionOutcome::Generated { launch, .. } => launch.clone(),
        _ => None,
    }
    .expect("resolved preview");
    assert!(app.validate_proposal_launch(&launch).await.is_err());
    for id in [
        "proposal:invalid",
        "proposal:preview:nope",
        "proposal:missing:1",
    ] {
        let mut invalid = launch.as_ref().clone();
        invalid.graph.id = id.into();
        assert!(
            app.validate_proposal_launch(&invalid).await.is_err(),
            "{id}"
        );
    }
}

async fn assert_changed_permission_rejected(app: &mut crate::app::App) {
    app.services
        .db()
        .sessions()
        .update_session_permission_mode("sess", PermissionMode::ReadOnly)
        .await
        .expect("restrict permission");
    let service = app.session_service();
    assert!(
        app.drive_session_request(service.approve_graph_proposal("preview", 2))
            .await
            .is_err()
    );
    app.services
        .db()
        .sessions()
        .update_session_permission_mode("sess", PermissionMode::AutoEdit)
        .await
        .expect("restore permission");
}

#[tokio::test]
async fn graph_mode_submits_literal_requests_without_subcommands() {
    for text in [
        "Inspect",
        "query the architecture",
        "approve the design",
        "/graph cancel preview 1",
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_multi_agent = true;
        let mut client = MockRunClient::new();
        client.expect_submit().times(0..=1).returning(|_| {
            Ok(OneShotSubmission {
                response: AgentResponse::plain(
                    serde_json::json!(GraphProposalResponse {
                        outcome: GraphProposalOutcome::Proposal {
                            proposal: graph_proposal()
                        }
                    })
                    .to_string(),
                ),
                stats: SessionStats::default(),
            })
        });
        app.services.set_run_client(Arc::new(client));

        // Act
        let outcome = app
            .submit_graph_request(crate::app::prompt_intent::PromptSubmission {
                session_id: SessionId::from("sess"),
                session_mode: crate::app::prompt_intent::PromptSessionMode::Existing,
                prompt: ag_protocol::TurnPrompt::from_text(text.into()),
            })
            .await;

        // Assert
        assert!(matches!(
            outcome,
            crate::app::prompt_intent::PromptWorkflowOutcome::ShowSession { .. }
        ));
        let states = app
            .services
            .db()
            .graph_proposals()
            .list(&SessionId::from("sess"))
            .await
            .expect("proposals");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].input.request.text, text);
        let messages = app
            .services
            .db()
            .sessions()
            .load_session_messages("sess")
            .await
            .expect("messages");
        let position = |kind: &str, content: &str| {
            messages
                .iter()
                .position(|message| message.kind == kind && message.content == content)
        };
        let prompt = position("user_prompt", text).expect("request echoed as user prompt");
        let notice =
            position("workflow_notice", "Generating execution graph…").expect("generation notice");
        assert!(prompt < notice, "request precedes the generation notice");
        app.services.wait_for_cleanup_tasks(None).await;
    }
}

#[tokio::test]
async fn created_proposal_refreshes_an_empty_graph_sidebar_without_a_session_update() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_multi_agent = true;
    let mut client = MockRunClient::new();
    client
        .expect_submit()
        .times(0..=1)
        .returning(|_| Err(OneShotError::new("offline")));
    app.services.set_run_client(Arc::new(client));
    let now = std::time::Instant::now();
    app.graph_chat.complete("sess".into(), 0, now, Ok(None));
    let request = ag_session::GenerateGraphProposalRequest {
        id: "api-preview".into(),
        request: ag_protocol::TurnPrompt::from_text("Inspect".into()),
        session_id: "sess".into(),
    };

    // Act
    app.generate_api_graph_proposal(request)
        .await
        .expect("generation admitted");

    // Assert
    assert!(app.graph_chat.needs_refresh("sess", 0, now));
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn graph_proposal_admission_rejects_invalid_requests_without_generation() {
    for case in [
        "feature",
        "empty",
        "attachments",
        "large",
        "missing-session",
        "conflict",
    ] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_multi_agent = case != "feature";
        let mut input = graph_proposal_state("preview", "sess").input;
        match case {
            "empty" => input.request.text.clear(),
            "attachments" => input
                .request
                .attachments
                .push(ag_protocol::TurnPromptAttachment {
                    local_image_path: "image.png".into(),
                    placeholder: "[Image #1]".into(),
                }),
            "large" => input.request.text = "x".repeat(70_000),
            "missing-session" => input.session_id = SessionId::from("missing"),
            "conflict" => {
                app.services
                    .db()
                    .graph_proposals()
                    .create(&graph_proposal_state("preview", "sess"))
                    .await
                    .expect("reservation");
                input.request.text = "different".into();
            }
            _ => {}
        }
        let service = app.session_service();

        // Act / Assert
        assert!(
            app.drive_session_request(service.generate_graph_proposal(input))
                .await
                .is_err(),
            "{case}"
        );
        assert!(
            app.services
                .proposal_generations
                .lock()
                .expect("generators")
                .is_empty()
        );
        app.services.wait_for_cleanup_tasks(None).await;
    }
}

#[tokio::test]
async fn graph_proposal_answers_regeneration_and_discard_keep_the_original_capture() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_multi_agent = true;
    let state = graph_proposal_state("preview", "sess");
    app.services
        .db()
        .graph_proposals()
        .create(&state)
        .await
        .expect("capture");
    let clarification = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Clarification {
                questions: vec![ag_protocol::QuestionItem::new("Target?")],
            },
        },
    };
    app.services
        .db()
        .graph_proposals()
        .finish("preview", 1, &clarification)
        .await
        .expect("questions");
    let mut client = MockRunClient::new();
    client
        .expect_submit()
        .times(2)
        .withf(|request| request.prompt.contains("Local"))
        .returning(|_| {
            Ok(OneShotSubmission {
                response: AgentResponse::plain(
                    serde_json::json!(GraphProposalResponse {
                        outcome: GraphProposalOutcome::Proposal {
                            proposal: graph_proposal()
                        }
                    })
                    .to_string(),
                ),
                stats: SessionStats::default(),
            })
        });
    app.services.set_run_client(Arc::new(client));
    let service = app.session_service();
    let wake = app.services.graph_wake.subscribe();

    // Act
    app.drive_session_request(service.answer_graph_proposal("preview", 1, vec!["Local".into()]))
        .await
        .expect("answer");
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_proposal(app.services.db(), wake.clone(), "preview"),
    )
    .await
    .expect("generation");
    app.drive_session_request(service.regenerate_graph_proposal("preview", 2))
        .await
        .expect("regenerate");
    let third = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_proposal(app.services.db(), wake, "preview"),
    )
    .await
    .expect("regeneration");
    app.drive_session_request(service.cancel_graph_proposal("preview", 3))
        .await
        .expect("cancel");

    // Assert
    assert_eq!(second.agent, state.agent);
    assert_eq!(third.definitions, state.definitions);
    assert_eq!(third.version.answers, second.version.answers);
    assert_eq!(third.limits, state.limits);
    assert!(
        app.drive_session_request(service.get_graph_proposal("preview"))
            .await
            .expect("query")
            .expect("state")
            .version
            .is_cancelled
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn graph_proposal_cancels_while_waiting_for_workspace_access() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_multi_agent = true;
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let lease = app
        .services
        .graph_resources
        .environment(&ag_session::EnvId::SessionWorktree("sess".into()))
        .write_owned()
        .await;
    let service = app.session_service();

    // Act
    app.drive_session_request(
        service.generate_graph_proposal(graph_proposal_state("preview", "sess").input),
    )
    .await
    .expect("reserve");
    app.drive_session_request(service.cancel_graph_proposal("preview", 1))
        .await
        .expect("cancel");
    drop(lease);

    // Assert
    assert!(
        app.services
            .proposal_generations
            .lock()
            .expect("generators")
            .is_empty()
    );
    assert!(
        app.drive_session_request(service.get_graph_proposal("preview"))
            .await
            .expect("query")
            .expect("state")
            .version
            .is_cancelled
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

#[test]
fn graph_proposal_preview_explains_outcomes_and_separates_output_acceptance() {
    for case in [
        "generating",
        "failed",
        "cancelled",
        "approved",
        "questions",
        "proposal",
        "unresolved",
    ] {
        // Arrange
        let mut state = graph_proposal_state("preview", "sess");
        match case {
            "failed" => {
                state.version.outcome = GraphProposalVersionOutcome::Failed("Unavailable".into());
            }
            "cancelled" => state.version.is_cancelled = true,
            "approved" => {
                state.version.approval = Some(ag_session::GraphProposalApproval {
                    graph_id: "published".into(),
                    limits: state.limits.clone(),
                    version: 1,
                });
            }
            "questions" => {
                state.version.outcome = GraphProposalVersionOutcome::Generated {
                    launch: None,
                    response: GraphProposalResponse {
                        outcome: GraphProposalOutcome::Clarification {
                            questions: vec![ag_protocol::QuestionItem::new("Target?")],
                        },
                    },
                }
            }
            "proposal" | "unresolved" => {
                let mut graph = crate::app::execution_graph::tests::graph();
                graph.nodes[0].definition.prompt =
                    "Saved instructions\nUse the captured definition.".into();
                state.version.outcome = GraphProposalVersionOutcome::Generated {
                    launch: (case == "proposal").then(|| {
                        Box::new(ag_session::StartExecutionGraphRequest {
                            graph,
                            provider_call_limits: std::collections::BTreeMap::new(),
                        })
                    }),
                    response: GraphProposalResponse {
                        outcome: GraphProposalOutcome::Proposal {
                            proposal: graph_proposal(),
                        },
                    },
                }
            }
            _ => {}
        }

        // Act
        let preview = crate::presentation::graph_chat::proposal_preview(&state);

        // Assert
        assert!(preview.contains("Graph proposal preview v1"));
        assert!(preview.contains("Limits:"));
        let expected = match case {
            "generating" => "sidebar updates automatically",
            "failed" => "Unavailable",
            "cancelled" => "Cancelled",
            "approved" => "output acceptance remains separate",
            "questions" => "Clarification required",
            "proposal" => "Preview awaiting exact-version approval",
            _ => "Reply in chat to approve, revise, or cancel",
        };
        assert!(preview.contains(expected), "{case}");
        if case == "proposal" {
            assert!(preview.contains(
                "Captured instructions:\nSaved instructions\nUse the captured definition."
            ));
        }
    }
}

async fn wait_proposal(
    db: &AppRepositories,
    mut wake: watch::Receiver<u64>,
    id: &str,
) -> GraphProposalState {
    loop {
        wake.borrow_and_update();
        let state = db
            .graph_proposals()
            .get(id, None)
            .await
            .expect("query")
            .expect("proposal");
        if !matches!(
            state.version.outcome,
            GraphProposalVersionOutcome::Generating
        ) {
            return state;
        }
        wake.changed().await.expect("generation wake");
    }
}

async fn wait_graph(db: &AppRepositories, mut wake: watch::Receiver<u64>, id: &str) {
    loop {
        wake.borrow_and_update();
        let progress = db
            .graphs()
            .progress(id)
            .await
            .expect("progress")
            .expect("published graph");
        if progress.status == ag_session::ExecutionGraphStatus::Completed {
            return;
        }
        wake.changed().await.expect("graph wake");
    }
}
