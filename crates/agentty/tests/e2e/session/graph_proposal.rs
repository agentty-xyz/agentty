//! Request, clarification, preview, exact approval, and restored pending
//! inspection.

use std::os::unix::fs::PermissionsExt;

use ag_protocol::{GraphProposalOutcome, GraphProposalResponse, QuestionItem};
use ag_session::{GraphProposalVersionOutcome, test_support as fixture};
use testty::assertion;
use testty::region::Region;

use crate::common::{self, BuilderEnv, FeatureTest, SessionSeed};
use crate::session::fixture::E2eResult;

const SESSION: &str = "proposal-session";

async fn seed(env: &BuilderEnv) -> E2eResult {
    std::fs::create_dir_all(env.agentty_root.join("wt").join("proposal"))?;
    common::seed_active_project_setting(env).await?;
    common::seed_session(
        env,
        SessionSeed::regular(SESSION, "claude-haiku-5-5", "main", "Review")
            .with_title("Graph proposals"),
    )
    .await?;
    let mut proposal = fixture::graph_proposal();
    proposal.nodes[0].agent_id = "planner".into();
    let response = GraphProposalResponse {
        outcome: GraphProposalOutcome::Proposal { proposal },
    };
    let response = serde_json::json!({"type":"result", "subtype":"success", "structured_output": response, "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    let question = serde_json::json!({"type":"result", "subtype":"success", "structured_output": GraphProposalResponse { outcome: GraphProposalOutcome::Clarification { questions: vec![QuestionItem::new("Which target?")] } }, "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    let completed = serde_json::json!({"type":"result", "subtype":"success", "structured_output":{"answer":"Graph execution complete"}, "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'claude 0.0.0-test\\n'; exit 0; \
         fi\nif [ \"$1\" = \"update\" ]; then exit 0; fi\nprompt=$(cat)\nprintf '%s\\n' \
         '{{\"type\":\"system\",\"subtype\":\"init\"}}'\ncase \"$prompt\" in\n  *'Propose an \
         initial execution graph'*)\n    case \"$prompt\" in\n      *'Need \
         clarification'*'Complete clarification answer history (JSON data):'*'[]'*) printf \
         '%s\\n' '{question}' ;;\n      *) printf '%s\\n' '{response}' ;;\n    esac ;;\n  *) \
         printf '%s\\n' '{completed}' ;;\nesac\n"
    );
    let provider = env.stub_bin.join("claude");
    std::fs::write(&provider, script)?;
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

#[tokio::test]
async fn graph_proposal_request_preview_approval_execution() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_proposal_request_preview_approval_execution")
        .with_git()
        .with_terminal_size(160, 50)
        .args(["--experimental-multi-agent".into()])
        .setup(|env| Box::pin(seed(env)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("/graph new preview Inspect the requested change")
                    .press_key("Enter")
                    .wait_for_text("Preview awaiting exact-version approval", 10000)
                    .capture_labeled("preview", "Resolved graph awaits exact approval")
                    .press_key("Enter")
                    .write_text("/graph approve  preview  1")
                    .press_key("Enter")
                    .wait_for_text("Approved exact version 1", 10000)
                    .press_key("w")
                    .wait_for_text("proposal:preview:1", 5000)
                    .press_key("r")
                    .wait_for_text("Succeeded", 10000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let preview = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_text_in_region(
                        &preview,
                        "Nodes have not launched",
                        &Region::full(preview.cols(), preview.rows()),
                    );
                    assertion::assert_text_in_region(
                        frame,
                        "Graph execution complete",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn graph_proposal_clarification_and_cancel() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_proposal_clarification_and_cancel")
        .with_git()
        .with_terminal_size(160, 50)
        .args(["--experimental-multi-agent".into()])
        .setup(|env| Box::pin(seed(env)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("/graph new questions Need clarification")
                    .press_key("Enter")
                    .wait_for_text("Which target?", 10000)
                    .press_key("Enter")
                    .write_text("/graph answer questions 1 [\"Local\"]")
                    .press_key("Enter")
                    .wait_for_text("Preview awaiting exact-version approval", 10000)
                    .press_key("Enter")
                    .write_text("/graph cancel questions 2")
                    .press_key("Enter")
                    .wait_for_text("Cancelled. No graph was launched", 5000)
            },
            |frame, _| {
                Box::pin(async move {
                    // Assert
                    assertion::assert_text_in_region(
                        frame,
                        "Cancelled. No graph was launched",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn graph_proposal_restored_pending_preview() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_proposal_restored_pending_preview")
        .with_git()
        .with_terminal_size(160, 50)
        .args(["--experimental-multi-agent".into()])
        .setup(|env| {
            Box::pin(async move {
                seed(env).await?;
                let database = common::open_database(env).await?;
                let mut state = fixture::graph_proposal_state("restored", SESSION);
                state.agent = ag_session::AgentSelection::new(
                    ag_session::AgentKind::Claude,
                    ag_session::AgentModel::ClaudeHaiku55,
                );
                state.definitions[0].prompt = "Saved instructions from the proposal \
                                               snapshot.\nPreserve these after restart."
                    .into();
                database.graph_proposals().create(&state).await?;
                let proposal = fixture::graph_proposal();
                let graph = ag_session::GraphProposalResolution {
                    agent: state.agent,
                    answers: &state.version.answers,
                    definitions: &state.definitions,
                    graph_id: "proposal:restored:1",
                    limits: &state.limits,
                    permission: state.permission,
                    request: &state.input.request,
                    session_id: &state.input.session_id,
                }
                .resolve(&proposal)?;
                let outcome = GraphProposalVersionOutcome::Generated {
                    launch: Some(Box::new(ag_session::StartExecutionGraphRequest {
                        provider_call_limits: std::collections::BTreeMap::from([(
                            "proposal:restored:1:calls".into(),
                            state.limits.provider_calls,
                        )]),
                        graph,
                    })),
                    response: GraphProposalResponse {
                        outcome: GraphProposalOutcome::Proposal { proposal },
                    },
                };
                database
                    .graph_proposals()
                    .finish("restored", 1, &outcome)
                    .await?;
                Ok(())
            })
        })
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("/graph query")
                    .press_key("Enter")
                    .wait_for_text("Preview awaiting exact-version approval", 5000)
            },
            |frame, _| {
                Box::pin(async move {
                    // Assert
                    assertion::assert_not_visible(frame, "Graph execution complete");
                    assertion::assert_text_in_region(
                        frame,
                        "/graph approve restored 1",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                    assertion::assert_text_in_region(
                        frame,
                        "Saved instructions from the proposal snapshot.",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                    assertion::assert_text_in_region(
                        frame,
                        "Preserve these after restart.",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}
