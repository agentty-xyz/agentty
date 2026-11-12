use std::sync::Arc;
use std::time::Duration;

use ag_contracts::{AgentRequestKind, OneShotSubmission, SessionStats};
use ag_protocol::{
    AgentResponse, GraphProposalOutcome, GraphProposalResponse, QuestionItem, TurnPrompt,
};
use ag_session::test_support::{graph_proposal, graph_proposal_state};
use ag_session::{GraphProposalState, GraphProposalVersionOutcome};
use ag_store::{DbError, MockGraphProposalRepository};
use ag_worker::MockRunClient;
use tempfile::TempDir;

use super::{ATTACHMENTS_UNSUPPORTED, DECISION_PENDING, clarification_questions};
use crate::app::App;
use crate::app::execution_graph::tests::seed_session;
use crate::app::graph_proposal_decision::tests::resolved_state;
use crate::app::prompt_intent::{PromptSessionMode, PromptSubmission, PromptWorkflowOutcome};
use crate::domain::session::SessionId;
use crate::test_support::new_test_app;

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
async fn rejected_replies_keep_the_composer_and_start_no_decision() {
    for case in ["attachments", "pending"] {
        // Arrange
        let (mut app, _directory) = pending_app(clarification_state(), "cancel").await;
        let mut prompt = TurnPrompt::from_text("Local".into());
        if case == "attachments" {
            prompt.attachments.push(ag_protocol::TurnPromptAttachment {
                local_image_path: "image.png".into(),
                placeholder: "[Image #1]".into(),
            });
        } else {
            app.services
                .proposal_reply_decisions
                .lock()
                .expect("decision lock")
                .insert("preview".into());
        }

        // Act
        let outcome = app
            .submit_graph_proposal_reply(&SessionId::from("sess"), &prompt)
            .await;

        // Assert
        assert_eq!(outcome, Some(PromptWorkflowOutcome::KeepPrompt), "{case}");
        let expected = if case == "attachments" {
            ATTACHMENTS_UNSUPPORTED
        } else {
            DECISION_PENDING
        };
        assert!(
            messages(&app)
                .await
                .iter()
                .any(|(_, content)| content.contains(expected)),
            "{case}"
        );
        assert!(!stored(&app).await.version.is_cancelled);
    }
}

#[tokio::test]
async fn sessions_without_a_pending_proposal_keep_ordinary_routing() {
    for case in ["disabled", "missing", "cancelled", "approved"] {
        // Arrange
        let (mut app, _directory) = new_test_app().await;
        seed_session(&mut app.sessions, app.services.db()).await;
        app.experimental_multi_agent = case != "disabled";
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
    app.experimental_multi_agent = true;
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
    app.experimental_multi_agent = true;
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
    app.experimental_multi_agent = true;
    let mut state = graph_proposal_state("preview", "sess");
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

    (app, directory)
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
