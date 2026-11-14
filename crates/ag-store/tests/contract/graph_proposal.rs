//! Proposal approval is an atomic publication receipt, never execution
//! admission.

use std::collections::BTreeMap;
use std::error::Error;

use ag_protocol::{
    GraphProposalOutcome, GraphProposalResponse, QuestionItem, TurnPrompt, TurnPromptAttachment,
};
use ag_session::test_support::{graph_proposal, graph_proposal_state};
use ag_session::{
    GraphProposalAction, GraphProposalReplyOutcome, GraphProposalResolution, GraphProposalState,
    GraphProposalVersionOutcome, StartExecutionGraphRequest,
};
use ag_store::Database;

async fn seed(database: &Database) -> Result<(), Box<dyn Error>> {
    let project = database.projects().upsert_project("project", None).await?;
    database
        .sessions()
        .insert_session("session", "gpt-6.1-sol", "main", "Review", project)
        .await?;

    Ok(())
}

fn resolved(state: &GraphProposalState) -> Result<GraphProposalVersionOutcome, String> {
    let proposal = graph_proposal();
    let graph = GraphProposalResolution {
        agent: state.agent,
        answers: &state.version.answers,
        definitions: &state.definitions,
        graph_id: &format!("proposal:{}:{}", state.input.id, state.version.number),
        limits: &state.limits,
        permission: state.permission,
        request: &state.input.request,
        revision_attachments: &state.version.attachments,
        session_id: &state.input.session_id,
    }
    .resolve(&proposal)?;
    let provider_call_limits =
        BTreeMap::from([(format!("{}:calls", graph.id), state.limits.provider_calls)]);

    Ok(GraphProposalVersionOutcome::Generated {
        launch: Some(Box::new(StartExecutionGraphRequest {
            graph,
            provider_call_limits,
        })),
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Proposal { proposal },
        },
    })
}

#[tokio::test]
async fn graph_proposal_approval_publishes_once_and_survives_restart_without_execution()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("proposal-")
        .tempdir_in(".")?;
    let path = directory.path().join("proposal.sqlite");
    let database = Database::open(&path).await?;
    seed(&database).await?;
    let state = graph_proposal_state("preview", "session");
    assert!(database.graph_proposals().create(&state).await?);
    assert!(!database.graph_proposals().create(&state).await?);
    database
        .runs()
        .create(&ag_worker::RunInfo {
            folder: "workspace".into(),
            id: "generator".into(),
            parent_id: Some(state.version.parent_id.clone()),
            project_id: None,
            purpose: "graph-proposal".into(),
            session_id: Some("session".into()),
        })
        .await?;
    database
        .graph_proposals()
        .finish("preview", 1, &resolved(&state)?)
        .await?;
    assert_eq!(database.graphs().list_graphs("session").await?, []);

    // Act
    let (first, second) = tokio::join!(
        database.graph_proposals().approve("preview", 1),
        database.graph_proposals().approve("preview", 1)
    );
    assert_ne!(first?, second?);
    let receipt = database
        .graph_proposals()
        .get("preview", None)
        .await?
        .expect("proposal");
    database.pool().close().await;
    drop(database);
    let reopened = Database::open(&path).await?;

    // Assert
    assert_eq!(
        reopened.graph_proposals().get("preview", Some(1)).await?,
        Some(receipt.clone())
    );
    assert_eq!(receipt.version.utility_run_id.as_deref(), Some("generator"));
    assert_eq!(
        receipt.version.approval.expect("approval").limits,
        state.limits
    );
    assert!(!reopened.graph_proposals().approve("preview", 1).await?);
    assert!(!reopened.graph_proposals().create(&state).await?);
    assert_eq!(reopened.graphs().list_graphs("session").await?.len(), 1);
    assert_eq!(
        reopened
            .graphs()
            .budget_limits("proposal:preview:1")
            .await?,
        BTreeMap::from([(
            "proposal:preview:1:calls".to_string(),
            state.limits.provider_calls
        )])
    );
    assert_eq!(
        reopened
            .graphs()
            .list_executions("proposal:preview:1", std::num::NonZeroU64::MIN)
            .await?,
        []
    );
    assert!(
        reopened
            .graph_proposals()
            .reserve("preview", 1, &GraphProposalAction::Regenerate)
            .await
            .is_err()
    );
    assert!(
        reopened
            .graph_proposals()
            .cancel("preview", 1)
            .await
            .is_err()
    );

    Ok(())
}

#[tokio::test]
async fn graph_proposal_clarification_versions_limits_and_cancellation_are_durable()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let state = graph_proposal_state("preview", "session");
    database.graph_proposals().create(&state).await?;
    let clarification = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Clarification {
                questions: vec![QuestionItem::new("Which target?")],
            },
        },
    };
    database
        .graph_proposals()
        .finish("preview", 1, &clarification)
        .await?;
    database
        .graph_proposals()
        .finish("preview", 1, &clarification)
        .await?;
    assert!(
        database
            .graph_proposals()
            .approve("preview", 1)
            .await
            .is_err()
    );
    assert_incomplete_answers_rejected(&database).await;

    // Act
    let answered = database
        .graph_proposals()
        .reserve(
            "preview",
            1,
            &GraphProposalAction::Answer(vec!["Local".into()]),
        )
        .await?;
    assert_eq!(answered.version.number, 2);
    assert!(answered.version.answers[0].contains("Which target?"));
    assert!(answered.version.answers[0].contains("Local"));
    database
        .graph_proposals()
        .finish("preview", 2, &resolved(&answered)?)
        .await?;
    let third = database
        .graph_proposals()
        .reserve("preview", 2, &GraphProposalAction::Regenerate)
        .await?;
    database.graph_proposals().cancel("preview", 3).await?;
    database.graph_proposals().cancel("preview", 3).await?;

    // Assert
    assert_eq!(third.version.answers, answered.version.answers);
    assert_eq!(
        database
            .graph_proposals()
            .get("preview", Some(1))
            .await?
            .expect("old version")
            .version
            .outcome,
        clarification
    );
    assert!(
        database
            .graph_proposals()
            .get("preview", None)
            .await?
            .expect("current")
            .version
            .is_cancelled
    );
    assert!(
        database
            .graph_proposals()
            .approve("preview", 2)
            .await
            .is_err()
    );
    assert!(
        database
            .graph_proposals()
            .finish("preview", 3, &resolved(&third)?)
            .await
            .is_err()
    );
    assert!(
        database
            .graph_proposals()
            .reserve("preview", 3, &GraphProposalAction::Regenerate)
            .await
            .is_err()
    );
    assert_eq!(database.graphs().list_graphs("session").await?, []);
    assert_eq!(database.graph_proposals().list("session").await?.len(), 1);
    assert!(
        database
            .graph_proposals()
            .get("missing", None)
            .await?
            .is_none()
    );

    Ok(())
}

#[tokio::test]
async fn graph_proposal_revision_records_feedback_with_open_questions() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let mut state = graph_proposal_state("preview", "session");
    state.limits.generations = 4;
    database.graph_proposals().create(&state).await?;
    let clarification = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: GraphProposalResponse {
            outcome: GraphProposalOutcome::Clarification {
                questions: vec![QuestionItem::new("Which target?")],
            },
        },
    };
    database
        .graph_proposals()
        .finish("preview", 1, &clarification)
        .await?;
    let empty = database
        .graph_proposals()
        .reserve("preview", 1, &GraphProposalAction::Revise(" ".into()))
        .await;

    // Act
    let answered = database
        .graph_proposals()
        .reserve(
            "preview",
            1,
            &GraphProposalAction::Revise("The local one".into()),
        )
        .await?;
    database
        .graph_proposals()
        .finish("preview", 2, &resolved(&answered)?)
        .await?;
    let image = TurnPromptAttachment {
        local_image_path: "sketch.png".into(),
        placeholder: "[Image #1]".into(),
    };
    let mut feedback = TurnPrompt::from_text("Add a test node like [Image #1]".into());
    feedback.attachments.push(image.clone());
    let revised = database
        .graph_proposals()
        .reserve("preview", 2, &GraphProposalAction::Revise(feedback))
        .await?;
    let superseded = database
        .graph_proposals()
        .reserve(
            "preview",
            3,
            &GraphProposalAction::Revise("Also lint".into()),
        )
        .await?;

    // Assert
    assert!(empty.is_err());
    assert_eq!(
        answered.version.answers,
        [r#"{"questions":["Which target?"],"reply":"The local one"}"#]
    );
    assert_eq!(revised.version.number, 3);
    assert_eq!(
        revised.version.answers[1],
        r#"{"feedback":"Add a test node like [Image #1]","images":[{"placeholder":"[Image #1]","task_input_attachment":1}]}"#
    );
    assert_eq!(revised.version.attachments, std::slice::from_ref(&image));
    assert_eq!(superseded.version.answers[2], r#"{"feedback":"Also lint"}"#);
    assert_eq!(superseded.version.attachments, [image]);
    assert_eq!(
        database
            .graph_proposals()
            .get("preview", Some(3))
            .await?
            .map(|stored| stored.version.attachments),
        Some(revised.version.attachments)
    );

    Ok(())
}

async fn assert_incomplete_answers_rejected(database: &Database) {
    for action in [
        GraphProposalAction::Answer(vec![]),
        GraphProposalAction::Answer(vec![" ".into()]),
        GraphProposalAction::Approve,
    ] {
        assert!(
            database
                .graph_proposals()
                .reserve("preview", 1, &action)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn graph_proposal_rejects_conflicts_and_rolls_back_failed_publication()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let mut state = graph_proposal_state("preview", "session");
    state.limits.generations = 1;
    database.graph_proposals().create(&state).await?;
    assert!(
        database
            .graph_proposals()
            .finish("preview", 1, &GraphProposalVersionOutcome::Generating)
            .await
            .is_err()
    );
    assert!(
        database
            .graph_proposals()
            .reserve("preview", 1, &GraphProposalAction::Regenerate)
            .await
            .is_err()
    );
    database
        .graph_proposals()
        .finish("preview", 1, &resolved(&state)?)
        .await?;
    assert!(
        database
            .graph_proposals()
            .finish(
                "preview",
                1,
                &GraphProposalVersionOutcome::Failed("conflict".into())
            )
            .await
            .is_err()
    );
    let mut conflict = state.clone();
    conflict.input.request.text = "different".into();
    assert!(database.graph_proposals().create(&conflict).await.is_err());
    sqlx::query(
        "CREATE TRIGGER reject_publication BEFORE INSERT ON execution_graph_revision BEGIN SELECT \
         RAISE(ABORT, 'test publication failure'); END",
    )
    .execute(database.pool())
    .await?;

    // Act
    assert!(
        database
            .graph_proposals()
            .approve("preview", 1)
            .await
            .is_err()
    );

    // Assert
    assert!(
        database
            .graph_proposals()
            .get("preview", None)
            .await?
            .expect("proposal")
            .version
            .approval
            .is_none()
    );
    assert_eq!(database.graphs().list_graphs("session").await?, []);
    sqlx::query("DROP TRIGGER reject_publication")
        .execute(database.pool())
        .await?;
    assert!(database.graph_proposals().approve("preview", 1).await?);
    let mut invalid = graph_proposal_state("", "session");
    assert!(database.graph_proposals().create(&invalid).await.is_err());
    invalid.input.id = "other".into();
    invalid.version.number = 2;
    assert!(database.graph_proposals().create(&invalid).await.is_err());

    Ok(())
}

#[tokio::test]
async fn graph_proposal_rejects_corrupt_persisted_json() -> Result<(), Box<dyn Error>> {
    for column in ["capture", "snapshot", "approval"] {
        // Arrange
        let database = Database::in_memory().await?;
        seed(&database).await?;
        database
            .graph_proposals()
            .create(&graph_proposal_state("preview", "session"))
            .await?;
        let statement = match column {
            "capture" => "UPDATE graph_proposal SET capture = '{' WHERE id = 'preview'",
            "snapshot" => {
                "UPDATE graph_proposal_version SET snapshot = '{' WHERE graph_proposal_id = \
                 'preview'"
            }
            _ => {
                "UPDATE graph_proposal_version SET approval = '{' WHERE graph_proposal_id = \
                 'preview'"
            }
        };
        sqlx::query(statement).execute(database.pool()).await?;

        // Act
        let result = database.graph_proposals().get("preview", None).await;

        // Assert
        assert!(
            matches!(result, Err(ag_store::DbError::InvalidData { .. })),
            "{column}"
        );
    }

    Ok(())
}

#[tokio::test]
async fn graph_proposal_actions_reject_missing_and_incomplete_captures()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let state = graph_proposal_state("preview", "session");
    database.graph_proposals().create(&state).await?;
    sqlx::query("DELETE FROM graph_proposal_version WHERE graph_proposal_id = 'preview'")
        .execute(database.pool())
        .await?;

    // Act
    let duplicate = database.graph_proposals().create(&state).await;
    let reserve = database
        .graph_proposals()
        .reserve("missing", 1, &GraphProposalAction::Regenerate)
        .await;
    let cancel = database.graph_proposals().cancel("missing", 1).await;

    // Assert
    for (result, expected) in [
        (duplicate.map(|_| ()), "Proposal disappeared"),
        (reserve.map(|_| ()), "Proposal is missing"),
        (cancel, "Proposal is missing"),
    ] {
        assert!(
            result
                .expect_err("invalid capture")
                .to_string()
                .contains(expected)
        );
    }

    Ok(())
}

#[tokio::test]
async fn graph_proposal_latest_loads_only_the_newest_session_proposal() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let first = graph_proposal_state("first", "session");
    let second = graph_proposal_state("second", "session");

    // Act
    let empty = database.graph_proposals().latest("session").await?;
    database.graph_proposals().create(&first).await?;
    database.graph_proposals().create(&second).await?;
    let latest = database.graph_proposals().latest("session").await?;
    let other = database.graph_proposals().latest("other").await?;

    // Assert
    assert!(empty.is_none());
    assert_eq!(latest, Some(second));
    assert!(other.is_none());

    Ok(())
}

#[tokio::test]
async fn graph_proposal_capture_preserves_request_images_through_resolution()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let mut state = graph_proposal_state("preview", "session");
    state.input.request.text.push_str(" [Image #1]");
    state.input.request.attachments.push(TurnPromptAttachment {
        local_image_path: "design.png".into(),
        placeholder: "[Image #1]".into(),
    });

    // Act
    let created = database.graph_proposals().create(&state).await?;
    database
        .graph_proposals()
        .finish("preview", 1, &resolved(&state)?)
        .await?;
    let stored = database
        .graph_proposals()
        .get("preview", None)
        .await?
        .expect("proposal");

    // Assert
    assert!(created);
    assert_eq!(stored.input, state.input);
    assert_eq!(stored.version.outcome, resolved(&state)?);

    Ok(())
}

#[tokio::test]
async fn proposal_replies_keep_their_first_decision_and_recover_until_settled()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let directory = tempfile::Builder::new()
        .prefix("proposal-reply-")
        .tempdir_in(".")?;
    let path = directory.path().join("reply.sqlite");
    let database = Database::open(&path).await?;
    seed(&database).await?;
    let state = graph_proposal_state("preview", "session");
    database.graph_proposals().create(&state).await?;
    let proposals = database.graph_proposals();

    // Act
    let recorded = proposals
        .record_reply("reply", "preview", 1, "Ship it")
        .await?;
    let redelivered = proposals
        .record_reply("reply", "preview", 1, "Ship it")
        .await?;
    let conflicting = proposals.record_reply("reply", "preview", 1, "Other").await;
    let blank = proposals.record_reply(" ", "preview", 1, "Ship it").await;
    let outdated = proposals.record_reply("stale", "preview", 2, "Later").await;
    database.pool().close().await;
    drop(database);
    let reopened = Database::open(&path).await?;
    let proposals = reopened.graph_proposals();
    let pending = proposals.unsettled_replies("session").await?;
    let decided = proposals
        .decide_reply("reply", GraphProposalReplyOutcome::Approve)
        .await?;
    let redecided = proposals
        .decide_reply("reply", GraphProposalReplyOutcome::Cancel)
        .await?;
    let missing_decision = proposals
        .decide_reply("missing", GraphProposalReplyOutcome::Revise)
        .await;
    proposals.settle_reply("reply").await?;
    let missing_settlement = proposals.settle_reply("missing").await;
    let settled = proposals.unsettled_replies("session").await?;
    proposals
        .record_reply("revise", "preview", 1, "Again")
        .await?;
    proposals
        .decide_reply("revise", GraphProposalReplyOutcome::Revise)
        .await?;
    proposals
        .reserve("preview", 1, &GraphProposalAction::Revise("Again".into()))
        .await?;
    let superseded = proposals.unsettled_replies("session").await?;
    proposals
        .record_reply("cancel", "preview", 2, "Drop it")
        .await?;
    let cancel = proposals
        .decide_reply("cancel", GraphProposalReplyOutcome::Cancel)
        .await?;
    proposals.cancel("preview", 2).await?;
    let after_cancel = proposals.unsettled_replies("session").await?;
    let messages = reopened.sessions().load_session_messages("session").await?;

    // Assert
    assert!(recorded);
    assert!(!redelivered);
    assert!(conflicting.is_err());
    assert!(blank.is_err());
    assert!(outdated.is_err());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message, "Ship it");
    assert_eq!(pending[0].session_id, "session");
    assert_eq!(pending[0].outcome, None);
    assert!(!pending[0].is_settled);
    assert_eq!(decided.outcome, Some(GraphProposalReplyOutcome::Approve));
    assert_eq!(redecided.outcome, Some(GraphProposalReplyOutcome::Approve));
    assert!(missing_decision.is_err());
    assert!(missing_settlement.is_err());
    assert_eq!(settled, []);
    assert_eq!(superseded, []);
    assert_eq!(cancel.outcome, Some(GraphProposalReplyOutcome::Cancel));
    assert_eq!(after_cancel, []);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["Ship it", "Again", "Drop it"]
    );

    Ok(())
}

#[tokio::test]
async fn approved_proposals_reject_new_replies() -> Result<(), Box<dyn Error>> {
    // Arrange
    let database = Database::in_memory().await?;
    seed(&database).await?;
    let state = graph_proposal_state("preview", "session");
    database.graph_proposals().create(&state).await?;
    database
        .graph_proposals()
        .finish("preview", 1, &resolved(&state)?)
        .await?;
    database.graph_proposals().approve("preview", 1).await?;

    // Act
    let reply = database
        .graph_proposals()
        .record_reply("late", "preview", 1, "Ship it")
        .await;

    // Assert
    assert!(
        reply
            .expect_err("approved")
            .to_string()
            .contains("Approved proposals")
    );

    Ok(())
}
