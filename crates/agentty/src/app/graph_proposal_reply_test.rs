use std::fs;
use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, OneShotSubmission, SessionStats};
use ag_git::MockGitClient;
use ag_protocol::{
    AgentResponse, GraphProposalOutcome, GraphProposalResponse, QuestionItem, TurnPrompt,
    TurnPromptAttachment,
};
use ag_session::test_support::{graph_proposal, graph_proposal_state};
use ag_session::{GraphProposalReplyOutcome, GraphProposalState, GraphProposalVersionOutcome};
use ag_store::{DbError, MockGraphProposalRepository};
use ag_worker::MockRunClient;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use super::{DECISION_PENDING, ProposalReplyDecision, clarification_questions};
use crate::app::execution_graph::tests::seed_session;
use crate::app::graph_proposal_decision::tests::resolved_state;
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::app::{App, session};
use crate::domain::session::SessionId;
use crate::infra::db::GraphProposalReplyRecord;
use crate::test_support::new_test_app;

/// Lets a recovered reply reach its wait for the session runtime.
const RUNTIME_WAIT_GRACE: Duration = Duration::from_millis(50);

#[tokio::test]
async fn cancel_reply_discards_the_inspected_version_and_reports_it_in_chat() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "cancel").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    let outcome = app.submit_prompt(submission("Never mind, drop it")).await;
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Graph proposal v1 cancelled.").await;

    // Assert
    assert_eq!(
        outcome,
        PromptWorkflowOutcome::ShowSession {
            session_id: "sess".into()
        }
    );
    assert!(stored(&app).await.version.is_cancelled);
    assert!(
        messages(&app)
            .await
            .contains(&("user_prompt".to_string(), "Never mind, drop it".to_string()))
    );
    assert!(
        app.services
            .proposal_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[tokio::test]
async fn revise_reply_answers_clarification_and_generates_the_next_version() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.submit_prompt(submission("The local target")).await;
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Generating graph proposal v2…").await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    let state = stored(&app).await;
    assert_eq!(state.version.number, 2);
    assert_eq!(
        state.version.answers,
        [r#"{"questions":["Which target?"],"reply":"The local target"}"#]
    );
    assert!(matches!(
        state.version.outcome,
        GraphProposalVersionOutcome::Generated {
            launch: Some(_),
            ..
        }
    ));
}

#[tokio::test]
async fn approve_reply_targets_the_inspected_version() {
    // Arrange
    let (mut app, _directory) = pending_app(resolved_state("sess"), "approve").await;
    let _consumer = app.sessions.foreground_consumer();

    // Act
    app.submit_prompt(submission("Looks good, run it")).await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Graph proposal").await;

    // Assert
    let state = stored(&app).await;
    assert_eq!(notice, "Graph proposal v1 approved · execution started");
    assert_eq!(
        state.version.approval.map(|approval| approval.version),
        Some(1)
    );
    app.services.wait_for_cleanup_tasks(None).await;
}

#[tokio::test]
async fn decided_action_fails_visibly_when_the_version_changed_meanwhile() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    let _consumer = app.sessions.foreground_consumer();
    app.submit_prompt(submission("Local")).await;
    let command = app.sessions.next_command().await;
    app.services
        .db()
        .graph_proposals()
        .cancel("preview", 1)
        .await
        .expect("cancel");

    // Act
    app.apply_session_runtime_command(command).await;
    let notice = wait_for_message(&app, "Graph proposal:").await;

    // Assert
    assert!(
        notice.contains("Stale or canceled proposal version"),
        "{notice}"
    );
    assert_eq!(stored(&app).await.version.number, 1);
}

#[tokio::test]
async fn reply_during_a_pending_decision_keeps_the_composer_and_starts_no_decision() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "cancel").await;
    let prompt = TurnPrompt::from_text("Local".into());
    mark_deciding(&app, "preview");

    // Act
    let outcome = app
        .submit_graph_proposal_reply(&SessionId::from("sess"), &prompt)
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    assert!(
        messages(&app)
            .await
            .iter()
            .any(|(_, content)| content.contains(DECISION_PENDING))
    );
    assert!(!stored(&app).await.version.is_cancelled);
}

#[tokio::test]
async fn revise_reply_with_an_image_keeps_it_for_the_next_version_and_its_nodes() {
    // Arrange
    let (mut app, _directory) = pending_app(resolved_state("sess"), "revise").await;
    let _consumer = app.sessions.foreground_consumer();
    let image = TurnPromptAttachment {
        local_image_path: "sketch.png".into(),
        placeholder: "[Image #1]".into(),
    };
    let mut reply = submission("Shape it like [Image #1]");
    reply.prompt.attachments.push(image.clone());

    // Act
    app.submit_prompt(reply).await;
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Generating graph proposal v2…").await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    let state = stored(&app).await;
    assert_eq!(state.version.number, 2);
    assert_eq!(state.version.attachments, std::slice::from_ref(&image));
    assert_eq!(
        state.version.answers,
        [
            r#"{"feedback":"Shape it like [Image #1]","images":[{"placeholder":"[Image #1]","task_input_attachment":1}]}"#
        ]
    );
    let launch = match state.version.outcome {
        GraphProposalVersionOutcome::Generated { launch, .. } => launch,
        _ => None,
    }
    .expect("Expected a resolved revision");
    assert!(
        launch
            .graph
            .nodes
            .iter()
            .all(|node| node.request.attachments == [image.clone()])
    );
}

#[tokio::test]
async fn approve_reply_with_an_image_revises_instead_of_dropping_it() {
    // Arrange
    let (mut app, _directory) = pending_app(resolved_state("sess"), "approve").await;
    let _consumer = app.sessions.foreground_consumer();
    let image = TurnPromptAttachment {
        local_image_path: "sketch.png".into(),
        placeholder: "[Image #1]".into(),
    };
    let mut reply = submission("Looks good, also match [Image #1]");
    reply.prompt.attachments.push(image.clone());

    // Act
    app.submit_prompt(reply).await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Generating graph proposal").await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    let state = stored(&app).await;
    assert_eq!(notice, "Generating graph proposal v2…");
    assert_eq!(state.version.number, 2);
    assert!(state.version.approval.is_none());
    assert_eq!(state.version.attachments, std::slice::from_ref(&image));
}

#[tokio::test]
async fn approve_reply_with_an_image_approves_at_the_generation_limit() {
    // Arrange
    let mut limited = resolved_state("sess");
    limited.limits.generations = 1;
    let (mut app, _directory) = pending_app(limited, "approve").await;
    let _consumer = app.sessions.foreground_consumer();
    let mut reply = submission("Looks good, also match [Image #1]");
    reply.prompt.attachments.push(TurnPromptAttachment {
        local_image_path: "sketch.png".into(),
        placeholder: "[Image #1]".into(),
    });

    // Act
    app.submit_prompt(reply).await;
    apply_next_command(&mut app).await;
    let notice = wait_for_message(&app, "Graph proposal").await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    let state = stored(&app).await;
    assert_eq!(
        notice,
        "Graph proposal v1 approved · execution started · reply images not used: generation limit \
         reached"
    );
    assert_eq!(state.version.number, 1);
    assert_eq!(
        state.version.approval.map(|approval| approval.version),
        Some(1)
    );
    assert_eq!(
        state.version.attachments,
        Vec::<TurnPromptAttachment>::new()
    );
}

#[tokio::test]
async fn sessions_without_a_pending_proposal_keep_ordinary_routing() {
    for case in ["disabled", "missing", "cancelled", "approved"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_graph = case != "disabled";
        let mut state = graph_proposal_state("preview", "sess");
        if case != "missing" {
            app.services
                .db()
                .graph_proposals()
                .create(&state)
                .await
                .expect("create");
        }
        if case == "cancelled" {
            state.version.is_cancelled = true;
        }
        if case == "approved" {
            state.version.approval = Some(ag_session::GraphProposalApproval {
                graph_id: "graph".into(),
                limits: state.limits.clone(),
                version: 1,
            });
        }
        if matches!(case, "cancelled" | "approved") {
            let mut proposals = MockGraphProposalRepository::new();
            proposals
                .expect_latest()
                .return_once(move |_| Ok(Some(state)));
            app.services
                .set_graph_proposal_repository(Arc::new(proposals));
        }

        // Act
        let outcome = app
            .submit_graph_proposal_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Hello".into()),
            )
            .await;

        // Assert
        assert_eq!(outcome, None, "{case}");
    }
}

#[tokio::test]
async fn failed_proposal_read_keeps_the_composer() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_graph = true;
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_latest().return_once(|_| {
        Err(DbError::InvalidData {
            entity: "proposal",
            reason: "read unavailable".into(),
        })
    });
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act
    let outcome = app
        .submit_graph_proposal_reply(
            &SessionId::from("sess"),
            &TurnPrompt::from_text("Hello".into()),
        )
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    assert!(
        messages(&app)
            .await
            .iter()
            .any(|(_, content)| content.contains("read unavailable"))
    );
}

#[tokio::test]
async fn reply_for_an_unloaded_session_keeps_the_composer_without_deciding() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    app.experimental_graph = true;
    let mut proposals = MockGraphProposalRepository::new();
    proposals
        .expect_latest()
        .return_once(|_| Ok(Some(graph_proposal_state("preview", "ghost"))));
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));

    // Act
    let outcome = app
        .submit_graph_proposal_reply(
            &SessionId::from("ghost"),
            &TurnPrompt::from_text("Run it".into()),
        )
        .await;

    // Assert
    assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
    assert!(
        app.services
            .proposal_reply_decisions
            .lock()
            .expect("decision lock")
            .is_empty()
    );
}

#[test]
fn only_clarification_versions_ask_questions() {
    // Arrange
    let mut failed = graph_proposal_state("preview", "sess");
    failed.version.outcome = GraphProposalVersionOutcome::Failed("invalid graph".into());
    let mut generating = graph_proposal_state("preview", "sess");
    generating.version.outcome = GraphProposalVersionOutcome::Generating;
    let clarification = clarification_state();
    let resolved = resolved_state("sess");

    // Act
    let clarification_questions_asked = clarification_questions(&clarification);
    let unanswerable = [&resolved, &failed, &generating].map(clarification_questions);

    // Assert
    assert_eq!(
        clarification_questions_asked,
        [QuestionItem::new("Which target?")]
    );
    assert!(unanswerable.iter().all(|questions| questions.is_empty()));
}

/// Seeds the shared review session with one settled proposal version and a
/// worker that answers the reply decision with `decision`.
pub(in crate::app) async fn pending_app(
    outcome_state: GraphProposalState,
    decision: &'static str,
) -> (App, TempDir) {
    let (mut app, directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    install_pending_proposal(&mut app, outcome_state, decision).await;

    (app, directory)
}

/// Stores version 1 of the `preview` proposal of `sess` with
/// `outcome_state`'s outcome and scripts every reply decision as `decision`.
async fn install_pending_proposal(
    app: &mut App,
    outcome_state: GraphProposalState,
    decision: &'static str,
) {
    app.experimental_graph = true;
    let mut state = graph_proposal_state("preview", "sess");
    state.limits = outcome_state.limits;
    app.services
        .db()
        .graph_proposals()
        .create(&state)
        .await
        .expect("create");
    state.version.outcome = outcome_state.version.outcome;
    app.services
        .db()
        .graph_proposals()
        .finish("preview", 1, &state.version.outcome)
        .await
        .expect("finish");
    let mut client = MockRunClient::new();
    client.expect_submit().returning(move |request| {
        let answer = if request.request_kind == AgentRequestKind::GraphProposal {
            serde_json::to_string(&GraphProposalResponse {
                outcome: GraphProposalOutcome::Proposal {
                    proposal: graph_proposal(),
                },
            })
            .expect("response")
        } else {
            decision.to_string()
        };

        Ok(OneShotSubmission {
            response: AgentResponse::plain(&answer),
            stats: SessionStats::default(),
        })
    });
    app.services.set_run_client(Arc::new(client));
}

pub(in crate::app) fn clarification_state() -> GraphProposalState {
    let mut state = graph_proposal_state("preview", "sess");
    state.version.outcome = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Clarification {
                questions: vec![QuestionItem::new("Which target?")],
            },
        },
    };

    state
}

fn submission(text: &str) -> PromptSubmission {
    PromptSubmission {
        prompt: TurnPrompt::from_text(text.into()),
        session_id: "sess".into(),
        session_mode: PromptSessionMode::Existing,
    }
}

/// Admits the action the background decision sent through the gateway.
pub(in crate::app) async fn apply_next_command(app: &mut App) {
    let command = app.sessions.next_command().await;
    app.apply_session_runtime_command(command).await;
}

pub(in crate::app) async fn stored(app: &App) -> GraphProposalState {
    app.services
        .db()
        .graph_proposals()
        .get("preview", None)
        .await
        .expect("read")
        .expect("proposal")
}

async fn messages(app: &App) -> Vec<(String, String)> {
    app.services
        .db()
        .sessions()
        .load_session_messages("sess")
        .await
        .expect("messages")
        .into_iter()
        .map(|message| (message.kind, message.content))
        .collect()
}

/// Waits for the first persisted workflow notice containing `text`.
pub(in crate::app) async fn wait_for_message(app: &App, text: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some((_, content)) = messages(app)
                .await
                .into_iter()
                .find(|(kind, content)| kind == "workflow_notice" && content.contains(text))
            {
                return content;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("notice")
}

/// Marks a proposal as having a reply decision in flight.
pub(in crate::app) fn mark_deciding(app: &App, proposal_id: &str) {
    app.services
        .proposal_reply_decisions
        .lock()
        .expect("decision lock")
        .insert(
            proposal_id.into(),
            ProposalReplyDecision {
                cancellation: CancellationToken::new(),
                is_abandoned: Arc::default(),
                session_id: "sess".into(),
            },
        );
}

#[tokio::test]
async fn restart_recovery_decides_an_undecided_reply_once_per_proposal() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "cancel").await;
    let _consumer = app.sessions.foreground_consumer();
    record_reply(&app, "first", "Never mind, drop it").await;
    record_reply(&app, "second", "Also drop it").await;

    // Act
    app.recover_graph_proposal_replies().await;
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Graph proposal v1 cancelled.").await;
    wait_for_idle_decisions(&app).await;

    // Assert
    assert!(stored(&app).await.version.is_cancelled);
    assert_eq!(unsettled(&app).await, Vec::<String>::new());
}

#[tokio::test]
async fn restart_recovery_reapplies_a_recorded_decision_without_deciding_again() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    let _consumer = app.sessions.foreground_consumer();
    record_cancel_decision(&app).await;

    // Act
    app.recover_graph_proposal_replies().await;
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Graph proposal v1 cancelled.").await;

    // Assert
    assert!(stored(&app).await.version.is_cancelled);
    assert_eq!(stored(&app).await.version.number, 1);
}

#[tokio::test]
async fn startup_recovery_applies_a_recorded_decision_once_the_runtime_accepts_commands() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    record_cancel_decision(&app).await;

    // Act
    app.recover_graph_proposal_replies().await;
    tokio::time::sleep(RUNTIME_WAIT_GRACE).await;
    let unsettled_before_start = unsettled(&app).await;
    let has_notice_before_start = has_workflow_notice(&app).await;
    let _consumer = app.sessions.foreground_consumer();
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Graph proposal v1 cancelled.").await;
    wait_for_idle_decisions(&app).await;

    // Assert
    assert_eq!(unsettled_before_start, ["reply"]);
    assert!(!has_notice_before_start);
    assert!(stored(&app).await.version.is_cancelled);
    assert_eq!(unsettled(&app).await, Vec::<String>::new());
}

#[tokio::test]
async fn recorded_decision_stays_unsettled_when_the_runtime_stops_before_answering() {
    for stops in ["before-start", "mid-action"] {
        // Arrange
        let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
        record_cancel_decision(&app).await;
        let consumer = (stops == "mid-action").then(|| app.sessions.foreground_consumer());

        // Act
        app.recover_graph_proposal_replies().await;
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
        assert!(!app.is_deciding_graph_proposal_reply("preview"), "{stops}");
        assert!(!stored(&app).await.version.is_cancelled, "{stops}");
        assert!(!has_workflow_notice(&app).await, "{stops}");
        assert_eq!(unsettled(&app).await, ["reply"], "{stops}");
    }
}

#[tokio::test]
async fn project_switch_recovers_a_reply_left_in_the_other_project() {
    // Arrange
    let (mut app, _directory, project_id) = app_with_unopened_session("cancel").await;
    let _consumer = app.sessions.foreground_consumer();
    record_reply(&app, "reply", "Never mind, drop it").await;
    app.recover_graph_proposal_replies().await;
    let deciding_before_switch = app.is_deciding_graph_proposal_reply("preview");

    // Act
    app.switch_project(project_id).await.expect("switch");
    let deciding_after_switch = app.is_deciding_graph_proposal_reply("preview");
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Graph proposal v1 cancelled.").await;
    wait_for_idle_decisions(&app).await;

    // Assert
    assert!(!deciding_before_switch);
    assert!(deciding_after_switch);
    assert!(stored(&app).await.version.is_cancelled);
    assert_eq!(unsettled(&app).await, Vec::<String>::new());
}

#[tokio::test]
async fn reply_decided_after_switching_away_is_applied_on_return() {
    // Arrange
    let (mut app, directory, project_id) = app_with_unopened_session("revise").await;
    let away_project_id = add_project(&app, &directory, "away").await;
    let _consumer = app.sessions.foreground_consumer();
    app.switch_project(project_id).await.expect("open project");
    app.submit_graph_proposal_reply(
        &SessionId::from("sess"),
        &TurnPrompt::from_text("The local target".into()),
    )
    .await;
    let command = app.sessions.next_command().await;

    // Act
    app.switch_project(away_project_id)
        .await
        .expect("switch away");
    app.apply_session_runtime_command(command).await;
    wait_for_idle_decisions(&app).await;
    let outcomes_while_away: Vec<_> = app
        .services
        .db()
        .graph_proposals()
        .unsettled_replies("sess")
        .await
        .expect("unsettled replies")
        .into_iter()
        .map(|reply| reply.outcome)
        .collect();
    let version_while_away = stored(&app).await.version.number;
    let has_notice_while_away = has_workflow_notice(&app).await;
    app.switch_project(project_id).await.expect("switch back");
    apply_next_command(&mut app).await;
    wait_for_message(&app, "Generating graph proposal v2…").await;
    wait_for_idle_decisions(&app).await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    assert_eq!(
        outcomes_while_away,
        [Some(GraphProposalReplyOutcome::Revise)]
    );
    assert_eq!(version_while_away, 1);
    assert!(!has_notice_while_away);
    assert_eq!(stored(&app).await.version.number, 2);
    assert_eq!(unsettled(&app).await, Vec::<String>::new());
}

#[tokio::test]
async fn restart_recovery_is_inactive_without_multi_agent_support() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "cancel").await;
    record_reply(&app, "reply", "Drop it").await;
    app.experimental_graph = false;

    // Act
    app.recover_graph_proposal_replies().await;

    // Assert
    assert!(!app.is_deciding_graph_proposal_reply("preview"));
    assert_eq!(unsettled(&app).await, ["reply"]);
}

#[tokio::test]
async fn stopping_the_session_abandons_an_undecided_reply() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    record_reply(&app, "reply", "Local").await;

    // Act
    app.recover_graph_proposal_replies().await;
    app.services.cancel_proposal_reply_decisions(Some("other"));
    app.services.cancel_agent_runs("sess");
    wait_for_message(&app, "reply decision was canceled").await;
    wait_for_idle_decisions(&app).await;

    // Assert
    assert_eq!(stored(&app).await.version.number, 1);
    assert_eq!(unsettled(&app).await, Vec::<String>::new());
}

#[tokio::test]
async fn shutdown_leaves_an_undecided_reply_for_restart_recovery() {
    // Arrange
    let (mut app, _directory) = pending_app(clarification_state(), "revise").await;
    record_reply(&app, "reply", "Local").await;

    // Act
    app.recover_graph_proposal_replies().await;
    app.services.wait_for_cleanup_tasks(None).await;

    // Assert
    assert!(!app.is_deciding_graph_proposal_reply("preview"));
    assert_eq!(stored(&app).await.version.number, 1);
    assert_eq!(unsettled(&app).await, ["reply"]);
}

#[tokio::test]
async fn failed_reply_persistence_keeps_the_composer_and_reports_the_error() {
    for failure in ["record", "decide"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_graph = true;
        let mut proposals = MockGraphProposalRepository::new();
        proposals
            .expect_latest()
            .returning(|_| Ok(Some(clarification_state())));
        proposals
            .expect_record_reply()
            .return_once(move |_, _, _, _| match failure {
                "record" => Err(unavailable("record unavailable")),
                _ => Ok(true),
            });
        proposals
            .expect_decide_reply()
            .returning(|_, _| Err(unavailable("decision unavailable")));
        app.services
            .set_graph_proposal_repository(Arc::new(proposals));
        let mut client = MockRunClient::new();
        client.expect_submit().returning(|_| {
            Ok(OneShotSubmission {
                response: AgentResponse::plain("cancel"),
                stats: SessionStats::default(),
            })
        });
        app.services.set_run_client(Arc::new(client));

        // Act
        let outcome = app
            .submit_graph_proposal_reply(
                &SessionId::from("sess"),
                &TurnPrompt::from_text("Drop it".into()),
            )
            .await;

        // Assert
        if failure == "record" {
            assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt));
            assert!(
                messages(&app)
                    .await
                    .iter()
                    .any(|(_, content)| content.contains("record unavailable"))
            );
        } else {
            wait_for_message(&app, "decision unavailable").await;
            wait_for_idle_decisions(&app).await;
        }
    }
}

/// Records a reply to the pending version, as an interrupted run left it.
async fn record_reply(app: &App, reply_id: &str, message: &str) {
    app.services
        .db()
        .graph_proposals()
        .record_reply(reply_id, "preview", 1, message)
        .await
        .expect("record reply");
}

/// Records the `reply` reply with a saved cancel decision whose action an
/// interrupted run never attempted.
async fn record_cancel_decision(app: &App) {
    record_reply(app, "reply", "Drop it").await;
    app.services
        .db()
        .graph_proposals()
        .decide_reply("reply", GraphProposalReplyOutcome::Cancel)
        .await
        .expect("decide");
}

async fn has_workflow_notice(app: &App) -> bool {
    messages(app)
        .await
        .iter()
        .any(|(kind, _)| kind == "workflow_notice")
}

/// Stores `sess` with a pending clarification proposal in a second project
/// that is not open yet, and scripts every reply decision as `decision`.
async fn app_with_unopened_session(decision: &'static str) -> (App, TempDir, i64) {
    let (mut app, directory) = new_test_app().await;
    let project_id = add_project(&app, &directory, "other").await;
    app.services
        .db()
        .sessions()
        .insert_session("sess", "gpt-6.1-sol", "main", "Review", project_id)
        .await
        .expect("session");
    fs::create_dir_all(session::session_folder(app.services.base_path(), "sess"))
        .expect("session folder");
    install_pending_proposal(&mut app, clarification_state(), decision).await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_detect_git_info()
        .returning(|_| Box::pin(async { None }));
    app.services.set_git_client(Arc::new(git_client));

    (app, directory, project_id)
}

/// Registers an openable project directory named `name`.
async fn add_project(app: &App, directory: &TempDir, name: &str) -> i64 {
    let path = directory.path().join(name);
    fs::create_dir_all(path.join(".git")).expect("repository marker");

    app.services
        .db()
        .projects()
        .upsert_project(&path.to_string_lossy(), None)
        .await
        .expect("project")
}

async fn unsettled(app: &App) -> Vec<String> {
    app.services
        .db()
        .graph_proposals()
        .unsettled_replies("sess")
        .await
        .expect("unsettled replies")
        .into_iter()
        .map(|reply| reply.id)
        .collect()
}

async fn wait_for_idle_decisions(app: &App) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.is_deciding_graph_proposal_reply("preview") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("decision settles");
}

fn unavailable(reason: &str) -> DbError {
    DbError::InvalidData {
        entity: "proposal",
        reason: reason.into(),
    }
}

#[tokio::test]
async fn recovery_skips_lost_proposals_and_tolerates_a_failed_abandonment_write() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app.sessions, app.services.db()).await;
    app.experimental_graph = true;
    let mut proposals = MockGraphProposalRepository::new();
    proposals.expect_unsettled_replies().returning(|_| {
        Ok(["lost", "other"]
            .map(|proposal_id| GraphProposalReplyRecord {
                id: format!("{proposal_id}-reply"),
                is_settled: false,
                message: "Local".into(),
                outcome: None,
                proposal_id: proposal_id.into(),
                session_id: "sess".into(),
                version: 1,
            })
            .to_vec())
    });
    proposals
        .expect_get()
        .returning(|id, _| Ok((id == "other").then(clarification_state)));
    proposals
        .expect_settle_reply()
        .once()
        .returning(|_| Err(unavailable("settlement unavailable")));
    app.services
        .set_graph_proposal_repository(Arc::new(proposals));
    let mut client = MockRunClient::new();
    client.expect_submit().returning(|_| {
        Ok(OneShotSubmission {
            response: AgentResponse::plain("revise"),
            stats: SessionStats::default(),
        })
    });
    app.services.set_run_client(Arc::new(client));

    // Act
    app.recover_graph_proposal_replies().await;
    let lost_deciding = app.is_deciding_graph_proposal_reply("lost");
    app.services.cancel_agent_runs("sess");
    wait_for_message(&app, "reply decision was canceled").await;

    // Assert
    assert!(!lost_deciding);
    assert!(!app.is_deciding_graph_proposal_reply("other"));
}
