use std::num::NonZeroU64;
use std::path::Path;

use ag_protocol::{TurnPrompt, TurnPromptAttachment};
use ag_session::{
    AgentKind, AgentModel, AgentSelection, ContinueGraphNodeRequest, ExecutionGraph,
    GraphExecutionRef, GraphNodeContinuation, GraphResultRef, PlanThenImplementAgents,
    SessionMessageKind, StartPlanThenImplementRequest, Task, TaskAgentRole, TaskChain,
    TaskChainPhase, TaskChainReplyOutcome,
};
use ag_worker::{GraphAdmission, GraphDispatch};

use crate::execution_graph::SqliteGraphRepository;
use crate::{Database, GraphEvidence, TaskAgentTurnMetadata};

#[path = "task_chain_concurrency_test.rs"]
mod concurrency;

fn chain_graph(chain_id: &str) -> ExecutionGraph {
    StartPlanThenImplementRequest {
        agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt61Sol),
        agents: Box::new(PlanThenImplementAgents::bundled().expect("bundled agents")),
        attachments: Vec::new(),
        chain_id: chain_id.into(),
        user_request: "Add search".into(),
    }
    .graph(&"session".into(), None)
}

async fn started() -> Database {
    let database = crate::test_support::task_database_fixture().await;
    assert!(
        database
            .graphs()
            .start_task_chain(&chain_graph("chain"), "Add search")
            .await
            .expect("start")
    );

    database
}

async fn claim(database: &Database, node: &str) -> GraphDispatch {
    let admission = database
        .graph_dispatch()
        .claim(
            "chain",
            NonZeroU64::MIN,
            node,
            &uuid::Uuid::new_v4().to_string(),
            Path::new("workspace"),
        )
        .await
        .expect("admission");
    let dispatch = match admission {
        GraphAdmission::Claimed(dispatch) => Some(dispatch),
        _ => None,
    }
    .expect("new node claim");

    *dispatch
}

fn operation_id(dispatch: &GraphDispatch) -> &str {
    match &dispatch.record.execution {
        GraphExecutionRef::TaskAttempt { operation_id, .. } => Some(operation_id.as_str()),
        GraphExecutionRef::UtilityRun { .. } => None,
    }
    .expect("task attempt")
}

fn attempt_id(dispatch: &GraphDispatch) -> &str {
    match &dispatch.record.execution {
        GraphExecutionRef::TaskAttempt { attempt_id, .. } => Some(attempt_id.as_str()),
        GraphExecutionRef::UtilityRun { .. } => None,
    }
    .expect("task attempt")
}

async fn settle(database: &Database, dispatch: &GraphDispatch, body: &str) {
    let operation = operation_id(dispatch);
    database
        .operations()
        .mark_session_operation_running(operation)
        .await
        .expect("running");
    database
        .graphs()
        .record_task_turn(
            dispatch,
            &TaskAgentTurnMetadata {
                assistant_message: body.into(),
                harness_turn_report: None,
                instruction_conversation_id: None,
                input_tokens: 1,
                output_tokens: 1,
                provider_conversation_id: None,
            },
        )
        .await
        .expect("turn");
    database
        .operations()
        .mark_session_operation_done(operation)
        .await
        .expect("done");
    database
        .graph_dispatch()
        .finish(dispatch, body)
        .await
        .expect("finish");
}

async fn plan_reference(database: &Database) -> GraphResultRef {
    database
        .graphs()
        .list_results("chain", "planner", "plan")
        .await
        .expect("results")
        .pop()
        .expect("plan")
        .result
        .reference
}

async fn chain(database: &Database) -> TaskChain {
    database
        .graphs()
        .load_task_chain("chain")
        .await
        .expect("load")
        .expect("chain")
}

fn reply(dispatch: &GraphDispatch, message: &str) -> ContinueGraphNodeRequest {
    ContinueGraphNodeRequest {
        action_id: uuid::Uuid::new_v4().to_string(),
        execution: dispatch.record.execution.clone(),
        graph_id: "chain".into(),
        kind: GraphNodeContinuation::Reply {
            attachments: vec![TurnPromptAttachment {
                local_image_path: "sketch.png".into(),
                placeholder: "[Image #1]".into(),
            }],
            message: message.into(),
        },
        node_id: dispatch.record.node_id.clone(),
        revision: NonZeroU64::MIN,
    }
}

async fn messages(database: &Database) -> Vec<(String, String)> {
    database
        .sessions()
        .load_session_messages("session")
        .await
        .expect("messages")
        .into_iter()
        .map(|message| (message.kind, message.content))
        .collect()
}

#[tokio::test]
async fn start_saves_the_graph_with_its_launch_messages_once() {
    // Arrange
    let database = started().await;

    // Act
    let redelivered = database
        .graphs()
        .start_task_chain(&chain_graph("chain"), "Add search")
        .await
        .expect("redelivery");
    let chains = database
        .graphs()
        .list_task_chains("session")
        .await
        .expect("list");

    // Assert
    assert!(!redelivered);
    assert_eq!(
        messages(&database).await,
        [
            (
                SessionMessageKind::UserPrompt.as_str().to_string(),
                "Add search".to_string()
            ),
            (
                SessionMessageKind::AgentGraph.as_str().to_string(),
                "chain".to_string()
            ),
        ]
    );
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].phase, TaskChainPhase::Planning);
    assert_eq!(chains[0].current_execution, None);
    assert_eq!(chains[0].user_request, "Add search");
}

#[tokio::test]
async fn start_rejects_other_graph_shapes_and_occupied_task_identities() {
    // Arrange
    let database = crate::test_support::task_database_fixture().await;
    let mut single = chain_graph("single");
    single.nodes.truncate(1);
    database
        .tasks()
        .create_task(
            "session",
            &ag_session::CreateTaskRequest {
                objective: "Taken".into(),
                task_id: "chain:taken:planner".into(),
            },
        )
        .await
        .expect("occupied task");

    // Act
    let shape = database.graphs().start_task_chain(&single, "Add").await;
    let occupied = database
        .graphs()
        .start_task_chain(&chain_graph("taken"), "Add")
        .await;
    database
        .graphs()
        .save_graph(&single, None)
        .await
        .expect("other graph shape");
    let unrelated = database
        .tasks()
        .create_task(
            "session",
            &ag_session::CreateTaskRequest {
                objective: "Ordinary task".into(),
                task_id: "chain:single:planner".into(),
            },
        )
        .await;
    let missing = database
        .graphs()
        .load_task_chain("missing")
        .await
        .expect("missing");

    // Assert
    assert!(shape.is_err());
    assert!(occupied.is_err());
    assert!(unrelated.is_ok());
    assert_eq!(missing, None);
    assert_eq!(messages(&database).await, [] as [(String, String); 0]);
}

#[tokio::test]
async fn acceptance_releases_the_implementer_with_the_exact_plan() {
    // Arrange
    let database = started().await;
    let planner = claim(&database, "planner").await;
    settle(&database, &planner, r#"{"answer":"Plan version 1"}"#).await;
    let awaiting = chain(&database).await;
    let reference = plan_reference(&database).await;

    // Act
    database
        .graphs()
        .accept_result(
            &reference,
            &GraphEvidence {
                actor: "user".into(),
                reason: "Approved".into(),
            },
        )
        .await
        .expect("accept");
    let accepted = chain(&database).await;
    let implementer = claim(&database, "implementer").await;
    let implementing = chain(&database).await;

    // Assert
    assert_eq!(awaiting.phase, TaskChainPhase::AwaitingPlanAcceptance);
    assert_eq!(awaiting.current_attempt_id(), Some(attempt_id(&planner)));
    assert_eq!(accepted.phase, TaskChainPhase::Implementing);
    assert_eq!(accepted.current_role, TaskAgentRole::Implementer);
    let plan = accepted.accepted_plan.expect("accepted plan");
    assert_eq!(plan.text, "Plan version 1");
    assert_eq!(plan.attempt_id, attempt_id(&planner));
    assert_eq!(plan.task_id, "chain:chain:planner");
    assert_eq!(implementer.inputs[0].reference.result, reference);
    assert_eq!(
        implementing.current_attempt_id(),
        Some(attempt_id(&implementer))
    );
    assert!(messages(&database).await.iter().any(|(kind, content)| {
        kind == SessionMessageKind::AgentAnswer.as_str() && content.contains("Plan version 1")
    }));
}

#[tokio::test]
async fn replies_revise_unaccepted_plans_and_carry_attachments() {
    // Arrange
    let database = started().await;
    let planner = claim(&database, "planner").await;
    settle(&database, &planner, r#"{"answer":"Plan version 1"}"#).await;
    let revision = reply(&planner, "Include search tests");

    // Act
    let recorded = database
        .graphs()
        .continue_node(&revision)
        .await
        .expect("revise");
    let stale = database
        .graphs()
        .accept_result(
            &plan_reference(&database).await,
            &GraphEvidence {
                actor: "user".into(),
                reason: "Approved".into(),
            },
        )
        .await;
    let revised = claim(&database, "planner").await;
    let history = database
        .graphs()
        .execution_history(&revised.record.execution)
        .await
        .expect("history")
        .expect("replay");

    // Assert
    assert!(recorded);
    assert!(stale.is_err());
    assert_eq!(attempt_id(&revised), revision.action_id);
    assert!(history.text.contains("Plan version 1"));
    assert!(history.text.contains("Include search tests"));
    assert_eq!(history.attachments.len(), 1);
    assert_eq!(chain(&database).await.phase, TaskChainPhase::Planning);
}

#[tokio::test]
async fn replies_reject_running_work_and_accepted_plans() {
    // Arrange
    let database = started().await;
    let planner = claim(&database, "planner").await;
    let running = database
        .graphs()
        .continue_node(&reply(&planner, "Too early"))
        .await;
    settle(&database, &planner, r#"{"answer":"Plan"}"#).await;
    database
        .graphs()
        .accept_result(
            &plan_reference(&database).await,
            &GraphEvidence {
                actor: "user".into(),
                reason: "Approved".into(),
            },
        )
        .await
        .expect("accept");

    // Act
    let accepted = database
        .graphs()
        .continue_node(&reply(&planner, "Change it"))
        .await;
    let mut blank = reply(&planner, " ");
    blank.kind = GraphNodeContinuation::Reply {
        attachments: Vec::new(),
        message: " ".into(),
    };
    let blank = database.graphs().continue_node(&blank).await;

    // Assert
    assert!(running.is_err());
    assert!(accepted.is_err());
    assert!(blank.is_err());
}

#[tokio::test]
async fn replies_cannot_revise_a_plan_already_consumed_by_a_dependent() {
    // Arrange
    let database = crate::test_support::task_database_fixture().await;
    let mut graph = chain_graph("chain");
    graph.nodes[1].dependencies[0].gate = ag_session::GraphCompletionGate::ResultValidated {
        output_name: "plan".into(),
    };
    database
        .graphs()
        .save_graph(&graph, None)
        .await
        .expect("graph");
    let planner = claim(&database, "planner").await;
    settle(&database, &planner, r#"{"answer":"Plan"}"#).await;
    claim(&database, "implementer").await;

    // Act
    let revised = database
        .graphs()
        .continue_node(&reply(&planner, "Change it"))
        .await;

    // Assert
    assert!(
        revised
            .expect_err("consumed output is final")
            .to_string()
            .contains("consumed output")
    );
}

#[tokio::test]
async fn replies_answer_questions_in_prose_and_follow_up_on_settled_work() {
    // Arrange
    let database = started().await;
    let planner = claim(&database, "planner").await;
    settle(
        &database,
        &planner,
        r#"{"answer":"","questions":[{"text":"Which index?","options":[]}]}"#,
    )
    .await;
    let questioned = chain(&database).await;

    // Act
    let answered = database
        .graphs()
        .continue_node(&reply(&planner, "Use the title index"))
        .await
        .expect("answer");

    // Assert
    assert_eq!(questioned.phase, TaskChainPhase::PlannerQuestion);
    assert!(answered);
    assert_eq!(chain(&database).await.phase, TaskChainPhase::Planning);
}

#[tokio::test]
async fn cancel_execution_cancels_one_stage_without_closing_the_graph() {
    // Arrange
    let database = started().await;
    let planner = claim(&database, "planner").await;

    // Act
    let canceled = database
        .graphs()
        .cancel_execution(&planner.record.execution)
        .await
        .expect("cancel");
    let repeated = database
        .graphs()
        .cancel_execution(&planner.record.execution)
        .await
        .expect("repeat");
    let retry = database
        .graphs()
        .continue_node(&ContinueGraphNodeRequest {
            kind: GraphNodeContinuation::Retry,
            ..reply(&planner, "unused")
        })
        .await;
    let utility = database
        .graphs()
        .cancel_execution(&GraphExecutionRef::UtilityRun {
            run_id: "run".into(),
        })
        .await;
    let missing = database
        .graphs()
        .cancel_execution(&GraphExecutionRef::TaskAttempt {
            attempt_id: "missing".into(),
            operation_id: "task-attempt:missing".into(),
            task_id: "chain:chain:planner".into(),
        })
        .await;

    // Assert
    assert!(canceled);
    assert!(!repeated);
    assert!(retry.expect("retry canceled stage"));
    assert!(utility.is_err());
    assert!(missing.is_err());
    let progress = database
        .graphs()
        .progress("chain")
        .await
        .expect("progress")
        .expect("graph");
    assert_ne!(progress.status, ag_session::ExecutionGraphStatus::Cancelled);
}

#[tokio::test]
async fn reply_records_bind_their_source_and_keep_the_first_decision() {
    // Arrange
    let database = started().await;
    let planner = claim(&database, "planner").await;
    settle(&database, &planner, r#"{"answer":"Plan"}"#).await;
    let prompt = TurnPrompt::from_text("Looks good".into());
    let repository = database.graphs();

    // Act
    let recorded = repository
        .record_task_chain_reply("chain", "reply", &prompt, &planner.record.execution)
        .await
        .expect("record");
    let redelivered = repository
        .record_task_chain_reply("chain", "reply", &prompt, &planner.record.execution)
        .await
        .expect("redelivery");
    let conflicting = repository
        .record_task_chain_reply(
            "chain",
            "reply",
            &TurnPrompt::from_text("Other".into()),
            &planner.record.execution,
        )
        .await;
    let blank = repository
        .record_task_chain_reply(
            "chain",
            "blank",
            &TurnPrompt::from_text(" ".into()),
            &planner.record.execution,
        )
        .await;
    let undecided = repository
        .load_task_chain_reply("session", "reply")
        .await
        .expect("load")
        .expect("reply");
    let first = repository
        .decide_task_chain_reply("reply", TaskChainReplyOutcome::PlanAccepted, None)
        .await
        .expect("decide");
    let second = repository
        .decide_task_chain_reply(
            "reply",
            TaskChainReplyOutcome::PlanRevised,
            Some("action".into()),
        )
        .await
        .expect("decided");
    let foreign = repository
        .load_task_chain_reply("other", "reply")
        .await
        .expect("foreign");
    let missing = repository
        .decide_task_chain_reply("missing", TaskChainReplyOutcome::Continued, None)
        .await;

    // Assert
    assert!(recorded);
    assert!(!redelivered);
    assert!(conflicting.is_err());
    assert!(blank.is_err());
    assert_eq!(undecided.outcome, None);
    assert_eq!(undecided.prompt, prompt);
    assert_eq!(undecided.source, planner.record.execution);
    assert_eq!(first.outcome, Some(TaskChainReplyOutcome::PlanAccepted));
    assert_eq!(second, first);
    assert_eq!(foreign, None);
    assert!(missing.is_err());
    let user_prompts = messages(&database)
        .await
        .into_iter()
        .filter(|(kind, content)| {
            kind == SessionMessageKind::UserPrompt.as_str() && content == "Looks good"
        })
        .count();
    assert_eq!(user_prompts, 1);
}

#[tokio::test]
async fn stage_cancellation_records_unclaimed_work_without_waiting_for_the_env() {
    for pending_reply in [false, true] {
        // Arrange
        let database = started().await;
        let mut continuation_id = None;
        if pending_reply {
            let planner = claim(&database, "planner").await;
            settle(&database, &planner, r#"{"answer":"Plan"}"#).await;
            assert_eq!(
                database
                    .graphs()
                    .cancel_task_chain_stage("chain")
                    .await
                    .expect("idle"),
                None
            );
            let guidance = reply(&planner, "Revise this plan");
            continuation_id = Some(guidance.action_id.clone());
            database
                .graphs()
                .continue_node(&guidance)
                .await
                .expect("pending reply");
        }
        database
            .operations()
            .insert_session_operation("ordinary", "session", "reply")
            .await
            .expect("busy Env");

        // Act
        let canceled = database
            .graphs()
            .cancel_task_chain_stage("chain")
            .await
            .expect("cancel pending stage")
            .expect("canceled execution");
        let missing = database.graphs().cancel_task_chain_stage("missing").await;
        let snapshot = chain(&database).await;

        // Assert
        assert_eq!(snapshot.phase, TaskChainPhase::Canceled);
        assert_eq!(snapshot.current_execution.as_ref(), Some(&canceled));
        if let Some(continuation_id) = continuation_id {
            assert_eq!(
                snapshot.current_attempt_id(),
                Some(continuation_id.as_str())
            );
        }
        assert!(missing.is_err());
        assert!(
            database
                .operations()
                .is_session_operation_unfinished("ordinary")
                .await
                .expect("unrelated work remains queued")
        );
        assert_eq!(
            database
                .graphs()
                .cancel_task_chain_stage("chain")
                .await
                .expect("repeated"),
            None
        );
        database
            .operations()
            .mark_session_operation_done("ordinary")
            .await
            .expect("release Env");
        let retry_id = uuid::Uuid::new_v4().to_string();
        database
            .graphs()
            .continue_node(&ContinueGraphNodeRequest {
                action_id: retry_id.clone(),
                execution: canceled,
                graph_id: "chain".into(),
                kind: GraphNodeContinuation::Retry,
                node_id: "planner".into(),
                revision: NonZeroU64::MIN,
            })
            .await
            .expect("explicit retry");
        let retry = claim(&database, "planner").await;
        assert_eq!(attempt_id(&retry), retry_id);
    }
}

#[tokio::test]
async fn reply_records_reject_foreign_sources_and_missing_workflows() {
    // Arrange
    let database = started().await;
    database
        .graphs()
        .start_task_chain(&chain_graph("second"), "Second")
        .await
        .expect("second chain");
    let planner = claim(&database, "planner").await;
    let prompt = TurnPrompt::from_text("Reply".into());

    // Act
    let foreign = database
        .graphs()
        .record_task_chain_reply("second", "reply", &prompt, &planner.record.execution)
        .await;
    let missing = database
        .graphs()
        .record_task_chain_reply("missing", "reply", &prompt, &planner.record.execution)
        .await;
    let missing_source = database
        .graphs()
        .record_task_chain_reply(
            "chain",
            "missing-source",
            &prompt,
            &GraphExecutionRef::TaskAttempt {
                task_id: "chain:chain:planner".into(),
                attempt_id: "missing".into(),
                operation_id: "missing".into(),
            },
        )
        .await;

    // Assert
    assert!(foreign.is_err());
    assert!(missing.is_err());
    assert!(
        missing_source
            .expect_err("source must exist")
            .to_string()
            .contains("Reply source execution is missing")
    );
}

#[tokio::test]
async fn workflow_queries_skip_graphs_without_workflow_roles() {
    // Arrange
    let database = started().await;
    let mut other = chain_graph("other");
    other.nodes.truncate(1);
    database
        .graphs()
        .save_graph(&other, None)
        .await
        .expect("other graph");

    // Act
    let chains = database
        .graphs()
        .list_task_chains("session")
        .await
        .expect("workflows");
    let unrelated = database
        .graphs()
        .load_task_chain("other")
        .await
        .expect("lookup");

    // Assert
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].id, "chain");
    assert_eq!(unrelated, None);
}

#[tokio::test]
async fn stage_cancellation_rolls_back_a_claim_blocked_after_its_snapshot() {
    // Arrange: inject closure inside the claim, after its phase read. The
    // blocked claim must roll back this transaction without orphaning a task.
    let database = started().await;
    let before = database
        .graphs()
        .progress("chain")
        .await
        .expect("before")
        .expect("graph");
    sqlx::query(
        "CREATE TRIGGER close_graph BEFORE UPDATE ON execution_graph BEGIN INSERT OR IGNORE INTO \
         execution_graph_cancellation (execution_graph_id, recorded_at) VALUES (NEW.id, 1); END",
    )
    .execute(database.pool())
    .await
    .expect("inject concurrent graph closure");

    // Act
    let canceled = database
        .graphs()
        .cancel_task_chain_stage("chain")
        .await
        .expect("cancel stage");
    let progress = database
        .graphs()
        .progress("chain")
        .await
        .expect("progress")
        .expect("graph");

    // Assert
    assert_eq!(canceled, None);
    assert_eq!(progress, before);
    assert_eq!(
        database.tasks().list_tasks("session").await.expect("tasks"),
        Vec::<Task>::new()
    );
}

#[tokio::test]
async fn accepted_plan_reads_reject_corrupt_execution_and_response_evidence() {
    for utility_source in [false, true] {
        // Arrange
        let database = started().await;
        let planner = claim(&database, "planner").await;
        settle(&database, &planner, r#"{"answer":"Plan"}"#).await;
        let mut reference = plan_reference(&database).await;
        database
            .graphs()
            .accept_result(
                &reference,
                &GraphEvidence {
                    actor: "user".into(),
                    reason: "approved".into(),
                },
            )
            .await
            .expect("accept plan");
        let body = if utility_source {
            reference.execution = GraphExecutionRef::UtilityRun {
                run_id: "utility".into(),
            };
            r#"{"answer":"Plan"}"#
        } else {
            "{"
        };
        sqlx::query("UPDATE execution_graph_result SET reference = ?, body = ?")
            .bind(serde_json::json!(reference).to_string())
            .bind(body)
            .execute(database.pool())
            .await
            .expect("inject corrupt evidence");
        let mut connection = database.pool().acquire().await.expect("connection");

        // Act
        let result =
            SqliteGraphRepository::accepted_plan(&mut connection, &chain_graph("chain")).await;

        // Assert
        let error = result.expect_err("corrupt accepted plan").to_string();
        assert!(
            error.contains(if utility_source {
                "Accepted plan has no task attempt"
            } else {
                "EOF"
            }),
            "{error}"
        );
    }
}

#[test]
fn reply_records_reject_invalid_outcomes_and_malformed_payloads() {
    // Arrange
    let source = serde_json::json!(GraphExecutionRef::TaskAttempt {
        task_id: "task".into(),
        attempt_id: "attempt".into(),
        operation_id: "operation".into(),
    })
    .to_string();
    let prompt = serde_json::json!(TurnPrompt::from_text("Guidance".into())).to_string();
    let cases = [
        (source.clone(), prompt.clone(), "unknown"),
        ("{".into(), prompt, "continued"),
        (source, "{".into(), "continued"),
    ];

    // Act and Assert
    for (source, prompt, outcome) in cases {
        let result = SqliteGraphRepository::reply_record((
            "chain".into(),
            source,
            prompt,
            Some(outcome.into()),
            Some("action".into()),
        ));
        assert!(matches!(result, Err(crate::DbError::InvalidData { .. })));
    }
}
