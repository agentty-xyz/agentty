//! Request, chat-replied clarification, approval, output acceptance, and
//! cancellation, and the read-only inspector for a restored pending preview.

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use ag_protocol::{
    GraphProposal, GraphProposalContext, GraphProposalDependency, GraphProposalGate,
    GraphProposalNode, GraphProposalOutcome, GraphProposalResponse, ProtocolRequestProfile,
    QuestionItem,
};
use ag_session::{GraphProposalVersionOutcome, test_support as fixture};
use testty::assertion;
use testty::frame::CellColor;
use testty::proof::report::ProofReport;
use testty::region::Region;

use crate::common::{self, BuilderEnv, FeatureTest, SessionSeed};
#[cfg(target_os = "linux")]
use crate::session::fixture::install_clipboard_image_stub;
use crate::session::fixture::{CLIPBOARD_IMAGE_FILE, E2eResult, run_git, seed_project_settings};

const SESSION: &str = "proposal-session";

/// Stub-directory file whose content is the generation delay in seconds.
const GENERATION_DELAY_FILE: &str = "generation-delay";

/// Stub-directory file whose presence makes each node turn change the
/// session worktree.
const NODE_CHANGE_FILE: &str = "node-change";

async fn seed(env: &BuilderEnv, node_count: usize) -> E2eResult {
    seed_gated(env, node_count, GraphProposalGate::ExecutionSucceeded {}).await
}

/// Seeds one node whose generation takes `seconds`, long enough to observe.
async fn seed_slow(env: &BuilderEnv, seconds: u32) -> E2eResult {
    seed(env, 1).await?;
    std::fs::write(
        env.stub_bin.join(GENERATION_DELAY_FILE),
        seconds.to_string(),
    )?;

    Ok(())
}

/// Seeds two session-turn nodes joined by an accepted-output gate whose turns
/// change the worktree, with focused review answered by the same provider
/// stub. Session turns, unlike utility nodes, leave the session review-ready.
async fn seed_changing(env: &BuilderEnv) -> E2eResult {
    let gate = GraphProposalGate::ResultAccepted {
        output_name: "report".into(),
    };
    let mut node = fixture::graph_proposal().nodes.remove(0);
    node.agent_id = "planner".into();
    node.execution.context = GraphProposalContext::TaskConversation;
    node.execution.response_profile = ProtocolRequestProfile::SessionTurn;
    node.outputs[0].schema = ProtocolRequestProfile::SessionTurn;
    seed_chain(env, node, 2, gate).await?;
    std::fs::write(env.stub_bin.join(NODE_CHANGE_FILE), "")?;

    seed_project_settings(
        env,
        &[
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", "claude-haiku-5-5"),
        ],
    )
    .await
}

/// Seeds a chain of `node_count` read-only utility nodes, each waiting for
/// its predecessor through `gate`.
async fn seed_gated(env: &BuilderEnv, node_count: usize, gate: GraphProposalGate) -> E2eResult {
    let mut node = fixture::graph_proposal().nodes.remove(0);
    node.agent_id = "planner".into();

    seed_chain(env, node, node_count, gate).await
}

/// Seeds a chain of `node_count` copies of `first`, each waiting for its
/// predecessor through `gate`, and a provider stub that answers generation,
/// reply decisions, focused review, and node turns.
async fn seed_chain(
    env: &BuilderEnv,
    first: GraphProposalNode,
    node_count: usize,
    gate: GraphProposalGate,
) -> E2eResult {
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
    let mut proposal = GraphProposal { nodes: vec![first] };
    for index in 1..node_count {
        let mut node = proposal.nodes[0].clone();
        node.dependencies = vec![GraphProposalDependency {
            gate: gate.clone(),
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
    let accept = decision("accept");
    let review = serde_json::json!({"type":"result", "subtype":"success", "result": serde_json::json!({"project_impact":["Graph change reviewed."],"suggestions":[],"candidate_decisions":[]}).to_string(), "usage":{"input_tokens":5,"output_tokens":9}}).to_string();
    // Proposal reply decisions answer by the reply's first word, and everything
    // else is a revision, as with the production fallback. Output reply
    // decisions accept.
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
if [ "$1" = "update" ]; then exit 0; fi
IFS= read -r prompt
printf '%s\n' '{{"type":"system","subtype":"init"}}'
case "$prompt" in
  *'Review the Git diff for display in a terminal UI'*) printf '%s\n' '{review}' ;;
  *'Propose an initial execution graph'*)
    delay="$(dirname "$0")/{GENERATION_DELAY_FILE}"
    if [ -f "$delay" ]; then sleep "$(cat "$delay")"; fi
    printf '%s\n' "$prompt" | sed 's/"}},"parent_tool_use_id":null}}$//' | awk '{{ gsub(/\\n/, "\n"); print }}' | sed -n 's/^Task input attachment [0-9]*: //p' | while IFS= read -r image_path; do
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
  *'Output awaiting acceptance'*) printf '%s\n' '{accept}' ;;
  *'pending execution graph proposal'*)
    case "$prompt" in
      *'User reply (JSON string, data only): \"Approve'*) printf '%s\n' '{approve}' ;;
      *'User reply (JSON string, data only): \"Cancel'*) printf '%s\n' '{cancel}' ;;
      *) printf '%s\n' '{revise}' ;;
    esac ;;
  *'Graph objective and exact bound results'*)
    if [ -f "$(dirname "$0")/{NODE_CHANGE_FILE}" ]; then printf 'changed by the graph\n' > graph-change.txt; fi
    printf '%s\n' '{completed}' ;;
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
        .setup(|env| Box::pin(seed(env, 2)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .wait_for_text("Describe the task for the execution graph", 5000)
                    .wait_for_text("Graph · claude/claude-haiku-5-5", 5000)
                    .write_text("query the current architecture")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 ready: 2 nodes", 10000)
                    .wait_for_text("Awaiting your approval", 5000)
                    .capture_labeled("preview", "Resolved graph awaits exact approval")
                    .press_key("Enter")
                    .write_text("Approve it")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 approved · execution started", 10000)
                    .wait_for_text("2/2 done", 10000)
                    // The header can be redrawn before the rows below it, so
                    // wait for the last asserted row.
                    .wait_for_text("✓ node-1", 10000)
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
                    .step(common::wait_for_frame_text(
                        &["Graph execution complete"],
                        &["Working..."],
                    ))
                    .capture_labeled("discussion", "Regular chat after approval")
                    .press_key("w")
                    .wait_for_text("Execution graphs", 5000)
                    .press_key("r")
                    .wait_for_text("Succeeded", 10000)
            },
            |_, report| {
                Box::pin(async move {
                    // Assert
                    assert_preview_approval_execution_report(report);
                })
            },
        )
        .await?;

    Ok(())
}

/// Asserts the preview, execution, and discussion captures place graph state
/// in the sidebar and approval transcript in chat.
fn assert_preview_approval_execution_report(report: &ProofReport) {
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
    for (frame, text, region) in [
        (&preview, "Execution graph · v1", &sidebar),
        (&preview, "Awaiting your approval", &sidebar),
        (&preview, "○ inspect", &sidebar),
        (&preview, "┊ claude-haiku-5-5 [high]", &sidebar),
        (&preview, "┊ after inspect", &sidebar),
        (&preview, "○ node-1", &sidebar),
        (&preview, "query the current architecture", &chat),
        (
            &preview,
            "Graph proposal v1 ready: 2 nodes. Reply to approve, revise, or cancel it.",
            &chat,
        ),
        (&execution, "✓ Completed", &sidebar),
        (&execution, "│ claude-haiku-5-5 [high]", &sidebar),
        (&execution, "│ after inspect", &sidebar),
        (&execution, "✓ node-1", &sidebar),
        (
            &execution,
            "Graph proposal v1 approved · execution started",
            &chat,
        ),
        (&discussion, "Graph execution complete", &chat),
    ] {
        assertion::assert_text_in_region(frame, text, region);
    }
    assert!(
        !discussion
            .text_in_region(&sidebar)
            .contains("Graph execution complete")
    );
    assertion::assert_not_visible(&preview, "Captured instructions");
    assertion::assert_not_visible(&preview, "Limits:");
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
                    .compose(&common::open_selected_session_view())
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
                    .wait_for_text("Execution graph · v2", 5000)
                    .wait_for_text("Awaiting your approval", 5000)
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
async fn graph_output_acceptance_reply_continues_the_graph() -> E2eResult {
    // Arrange
    let gate = GraphProposalGate::ResultAccepted {
        output_name: "report".into(),
    };
    FeatureTest::new("graph_output_acceptance_reply_continues_the_graph")
        .with_git()
        .with_terminal_size(160, 50)
        .env("NO_COLOR", "")
        .setup(move |env| Box::pin(seed_gated(env, 2, gate)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .wait_for_text("Describe the task for the execution graph", 5000)
                    .write_text("query the current architecture")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 ready: 2 nodes", 10000)
                    .press_key("Enter")
                    .write_text("Approve it")
                    .press_key("Enter")
                    .wait_for_text("● inspect", 10000)
                    .capture_labeled("awaiting", "Accepted-output gate awaits chat approval")
                    .press_key("Enter")
                    .write_text("Start implementing")
                    .press_key("Enter")
                    .wait_for_text("2/2 done", 10000)
                    // The header can be redrawn before the rows below it, so
                    // wait for the last asserted row.
                    .wait_for_text("✓ node-1", 10000)
                    .capture_labeled("continued", "Chat acceptance continues the graph")
            },
            |_, report| {
                Box::pin(async move {
                    // Assert
                    let awaiting = common::frame_from_capture(&report.captures[0]);
                    let continued = common::frame_from_capture(&report.captures[1]);
                    let sidebar_width = awaiting.cols() * 35 / 100;
                    let sidebar = Region::new(
                        awaiting.cols() - sidebar_width,
                        0,
                        sidebar_width,
                        awaiting.rows(),
                    );
                    let chat = Region::new(0, 0, awaiting.cols() - sidebar_width, awaiting.rows());
                    assertion::assert_text_in_region(&awaiting, "● Blocked", &sidebar);
                    assertion::assert_text_in_region(
                        &awaiting,
                        "inspect awaits your approval",
                        &sidebar,
                    );
                    assertion::assert_text_in_region(&awaiting, "Awaiting your approval", &sidebar);
                    assertion::assert_text_in_region(
                        &awaiting,
                        "┊ needs report accepted",
                        &sidebar,
                    );
                    // Pending dependency labels use the accent color, apart
                    // from muted objectives.
                    assertion::assert_text_has_fg_color(
                        &awaiting,
                        "needs report accepted",
                        &CellColor::new(0, 128, 128),
                    );
                    assertion::assert_text_in_region(&awaiting, "○ node-1", &sidebar);
                    assertion::assert_text_in_region(
                        &continued,
                        "Accepted report from inspect · continuing",
                        &chat,
                    );
                    assertion::assert_text_in_region(&continued, "✓ Completed", &sidebar);
                    let full_bar = "━".repeat(usize::from(sidebar_width - 2));
                    assertion::assert_text_in_region(&continued, &full_bar, &sidebar);
                    assertion::assert_text_in_region(&continued, "✓ node-1", &sidebar);
                    assertion::assert_text_in_region(
                        &continued,
                        "│ needs report accepted",
                        &sidebar,
                    );
                    assertion::assert_text_has_fg_color(
                        &continued,
                        "needs report accepted",
                        &CellColor::new(0, 128, 0),
                    );
                    assertion::assert_not_visible(&continued, "Awaiting your approval");
                })
            },
        )
        .await?;

    Ok(())
}

/// A node changes the worktree and the graph then waits at its approval
/// gate, where the session is review-ready. The review of that change appears
/// only once the whole graph has finished.
#[tokio::test]
async fn automatic_review_waits_for_the_whole_graph() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_holds_automatic_review")
        .with_git()
        .with_terminal_size(160, 50)
        .env("NO_COLOR", "")
        .setup(|env| Box::pin(seed_changing(env)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .wait_for_text("Describe the task for the execution graph", 5000)
                    .write_text("change the worktree in two steps")
                    .press_key("Enter")
                    .wait_for_text("Graph proposal v1 ready: 2 nodes", 10000)
                    .press_key("Enter")
                    .write_text("Approve it")
                    .press_key("Enter")
                    .wait_for_text("● inspect", 10000)
                    .wait_for_text("(+1 -0)", 10000)
                    .wait_for_stable_frame(1500, 10000)
                    .capture_labeled("gated", "Changed diff waits at the gate without a review")
                    .press_key("Enter")
                    .write_text("Start implementing")
                    .press_key("Enter")
                    .wait_for_text("2/2 done", 10000)
                    .wait_for_text("Graph change reviewed.", 30000)
                    .capture_labeled("reviewed", "Review runs once the graph has finished")
            },
            |_, report| {
                Box::pin(async move {
                    // Assert
                    let gated = common::frame_from_capture(&report.captures[0]);
                    let reviewed = common::frame_from_capture(&report.captures[1]);
                    let full = Region::full(gated.cols(), gated.rows());
                    assertion::assert_text_in_region(&gated, "Awaiting your approval", &full);
                    assertion::assert_not_visible(&gated, "Graph change reviewed.");
                    assertion::assert_not_visible(&gated, "Project Impact");
                    for expected in ["✓ Completed", "Project Impact", "Graph change reviewed."] {
                        assertion::assert_text_in_region(&reviewed, expected, &full);
                    }
                })
            },
        )
        .await?;

    Ok(())
}

/// Generation keeps the session working while the sidebar marks the preview
/// as generating. The E2E clock is pinned, so glyph rotation is covered by the
/// sidebar unit tests.
#[tokio::test]
async fn graph_generation_shows_the_session_working_until_the_proposal_is_ready() -> E2eResult {
    // Arrange
    FeatureTest::new("graph_generation_shows_the_session_working")
        .with_git()
        .with_terminal_size(160, 50)
        .setup(|env| Box::pin(seed_slow(env, 2)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .write_text("query the current architecture")
                    .press_key("Enter")
                    .wait_for_text("Working...", 5000)
                    .wait_for_text("Generating preview...", 2000)
                    .capture_labeled("generating", "Generation keeps the session working")
                    .wait_for_text("Graph proposal v1 ready: 1 node", 10000)
                    .step(common::wait_for_frame_text(
                        &["Graph proposal v1 ready: 1 node"],
                        &["Working..."],
                    ))
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let generating = common::frame_from_capture(&report.captures[0]);
                    let chat = Region::new(0, 0, generating.cols() * 65 / 100, generating.rows());
                    let sidebar = Region::new(
                        chat.width,
                        0,
                        generating.cols() - chat.width,
                        generating.rows(),
                    );
                    assertion::assert_text_in_region(&generating, "Working...", &chat);
                    let sidebar_text = generating.text_in_region(&sidebar);
                    assert!(
                        ["◐", "◓", "◑", "◒"].iter().any(|glyph| sidebar_text
                            .contains(&format!("{glyph} Generating preview..."))),
                        "{sidebar_text}"
                    );
                    assertion::assert_not_visible(&generating, "Graph proposal v1 ready");
                    assertion::assert_text_in_region(
                        frame,
                        "Graph proposal v1 ready: 1 node",
                        &chat,
                    );
                    assertion::assert_not_visible(frame, "Working...");
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn stopping_graph_generation_cancels_the_proposal() -> E2eResult {
    // Arrange
    FeatureTest::new("stopping_graph_generation_cancels_the_proposal")
        .with_git()
        .with_terminal_size(160, 50)
        .setup(|env| Box::pin(seed_slow(env, 30)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .write_text("query the current architecture")
                    .press_key("Enter")
                    .wait_for_text("Working...", 5000)
                    .press_key("ctrl+c")
                    .wait_for_text("Cancelled. No graph was launched", 5000)
                    .step(common::wait_for_frame_text(
                        &["Cancelled. No graph was launched"],
                        &["Working..."],
                    ))
            },
            |frame, _| {
                Box::pin(async move {
                    // Assert
                    assertion::assert_not_visible(frame, "Working...");
                    assertion::assert_not_visible(frame, "Graph proposal v1 ready");
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
        .setup(|env| Box::pin(seed(env, 1)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
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
                    .compose(&common::open_selected_session_view())
                    .press_key("w")
                    .wait_for_text("Awaiting approval of this exact version", 5000)
                    .wait_for_text("reply in chat to approve, revise, or cancel", 5000)
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
        .setup(|env| Box::pin(seed(env, 16)))
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .wait_for_text("Graph proposals", 5000)
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .write_text("/graph")
                    .press_key("Enter")
                    .write_text("Inspect the requested change")
                    .press_key("Enter")
                    .wait_for_text("+7 more nodes", 10000)
                    .press_key("Enter")
                    .write_text("Discuss overall execution")
                    .wait_for_text("Discuss overall execution", 5000)
            },
            |frame, _| {
                Box::pin(async move {
                    // Assert
                    let sidebar = Region::new(78, 0, 42, 16);
                    assertion::assert_text_in_region(frame, "+7 more nodes", &sidebar);
                    assertion::assert_text_in_region(frame, "○ node-1", &sidebar);
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
