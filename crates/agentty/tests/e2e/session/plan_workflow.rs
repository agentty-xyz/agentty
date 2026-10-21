//! One regular-chat journey through the orchestrated Planner → Implementer
//! chain: questions, revision, natural-language approval, cancel, retry, and
//! follow-up.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};
use testty::assertion;
use testty::frame::TerminalFrame;
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::common::{self, BuilderEnv, FeatureTest};
use crate::session::fixture::{E2eResult, run_git, seed_project_settings};

const WORKFLOW_MODEL_ID: &str = "claude-haiku-4-5-20251001";

const IMPLEMENTER_REVIEW_TEXT: &str = "Implementer focused review completed.";
const CLEAN_REVIEW_TEXT: &str = "No suggestions remain after implementation review.";

/// Seeds a scripted Claude provider for every chain role and the
/// orchestrator decision.
///
/// The Planner first asks a question, then plans from the answer and
/// revision. The decision approves only "start implementing". The first
/// Implementer run blocks until canceled; later runs finish immediately.
async fn seed_workflow_project(env: &BuilderEnv) -> E2eResult {
    std::fs::write(
        env.workdir.join("plan_lookup_target.rs"),
        "// lookup target\n",
    )?;
    run_git(&env.workdir, &["add", "plan_lookup_target.rs"])?;
    run_git(&env.workdir, &["commit", "-m", "Add plan lookup target"])?;
    seed_project_settings(
        env,
        &[
            ("DefaultSmartAgent", "claude"),
            ("DefaultSmartModel", WORKFLOW_MODEL_ID),
            ("DefaultReviewAgent", "claude"),
            ("DefaultReviewModel", WORKFLOW_MODEL_ID),
            ("DefaultFastAgent", "claude"),
            ("DefaultFastModel", WORKFLOW_MODEL_ID),
        ],
    )
    .await?;
    let provider = env.stub_bin.join("claude");
    std::fs::write(
        &provider,
        r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude 0.0.0-test\n'; exit 0; fi
prompt=$(cat)
stub_root=$(dirname "$0")
printf '%s\n' '{"type":"system","subtype":"init"}'
questions=''
case "$prompt" in
  *"Generate a concise, commit-style title"*)
    answer='Add indexed search'
    ;;
  *'Generate the canonical session commit message'*)
    if [ "${AGENTTY_TEST_HOLD_COMMIT:-}" = '1' ]; then
      while [ -d "$stub_root" ]; do sleep 0.02; done
      exit 0
    fi
    answer='Implement indexed search'
    ;;
  *'Review the Git diff for display in a terminal UI.'*)
    if grep -q 'reviewed' search.txt 2>/dev/null; then
      result='{\"project_impact\":[\"No suggestions remain after implementation review.\"],\"suggestions\":[]}'
    else
      result='{\"project_impact\":[\"Implementer focused review completed.\"],\"suggestions\":[{\"details\":\"Mark the indexed search implementation as reviewed.\",\"severity\":\"medium\"}],\"candidate_decisions\":[{\"candidate_index\":0,\"suggestion_index\":0,\"reason\":\"The fixture search implementation needs review.\"}]}'
    fi
    printf '%s\n' "{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"$result\",\"usage\":{\"input_tokens\":5,\"output_tokens\":9}}"
    exit 0
    ;;
  *"Decide whether the user's reply approves"*)
    case "$prompt" in
      *'start implementing'*) answer='implement' ;;
      *) answer='revise' ;;
    esac
    ;;
  *'Act as the planner'*)
    case "$prompt" in
      *'Include search tests'*) answer='Plan version 2: index titles and add search tests.' ;;
      *'Use the title index'*) answer='Plan version 1: index titles.' ;;
      *)
        answer=''
        questions=',"questions":[{"text":"Which index should search use?","options":[]}]'
        ;;
    esac
    ;;
  *'Act as the implementer'*)
    if [ ! -f "$AGENTTY_TEST_IMPLEMENTER_STARTED" ]; then
      : > "$AGENTTY_TEST_IMPLEMENTER_STARTED"
      while [ -d "$stub_root" ]; do sleep 0.02; done
      exit 0
    fi
    case "$prompt" in
      *'Also update the docs'*)
        printf 'Indexed search documentation\n' > search-guide.txt
        answer='Docs updated for indexed search.'
        ;;
      *'Verify the focused-review suggestions against the current code'*)
        printf 'reviewed indexed search\n' > search.txt
        answer='Review suggestion applied by the Implementer.'
        ;;
      *)
        printf 'indexed search\n' > search.txt
        answer='Implementation finished from accepted plan version 2.'
        ;;
    esac
    ;;
  *) answer='Session unchanged.' ;;
esac
printf '{"type":"result","subtype":"success","structured_output":{"answer":"%s"%s},"usage":{"input_tokens":8,"output_tokens":6}}\n' "$answer" "$questions"
"#,
    )?;
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Footer text shown only while the composer owns input.
const COMPOSER_FOOTER: &str = "Enter: send";

/// Opens the composer, sends `message`, and returns to the chat view.
fn send_chat_message(scenario: Scenario, message: &str) -> Scenario {
    open_composer(scenario)
        .write_text(message)
        .press_key("Enter")
}

/// Opens the composer from the chat view.
fn open_composer(scenario: Scenario) -> Scenario {
    scenario
        .press_key("Enter")
        .wait_for_text(COMPOSER_FOOTER, 5000)
}

/// Waits until the scripted Implementer process runs, so cancellation
/// exercises the running worker rather than queued admission.
fn wait_for_implementer(scenario: Scenario, started: PathBuf) -> Scenario {
    scenario.eventually(
        Duration::from_secs(10),
        Duration::from_millis(20),
        move |frame| {
            let expected = if started.is_file() {
                "Implementer  ◐ Working"
            } else {
                "Scripted Implementer startup marker"
            };
            assertion::match_text_in_region(
                frame,
                expected,
                &Region::full(frame.cols(), frame.rows()),
            )
        },
    )
}

/// Asserts each expected text is visible in one captured frame.
fn assert_visible(frame: &TerminalFrame, expected: &[&str]) {
    let full = Region::full(frame.cols(), frame.rows());
    for text in expected {
        assertion::assert_text_in_region(frame, text, &full);
    }
}

#[tokio::test]
async fn plan_command_is_unavailable_by_default() -> E2eResult {
    // Arrange, Act, Assert: prove the default launch hides and rejects `/plan`.
    FeatureTest::new("plan_command_disabled")
        .with_git()
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .press_key("/")
                    .wait_for_text("Slash Command", 3000)
                    .capture_labeled("commands", "Planning is absent from the default menu")
                    .write_text("plan")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 3000)
                    .capture_labeled("disabled", "Typing /plan cannot arm the composer")
            },
            |frame, report| {
                Box::pin(async move {
                    let menu = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_not_visible(&menu, "/plan");
                    assertion::assert_not_visible(frame, "Plan then implement");
                    assertion::assert_not_visible(frame, "describe the task for the Planner");
                    assertion::assert_text_in_region(
                        frame,
                        "/plan",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn implementation_diff_is_available_during_post_processing() -> E2eResult {
    // Arrange, Act, Assert: inspect implementation changes while commit
    // generation is held.
    let gate = tempfile::tempdir()?;
    let started = gate.path().join("implementation-started");
    std::fs::write(&started, "started")?;
    FeatureTest::new("implementation_diff_during_post_processing")
        .with_git()
        .with_terminal_size(100, 40)
        .args(["--experimental-multi-agent".to_string()])
        .env(
            "AGENTTY_TEST_IMPLEMENTER_STARTED",
            started.to_string_lossy(),
        )
        .env("AGENTTY_TEST_HOLD_COMMIT", "1")
        .setup(|env| Box::pin(async move { seed_workflow_project(env).await }))
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .write_text("/plan Add indexed search")
                    .press_key("Enter")
                    .wait_for_text("Which index should search use?", 20000);
                let scenario = send_chat_message(scenario, "Use the title index")
                    .wait_for_text("Planner      ● Awaiting your approval", 20000);
                send_chat_message(scenario, "Looks good, start implementing")
                    .wait_for_text(
                        "Implementation finished from accepted plan version 2.",
                        20000,
                    )
                    .wait_for_text("(+1 -0)", 10000)
                    .capture_labeled(
                        "changes",
                        "Changes remain inspectable during commit generation",
                    )
                    .press_key("d")
                    .wait_for_text("+indexed search", 10000)
                    .capture_labeled("diff", "The diff shows the Implementer's changes")
            },
            |frame, report| {
                Box::pin(async move {
                    assert_visible(
                        &common::frame_from_capture(&report.captures[0]),
                        &["(+1 -0)", "Implementer  ◐ Working"],
                    );
                    assert_visible(frame, &["search.txt", "+indexed search"]);
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn plan_conversation_image_paste_reports_clipboard_errors() -> E2eResult {
    // Arrange, Act, Assert: clipboard capture failures stay in the plan
    // composer.
    FeatureTest::new("plan_image_paste_unavailable")
        .with_git()
        .args(["--experimental-multi-agent".to_string()])
        .env("AGENTTY_DISABLE_CLIPBOARD", "1")
        .setup(|env| Box::pin(async move { seed_workflow_project(env).await }))
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .write_text("/plan Add search")
                    .press_key("Enter")
                    .wait_for_text("Which index should search use?", 20000)
                    .wait_for_text("Enter: reply", 10000);
                open_composer(scenario)
                    .write_text("Use this layout ")
                    .press_key("ctrl+v")
                    .wait_for_text("Paste Image Error", 5000)
                    .capture_labeled("paste_error", "Image paste in a Planner conversation")
            },
            |frame, _report| {
                Box::pin(async move {
                    assert_visible(
                        frame,
                        &[
                            "Orchestrator · Planner",
                            "Use this layout",
                            "Paste Image Error",
                            "Clipboard is unavailable",
                        ],
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// A scripted Wayland clipboard exercises real capture without a host
/// clipboard.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn plan_conversation_pastes_images_into_initial_and_revision_messages() -> E2eResult {
    // Arrange, Act, Assert: provider delivery is asserted by the scripted
    // answer.
    FeatureTest::new("plan_conversation_images")
        .with_git()
        .with_terminal_size(100, 40)
        .args(["--experimental-multi-agent".to_string()])
        .env("WAYLAND_DISPLAY", "agentty-test-wayland")
        .setup(|env| Box::pin(async move { seed_image_plan_project(env).await }))
        .run(
            |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .write_text("/plan")
                    .press_key("Enter")
                    .wait_for_text("Plan · describe the task for the Planner", 5000)
                    .write_text("Design the screen ")
                    .press_key("ctrl+v")
                    .wait_for_text("Design the screen [Image #1]", 5000)
                    .press_key("Enter")
                    .wait_for_text("Initial image received by Planner", 20000)
                    .wait_for_text("Enter: reply", 10000);
                open_composer(scenario)
                    .write_text("Revise the screen ")
                    .press_key("ctrl+v")
                    .wait_for_text("Revise the screen [Image #1]", 5000)
                    .press_key("Enter")
                    .wait_for_text("Revision image received by Planner", 20000)
                    .capture_labeled("revised", "Pasted images reach the Planner")
            },
            |frame, _report| {
                Box::pin(async move {
                    assert_visible(
                        frame,
                        &[
                            "Revision image received by Planner",
                            "Planner      ● Awaiting your approval",
                        ],
                    );
                })
            },
        )
        .await?;

    Ok(())
}

#[cfg(target_os = "linux")]
async fn seed_image_plan_project(env: &BuilderEnv) -> E2eResult {
    seed_workflow_project(env).await?;

    install_image_plan_stubs(&env.stub_bin)
}

/// Installs the clipboard fixture and provider that checks rendered image
/// access.
fn install_image_plan_stubs(stub_bin: &Path) -> E2eResult {
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)?;
    std::fs::write(stub_bin.join("clipboard.png"), png)?;
    for (name, script) in [
        (
            "wl-paste",
            r#"#!/bin/sh
case "$1" in
  --version) printf 'wl-paste test\n' ;;
  --list-types) printf 'image/png\n' ;;
  --no-newline) cat "$(dirname "$0")/clipboard.png" ;;
  *) exit 1 ;;
esac
"#,
        ),
        (
            "claude",
            r#"#!/bin/sh
if [ "$1" = "update" ]; then exit 0; fi
if [ "$1" = "--version" ]; then printf 'claude test\n'; exit 0; fi
prompt=$(cat)
fixture="$(dirname "$0")/clipboard.png"
verify_image() {
  image_number=$1
  shift
  image_path=$(printf '%s\n' "$prompt" | sed -n "s/^Task input attachment $image_number: //p")
  [ -f "$image_path" ] && [ -r "$image_path" ] || return 1
  cmp -s "$image_path" "$fixture" || return 1
  image_parent=$(dirname "$image_path")
  previous_argument=''
  for argument in "$@"; do
    if [ "$previous_argument" = '--add-dir' ] && [ "$argument" = "$image_parent" ]; then
      return 0
    fi
    previous_argument=$argument
  done
  return 1
}
case "$prompt" in
  *'Task input attachment 2:'*) image_count=2; answer='Revision image received by Planner' ;;
  *'Task input attachment 1:'*) image_count=1; answer='Initial image received by Planner' ;;
  *) printf 'Missing image attachment\n' >&2; exit 1 ;;
esac
image_number=1
while [ "$image_number" -le "$image_count" ]; do
  verify_image "$image_number" "$@" || {
    printf 'Image attachment %s was not delivered with readable PNG data and --add-dir access\n' "$image_number" >&2
    exit 1
  }
  image_number=$((image_number + 1))
done
printf '{"type":"result","subtype":"success","structured_output":{"answer":"%s"},"usage":{"input_tokens":8,"output_tokens":6}}\n' "$answer"
"#,
        ),
    ] {
        let path = stub_bin.join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o750))?;
    }

    Ok(())
}

#[tokio::test]
async fn image_plan_provider_requires_rendered_readable_images_and_access_roots() -> E2eResult {
    // Arrange: paths with spaces exercise shell argument and prompt parsing.
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
    let stub_bin = directory.path().join("provider fixtures");
    let images = directory.path().join("initial images");
    let revisions = directory.path().join("revision images");
    for path in [&stub_bin, &images, &revisions] {
        std::fs::create_dir(path)?;
    }
    install_image_plan_stubs(&stub_bin)?;
    let first = images.join("first.png");
    let second = revisions.join("second.png");
    for path in [&first, &second] {
        std::fs::copy(stub_bin.join("clipboard.png"), path)?;
    }
    let corrupt = revisions.join("corrupt.png");
    std::fs::write(&corrupt, b"not the clipboard PNG")?;
    let initial = format!("Task input attachment 1: {}", first.display());
    let revision = format!("{initial}\nTask input attachment 2: {}", second.display());
    let unrendered = "Task input attachment 1: [Task image #1]".to_string();
    let missing = format!(
        "Task input attachment 1: {}",
        images.join("missing.png").display()
    );
    let mismatched = format!("{initial}\nTask input attachment 2: {}", corrupt.display());
    let second_unrendered = format!("{initial}\nTask input attachment 2: [Task image #2]");
    let both = &[&images, &revisions][..];
    let first_only = &[&images][..];
    let wrong_root = &[&revisions][..];
    let cases = [
        ("initial", initial.clone(), first_only, "--add-dir", true),
        ("revision", revision.clone(), both, "--add-dir", true),
        ("no images", "Design task".into(), both, "--add-dir", false),
        ("unrendered", unrendered, first_only, "--add-dir", false),
        (
            "second unrendered",
            second_unrendered,
            both,
            "--add-dir",
            false,
        ),
        ("missing file", missing, first_only, "--add-dir", false),
        ("wrong PNG", mismatched, both, "--add-dir", false),
        (
            "missing access",
            initial.clone(),
            &[][..],
            "--add-dir",
            false,
        ),
        (
            "wrong root",
            initial.clone(),
            wrong_root,
            "--add-dir",
            false,
        ),
        ("unrelated argument", initial, first_only, "--model", false),
        (
            "missing second root",
            revision,
            first_only,
            "--add-dir",
            false,
        ),
    ];
    for (name, prompt, roots, flag, succeeds) in cases {
        // Act
        let output =
            run_image_plan_provider(&stub_bin.join("claude"), &prompt, roots, flag).await?;

        // Assert
        assert_eq!(
            output.status.success(),
            succeeds,
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).contains("image received by Planner"),
            succeeds,
            "{name}"
        );
    }

    Ok(())
}

/// Exercises the exact provider executable installed for the PTY journey.
async fn run_image_plan_provider(
    provider: &Path,
    prompt: &str,
    roots: &[&PathBuf],
    flag: &str,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut command = Command::new(provider);
    for root in roots {
        command.arg(flag).arg(root);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("provider stdin is missing")?;
    stdin.write_all(prompt.as_bytes()).await?;
    drop(stdin);

    Ok(child.wait_with_output().await?)
}

#[tokio::test]
async fn plan_then_implement_runs_as_an_orchestrated_chat() -> E2eResult {
    // Arrange, Act, Assert: lower tests own routing and phase permutations.
    let gate = tempfile::tempdir()?;
    let started = gate.path().join("implementation-started");
    FeatureTest::new("plan_then_implement")
        .with_git()
        .with_terminal_size(100, 80)
        .args(["--experimental-multi-agent".to_string()])
        .env(
            "AGENTTY_TEST_IMPLEMENTER_STARTED",
            started.to_string_lossy(),
        )
        .setup(|env| Box::pin(async move { seed_workflow_project(env).await }))
        .run(
            move |scenario| {
                let scenario = scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("a")
                    .wait_for_text("Regular", 5000)
                    .press_key("Enter")
                    .wait_for_text("Type your message", 10000)
                    .press_key("/")
                    .write_text("plan")
                    .wait_for_text("Plan then implement", 3000)
                    .press_key("Enter")
                    .wait_for_text("Plan · describe the task for the Planner", 5000)
                    .capture_labeled("armed", "/plan arms the regular composer")
                    .write_text("/src/search.rs @plan_lookup")
                    .wait_for_text("plan_lookup_target.rs", 5000)
                    .press_key("Tab")
                    .wait_for_text("/src/search.rs @plan_lookup_target.rs", 5000)
                    .press_key("Esc")
                    .eventually(Duration::from_secs(5), Duration::from_millis(20), |frame| {
                        assertion::match_not_visible(
                            frame,
                            "Plan · describe the task for the Planner",
                        )
                    })
                    .press_key("ctrl+u")
                    .wait_for_text("Type your message", 5000)
                    .write_text("/plan /src/search.rs needs indexed search")
                    .press_key("Enter")
                    .wait_for_text("Which index should search use?", 20000)
                    .wait_for_text("Planner      ? Needs your input", 10000)
                    .wait_for_text("Add indexed search", 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("question", "The Planner asks in the chat");
                let scenario = send_chat_message(scenario, "Use the title index")
                    .wait_for_text("Plan version 1: index titles.", 20000)
                    .wait_for_text("Planner      ● Awaiting your approval", 10000)
                    .wait_for_stable_frame(300, 5000);
                let scenario = open_composer(scenario)
                    .wait_for_text("tell the orchestrator to start implementing", 5000)
                    .capture_labeled(
                        "awaiting_approval",
                        "The composer shows the orchestrator state",
                    )
                    .write_text("Include search tests")
                    .press_key("Enter")
                    .wait_for_text("Plan version 2: index titles and add search tests.", 20000)
                    .wait_for_text("Planner      ● Awaiting your approval", 10000)
                    .wait_for_stable_frame(300, 5000);
                let scenario = send_chat_message(scenario, "Looks good, start implementing")
                    .wait_for_text("Plan accepted · Implementer started", 20000);
                let scenario = wait_for_implementer(scenario, started)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("implementing", "Planner done, Implementer working");
                let scenario = send_chat_message(scenario, "Hurry up")
                    .wait_for_text("The Implementer is still working", 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("busy", "A reply while the Implementer works is kept")
                    .press_key("Esc")
                    .wait_for_text("Ctrl+c: stop", 10000)
                    .press_key("ctrl+c")
                    .wait_for_text("Implementer  ⊘ Canceled", 20000)
                    .wait_for_text("Enter: reply", 20000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("canceled", "Ctrl+c cancels the running Implementer");
                complete_reviewed_implementation(scenario)
            },
            |frame, report| {
                Box::pin(async move {
                    assert_orchestrated_chat(frame, report);
                })
            },
        )
        .await?;

    Ok(())
}

/// Finishes the retry, applies focused-review feedback, and sends a follow-up.
fn complete_reviewed_implementation(scenario: Scenario) -> Scenario {
    let scenario = send_chat_message(scenario, "Retry please")
        .wait_for_text(IMPLEMENTER_REVIEW_TEXT, 30000)
        .wait_for_text("Implementer  ✓ Done", 10000)
        .wait_for_text("(+1 -0)", 10000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled("implemented", "The Implementer starts focused review")
        .press_key("d")
        .wait_for_text("+indexed search", 10000)
        .press_key("q")
        .wait_for_text(IMPLEMENTER_REVIEW_TEXT, 10000);
    let scenario = open_composer(scenario)
        .write_text("/apply")
        .press_key("Enter")
        .wait_for_text("Review suggestion applied by the Implementer.", 30000)
        .wait_for_text(CLEAN_REVIEW_TEXT, 30000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled(
            "applied",
            "/apply routes review feedback to the Implementer",
        );
    let scenario = send_chat_message(scenario, "Also update the docs")
        .wait_for_text("Docs updated for indexed search.", 20000)
        .wait_for_text(CLEAN_REVIEW_TEXT, 30000)
        .wait_for_stable_frame(300, 5000);

    open_composer(scenario)
        .wait_for_text("Ask the Implementer for more changes", 5000)
        .wait_for_stable_frame(300, 5000)
        .capture_labeled("follow_up", "Follow-ups reach the Implementer")
}

/// Checks the visible conversation states captured by the chat journey.
fn assert_orchestrated_chat(frame: &TerminalFrame, report: &ProofReport) {
    let capture = |index: usize| common::frame_from_capture(&report.captures[index]);
    assert_visible(
        &capture(0),
        &[
            "Plan · describe the task for the Planner",
            "Describe the task for the Planner",
        ],
    );
    assert_visible(
        &capture(1),
        &[
            "› /src/search.rs needs indexed search",
            "⬡ Execution graph",
            "◆ Planner",
            "Which index should search use?",
            "Add indexed search",
            "Orchestrator · Planner · Needs your input",
        ],
    );
    assert_visible(
        &capture(2),
        &["Orchestrator · Planner · Awaiting your approval"],
    );
    assert_visible(
        &capture(3),
        &[
            "Planner      ✓ Done",
            "│ accepted plan ✓",
            "Implementer  ◐ Working",
            "Plan version 2: index titles and add search tests.",
        ],
    );
    assert_visible(
        &capture(4),
        &[
            "The Implementer is still working",
            "Hurry up",
            "Orchestrator · Implementer · Working",
        ],
    );
    assert_visible(&capture(5), &["Implementer  ⊘ Canceled"]);
    assert_visible(
        &capture(6),
        &[
            "◆ Implementer",
            "Implementation finished from accepted plan version 2.",
            "Implementer  ✓ Done",
            IMPLEMENTER_REVIEW_TEXT,
        ],
    );
    assert_visible(
        &capture(7),
        &[
            "Review suggestion applied by the Implementer.",
            CLEAN_REVIEW_TEXT,
        ],
    );
    assert_visible(
        frame,
        &[
            "› Also update the docs",
            "Docs updated for indexed search.",
            "Orchestrator · Implementer · Done",
        ],
    );
}
