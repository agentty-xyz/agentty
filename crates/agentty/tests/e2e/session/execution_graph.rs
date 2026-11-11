//! Operate saved branches and a join without ordinary-chat routing.

use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt};
use ag_session::{
    AgentDefinition, EnvId, ExecutionGraph, GraphCompletionGate, GraphContext, GraphDependency,
    GraphExecutionProfile, GraphExecutionRef, GraphNode, GraphOutput, GraphPostProcessing,
    GraphWorkspaceAccess, test_support as model_fixture,
};
use ag_worker::GraphAdmission;
use testty::assertion;
use testty::journey::Journey;
use testty::region::Region;
use testty::step::Step;

use super::fixture::{E2eResult, run_git};
use crate::common::{self, BuilderEnv, FeatureTest, ReleaseGate, SessionSeed};

const SESSION: &str = "graph-ui-session";

async fn seed_graph(env: &BuilderEnv) -> E2eResult {
    std::fs::create_dir_all(env.agentty_root.join("wt").join("graph-ui"))?;
    common::seed_active_project_setting(env).await?;
    common::seed_session(
        env,
        SessionSeed::regular(SESSION, model_fixture::CODEX_MODEL_ID, "main", "Review")
            .with_title("Graph controls"),
    )
    .await?;
    let database = common::open_database(env).await?;
    let definition = AgentDefinition::bundled("planner").ok_or("Bundled planner missing")?;
    let node = |id: &str| GraphNode {
        agent: model_fixture::codex_selection(),
        definition: definition.clone(),
        dependencies: Vec::new(),
        env_id: EnvId::SessionWorktree(SESSION.into()),
        execution: GraphExecutionProfile {
            context: GraphContext::TaskConversation {
                task_id: format!("task-{id}"),
            },
            post_processing: GraphPostProcessing::None,
            response_profile: ProtocolRequestProfile::SessionTurn,
            shared_budget_id: Some("calls".into()),
            workspace_access: GraphWorkspaceAccess::SharedRead,
        },
        id: id.into(),
        inputs: Vec::new(),
        objective: if id == "design" {
            format!("Inspect {id}: {}METADATA_SUFFIX", "nested/".repeat(38))
        } else {
            format!("Inspect {id}")
        },
        outputs: vec![GraphOutput {
            name: "response".into(),
            schema: ProtocolRequestProfile::SessionTurn,
        }],
        request: TurnPrompt::from_agent_data("Inspect repository".into()),
    };
    let mut join = node("join");
    for id in ["design", "inspect", "check"] {
        join.dependencies.push(GraphDependency {
            node_id: id.into(),
            gate: if id == "design" {
                GraphCompletionGate::ResultAccepted {
                    output_name: "response".into(),
                }
            } else {
                GraphCompletionGate::ExecutionSucceeded
            },
        });
    }
    let graph = ExecutionGraph {
        id: "saved-branches".into(),
        nodes: vec![node("design"), node("inspect"), node("check"), join],
        revision: NonZeroU64::MIN,
        session_id: SESSION.into(),
    };
    database.graphs().save_graph(&graph, None).await?;
    for (index, id) in ["design", "inspect", "check"].iter().enumerate() {
        let admission = database
            .graph_dispatch()
            .claim(
                &graph.id,
                graph.revision,
                id,
                &uuid::Uuid::from_u128(index as u128 + 1).to_string(),
                &env.workdir,
            )
            .await?;
        let dispatch = match admission {
            GraphAdmission::Claimed(dispatch) => Some(dispatch),
            _ => None,
        }
        .ok_or("Expected graph claim")?;
        let operation_id = match &dispatch.record.execution {
            GraphExecutionRef::TaskAttempt { operation_id, .. } => Some(operation_id.as_str()),
            GraphExecutionRef::UtilityRun { .. } => None,
        }
        .ok_or("Expected task attempt")?;
        database
            .operations()
            .mark_session_operation_running(operation_id)
            .await?;
        if *id == "check" {
            database
                .operations()
                .mark_session_operation_failed(operation_id, "Fixture failure")
                .await?;
        } else {
            database
                .operations()
                .mark_session_operation_done(operation_id)
                .await?;
            let body = node_output_body(id);
            database.graph_dispatch().finish(&dispatch, &body).await?;
        }
    }

    Ok(())
}

fn node_output_body(id: &str) -> String {
    let body = if id == "design" {
        serde_json::json!({
            "answer": format!(
                "## Exact design output\n\n**Readable plan**\n\n- First step\n- Second step\n\n```rust\nlet ready = true;\n```\n\n`{}OUTPUT_SUFFIX`",
                "nested/".repeat(200)
            ),
            "questions": [],
            "review_comment_outcomes": []
        })
    } else {
        serde_json::json!({
            "answer": "",
            "questions": [{ "text": "Which target?", "options": ["Local", "Remote"] }],
            "review_comment_outcomes": []
        })
    };

    body.to_string()
}

/// Waits for the editor to disappear as well as the controls to reappear.
/// A partially painted frame can still contain controls from before editing.
fn wait_for_graph_controls() -> Journey {
    Journey::new("graph_controls_visible").step(Step::eventually(
        Duration::from_secs(5),
        Duration::from_millis(50),
        |frame| {
            assertion::match_text_in_region(
                frame,
                "q back",
                &Region::full(frame.cols(), frame.rows()),
            )?;

            assertion::match_not_visible(frame, "Graph input")
        },
    ))
}

fn inspect_long_output() -> Journey {
    Journey::new("inspect_long_output")
        .step(Step::eventually(
            Duration::from_secs(5),
            Duration::from_millis(50),
            |frame| assertion::match_not_visible(frame, "OUTPUT_SUFFIX"),
        ))
        .step(Step::write_text("JJ"))
        .step(Step::wait_for_text("OUTPUT_SUFFIX", 5000))
        .step(Step::write_text("KK"))
        .step(Step::wait_for_text("Exact design output", 5000))
}

#[tokio::test]
async fn saved_execution_graph_controls() -> E2eResult {
    // Arrange
    FeatureTest::new("saved_execution_graph_controls")
        .with_git()
        .with_terminal_size(140, 40)
        .setup(|env| Box::pin(seed_graph(env)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph controls", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("w")
                    .wait_for_text("saved-branches", 5000)
                    .wait_for_text("METADATA_SUFFIX", 5000)
                    .wait_for_text("Exact design output", 5000)
                    .wait_for_text("let ready = true;", 5000)
                    .compose(&inspect_long_output())
                    .press_key("a")
                    .wait_for_text("Accepted", 5000)
                    .capture_labeled("accepted", "Exact output accepted")
                    .press_key("j")
                    .wait_for_text("Which target?", 5000)
                    .press_key("n")
                    .wait_for_text("Question 1/1", 5000)
                    .write_text("Local")
                    .press_key("Enter")
                    .wait_for_text("Action admitted", 5000)
                    .press_key("j")
                    .wait_for_text("Fixture failure", 5000)
                    .press_key("t")
                    .wait_for_text("Action admitted", 5000)
                    .press_key("j")
                    .wait_for_text("Requires design", 5000)
                    .wait_for_text("resume required", 5000)
                    .capture_labeled("join", "Join remains paused after explicit continuations")
                    .press_key("s")
                    .step(Step::eventually(
                        Duration::from_secs(5),
                        Duration::from_millis(50),
                        |frame| {
                            assertion::match_text_in_region(
                                frame,
                                "Fresh limits after restart",
                                &Region::full(frame.cols(), frame.rows()),
                            )?;

                            assertion::match_not_visible(frame, "q back")
                        },
                    ))
                    .press_key("Esc")
                    .compose(&wait_for_graph_controls())
                    .press_key("x")
                    .wait_for_text("Type cancel", 5000)
                    .write_text("cancel")
                    .press_key("Enter")
                    .wait_for_text("Cancelled", 5000)
                    .capture_labeled("cancelled", "Graph cancellation is terminal")
                    .press_key("q")
                    .wait_for_text("Enter: reply", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let accepted = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_text_in_region(
                        &accepted,
                        "response v1 (Accepted)",
                        &Region::full(accepted.cols(), accepted.rows()),
                    );
                    for expected in [
                        "Exact design output",
                        "Readable plan",
                        "First step",
                        "let ready = true;",
                    ] {
                        assertion::assert_text_in_region(
                            &accepted,
                            expected,
                            &Region::full(accepted.cols(), accepted.rows()),
                        );
                    }
                    for wrapper in [r#"{"answer""#, r"\n", "**Readable plan**", "```rust"] {
                        assertion::assert_not_visible(&accepted, wrapper);
                    }
                    let join = common::frame_from_capture(&report.captures[1]);
                    assertion::assert_text_in_region(
                        &join,
                        "Requires inspect",
                        &Region::full(join.cols(), join.rows()),
                    );
                    let cancelled = common::frame_from_capture(&report.captures[2]);
                    assertion::assert_not_visible(&cancelled, "s resume graph");
                    assertion::assert_not_visible(frame, "Local");
                    assertion::assert_not_visible(frame, "Execution graphs");
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn rejected_graph_drafts_stay_visible_until_explicit_reconciliation() -> E2eResult {
    // Arrange
    FeatureTest::new("rejected_graph_drafts")
        .with_git()
        .with_terminal_size(80, 24)
        .setup(|env| {
            Box::pin(async move {
                seed_graph(env).await?;
                let database = common::open_database(env).await?;
                sqlx::query(
                    "CREATE TRIGGER reject_graph_continuation BEFORE INSERT ON \
                     execution_graph_continuation BEGIN SELECT RAISE(FAIL, \
                     'PERSISTENCE_REJECTED'); END",
                )
                .execute(database.pool())
                .await?;

                Ok(())
            })
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph controls", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("w")
                    .wait_for_text("saved-branches", 5000)
                    .press_key("e")
                    .write_text(
                        "\x1b[200~Preserved revision guidance\r\nWindows line\rClassic \
                         line\x1b[201~",
                    )
                    .wait_for_text("Windows line", 5000)
                    .wait_for_text("Classic line", 5000)
                    .press_key("Enter")
                    .wait_for_text("Draft retained", 5000)
                    .wait_for_text("Ctrl+r reconcile draft", 5000)
                    .step(Step::eventually(
                        Duration::from_secs(5),
                        Duration::from_millis(50),
                        |frame| {
                            assertion::match_text_in_region(
                                frame,
                                "PERSISTENCE_REJECTED",
                                &Region::full(frame.cols(), frame.rows()),
                            )
                        },
                    ))
                    .capture_labeled("reply", "Rejected reply remains in its editor")
                    .press_key("Enter")
                    .wait_for_text("before submitting", 5000)
                    .press_key("Ctrl+r")
                    .wait_for_text("Draft reconciled", 5000)
                    .wait_for_text("Preserved revision guidance", 5000)
                    .capture_labeled("reconciled", "Explicit reconciliation retains guidance")
                    .press_key("Esc")
                    .compose(&wait_for_graph_controls())
                    .press_key("j")
                    .wait_for_text("Which target?", 5000)
                    .press_key("n")
                    .wait_for_text("Question 1/1", 5000)
                    .write_text("Local documentation only")
                    .press_key("Enter")
                    .wait_for_text("Draft retained", 5000)
                    .wait_for_text("Ctrl+r reconcile draft", 5000)
                    .capture_labeled("answers", "Rejected answer stays bound to its question")
                    .press_key("Esc")
                    .compose(&wait_for_graph_controls())
                    .press_key("q")
                    .wait_for_text("Enter: reply", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    for (index, expected) in [
                        (0, "Preserved revision guidance"),
                        (1, "Preserved revision guidance"),
                        (2, "Local documentation only"),
                    ] {
                        let captured = common::frame_from_capture(&report.captures[index]);
                        assertion::assert_text_in_region(
                            &captured,
                            expected,
                            &Region::full(captured.cols(), captured.rows()),
                        );
                    }
                    let reply = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(reply.cols(), reply.rows());
                    assertion::assert_text_in_region(&reply, "Windows line", &full);
                    assertion::assert_text_in_region(&reply, "Classic line", &full);
                    assertion::assert_not_visible(frame, "Preserved revision guidance");
                    assertion::assert_not_visible(frame, "Local");
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn graph_draft_survives_a_background_session_question() -> E2eResult {
    // Arrange
    let turn = ReleaseGate::new()?;
    let hold_turn = turn.shell_wait();
    FeatureTest::new("graph_draft_background_question")
        .with_git()
        .with_terminal_size(100, 30)
        .setup(move |env| {
            Box::pin(async move {
                seed_graph(env).await?;
                let workspace = env.agentty_root.join("wt").join("graph-ui");
                run_git(&env.workdir, &[
                    "worktree", "add", "-b", "wt/graph-ui",
                    workspace.to_str().ok_or("UTF-8 fixture worktree path")?,
                ])?;
                let database = common::open_database(env).await?;
                database.sessions().update_session_agent_model(
                    SESSION, "claude", "claude-haiku-5-5",
                ).await?;
                let script = format!(r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
cat > /dev/null 2>&1
{hold_turn}
printf '%s\n' '{{"type":"system","subtype":"init"}}'
printf '%s\n' '{{"type":"result","subtype":"success","result":"{{\"answer\":\"Background turn settled.\",\"questions\":[{{\"text\":\"Background session clarification?\",\"options\":[\"Yes\",\"No\"]}}]}}","usage":{{"input_tokens":5,"output_tokens":9}}}}'
"#);
                let executable = env.stub_bin.join("claude");
                std::fs::write(&executable, script)?;
                std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o750))?;

                Ok(())
            })
        })
        .run(
            move |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph controls", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("Ask a session question")
                    .press_key("Enter")
                    .step(Step::eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(50),
                        |frame| assertion::match_text_in_region(
                            frame, "Ctrl+c: stop", &Region::full(frame.cols(), frame.rows()),
                        ),
                    ))
                    .press_key("w")
                    .wait_for_text("saved-branches", 5000)
                    .press_key("e")
                    .write_text("Unsent graph guidance")
                    .compose(&turn.release())
                    .wait_for_stable_frame(500, 10000)
                    .capture_labeled("retained", "Background question preserves the graph draft")
                    .press_key("Esc")
                    .compose(&wait_for_graph_controls())
                    .press_key("q")
                    .wait_for_text("Background session clarification?", 10000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let retained = common::frame_from_capture(&report.captures[0]);
                    let full = Region::full(retained.cols(), retained.rows());
                    assertion::assert_text_in_region(&retained, "Execution graphs", &full);
                    assertion::assert_text_in_region(&retained, "Unsent graph guidance", &full);
                    assertion::assert_text_in_region(
                        frame, "Question 1/1", &Region::full(frame.cols(), frame.rows()),
                    );
                    assertion::assert_not_visible(frame, "Unsent graph guidance");
                })
            },
        )
        .await?;

    Ok(())
}
