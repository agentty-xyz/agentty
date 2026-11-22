//! Chat reply unit tests for accepting or revising graph outputs awaiting
//! acceptance.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_contracts::{
    AgentRequestKind, MockAgentChannel, OneShotError, OneShotRequest, OneShotSubmission,
    SessionStats, TurnRequest, TurnResult,
};
use ag_git::{DiffStats, MockGitClient};
use ag_protocol::{AgentResponse, ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    ExecutionGraph, ExecutionGraphProgress, ExecutionGraphStatus, GraphContext, GraphNode,
    GraphNodeContinuation, GraphNodeStatus, GraphOutputReplyOutcome, GraphPostProcessing,
    GraphWorkspaceAccess, StartExecutionGraphRequest,
};
use ag_store::{DbError, GraphOutputReplyRecord, MockGraphRepository, StoredGraphResult};
use ag_worker::{
    GraphExecutor, GraphNodeRunner, GraphRunRequest, MockRunClient, RunClient, SessionRunClient,
};
use async_trait::async_trait;
use tempfile::TempDir;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::execution_graph::GraphDriverRegistration;
use crate::app::execution_graph::tests::{graph, seed_session};
use crate::app::graph_node_reply::tests::{app_with, messages, screenshot};
use crate::app::graph_proposal_reply::tests::{apply_next_command, wait_for_message};
use crate::app::graph_reply::GraphReplyOutcome;
use crate::app::graph_reply_decision::ReplyDecision;
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::app::service::MockSessionRunFactory;
use crate::domain::session::{SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::fs::MockFsClient;
use crate::presentation::execution_graph::tests::{acceptance_gated_view, consumed_gated_view};
use crate::test_support::{new_test_app_with_clients, test_app_clients};

/// Identity of a reply an interrupted run left recorded.
const REPLY_ID: &str = "5b0f9a0e-6c1d-4f4e-9f0a-3d2c1b0a9e8f";

/// Lets a recovered reply reach its wait for the session runtime.
const RUNTIME_WAIT_GRACE: Duration = Duration::from_millis(50);

#[tokio::test]
async fn start_implementing_in_chat_accepts_the_awaited_output_and_continues_the_graph() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = reader_graph_app().await;
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();
    let request = StartExecutionGraphRequest {
        graph: reader_graph(),
        provider_call_limits: BTreeMap::from([("calls".into(), 2)]),
    };
    app.drive_session_request(service.start_execution_graph(request))
        .await
        .expect("start");
    app.drive_session_request(async { observed_rx.recv().await.expect("first turn") })
        .await;
    wait_for_node(&mut app, 0, GraphNodeStatus::Succeeded).await;

    // Act
    let outcome = app.submit_prompt(submission("Start implementing")).await;
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("second turn") }),
    )
    .await
    .expect("accepted output dispatches the waiting node");
    let completed = wait_for_node(&mut app, 1, GraphNodeStatus::Succeeded).await;
    let notice = wait_for_message(&app, "Accepted").await;

    // Assert
    assert_eq!(
        outcome,
        PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        }
    );
    assert!(second.prompt.agent_text().contains("Plan ready"));
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(notice, "Accepted `response` from `first` · continuing");
    assert!(
        messages(&app)
            .await
            .contains(&("user_prompt".into(), "Start implementing".into()))
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn revise_reply_sends_guidance_to_the_producer() {
    // Arrange
    let (graph, progress) = gated();
    let expected_execution = progress.nodes[0].executions[0].clone();
    let mut repository = listing(graph, progress);
    repository
        .expect_continue_node()
        .once()
        .withf(move |request| {
            request.graph_id == "graph"
                && request.node_id == "first"
                && request.execution == expected_execution
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: Vec::new(),
                        message: "Also add tests".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory, _driver) = decided_app(repository, "revise").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Also add tests".into()),
        )
        .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Revision guidance").await;

    // Assert
    assert_eq!(outcome, Some(GraphReplyOutcome::Accepted));
    assert_eq!(notice, "Revision guidance sent to `first` · continuing");
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn api_message_replies_to_the_awaited_output_instead_of_starting_a_turn() {
    // Arrange
    let (graph, progress) = gated();
    let mut repository = listing(graph, progress);
    repository
        .expect_continue_node()
        .once()
        .withf(|request| {
            request.node_id == "first"
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: Vec::new(),
                        message: "Also add tests".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory, _driver) = decided_app(repository, "revise").await;
    let _consumer = app.sessions.foreground_consumer();
    let service = app.session_service();

    // Act
    let result = app
        .drive_session_request(service.send_message(&SessionId::from("sess"), "Also add tests"))
        .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Revision guidance").await;

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(notice, "Revision guidance sent to `first` · continuing");
}

#[tokio::test]
async fn revise_reply_reports_a_producer_that_cannot_take_guidance() {
    // Arrange
    let (mut graph, progress) = gated();
    graph.nodes[0].execution.context = GraphContext::Fresh;
    let (mut app, _directory, _driver) = decided_app(listing(graph, progress), "revise").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.submit_graph_output_reply(
        &SessionId::from("sess"),
        &TurnPrompt::from_text("Different approach".into()),
    )
    .await;
    let notice = wait_for_message(&app, "Graph output:").await;

    // Assert
    assert!(
        notice.contains(
            "`first` cannot take revision guidance. Reply with clear acceptance, or accept or \
             cancel it from `w`."
        ),
        "{notice}"
    );
}

#[tokio::test]
async fn reply_with_attachments_to_a_producer_that_cannot_take_guidance_keeps_the_composer() {
    // Arrange
    let (mut graph, progress) = gated();
    graph.nodes[0].execution.context = GraphContext::Fresh;
    let (mut app, _directory) = app_with(listing(graph, progress)).await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Go ahead like this [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_output_reply(&SessionId::from("sess"), &prompt)
        .await;

    // Assert
    assert!(matches!(outcome, Some(GraphReplyOutcome::Rejected(_))));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].1.contains(
            "`first` cannot take images as revision guidance. Remove them to accept its output in \
             chat, or use `w`."
        ),
        "{messages:?}"
    );
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn reply_with_attachments_to_an_output_a_started_dependent_consumed_keeps_the_composer() {
    // Arrange
    let view = consumed_gated_view();
    let (mut app, _directory) = app_with(listing(
        view.graphs[0].clone(),
        view.progress.expect("consumed progress"),
    ))
    .await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Redo it like this [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_output_reply(&SessionId::from("sess"), &prompt)
        .await;

    // Assert
    assert!(matches!(outcome, Some(GraphReplyOutcome::Rejected(_))));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0]
            .1
            .contains("`first` cannot take images as revision guidance."),
        "{messages:?}"
    );
}

#[tokio::test]
async fn reply_with_attachments_revises_without_deciding() {
    // Arrange
    let (graph, progress) = gated();
    let mut repository = listing(graph, progress);
    repository
        .expect_continue_node()
        .once()
        .withf(|request| {
            request.node_id == "first"
                && request.kind
                    == GraphNodeContinuation::Reply {
                        attachments: vec![screenshot()],
                        message: "Start implementing, match this [Image #1]".into(),
                    }
        })
        .returning(|_| Ok(true));
    let (mut app, _directory) = app_with(settling(repository)).await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let _consumer = app.sessions.foreground_consumer();
    let prompt = TurnPrompt {
        attachments: vec![screenshot()],
        ..TurnPrompt::from_text("Start implementing, match this [Image #1]".into())
    };

    // Act
    let outcome = app
        .submit_graph_output_reply(&SessionId::from("sess"), &prompt)
        .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Revision guidance").await;

    // Assert
    assert_eq!(outcome, Some(GraphReplyOutcome::Accepted));
    assert_eq!(notice, "Revision guidance sent to `first` · continuing");
}

#[tokio::test]
async fn accept_reply_reports_a_stale_output_instead_of_retargeting_it() {
    // Arrange
    let (graph, progress) = gated();
    let mut stale = graph.clone();
    stale.revision = stale.revision.saturating_add(1);
    let mut repository = listing(graph, progress);
    repository
        .expect_load_graph()
        .returning(move |_, _| Ok(Some(stale.clone())));
    let (mut app, _directory, _driver) = decided_app(repository, "accept").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.submit_graph_output_reply(
        &SessionId::from("sess"),
        &TurnPrompt::from_text("Go ahead".into()),
    )
    .await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Graph output:").await;

    // Assert
    assert!(notice.contains("Stale graph acceptance"), "{notice}");
}

#[tokio::test]
async fn stopping_the_session_abandons_an_undecided_output_reply() {
    // Arrange
    let (graph, progress) = gated();
    let mut repository = deciding(listing(graph, progress));
    repository
        .expect_settle_output_reply()
        .once()
        .returning(|_| Ok(()));
    let (mut app, _directory) = app_with(repository).await;
    let _driver = live_driver(&app);
    let gate = Arc::new(Semaphore::new(0));
    app.services
        .set_run_client(Arc::new(GatedAcceptance(Arc::clone(&gate))));
    let status = Arc::clone(
        &app.sessions
            .session_handles_or_err("sess")
            .expect("handles")
            .status,
    );

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Looks good".into()),
        )
        .await;
    wait_for_status(&status, Status::InProgress).await;
    app.services.stop_graph_activity("sess").await;
    gate.add_permits(1);
    let notice = wait_for_message(&app, "was canceled").await;
    wait_for_status(&status, Status::Review).await;

    // Assert
    assert_eq!(outcome, Some(GraphReplyOutcome::Accepted));
    assert_eq!(
        notice,
        TranscriptNotice::ReplyError.format(
            "Graph output: the reply decision for `first` was canceled. Reply again to act on its \
             output."
        )
    );
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn reply_while_the_previous_one_is_decided_keeps_the_composer() {
    // Arrange
    let (graph, progress) = gated();
    let (mut app, _directory, _driver) = decided_app(listing(graph, progress), "accept").await;
    mark_deciding(&app);

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Go ahead".into()),
        )
        .await;

    // Assert
    assert!(matches!(outcome, Some(GraphReplyOutcome::Rejected(_))));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0]
            .1
            .contains("Still deciding on your previous reply to `first`"),
        "{messages:?}"
    );
}

#[tokio::test]
async fn reply_for_an_unloaded_session_keeps_the_composer_without_deciding() {
    // Arrange
    let (graph, progress) = gated();
    let (mut app, _directory, _driver) = decided_app(listing(graph, progress), "accept").await;

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("ghost"),
            &TurnPrompt::from_text("Go ahead".into()),
        )
        .await;

    // Assert
    assert!(matches!(outcome, Some(GraphReplyOutcome::Rejected(_))));
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn graphs_without_an_awaited_output_leave_ordinary_routing() {
    for case in ["no-progress", "cancelled", "accepted", "no-execution"] {
        // Arrange
        let (graph, mut progress) = gated();
        match case {
            "cancelled" => progress.status = ExecutionGraphStatus::Cancelled,
            "accepted" => progress.nodes[1].blocked.clear(),
            "no-execution" => progress.nodes[0].executions.clear(),
            _ => {}
        }
        let mut repository = MockGraphRepository::new();
        repository
            .expect_list_graphs()
            .returning(move |_| Ok(vec![graph.clone()]));
        repository
            .expect_progress()
            .returning(move |_| Ok((case != "no-progress").then(|| progress.clone())));
        let (mut app, _directory) = app_with(repository).await;
        let _driver = live_driver(&app);

        // Act
        let outcome = app
            .submit_graph_output_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Go ahead".into()),
            )
            .await;

        // Assert
        assert_eq!(outcome, None, "{case}");
        assert!(messages(&app).await.is_empty(), "{case}");
    }
}

#[tokio::test]
async fn reply_to_a_paused_graph_resumes_it_and_continues_inside_the_graph() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = reader_graph_app().await;
    let _consumer = app.sessions.foreground_consumer();
    let paused = pause_after_first_node(&mut app, &mut observed_rx).await;

    // Act
    let outcome = app.submit_prompt(submission("Start implementing")).await;
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("second turn") }),
    )
    .await
    .expect("the resumed graph dispatches the waiting node");
    let completed = wait_for_node(&mut app, 1, GraphNodeStatus::Succeeded).await;
    let notice = wait_for_message(&app, "Accepted").await;

    // Assert
    assert_eq!(paused.status, ExecutionGraphStatus::Paused);
    assert_eq!(
        outcome,
        PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        }
    );
    assert!(second.prompt.agent_text().contains("Plan ready"));
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(notice, "Accepted `response` from `first` · continuing");
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn older_paused_graph_leaves_ordinary_routing_once_a_newer_graph_exists() {
    // Arrange: the resume lookup is not mocked, so reaching it would panic.
    let (graph, progress) = gated();
    let mut newer = graph.clone();
    newer.id = "newer".into();
    let (mut app, _directory) = app_with(listing_before(graph, progress, vec![newer])).await;

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Unrelated follow-up".into()),
        )
        .await;

    // Assert
    assert_eq!(outcome, None);
    assert_eq!(messages(&app).await, Vec::new());
}

#[tokio::test]
async fn reply_to_a_paused_graph_that_cannot_resume_keeps_the_composer() {
    // Arrange: a focused review that ran since is not a newer working graph.
    let (graph, progress) = gated();
    let mut review = graph.clone();
    review.id = "review".into();
    review.nodes[0].execution.response_profile = ProtocolRequestProfile::FocusedReview;
    review.nodes[0].execution.workspace_access = GraphWorkspaceAccess::BackgroundRead;
    let mut repository = listing_before(graph, progress, vec![review]);
    repository.expect_load_graph().returning(|_, _| Ok(None));
    let (mut app, _directory) = app_with(repository).await;
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Go ahead".into()),
        )
        .await;

    // Assert
    assert!(matches!(outcome, Some(GraphReplyOutcome::Rejected(_))));
    let messages = messages(&app).await;
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].1.contains(
            "the paused graph could not resume to act on `first`: Missing or stale graph. Resume \
             it from `w`, then reply again."
        ),
        "{messages:?}"
    );
    assert!(
        app.services
            .output_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn failed_reads_report_the_error_and_keep_the_composer() {
    for fault in ["list", "progress", "result"] {
        // Arrange
        let (graph, progress) = gated();
        let mut repository = MockGraphRepository::new();
        repository.expect_list_graphs().returning(move |_| {
            if fault == "list" {
                Err(unavailable())
            } else {
                Ok(vec![graph.clone()])
            }
        });
        repository.expect_progress().returning(move |_| {
            if fault == "progress" {
                Err(unavailable())
            } else {
                Ok(Some(progress.clone()))
            }
        });
        repository
            .expect_load_result()
            .returning(|_| Err(unavailable()));
        let (mut app, _directory) = app_with(repository).await;
        let _driver = live_driver(&app);

        // Act
        let outcome = app
            .submit_graph_output_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Go ahead".into()),
            )
            .await;

        // Assert
        assert!(
            matches!(outcome, Some(GraphReplyOutcome::Rejected(_))),
            "{fault}"
        );
        let messages = messages(&app).await;
        assert_eq!(messages.len(), 1, "{fault}: {messages:?}");
        assert!(
            messages[0].1.contains("Graph output:") && messages[0].1.contains("Read failed"),
            "{fault}: {messages:?}"
        );
    }
}

#[tokio::test]
async fn failed_reply_persistence_keeps_the_composer_or_reports_the_error() {
    for failure in ["record", "decide", "settle"] {
        // Arrange: only a failed settlement lets the accepted action through.
        let (graph, progress) = gated();
        let stored = graph.clone();
        let mut repository = listing(graph, progress);
        repository
            .expect_record_output_reply()
            .return_once(move |_| match failure {
                "record" => Err(unavailable()),
                _ => Ok(true),
            });
        repository
            .expect_decide_output_reply()
            .returning(move |_, outcome| match failure {
                "decide" => Err(unavailable()),
                _ => Ok(outcome),
            });
        repository
            .expect_load_graph()
            .returning(move |_, _| Ok(Some(stored.clone())));
        repository.expect_accept_result().returning(|_, _| Ok(true));
        repository
            .expect_settle_output_reply()
            .returning(|_| Err(unavailable()));
        let (mut app, _directory) = app_with(repository).await;
        let _driver = live_driver(&app);
        app.services
            .set_run_client(Arc::new(GatedAcceptance(Arc::new(Semaphore::new(1)))));
        let _consumer = app.sessions.foreground_consumer();

        // Act
        let outcome = app
            .submit_graph_output_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Go ahead".into()),
            )
            .await;
        if failure == "settle" {
            apply_next_command(&mut app).await;
        }

        // Assert
        if failure == "record" {
            assert!(
                matches!(outcome, Some(GraphReplyOutcome::Rejected(_))),
                "{failure}"
            );
            let messages = messages(&app).await;
            assert_eq!(messages.len(), 1, "{failure}: {messages:?}");
            assert!(messages[0].1.contains("Read failed"), "{messages:?}");
        } else {
            let notice = wait_for_message(&app, "Graph output:").await;
            assert!(notice.contains("Read failed"), "{failure}: {notice}");
        }
        wait_for_idle_decisions(&app).await;
    }
}

#[tokio::test]
async fn restart_recovery_resumes_the_paused_graph_and_decides_an_undecided_reply() {
    // Arrange
    let (mut app, _directory, mut observed_rx) = reader_graph_app().await;
    let _consumer = app.sessions.foreground_consumer();
    let paused = pause_after_first_node(&mut app, &mut observed_rx).await;
    let reply = GraphOutputReplyRecord {
        attachments: Vec::new(),
        execution: paused.nodes[0].executions[0].clone(),
        id: REPLY_ID.into(),
        message: "Start implementing".into(),
        outcome: None,
        reference: paused.nodes[0].results[0].reference.clone(),
    };
    app.services
        .db()
        .graphs()
        .record_output_reply(&reply)
        .await
        .expect("record reply");

    // Act
    app.recover_graph_output_replies().await;
    let second = tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async { observed_rx.recv().await.expect("second turn") }),
    )
    .await
    .expect("the recovered reply resumes the graph and dispatches the waiting node");
    let completed = wait_for_node(&mut app, 1, GraphNodeStatus::Succeeded).await;
    let notice = wait_for_message(&app, "Accepted").await;
    wait_for_idle_decisions(&app).await;
    let unsettled = app
        .services
        .db()
        .graphs()
        .unsettled_output_replies("sess")
        .await
        .expect("unsettled replies");

    // Assert
    assert_eq!(paused.status, ExecutionGraphStatus::Paused);
    assert!(second.prompt.agent_text().contains("Plan ready"));
    assert_eq!(completed.status, ExecutionGraphStatus::Completed);
    assert_eq!(notice, "Accepted `response` from `first` · continuing");
    assert_eq!(unsettled, []);
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn restart_recovery_reapplies_a_recorded_decision_without_deciding_again() {
    // Arrange
    let (graph, progress) = gated();
    let stored = graph.clone();
    let mut repository = recovering(graph, progress, Some(GraphOutputReplyOutcome::Accept));
    repository
        .expect_load_graph()
        .returning(move |_, _| Ok(Some(stored.clone())));
    repository
        .expect_accept_result()
        .once()
        .withf(|reference, _| reference.id == "result")
        .returning(|_, _| Ok(true));
    let (mut app, _directory) = app_with(settling(repository)).await;
    let _driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().never();
    app.services.set_run_client(Arc::new(client));
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.recover_graph_output_replies().await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Accepted").await;
    wait_for_idle_decisions(&app).await;

    // Assert
    assert_eq!(notice, "Accepted `response` from `first` · continuing");
}

#[tokio::test]
async fn recovery_skips_a_reply_being_decided_or_whose_graph_cannot_be_read() {
    for case in ["deciding", "lost"] {
        // Arrange: a reply being decided is skipped before its graph is read.
        // Recording or deciding the reply is not mocked.
        let (graph, progress) = gated();
        let mut repository = recovering(graph, progress, None);
        repository
            .expect_load_graph()
            .times(usize::from(case == "lost"))
            .returning(|_, _| Ok(None));
        let (mut app, _directory) = app_with(repository).await;
        if case == "deciding" {
            mark_deciding(&app);
        }

        // Act
        app.recover_graph_output_replies().await;

        // Assert
        assert_eq!(deciding_count(&app), usize::from(case == "deciding"));
        assert_eq!(messages(&app).await, Vec::new(), "{case}");
    }
}

#[tokio::test]
async fn recorded_decision_stays_unsettled_when_the_runtime_stops_before_answering() {
    for stops in ["before-start", "mid-resume", "mid-action"] {
        // Arrange: only a paused graph is resumed before the action.
        let (graph, progress) = gated();
        let stored = graph.clone();
        let mut repository = deciding(recovering(
            graph,
            progress,
            Some(GraphOutputReplyOutcome::Accept),
        ));
        repository
            .expect_load_graph()
            .returning(move |_, _| Ok(Some(stored.clone())));
        repository.expect_settle_output_reply().never();
        let (mut app, _directory) = app_with(repository).await;
        let _driver = (stops != "mid-resume").then(|| live_driver(&app));
        let consumer = (stops != "before-start").then(|| app.sessions.foreground_consumer());

        // Act
        app.recover_graph_output_replies().await;
        if consumer.is_some() {
            // The runtime takes the command and stops without answering it.
            let _unanswered = app.sessions.next_command().await;
            drop(consumer);
            wait_for_idle_decisions(&app).await;
        } else {
            tokio::time::sleep(RUNTIME_WAIT_GRACE).await;
        }
        app.services.wait_for_cleanup_tasks(None).await;

        // Assert
        assert_eq!(deciding_count(&app), 0, "{stops}");
        assert_eq!(messages(&app).await, Vec::new(), "{stops}");
    }
}

#[tokio::test]
async fn stopping_the_session_abandons_a_decided_reply_whose_action_was_not_attempted() {
    for stops in ["waiting-for-runtime", "mid-action"] {
        // Arrange: the recorded decision is written again before the action.
        let (graph, progress) = gated();
        let stored = graph.clone();
        let mut repository = recovering(graph, progress, Some(GraphOutputReplyOutcome::Accept));
        repository
            .expect_load_graph()
            .returning(move |_, _| Ok(Some(stored.clone())));
        let decided = Arc::new(Semaphore::new(0));
        repository.expect_decide_output_reply().returning({
            let decided = Arc::clone(&decided);

            move |_, outcome| {
                decided.add_permits(1);

                Ok(outcome)
            }
        });
        repository
            .expect_settle_output_reply()
            .once()
            .withf(|reply_id| reply_id == REPLY_ID)
            .returning(|_| Ok(()));
        let (mut app, _directory) = app_with(repository).await;
        let _driver = live_driver(&app);
        let consumer = (stops == "mid-action").then(|| app.sessions.foreground_consumer());

        // Act
        app.recover_graph_output_replies().await;
        if consumer.is_some() {
            // The runtime takes the command and stops without answering it.
            let _unanswered = app.sessions.next_command().await;
            app.services.stop_graph_activity("sess").await;
            drop(consumer);
        } else {
            // The reply waits for a runtime that never accepts commands.
            let _decided = decided.acquire().await.expect("decision recorded");
            app.services.stop_graph_activity("sess").await;
        }
        let notice = wait_for_message(&app, "was canceled").await;
        app.services.wait_for_cleanup_tasks(None).await;

        // Assert
        assert_eq!(
            notice,
            TranscriptNotice::ReplyError.format(
                "Graph output: the reply decision for `first` was canceled. Reply again to act on \
                 its output."
            ),
            "{stops}"
        );
        assert_eq!(deciding_count(&app), 0, "{stops}");
    }
}

#[tokio::test]
async fn recovered_reply_reports_a_paused_graph_that_cannot_resume() {
    // Arrange: the graph is read for the reply, then lost before the resume.
    let (graph, progress) = gated();
    let stored = graph.clone();
    let mut repository = recovering(graph, progress, Some(GraphOutputReplyOutcome::Accept));
    let mut reads = 0;
    repository.expect_load_graph().returning(move |_, _| {
        reads += 1;

        Ok((reads == 1).then(|| stored.clone()))
    });
    let (mut app, _directory) = app_with(settling(repository)).await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.recover_graph_output_replies().await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Graph output:").await;
    wait_for_idle_decisions(&app).await;

    // Assert
    assert!(
        notice.contains(
            "the paused graph could not resume to act on `first`: Missing or stale graph. Resume \
             it from `w`, then reply again."
        ),
        "{notice}"
    );
}

#[tokio::test]
async fn shutdown_leaves_an_undecided_output_reply_for_restart_recovery() {
    // Arrange
    let (graph, progress) = gated();
    let mut repository = deciding(listing(graph, progress));
    repository.expect_settle_output_reply().never();
    let (mut app, _directory) = app_with(repository).await;
    let _driver = live_driver(&app);
    let gate = Arc::new(Semaphore::new(0));
    app.services
        .set_run_client(Arc::new(GatedAcceptance(Arc::clone(&gate))));

    // Act
    let outcome = app
        .submit_graph_output_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Looks good".into()),
        )
        .await;
    app.services.cancel_graph_reply_decisions(None);
    gate.add_permits(1);
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    assert_eq!(outcome, Some(GraphReplyOutcome::Accepted));
    assert_eq!(deciding_count(&app), 0);
    assert_eq!(messages(&app).await, Vec::new());
}

/// Blocked graph whose `second` node waits for acceptance of `first`'s
/// validated `response` output.
fn gated() -> (ExecutionGraph, ExecutionGraphProgress) {
    let view = acceptance_gated_view();

    (
        view.graphs[0].clone(),
        view.progress.expect("gated progress"),
    )
}

/// Repository that lists the gated graph and its stored output body.
fn listing(graph: ExecutionGraph, progress: ExecutionGraphProgress) -> MockGraphRepository {
    listing_before(graph, progress, Vec::new())
}

/// Repository that lists the gated graph, then `newer` graphs created after
/// it that have no progress yet, with the gated graph's stored output body.
fn listing_before(
    graph: ExecutionGraph,
    progress: ExecutionGraphProgress,
    newer: Vec<ExecutionGraph>,
) -> MockGraphRepository {
    let result = progress.nodes[0].results[0].clone();
    let gated_id = graph.id.clone();
    let graphs: Vec<_> = std::iter::once(graph).chain(newer).collect();
    let mut repository = MockGraphRepository::new();
    repository
        .expect_list_graphs()
        .returning(move |_| Ok(graphs.clone()));
    repository
        .expect_progress()
        .returning(move |id| Ok((id == gated_id).then(|| progress.clone())));
    repository.expect_load_result().returning(move |_| {
        Ok(Some(StoredGraphResult {
            acceptance: None,
            body: r#"{"answer":"Plan body"}"#.into(),
            result: result.clone(),
            validation: None,
        }))
    });

    repository
}

/// Lets `repository` record replies and keep the decision it is given.
fn deciding(mut repository: MockGraphRepository) -> MockGraphRepository {
    repository
        .expect_record_output_reply()
        .returning(|_| Ok(true));
    repository
        .expect_decide_output_reply()
        .returning(|_, outcome| Ok(outcome));

    repository
}

/// [`deciding`] repository that also settles replies.
fn settling(repository: MockGraphRepository) -> MockGraphRepository {
    let mut repository = deciding(repository);
    repository
        .expect_settle_output_reply()
        .returning(|_| Ok(()));

    repository
}

/// [`listing`] repository whose session has one unsettled reply to the gated
/// output, as an interrupted run left it, with its first decision when
/// `outcome` is set.
fn recovering(
    graph: ExecutionGraph,
    progress: ExecutionGraphProgress,
    outcome: Option<GraphOutputReplyOutcome>,
) -> MockGraphRepository {
    let reply = GraphOutputReplyRecord {
        attachments: Vec::new(),
        execution: progress.nodes[0].executions[0].clone(),
        id: REPLY_ID.into(),
        message: "Start implementing".into(),
        outcome,
        reference: progress.nodes[0].results[0].reference.clone(),
    };
    let mut repository = listing(graph, progress);
    repository
        .expect_unsettled_output_replies()
        .returning(move |_| Ok(vec![reply.clone()]));

    repository
}

/// Marks the gated output as having a reply decision in flight.
fn mark_deciding(app: &App) {
    app.services
        .output_reply_decisions
        .lock()
        .expect("decision lock")
        .insert(
            "result".into(),
            ReplyDecision {
                cancellation: CancellationToken::new(),
                is_abandoned: Arc::default(),
                session_id: SessionId::from("sess"),
            },
        );
}

/// Counts the outputs whose reply decision is in flight.
fn deciding_count(app: &App) -> usize {
    app.services
        .output_reply_decisions
        .lock()
        .expect("decision lock")
        .len()
}

async fn wait_for_idle_decisions(app: &App) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while deciding_count(app) > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("decision settles");
}

/// App over `repository` with a live gated-graph driver whose reply decision
/// answers `decision` after checking that the prompt carries the decoded
/// output.
async fn decided_app(
    repository: MockGraphRepository,
    decision: &'static str,
) -> (App, TempDir, GraphDriverRegistration) {
    let (mut app, directory) = app_with(settling(repository)).await;
    let driver = live_driver(&app);
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        assert_eq!(request.request_kind, AgentRequestKind::UtilityPrompt);
        assert!(request.prompt.contains(
            r#""Node `first` (Inspect first) produced output `response`:\n\nPlan body""#
        ));

        Ok(OneShotSubmission {
            response: AgentResponse::plain(decision),
            stats: SessionStats::default(),
        })
    });
    app.services.set_run_client(Arc::new(client));

    (app, directory, driver)
}

/// Registers a live driver for the gated graph, so chat replies may act on
/// it; dropping the registration pauses the graph again.
fn live_driver(app: &App) -> GraphDriverRegistration {
    let executor = GraphExecutor::new(
        app.services.db().graph_dispatch(),
        Arc::clone(&app.services.graph_resources),
        Arc::new(IdleRunner),
    );

    GraphDriverRegistration::register(&app.services.graph_drivers, &gated().0, Arc::new(executor))
}

/// Two-reader graph whose second node waits for acceptance of the first
/// node's output.
fn reader_graph() -> ExecutionGraph {
    let mut graph = graph();
    let second = &mut graph.nodes[1];
    second.definition.read_only = true;
    second.execution.post_processing = GraphPostProcessing::None;
    second.execution.workspace_access = GraphWorkspaceAccess::SharedRead;

    graph
}

/// App whose graph tasks answer `Plan ready` and whose reply decision
/// accepts.
async fn reader_graph_app() -> (App, TempDir, mpsc::UnboundedReceiver<TurnRequest>) {
    let (observed_tx, observed_rx) = mpsc::unbounded_channel();
    let mut factory = MockSessionRunFactory::new();
    factory.expect_create().returning(|id, _| {
        let mut channel = MockAgentChannel::new();
        channel
            .expect_shutdown_session()
            .returning(|_| Box::pin(async { Ok(()) }));
        SessionRunClient::from_channel(id.to_string(), Arc::new(channel))
    });
    factory
        .expect_create_graph_task()
        .times(2)
        .returning(move |context, _, _| {
            let mut channel = MockAgentChannel::new();
            let observed = observed_tx.clone();
            channel
                .expect_run_turn()
                .once()
                .returning(move |_, mut request, _| {
                    // Observe the request without its budget: a retained
                    // clone keeps the graph's spending writer open, so
                    // cleanup would wait out its shutdown deadline.
                    request
                        .provider_call_budget
                        .take()
                        .expect("budget")
                        .consume()
                        .expect("credit");
                    observed.send(request).expect("observe");
                    Box::pin(async {
                        Ok(TurnResult {
                            assistant_message: AgentResponse::plain("Plan ready"),
                            context_reset: false,
                            harness_report: None,
                            input_tokens: 1,
                            output_tokens: 1,
                            provider_conversation_id: None,
                        })
                    })
                });
            channel
                .expect_shutdown_session()
                .once()
                .returning(|_| Box::pin(async { Ok(()) }));
            Ok(SessionRunClient::from_channel(
                context.into(),
                Arc::new(channel),
            ))
        });
    let (mut app, directory) =
        new_test_app_with_clients(test_app_clients().with_session_run_factory(Arc::new(factory)))
            .await;
    let mut fs = MockFsClient::new();
    fs.expect_is_dir().returning(|_| true);
    app.services.set_fs_client(Arc::new(fs));
    let mut git = MockGitClient::new();
    git.expect_detect_git_info()
        .returning(|_| Box::pin(async { Some("wt/sess".into()) }));
    git.expect_main_checkout_working_tree()
        .returning(|_| Box::pin(async { Ok(None) }));
    git.expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    git.expect_diff_stats()
        .returning(|_, _| Box::pin(async { Ok(DiffStats::default()) }));
    app.services.set_git_client(Arc::new(git));
    seed_session(&mut app.sessions, app.services.db()).await;
    let mut client = MockRunClient::new();
    client.expect_submit().once().returning(|request| {
        assert!(request.prompt.contains(r#""Start implementing""#));

        Ok(OneShotSubmission {
            response: AgentResponse::plain("accept"),
            stats: SessionStats::default(),
        })
    });
    app.services.set_run_client(Arc::new(client));

    (app, directory, observed_rx)
}

/// Starts the reader graph and runs its first node, then drops the driver as
/// a restart or a stopped session does, and returns the paused progress.
async fn pause_after_first_node(
    app: &mut App,
    observed_rx: &mut mpsc::UnboundedReceiver<TurnRequest>,
) -> ExecutionGraphProgress {
    let service = app.session_service();
    let request = StartExecutionGraphRequest {
        graph: reader_graph(),
        provider_call_limits: BTreeMap::from([("calls".into(), 2)]),
    };
    app.drive_session_request(service.start_execution_graph(request))
        .await
        .expect("start");
    app.drive_session_request(async { observed_rx.recv().await.expect("first turn") })
        .await;
    wait_for_node(app, 0, GraphNodeStatus::Succeeded).await;
    app.services.stop_graph_drivers(None);
    let budget_writers = app.services.graph_budget_writers.clone();
    tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async move {
            while budget_writers.has_unrecorded_spending("graph") {
                tokio::task::yield_now().await;
            }
        }),
    )
    .await
    .expect("stopped driver records its spending");

    app.drive_session_request(service.get_execution_graph("graph"))
        .await
        .expect("progress")
        .expect("graph")
}

/// Drives foreground commands until node `index` reaches `status`.
async fn wait_for_node(
    app: &mut App,
    index: usize,
    status: GraphNodeStatus,
) -> ExecutionGraphProgress {
    let db = app.services.db().clone();
    let mut wake = app.services.graph_wake.subscribe();

    tokio::time::timeout(
        Duration::from_secs(5),
        app.drive_session_request(async move {
            loop {
                wake.borrow_and_update();
                let progress = db
                    .graphs()
                    .progress("graph")
                    .await
                    .expect("progress")
                    .expect("graph");
                if progress.nodes[index].status == status {
                    return progress;
                }
                wake.changed().await.expect("graph wake");
            }
        }),
    )
    .await
    .expect("node status")
}

fn submission(text: &str) -> PromptSubmission {
    PromptSubmission {
        prompt: TurnPrompt::from_text(text.into()),
        session_id: "sess".into(),
        session_mode: PromptSessionMode::Existing,
    }
}

fn unavailable() -> DbError {
    DbError::InvalidData {
        entity: "graph",
        reason: "Read failed".into(),
    }
}

/// Lets background tasks run until the live session status reaches
/// `expected`.
async fn wait_for_status(status: &Mutex<Status>, expected: Status) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while *status.lock().expect("status") != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session status");
}

/// Decision run that accepts once the test opens its gate.
struct GatedAcceptance(Arc<Semaphore>);

#[async_trait]
impl RunClient for GatedAcceptance {
    async fn submit(&self, _request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let _permit = self.0.acquire().await.expect("gate");

        Ok(OneShotSubmission {
            response: AgentResponse::plain("accept"),
            stats: SessionStats::default(),
        })
    }
}

/// Runner of a registered driver that never dispatches in these tests.
struct IdleRunner;

#[async_trait]
impl GraphNodeRunner for IdleRunner {
    fn supports(&self, _node: &GraphNode) -> bool {
        true
    }

    async fn run(&self, _request: GraphRunRequest) -> Result<String, OneShotError> {
        Err(OneShotError::new("idle runner never executes"))
    }
}
