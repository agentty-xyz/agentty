use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use ag_contracts::OneShotError;
use ag_protocol::{GraphProposalOutcome, GraphProposalResponse};
use ag_session::test_support::{graph_proposal, graph_proposal_state};
use ag_session::{
    GraphProposalResolution, GraphProposalVersionOutcome, StartExecutionGraphRequest,
};
use ag_store::{DbError, MockGraphProposalRepository};
use ag_worker::MockRunClient;
use tokio_util::sync::CancellationToken;

use super::super::{ProposalGeneration, finish_proposal_generation};
use crate::app::execution_graph::tests::seed_session;
use crate::domain::session::Status;
use crate::infra::agent_profile::MockAgentCatalogClient;
use crate::test_support::new_test_app;

#[test]
fn old_generation_cleanup_preserves_regenerated_cancellation() {
    // Arrange
    let old = CancellationToken::new();
    let newer = CancellationToken::new();
    let generations = Mutex::new(HashMap::from([(
        "preview".into(),
        ProposalGeneration {
            cancellation: old.clone(),
            version: 1,
        },
    )]));

    // Act: regeneration installs v2 before the suspended v1 cleanup resumes.
    generations.lock().expect("generations").insert(
        "preview".into(),
        ProposalGeneration {
            cancellation: newer.clone(),
            version: 2,
        },
    );
    finish_proposal_generation(&generations, "preview", 1);

    // Assert: cancellation still reaches v2; only v2 cleanup can remove it.
    let retained = generations.lock().expect("generations");
    assert_eq!(retained["preview"].version, 2);
    retained["preview"].cancellation.cancel();
    assert!(newer.is_cancelled());
    assert!(!old.is_cancelled());
    drop(retained);
    finish_proposal_generation(&generations, "preview", 2);
    finish_proposal_generation(&generations, "preview", 1);
    assert!(generations.lock().expect("generations").is_empty());
}

#[tokio::test]
async fn duplicate_generation_and_approval_receipts_do_not_start_workers() {
    for approve in [false, true] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_graph = true;
        let mut state = graph_proposal_state("preview", "sess");
        let mut repository = MockGraphProposalRepository::new();
        if approve {
            let proposal = graph_proposal();
            let graph = GraphProposalResolution {
                agent: state.agent,
                answers: &state.version.answers,
                definitions: &state.definitions,
                graph_id: "proposal:preview:1",
                limits: &state.limits,
                permission: state.permission,
                request: &state.input.request,
                revision_attachments: &state.version.attachments,
                session_id: &state.input.session_id,
            }
            .resolve(&proposal)
            .expect("resolved preview");
            state.version.outcome = GraphProposalVersionOutcome::Generated {
                launch: Some(Box::new(StartExecutionGraphRequest {
                    graph,
                    provider_call_limits: BTreeMap::from([("proposal:preview:1:calls".into(), 48)]),
                })),
                response: GraphProposalResponse {
                    outcome: GraphProposalOutcome::Proposal { proposal },
                },
            };
            repository
                .expect_get()
                .once()
                .return_once(move |_, _| Ok(Some(state)));
            repository
                .expect_approve()
                .once()
                .returning(|_, _| Ok(false));
        } else {
            repository.expect_get().once().returning(|_, _| Ok(None));
            repository.expect_create().once().returning(|_| Ok(false));
        }
        app.services
            .set_graph_proposal_repository(Arc::new(repository));
        let service = app.session_service();

        // Act
        let result = if approve {
            app.drive_session_request(service.approve_graph_proposal("preview", 1))
                .await
        } else {
            app.drive_session_request(
                service.generate_graph_proposal(graph_proposal_state("preview", "sess").input),
            )
            .await
        };

        // Assert
        result.expect("idempotent admission");
        assert!(
            app.services
                .proposal_generations
                .lock()
                .expect("generations")
                .is_empty()
        );
        assert!(
            app.services
                .graph_drivers
                .lock()
                .expect("drivers")
                .is_empty()
        );
    }
}

#[tokio::test]
async fn proposal_query_reports_repository_failure_and_missing_actions_fail() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    app.experimental_graph = true;
    let mut repository = MockGraphProposalRepository::new();
    repository.expect_get().once().return_once(|_, _| {
        Err(DbError::InvalidData {
            entity: "graph proposal",
            reason: "proposal storage unavailable".into(),
        })
    });
    repository.expect_get().once().return_once(|_, _| Ok(None));
    app.services
        .set_graph_proposal_repository(Arc::new(repository));
    let service = app.session_service();

    // Act
    let query = app
        .drive_session_request(service.get_graph_proposal("preview"))
        .await;
    let cancel = app
        .drive_session_request(service.cancel_graph_proposal("preview", 1))
        .await;

    // Assert
    assert!(
        query
            .expect_err("repository failure")
            .to_string()
            .contains("proposal storage unavailable")
    );
    assert!(
        cancel
            .expect_err("missing proposal")
            .to_string()
            .contains("Proposal is missing")
    );
}

#[tokio::test]
async fn unresolved_proposal_cannot_bypass_launch_approval() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    let state = graph_proposal_state("preview", "sess");
    app.services
        .db()
        .graph_proposals()
        .create(&state)
        .await
        .expect("capture");
    let mut graph = crate::app::execution_graph::tests::graph();
    graph.id = "proposal:preview:1".into();
    app.experimental_graph = true;
    let service = app.session_service();

    // Act
    let result = app
        .validate_proposal_launch(&StartExecutionGraphRequest {
            graph,
            provider_call_limits: BTreeMap::new(),
        })
        .await;
    let approval = app
        .drive_session_request(service.approve_graph_proposal("preview", 1))
        .await;

    // Assert
    assert!(
        result
            .expect_err("unresolved")
            .to_string()
            .contains("no resolved snapshot")
    );
    assert!(
        approval
            .expect_err("unresolved approval")
            .to_string()
            .contains("Inspect a resolved preview before approval")
    );
}

#[tokio::test]
async fn graph_proposal_failed_completion_releases_generation_and_workspace() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    let mut client = MockRunClient::new();
    client
        .expect_submit()
        .once()
        .returning(|_| Err(OneShotError::new("worker unavailable")));
    app.services.set_run_client(Arc::new(client));
    let mut repository = MockGraphProposalRepository::new();
    repository
        .expect_finish()
        .once()
        .withf(|id, version, outcome| {
            id == "preview"
                && *version == 1
                && matches!(outcome, GraphProposalVersionOutcome::Failed(_))
        })
        .returning(|_, _, _| Err(DbError::Query(sqlx::Error::PoolClosed)));
    app.services
        .set_graph_proposal_repository(Arc::new(repository));

    // Act
    app.spawn_graph_proposal_generation(
        graph_proposal_state("preview", "sess"),
        "workspace".into(),
    );
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    assert!(
        app.services
            .proposal_generations
            .lock()
            .expect("generations")
            .is_empty()
    );
    assert!(
        app.services
            .graph_resources
            .environment(&ag_session::EnvId::SessionWorktree("sess".into()))
            .try_write_owned()
            .is_ok()
    );
}

#[tokio::test]
async fn graph_proposal_requires_prepared_idle_session_and_available_definitions() {
    for case in ["busy", "draft", "empty-catalog", "unavailable-definition"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_graph = true;
        let expected = if matches!(case, "busy" | "draft") {
            let status = if case == "busy" {
                Status::InProgress
            } else {
                Status::Draft
            };
            app.sessions.sessions_mut()[0].is_draft = case == "draft";
            *app.sessions.session_handles()["sess"]
                .status
                .lock()
                .expect("status") = status;
            "prepared idle session"
        } else {
            let mut catalog = MockAgentCatalogClient::new();
            catalog.expect_list_summaries().once().returning(move |_| {
                Box::pin(async move {
                    if case == "empty-catalog" {
                        Vec::new()
                    } else {
                        vec![graph_proposal_state("preview", "sess").definitions[0].summary()]
                    }
                })
            });
            if case == "unavailable-definition" {
                catalog
                    .expect_resolve()
                    .once()
                    .returning(|_, _| Box::pin(async { None }));
            }
            app.services
                .set_agent_profile_catalog_client(Arc::new(catalog));
            "No enabled agent definitions"
        };
        let service = app.session_service();

        // Act
        let result = app
            .drive_session_request(
                service.generate_graph_proposal(graph_proposal_state("preview", "sess").input),
            )
            .await;

        // Assert
        assert!(
            result.expect_err(case).to_string().contains(expected),
            "{case}"
        );
        assert!(
            app.services
                .proposal_generations
                .lock()
                .expect("generations")
                .is_empty()
        );
    }
}
