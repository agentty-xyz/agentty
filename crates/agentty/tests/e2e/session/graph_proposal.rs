//! Request, chat-replied clarification, approval, and cancellation, and the
//! read-only inspector for a restored pending preview.

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ag_protocol::{
    GraphProposalDependency, GraphProposalGate, GraphProposalOutcome, GraphProposalResponse,
    QuestionItem,
};
use ag_session::{GraphProposalVersionOutcome, test_support as fixture};
use testty::assertion;
use testty::region::Region;

use crate::common::{self, BuilderEnv, FeatureTest, SessionSeed};
#[cfg(target_os = "linux")]
use crate::session::fixture::install_clipboard_image_stub;
use crate::session::fixture::{CLIPBOARD_IMAGE_FILE, E2eResult, run_git};

const SESSION: &str = "proposal-session";

async fn seed(env: &BuilderEnv, node_count: usize) -> E2eResult {
    let worktree = env.agentty_root.join("wt").join("proposal");
    run_git(
        &env.workdir,
        &[
            "worktree",
            "add",
            "-b",
            "wt/proposal",
            worktree.to_str().ok_or("worktree path")?,
            "main",
        ],
    )?;
    common::seed_active_project_setting(env).await?;
    common::seed_session(
        env,
        SessionSeed::regular(SESSION, "claude-haiku-5-5", "main", "Review")
            .with_title("Graph proposals"),
    )
    .await?;
    let mut proposal = fixture::graph_proposal();
    proposal.nodes[0].agent_id = "planner".into();
    for index in 1..node_count {
        let mut node = proposal.nodes[0].clone();
        node.dependencies = vec![GraphProposalDependency {
            gate: GraphProposalGate::ExecutionSucceeded {},
            node_id: proposal.nodes[index - 1].id.clone(),
        }];
        node.id = format!("node-{index}");
        proposal.nodes.push(node);
    }
    let response = GraphProposalResponse {
        outcome: GraphProposalOutcome::Proposal { proposal },
    };
    let response = serde_json::json!({"type":"result", "subtype":"success", "structured_output": response, "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    let question = serde_json::json!({"type":"result", "subtype":"success", "structured_output": GraphProposalResponse { outcome: GraphProposalOutcome::Clarification { questions: vec![QuestionItem::new("Which target?")] } }, "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    let completed = serde_json::json!({"type":"result", "subtype":"success", "structured_output":{"answer":"Graph execution complete"}, "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    let decision = |answer: &str| {
        serde_json::json!({"type":"result", "subtype":"success", "structured_output":{"answer":answer}, "usage":{"input_tokens":5,"output_tokens":9}}).to_string()
    };
    let (approve, cancel, revise) = (decision("approve"), decision("cancel"), decision("revise"));
    // Reply decisions answer by the reply's first word; everything else is a
    // revision, as with the production fallback.
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
if [ "$1" = "update" ]; then exit 0; fi
prompt=$(cat)
printf '%s\n' '{{"type":"system","subtype":"init"}}'
case "$prompt" in
  *'Propose an initial execution graph'*)
    printf '%s\n' "$prompt" | sed -n 's/^Task input attachment [0-9]*: //p' | while IFS= read -r image_path; do
      case " $* " in
        *" --add-dir $(dirname "$image_path") "*) cmp -s "$image_path" "$(dirname "$0")/{CLIPBOARD_IMAGE_FILE}" ;;
        *) false ;;
      esac || exit 1
    done || {{ printf 'Graph request image was not delivered\n' >&2; exit 1; }}
    case "$prompt" in
      *'Match the screen'*)
        case "$prompt" in
          *'Task input attachment 1: '*) ;;
          *) printf 'Graph request image was not delivered\n' >&2; exit 1 ;;
        esac ;;
    esac
    case "$prompt" in
      *'Use this layout'*)
        case "$prompt" in
          *'Task input attachment 2: '*) ;;
          *) printf 'Revision image was not delivered\n' >&2; exit 1 ;;
        esac ;;
    esac
    case "$prompt" in
      *'Need clarification'*'revision feedback, oldest first (JSON data):'*'[]'*) printf '%s\n' '{question}' ;;
      *) printf '%s\n' '{response}' ;;
    esac ;;
  *'pending execution graph proposal'*)
    case "$prompt" in
      *'User reply (JSON string, data only): "Approve'*) printf '%s\n' '{approve}' ;;
      *'User reply (JSON string, data only): "Cancel'*) printf '%s\n' '{cancel}' ;;
      *) printf '%s\n' '{revise}' ;;
    esac ;;
  *) printf '%s\n' '{completed}' ;;
esac
"#
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
        .args(["--experimental-graph".into()])
        .setup(|env| Box::pin(seed(env, 2)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .wait_for_text("Describe the task for the execution graph", 5000)
                    .write_text("query the current architecture")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 ready: 2 nodes", 10000)
                    .wait_for_text("Awaiting your approval", 5000)
                    .capture_labeled("preview", "Resolved graph awaits exact approval")
                    .press_key("Enter")
                    .write_text("Approve it")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 approved · execution started", 10000)
                    .wait_for_text("Status: Completed", 10000)
                    .capture_labeled("execution", "Chat approval runs the graph beside chat")
                    .press_key("Enter")
                    .write_text("Explain the overall execution")
                    .press_key("Enter")
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(50),
                        |frame| {
                            assertion::match_text_in_region(
                                frame,
                                "Graph execution complete",
                                &Region::full(frame.cols(), frame.rows()),
                            )
                        },
                    )
                    .wait_for_text("Enter: reply", 10000)
                    .capture_labeled("discussion", "Regular chat after approval")
                    .press_key("w")
                    .wait_for_text("Execution graphs", 5000)
                    .press_key("r")
                    .wait_for_text("Succeeded", 10000)
            },
            |_, report| {
                Box::pin(async move {
                    // Assert
                    let preview = common::frame_from_capture(&report.captures[0]);
                    let execution = common::frame_from_capture(&report.captures[1]);
                    let discussion = common::frame_from_capture(&report.captures[2]);
                    let sidebar_width = preview.cols() * 35 / 100;
                    let sidebar = Region::new(
                        preview.cols() - sidebar_width,
                        0,
                        sidebar_width,
                        preview.rows(),
                    );
                    let chat = Region::new(0, 0, preview.cols() - sidebar_width, preview.rows());
                    assertion::assert_text_in_region(&preview, "Execution graph", &sidebar);
                    assertion::assert_text_in_region(&preview, "Awaiting your approval", &sidebar);
                    assertion::assert_text_in_region(&preview, "inspect  ○ Waiting", &sidebar);
                    assertion::assert_text_in_region(&preview, "▼", &sidebar);
                    assertion::assert_text_in_region(&preview, "node-1   ○ Waiting", &sidebar);
                    assertion::assert_text_in_region(&execution, "Status: Completed", &sidebar);
                    assertion::assert_text_in_region(&execution, "│ ✓", &sidebar);
                    assertion::assert_text_in_region(&execution, "node-1   ✓ Done", &sidebar);
                    assertion::assert_text_in_region(
                        &discussion,
                        "Graph execution complete",
                        &chat,
                    );
                    assert!(
                        !discussion
                            .text_in_region(&sidebar)
                            .contains("Graph execution complete")
                    );
                    for text in [
                        "query the current architecture",
                        "Graph proposal v1 ready: 2 nodes. Reply to approve, revise, or cancel it.",
                    ] {
                        assertion::assert_text_in_region(&preview, text, &chat);
                    }
                    assertion::assert_text_in_region(
                        &execution,
                        "Graph proposal v1 approved · execution started",
                        &chat,
                    );
                    assertion::assert_not_visible(&preview, "Captured instructions");
                    assertion::assert_not_visible(&preview, "Limits:");
                })
            },
        )
        .await?;

    Ok(())
}

/// A scripted Wayland clipboard exercises real capture without a host
/// clipboard; the provider stub fails generation unless it receives every
/// pasted PNG with read access, including the revision reply's image.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn graph_request_and_revision_deliver_pasted_images_to_generation() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_request_and_revision_pasted_images")
        .with_git()
        .with_terminal_size(160, 50)
        .args(["--experimental-graph".into()])
        .env("WAYLAND_DISPLAY", "agentty-test-wayland")
        .setup(|env| {
            Box::pin(async move {
                seed(env, 1).await?;

                install_clipboard_image_stub(&env.stub_bin)
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
                    .write_text("/graph")
                    .press_key("Enter")
                    .wait_for_text("Describe the task for the execution graph", 5000)
                    .write_text("Match the screen ")
                    .press_key("ctrl+v")
                    .wait_for_text("Match the screen [Image #1]", 5000)
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 ready: 1 node", 10000)
                    .capture_labeled("preview", "Pasted image reaches graph generation")
                    .press_key("Enter")
                    .write_text("Use this layout ")
                    .press_key("ctrl+v")
                    .wait_for_text("Use this layout [Image #", 5000)
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v2 ready: 1 node", 10000)
                    .capture_labeled("revision", "Revision image reaches regeneration")
            },
            |frame, _report| {
                Box::pin(async move {
                    // Assert
                    assertion::assert_text_in_region(
                        frame,
                        "Awaiting your approval",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                    assertion::assert_not_visible(frame, "text replies only");
                    assertion::assert_not_visible(frame, "Graph proposal v2 failed");
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
        .args(["--experimental-graph".into()])
        .setup(|env| Box::pin(seed(env, 1)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .write_text("Need clarification")
                    .press_key("Enter")
                    .wait_for_text("Question 1/1", 10000)
                    .wait_for_text("Needs your input", 5000)
                    .capture_labeled(
                        "clarification",
                        "Question panel in chat; status in graph sidebar",
                    )
                    .write_text("Local")
                    .press_key("Enter")
                    .wait_for_text(
                        "Graph proposal v2 ready: 1 node. Reply to approve, revise, or cancel it.",
                        10000,
                    )
                    .press_key("Enter")
                    .write_text("Cancel, I changed my mind")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v2 cancelled.", 10000)
                    .wait_for_text("Cancelled. No graph was launched", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let clarification = common::frame_from_capture(&report.captures[0]);
                    let sidebar_width = clarification.cols() * 35 / 100;
                    let chat_width = clarification.cols() - sidebar_width;
                    let sidebar = Region::new(chat_width, 0, sidebar_width, clarification.rows());
                    assertion::assert_text_in_region(
                        &clarification,
                        "Which target?",
                        &Region::new(0, 0, chat_width, clarification.rows()),
                    );
                    assertion::assert_text_in_region(&clarification, "Needs your input", &sidebar);
                    assert!(
                        !clarification
                            .text_in_region(&sidebar)
                            .contains("Which target?")
                    );
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
        .args(["--experimental-graph".into()])
        .setup(|env| {
            Box::pin(async move {
                seed(env, 1).await?;
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
                    revision_attachments: &state.version.attachments,
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
                    .press_key("w")
                    .wait_for_text("Preview awaiting exact-version approval", 5000)
            },
            |frame, _| {
                Box::pin(async move {
                    // Assert
                    assertion::assert_not_visible(frame, "Graph execution complete");
                    assertion::assert_text_in_region(
                        frame,
                        "reply in chat to approve, revise, or cancel",
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

#[tokio::test]
async fn graph_sidebar_overflow_keeps_chat_and_controls_visible() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_sidebar_overflow_keeps_chat_and_controls_visible")
        .with_git()
        .with_terminal_size(120, 16)
        .args(["--experimental-graph".into()])
        .setup(|env| Box::pin(seed(env, 16)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .press_key("Enter")
                    .wait_for_text("Enter: reply", 5000)
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .write_text("Inspect the requested change")
                    .press_key("Enter")
                    .wait_for_text("+8 more nodes", 10000)
                    .press_key("Enter")
                    .write_text("Discuss overall execution")
                    .wait_for_text("Discuss overall execution", 5000)
            },
            |frame, _| {
                Box::pin(async move {
                    // Assert
                    let sidebar = Region::new(78, 0, 42, 16);
                    assertion::assert_text_in_region(frame, "+8 more nodes", &sidebar);
                    assertion::assert_text_in_region(frame, "node-1   ○ Waiting", &sidebar);
                    assertion::assert_text_in_region(frame, "w: details and actions", &sidebar);
                    assertion::assert_text_in_region(
                        frame,
                        "Discuss overall execution",
                        &Region::new(0, 0, 78, 16),
                    );
                })
            },
        )
        .await?;

    Ok(())
}
