//! Review request publication, status, and cleanup.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(unix)]
use ag_session::test_support as model_fixture;
use agentty::domain::session_message::SessionMessageKind;
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use super::fixture::{
    E2eResult, RESOLVED_DECISION_REVIEW_TEXT, run_git, run_git_stdout,
    seed_review_description_provider, seed_review_ready_session,
    seed_review_ready_session_with_review_request, seed_review_with_resolved_decision,
    seed_review_worktree_with_diff, seed_sessions_tab,
};
use crate::common;
use crate::common::{
    BuilderEnv, FeatureTest, ReleaseGate, SessionSeed, VHS_REPLAY_ENV_VAR, VHS_SETUP_SCRIPT_NAME,
};

/// Review-request notice body used by the timeline-order regression.
const REVIEW_REQUEST_TIMELINE_NOTICE_TEXT: &str =
    "Created PR https://github.com/agentty-xyz/agentty/pull/42";

/// Inactive composer border rendered only in an active Codex session view.
const SESSION_COMPOSER_MARKER: &str = "╭ codex/";

/// Timing row label that remains in an archived session view without a
/// composer.
const ARCHIVED_TIMING_MARKER: &str = "Timer: ";

/// Holds publish steps until the scenario releases them, keeping the create
/// command active while the UI refreshes its persisted upstream reference.
async fn seed_gated_review_request_publish(
    env: &BuilderEnv,
    hold_push: &str,
    hold_create: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env).await?;
    seed_review_worktree_with_diff(env)?;

    seed_successful_review_request_publish(env, hold_push, hold_create, "wt/review-s", false)?;

    // `FeatureTest` restores the post-setup database before VHS replay; the
    // replay must also start before the forge stub recorded a publication.
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"));
    std::fs::write(
        env.stub_bin.join(VHS_SETUP_SCRIPT_NAME),
        format!(
            "set -e\nrm -f {marker}\n",
            marker = quote(&env.stub_bin.join("gh.created")),
        ),
    )?;

    Ok(())
}

/// Uses a scenario gate for PTY proof and a bounded delay for VHS replay.
fn gate_or_recording_delay(gate: &ReleaseGate, seconds: u64) -> String {
    format!(
        "if [ \"${VHS_REPLAY_ENV_VAR}\" = 1 ]; then sleep {seconds}; else {}; fi",
        gate.shell_wait(),
    )
}

/// Seeds a review-ready session whose chosen review branch was deleted from
/// the remote before its first publish.
async fn seed_review_request_publish_with_deleted_remote_branch(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env).await?;
    seed_review_worktree_with_diff(env)?;

    seed_successful_review_request_publish(env, ":", ":", "review/deleted", true)
}

/// Seeds a live focused review that completes before a delayed review-request
/// publish, reproducing the cross-source transcript ordering boundary.
async fn seed_review_request_timeline(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_with_resolved_decision(env).await?;

    seed_successful_review_request_publish(env, ":", ":", "wt/review-s", false)
}

/// Configures the review-ready worktree and forge stubs for one successful
/// review-request publish with explicit command barriers.
fn seed_successful_review_request_publish(
    env: &BuilderEnv,
    hold_push: &str,
    hold_create: &str,
    review_branch_name: &str,
    remote_branch_was_deleted: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_description_provider(env)?;
    let session_worktree = env.agentty_root.join("wt").join("review-s");
    run_git(&session_worktree, &["branch", "-m", "wt/review-s"])?;
    run_git(&session_worktree, &["branch", "main", "HEAD"])?;
    if remote_branch_was_deleted {
        let stale_tracking_ref = format!("refs/remotes/origin/{review_branch_name}");
        run_git(
            &session_worktree,
            &["update-ref", stale_tracking_ref.as_str(), "HEAD"],
        )?;
    }

    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let git_path = env.stub_bin.join("git");
    // Branch-existence lookups see only the published `main` review target.
    let remote_lookup = r#"if [ "$1" = "ls-remote" ] && [ "$2" = "--heads" ]; then
  case "$4" in
    main) printf 'main\trefs/heads/main\n' ;;
  esac
  exit 0
fi
"#;
    let push_lease_guard = if remote_branch_was_deleted {
        format!(
            r#"  case "$*" in
    *"--force-with-lease=refs/heads/{review_branch_name}: --set-upstream"*) ;;
    *) printf '%s\n' 'missing empty lease for deleted remote branch' >&2; exit 1 ;;
  esac
"#
        )
    } else {
        String::new()
    };
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "ls-remote" ] && [ "$2" = "--refs" ]; then
  published_hash=$('{}' rev-parse HEAD)
  printf '%s\trefs/heads/main\n%s\trefs/heads/{review_branch_name}\n' "$published_hash" "$published_hash"
  exit 0
fi
if [ "$1" = "fetch" ] && [ "$2" = "--no-tags" ]; then
  exit 0
fi
{remote_lookup}if [ "$1" = "push" ]; then
{push_lease_guard}  {hold_push}
  case " $* " in
    *" --porcelain "*) printf '*\tHEAD:refs/heads/{review_branch_name}\t[new branch]\n' ;;
  esac
  '{}' update-ref 'refs/remotes/origin/{review_branch_name}' HEAD
  exit 0
fi
exec '{}' "$@"
"#,
        real_git.display(),
        real_git.display(),
        real_git.display()
    );
    std::fs::write(&git_path, script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    let gh_path = env.stub_bin.join("gh");
    let gh_script = r###"#!/bin/sh
marker_path="${0}.created"
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"api"*"/pulls"*)
    if [ -f "$marker_path" ]; then
      printf '%s\n' '[{"number":42}]'
    else
      printf '%s\n' '[]'
    fi
    ;;
  *"pr create"*)
    case "$*" in
      *"## Why?"*"## What?"*"## Impact?"*) ;;
      *) echo 'missing generated review description' >&2; exit 1 ;;
    esac
    __HOLD_CREATE__
    touch "$marker_path"
    ;;
  *"mergeStateStatus"*)
    printf '%s\n' '{"data":{"repository":{"pullRequest":{"number":42,"title":"Review-ready session shortcuts","state":"OPEN","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"__REVIEW_BRANCH_NAME__","isDraft":false,"mergeStateStatus":"BLOCKED","reviewDecision":"REVIEW_REQUIRED","mergedAt":null}}}}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"###
    .replace("__HOLD_CREATE__", hold_create)
    .replace("__REVIEW_BRANCH_NAME__", review_branch_name);
    std::fs::write(&gh_path, gh_script)?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Seeds one published review-ready session whose latest auto-push completion
/// is persisted as transcript output.
async fn seed_session_with_published_branch_push_notice(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = "published-push-0001";

    common::seed_session(
        env,
        SessionSeed::regular(session_id, model_fixture::CODEX_MODEL_ID, "main", "Review")
            .with_title("Published push notice"),
    )
    .await?;

    (async {
        let database = common::open_database(env).await?;
        database
            .sessions()
            .update_session_published_upstream_ref(
                session_id,
                Some("origin/wt/published-push".to_string()),
            )
            .await?;
        database
            .sessions()
            .append_session_message(
                session_id,
                SessionMessageKind::WorkflowNotice,
                "\n[Branch Push] Auto-pushed published branch after completed turn.\n",
            )
            .await
    })
    .await?;

    // Match `session_folder()` so startup loads the seeded review session.
    let worktree_name = &session_id[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    Ok(())
}

/// Seeds a merged GitHub review response and delays runtime worktree removal
/// so the feature scenario can prove terminal rendering stays responsive.
async fn seed_slow_merged_review_request_status(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env).await?;

    let sync_origin = env.agentty_root.join("sync-origin.git");
    std::fs::create_dir_all(&sync_origin)?;
    run_git(&sync_origin, &["init", "--bare", "."])?;
    let sync_origin_path = sync_origin.to_string_lossy().into_owned();
    run_git(
        &env.workdir,
        &["remote", "add", "origin", sync_origin_path.as_str()],
    )?;
    run_git(&env.workdir, &["push", "--set-upstream", "origin", "main"])?;

    let gh_path = env.stub_bin.join("gh");
    std::fs::write(
        &gh_path,
        r#"#!/bin/sh
case "$*" in
  *"auth status"*)
    exit 0
    ;;
  *"mergeStateStatus"*)
    printf '%s\n' '{"data":{"repository":{"pullRequest":{"number":42,"title":"Review-ready session shortcuts","state":"MERGED","url":"https://github.com/agentty-xyz/agentty/pull/42","baseRefName":"main","headRefName":"wt/review-s","isDraft":false,"mergeStateStatus":"CLEAN","reviewDecision":"APPROVED","mergedAt":"2026-01-01T00:00:00Z"}}}}'
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 1
    ;;
esac
"#,
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o750))?;

    install_delayed_worktree_remove_stub(env, 4)
}

/// Seeds one linked PR whose merge state reports readiness.
async fn seed_ready_pr_status(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env).await?;
    let gh_path = env.stub_bin.join("gh");
    let gh_script = std::fs::read_to_string(&gh_path)?.replace(
        "\"mergeStateStatus\":\"BLOCKED\"",
        "\"mergeStateStatus\":\"CLEAN\"",
    );
    std::fs::write(&gh_path, gh_script)?;

    Ok(())
}

/// Seeds one linked PR whose passing checks, including every base-branch
/// required check, leave only a required approval the viewer may bypass.
async fn seed_bypass_ready_pr_status(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env).await?;
    let gh_path = env.stub_bin.join("gh");
    let gh_script = std::fs::read_to_string(&gh_path)?.replace(
        "\"mergedAt\":null",
        "\"mergedAt\":null,\"statusCheckRollup\":{\"state\":\"SUCCESS\",\"contexts\":{\"nodes\":\
         [{\"name\":\"ci\"}]}},\"baseRef\":{\"refUpdateRule\":null,\"rules\":{\"totalCount\":1,\"\
         nodes\":[{\"parameters\":{\"requiredStatusChecks\":[{\"context\":\"ci\"}]}}]}},\"\
         viewerCanMergeAsAdmin\":true",
    );
    std::fs::write(&gh_path, gh_script)?;

    Ok(())
}

/// Installs a git wrapper that delays worktree removal while forwarding all
/// other commands to the real executable.
fn install_delayed_worktree_remove_stub(
    env: &BuilderEnv,
    delay_seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "git not found"))?;
    let real_git = real_git.to_string_lossy().replace('\'', "'\"'\"'");
    let git_path = env.stub_bin.join("git");
    std::fs::write(
        &git_path,
        format!(
            r#"#!/bin/sh
if [ "$1" = "worktree" ] && [ "$2" = "remove" ]; then
  exec sleep {delay_seconds}
fi
exec '{real_git}' "$@"
"#
        ),
    )?;
    #[cfg(unix)]
    std::fs::set_permissions(&git_path, std::fs::Permissions::from_mode(0o750))?;

    Ok(())
}

/// Verify that completed published-branch auto-push feedback is rendered as a
/// transcript message rather than as a transient status line.
#[tokio::test]
async fn published_branch_push_notice_renders_as_transcript_message() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("published_branch_push_notice")
        .with_git()
        .setup(|env| {
            Box::pin(async move { seed_session_with_published_branch_push_notice(env).await })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("[Branch Push]", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "published_branch_push_notice",
                        "Published branch auto-push completion rendered as a transcript message",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    let view_text = frame.text_in_region(&full);

                    assertion::assert_text_in_region(frame, "[Branch Push]", &full);
                    assert_eq!(
                        view_text
                            .matches("Auto-pushed published branch after completed turn.")
                            .count(),
                        1
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify the linked PR status refresh paints a ready label in the session
/// list.
#[tokio::test]
async fn test_pr_ready_label() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("pr_ready_label")
        .with_git()
        .setup(|env| Box::pin(async move { seed_ready_pr_status(env).await }))
        .zola(
            "Pull request ready label",
            "See when repository rules allow a linked pull request to merge.",
            54,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("[ready]", 10_000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("pr_ready_label", "Linked pull request ready in Sessions")
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "[ready]", &full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify a linked PR blocked only by a bypassable required approval paints
/// the bypass ready label in the session list.
#[tokio::test]
async fn test_pr_bypass_ready_label() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("pr_bypass_ready_label")
        .with_git()
        .setup(|env| Box::pin(async move { seed_bypass_ready_pr_status(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("[ready (bypass)]", 10_000)
                    .capture_labeled(
                        "pr_bypass_ready_label",
                        "Linked pull request ready with bypass in Sessions",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "[ready (bypass)]", &full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that pressing `p` in a review-ready session opens the review-request
/// publish popup.
#[tokio::test]
async fn review_request_publish_shortcut_opens_publish_popup() -> E2eResult {
    // Arrange
    FeatureTest::new("review_request_publish_shortcut")
        .with_git()
        .setup(|env| {
            Box::pin(async move {
                seed_review_ready_session(env).await?;
                seed_sessions_tab(env).await
            })
        })
        .zola(
            "Review request publish shortcut",
            "Open the review-request publish popup directly from session view with `p`.",
            42,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_publish_popup",
                        "Review-request publish popup after pressing p",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    // Assert
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Publish Review Request", &full);
                    assertion::assert_text_in_region(frame, "Enter: publish review request", &full);
                    assertion::assert_text_in_region(
                        frame,
                        "Leave blank to push as `wt/review-s`",
                        &full,
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that a first review-request publish can recreate a custom branch
/// that was deleted remotely despite a stale local remote-tracking ref.
#[tokio::test]
async fn review_request_publish_recreates_deleted_remote_branch() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_deleted_remote_branch")
        .with_git()
        .setup(|env| {
            Box::pin(
                async move { seed_review_request_publish_with_deleted_remote_branch(env).await },
            )
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .write_text("review/deleted")
                    .press_key("Enter")
                    .compose(&common::decline_review_watch_before_publish())
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "deleted_remote_branch_recreated",
                        "Review request published after its former remote branch was deleted",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                    assertion::assert_not_visible(frame, "already exists");
                    assertion::assert_not_visible(frame, "Auto-resolve checks");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that confirming review-request publish returns to an interactive
/// session chat while the push runs in the background.
#[tokio::test]
async fn review_request_publish_runs_in_background() -> E2eResult {
    // Arrange
    let push = ReleaseGate::new()?;
    let create = ReleaseGate::new()?;
    // VHS reuses these fixtures after the PTY proof has released both gates.
    // Keep its recording cadence independent of the proof's callbacks.
    let hold_push = gate_or_recording_delay(&push, 3);
    let hold_create = gate_or_recording_delay(&create, 15);
    FeatureTest::new("review_request_background_publish")
        .with_git()
        .zola(
            "Background review-request publish",
            "Publish a review request in the background and receive its link in session chat.",
            41,
        )
        .setup(move |env| {
            Box::pin(async move {
                seed_gated_review_request_publish(env, &hold_push, &hold_create).await
            })
        })
        .run(
            move |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .compose(&common::decline_review_watch_before_publish())
                    .wait_for_text("Publishing review request...", 3000)
                    .capture_labeled(
                        "background_publish_started",
                        "Review-request publish progress shown inline in session chat",
                    )
                    .press_key("?")
                    .wait_for_text("Keybindings", 3000)
                    .capture_labeled(
                        "background_publish_help",
                        "Session help remains available while publishing",
                    )
                    .press_key("q")
                    .compose(&push.release())
                    .wait_for_text("origin/wt/review-s", 10000)
                    .viewing_pause_ms(5500)
                    .capture_labeled(
                        "background_publish_waiting_for_url",
                        "Review-request publish progress remains until its URL is ready",
                    )
                    .compose(&create.release())
                    .wait_for_text("[Review Request] Created PR", 20000)
                    .capture_labeled(
                        "background_publish_finished",
                        "Review-request link recorded in session transcript history",
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let loading_frame = common::frame_from_capture(&report.captures[0]);
                    let loading_full = Region::full(loading_frame.cols(), loading_frame.rows());
                    assertion::assert_text_in_region(
                        &loading_frame,
                        "Publishing review request...",
                        &loading_full,
                    );
                    assertion::assert_text_in_region(
                        &loading_frame,
                        SESSION_COMPOSER_MARKER,
                        &loading_full,
                    );

                    let help_frame = common::frame_from_capture(&report.captures[1]);
                    let help_full = Region::full(help_frame.cols(), help_frame.rows());
                    assertion::assert_text_in_region(&help_frame, "Keybindings", &help_full);

                    let waiting_frame = common::frame_from_capture(&report.captures[2]);
                    let waiting_full = Region::full(waiting_frame.cols(), waiting_frame.rows());
                    assertion::assert_text_in_region(
                        &waiting_frame,
                        "Publishing review request...",
                        &waiting_full,
                    );
                    assertion::assert_text_in_region(
                        &waiting_frame,
                        "origin/wt/review-s",
                        &waiting_full,
                    );

                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                    frame,
                    "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                    &full,
                );
                    assertion::assert_text_in_region(frame, SESSION_COMPOSER_MARKER, &full);
                    assertion::assert_not_visible(frame, "Auto-resolve checks");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify a review-request notice created after focused review completion is
/// rendered below that review instead of being regrouped above it.
#[tokio::test]
async fn review_request_notice_follows_completed_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_timeline_order")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(|env| Box::pin(async move { seed_review_request_timeline(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("f")
                    .wait_for_text(RESOLVED_DECISION_REVIEW_TEXT, 30000)
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 3000)
                    .press_key("Enter")
                    .compose(&common::decline_review_watch_before_publish())
                    .wait_for_text(REVIEW_REQUEST_TIMELINE_NOTICE_TEXT, 10000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "review_request_timeline_order",
                        "Review-request result follows the earlier focused review",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let review_finding = frame
                        .find_text(RESOLVED_DECISION_REVIEW_TEXT)
                        .into_iter()
                        .next()
                        .expect("completed focused review should render");
                    let review_request_notice = frame
                        .find_text(REVIEW_REQUEST_TIMELINE_NOTICE_TEXT)
                        .into_iter()
                        .next()
                        .expect("review-request notice should render");

                    assert!(
                        review_finding.rect.row < review_request_notice.rect.row,
                        "review-request notice should follow the earlier focused review"
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that linked review requests refresh in the background without a
/// manual review-request sync shortcut and disable local merge queueing.
#[tokio::test]
async fn review_request_sync_runs_in_background() -> E2eResult {
    // Arrange
    FeatureTest::new("review_request_background_sync")
        .with_git()
        .setup(|env| {
            Box::pin(async move {
                seed_review_ready_session_with_review_request(env).await?;
                seed_sessions_tab(env).await
            })
        })
        .zola(
            "Background review-request sync",
            "Review sessions track linked pull requests in the background instead of exposing a \
             manual sync shortcut.",
            43,
        )
        .run(
            |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_text("p: Create or refresh", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_background_sync",
                        "Review session help overlay without a manual sync shortcut",
                    )
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 3000)
                    .press_key("m")
                    .wait_for_stable_frame(300, 3000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let help_frame = common::frame_from_capture(&report.captures[0]);
                    let help_full = Region::full(help_frame.cols(), help_frame.rows());
                    let help_text = help_frame.text_in_region(&help_full);

                    assertion::assert_text_in_region(
                        &help_frame,
                        "p: Create or refresh",
                        &help_full,
                    );
                    assert!(
                        !help_text.contains("s: Sync"),
                        "manual sync help action should be absent"
                    );
                    assertion::assert_not_visible(&help_frame, "Add to merge queue");

                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, SESSION_COMPOSER_MARKER, &full);
                    assertion::assert_not_visible(frame, "Keybindings");
                    assertion::assert_not_visible(frame, "Confirm Merge");
                })
            },
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn review_request_closure_leaves_focused_reply_composer() -> E2eResult {
    // Arrange
    let gate_dir = tempfile::tempdir()?;
    let release_path = gate_dir.path().join("close-review");
    let setup_release_path = release_path.clone();
    FeatureTest::new("review_request_closure_focused_reply")
        .with_git()
        .setup(move |env| {
            Box::pin(async move {
                seed_review_ready_session_with_review_request(env).await?;
                let gh_path = env.stub_bin.join("gh");
                let release = setup_release_path
                    .to_string_lossy()
                    .replace('\'', "'\"'\"'");
                let script = std::fs::read_to_string(&gh_path)?
                    .replace("\"state\":\"OPEN\"", "\"state\":\"CLOSED\"")
                    .replace(
                        "*\"mergeStateStatus\"*)",
                        &format!(
                            "*\"mergeStateStatus\"*)\n    until [ -f '{release}' ]; do sleep \
                             0.05; done"
                        ),
                    );
                std::fs::write(gh_path, script)?;

                Ok(())
            })
        })
        .run(
            move |scenario| {
                // Act
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("Enter")
                    .wait_for_text("Type your message", 5000)
                    .write_text("unfinished reply")
                    .eventually(
                        Duration::from_secs(5),
                        Duration::from_millis(20),
                        move |frame| {
                            assertion::match_text_in_region(
                                frame,
                                "unfinished reply",
                                &Region::full(frame.cols(), frame.rows()),
                            )?;
                            std::fs::write(&release_path, "close")
                                .expect("release closed review status");
                            Ok(())
                        },
                    )
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(20),
                        |frame| {
                            assertion::match_text_in_region(
                                frame,
                                ARCHIVED_TIMING_MARKER,
                                &Region::full(frame.cols(), frame.rows()),
                            )?;
                            assertion::match_not_visible(frame, "unfinished reply")?;
                            assertion::match_not_visible(frame, SESSION_COMPOSER_MARKER)
                        },
                    )
                    .capture_labeled("archived", "Background closure exits the focused composer")
                    .press_key("c")
                    .wait_for_text("Confirm Continue", 5000)
                    .press_key("Esc")
                    .step(common::wait_for_frame_text(
                        &[ARCHIVED_TIMING_MARKER],
                        &["Confirm Continue"],
                    ))
                    .press_key("q")
                    .wait_for_text("ARCHIVE", 5000)
            },
            |frame, report| {
                Box::pin(async move {
                    // Assert
                    let archived = common::frame_from_capture(&report.captures[0]);
                    assertion::assert_not_visible(&archived, "unfinished reply");
                    assertion::assert_not_visible(&archived, "╭");
                    assertion::assert_text_in_region(
                        frame,
                        "Canceled",
                        &Region::full(frame.cols(), frame.rows()),
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that a remote merge stays read-only `Merged` until the user syncs
/// main, then moves to `Done` without waiting for slow worktree cleanup.
#[tokio::test]
async fn test_merged_review_request_waits_for_manual_sync() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("merged_review_request_manual_sync")
        .with_git()
        .setup(|env| Box::pin(async move { seed_slow_merged_review_request_status(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .wait_for_text("Merged", 10_000)
                    .capture_labeled(
                        "merged_status",
                        "Merged session waits in Active for manual main sync",
                    )
                    .compose(&common::open_selected_session_view())
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "merged_read_only_actions",
                        "Merged session exposes only read-only review actions",
                    )
                    .press_key("?")
                    .press_key("d")
                    .wait_for_text("main.rs", 5000)
                    .press_key("j")
                    .press_key("Enter")
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "merged_diff_read_only",
                        "Merged diff hides inline comment actions",
                    )
                    .press_key("?")
                    .wait_for_text("Keybindings", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled("merged_diff_help", "Merged diff help omits comment actions")
                    .press_key("?")
                    .press_key("q")
                    .press_key("q")
                    .press_key("s")
                    .wait_for_text("Synced test-project/main", 10_000)
                    .wait_for_text("Done", 10_000)
                    .capture_labeled(
                        "merged_done_after_sync",
                        "Manual main sync archives the merged session",
                    )
            },
            |frame, report| {
                Box::pin(async move {
                    assert_eq!(report.captures.len(), 5);
                    let merged_frame = common::frame_from_capture(&report.captures[0]);
                    let merged_full = Region::full(merged_frame.cols(), merged_frame.rows());
                    assertion::assert_text_in_region(&merged_frame, "ACTIVE —— 1", &merged_full);
                    assertion::assert_text_in_region(&merged_frame, "Merged", &merged_full);

                    let read_only_frame = common::frame_from_capture(&report.captures[1]);
                    let read_only_full =
                        Region::full(read_only_frame.cols(), read_only_frame.rows());
                    assertion::assert_text_in_region(
                        &read_only_frame,
                        "Show diff",
                        &read_only_full,
                    );
                    let read_only_text = read_only_frame.text_in_region(&read_only_full);
                    for mutating_action in
                        ["Reply", "Open commands menu", "Add to merge queue", "Sync"]
                    {
                        assert!(
                            !read_only_text.contains(mutating_action),
                            "Merged help must hide `{mutating_action}`"
                        );
                    }

                    let diff_frame = common::frame_from_capture(&report.captures[2]);
                    let diff_full = Region::full(diff_frame.cols(), diff_frame.rows());
                    assertion::assert_text_in_region(&diff_frame, ") Diff — ", &diff_full);
                    assertion::assert_not_visible(&diff_frame, "New line");

                    let diff_help_frame = common::frame_from_capture(&report.captures[3]);
                    let diff_help_full =
                        Region::full(diff_help_frame.cols(), diff_help_frame.rows());
                    assertion::assert_text_in_region(
                        &diff_help_frame,
                        "Enter/l: Open",
                        &diff_help_full,
                    );
                    for comment_key in ["Shift+C", "Shift+V"] {
                        assertion::assert_not_visible(&diff_help_frame, comment_key);
                    }

                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(frame, "Done", &full);
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that confirming quit does not wait indefinitely for externally
/// merged worktree cleanup.
#[tokio::test]
async fn merged_review_request_cleanup_does_not_block_quit() -> E2eResult {
    // Arrange
    let _test_guard = common::acquire_e2e_test_lock().await;
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    env.init_git()?;
    seed_slow_merged_review_request_status(&env).await?;
    install_delayed_worktree_remove_stub(&env, 30)?;
    let mut session = env.builder().spawn()?;
    let scenario = Scenario::new("merged_cleanup_quit")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .wait_for_text("Merged", 10_000)
        .press_key("s")
        .wait_for_text("Synced test-project/main", 10_000)
        .wait_for_text("Done", 10_000)
        .compose(&common::open_quit_dialog())
        .press_key("y");

    // Act
    scenario.execute_in_pty(&mut session)?;
    let exited_successfully = session.wait_for_exit(Duration::from_secs(8))?;

    // Assert
    assert!(
        exited_successfully,
        "confirmed quit should cancel cleanup after the shutdown deadline"
    );

    Ok(())
}

/// Verify that the title, change totals, and review URL share the header row.
#[tokio::test]
async fn review_request_url_appears_in_session_header() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_url_header")
        .with_git()
        .with_terminal_size(120, 24)
        .setup(|env| {
            Box::pin(async move { seed_review_ready_session_with_review_request(env).await })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("https://github.com/agentty-xyz/agentty/pull/42", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_url_header",
                        "Linked review-request URL visible in the session header",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let title = frame
                        .find_text("Review-ready session shortcuts (+")
                        .into_iter()
                        .next()
                        .expect("title should be immediately followed by change totals");
                    let url = frame
                        .find_text("https://github.com/agentty-xyz/agentty/pull/42")
                        .into_iter()
                        .next()
                        .expect("review URL should be visible");
                    assert_eq!(title.rect.row, url.rect.row);
                    assert!(title.rect.col < url.rect.col);
                })
            },
        )
        .await?;

    Ok(())
}

/// Seeds a review-request publish whose pull request then reports one
/// completed failing check and no review threads.
///
/// The check lookup waits on `hold_checks`, so the scenario observes the
/// watch confirmation before the automatic follow-up appends its prompt and
/// turn output, which would otherwise scroll that notice out of view.
async fn seed_watched_review_request_publish(
    env: &BuilderEnv,
    hold_checks: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env).await?;
    seed_review_worktree_with_diff(env)?;
    seed_successful_review_request_publish(env, ":", ":", "wt/review-s", false)?;

    let gh_path = env.stub_bin.join("gh");
    let check_lookup = format!(
        "  *\"headRefOid\"*)\n    {hold_checks}\n{}",
        r#"    printf '%s\n' '[{"data":{"repository":{"pullRequest":{"headRefOid":"abc1234","mergeable":"MERGEABLE","statusCheckRollup":{"contexts":{"nodes":[{"__typename":"CheckRun","name":"unit-tests","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/agentty-xyz/agentty/actions/runs/7"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}}]'
    ;;
  *"isOutdated"*)
    printf '%s\n' '[{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]'
    ;;
  *"mergeStateStatus"*)"#
    );
    let gh_script =
        std::fs::read_to_string(&gh_path)?.replace("  *\"mergeStateStatus\"*)", &check_lookup);
    std::fs::write(&gh_path, gh_script)?;

    Ok(())
}

/// Seeds a review-request publish for a session whose branch already has a
/// published upstream, so the publish popup locks the branch field.
async fn seed_pushed_branch_review_request_publish(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env).await?;
    seed_review_worktree_with_diff(env)?;
    seed_successful_review_request_publish(env, ":", ":", "wt/review-s", false)?;

    let database = common::open_database(env).await?;
    database
        .sessions()
        .update_session_published_upstream_ref(
            "review-shortcut-0001",
            Some("origin/wt/review-s".to_string()),
        )
        .await?;

    Ok(())
}

/// Verify that publishing a review request for an already pushed branch
/// creates it right away without asking whether to watch it.
#[tokio::test]
async fn review_request_publish_skips_watch_question_for_pushed_branch() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_pushed_branch_without_watch_question")
        .with_git()
        .setup(|env| Box::pin(async move { seed_pushed_branch_review_request_publish(env).await }))
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("[Review Request] Created PR", 15000)
                    .capture_labeled(
                        "pushed_branch_review_request_created",
                        "Review request created for a pushed branch without a watch question",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                        frame,
                        "[Review Request] Created PR https://github.com/agentty-xyz/agentty/pull/42",
                        &full,
                    );
                    assertion::assert_not_visible(frame, "Watch Review Request");
                    assertion::assert_not_visible(frame, "Auto-resolve checks");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that confirming the branch name asks whether to watch the review
/// request before pushing with `Yes` preselected, that accepting with `Enter`
/// moves the published session to `Watching`, and that a failing check then
/// starts an automatic follow-up.
#[tokio::test]
async fn review_request_publish_asks_to_watch_and_follows_up_on_failed_checks() -> E2eResult {
    // Arrange
    let checks = ReleaseGate::new()?;
    let hold_checks = checks.shell_wait();

    // Act, Assert
    FeatureTest::new("review_request_watch")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(move |env| {
            Box::pin(async move { seed_watched_review_request_publish(env, &hold_checks).await })
        })
        .run(
            move |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .press_key("p")
                    .wait_for_text("Publish Review Request", 5000)
                    .press_key("Enter")
                    .wait_for_text("Watch Review Request", 5000)
                    .wait_for_stable_frame(300, 5000)
                    .capture_labeled(
                        "review_request_watch_offer",
                        "Watch question shown before the branch is pushed",
                    )
                    .press_key("Enter")
                    .wait_for_text("[Review Watch] Watching pull request #42", 20000)
                    .capture_labeled(
                        "review_request_watch_enabled",
                        "Session notice confirming the review request is watched",
                    )
                    .compose(&checks.release())
                    .wait_for_text("[Review Watch] Fixing 1 failing check: unit-tests", 20000)
                    .capture_labeled(
                        "review_request_watch_follow_up",
                        "Automatic follow-up started for the failing check",
                    )
            },
            |_frame, report| {
                Box::pin(async move {
                    let offer_frame = common::frame_from_capture(&report.captures[0]);
                    let offer_full = Region::full(offer_frame.cols(), offer_frame.rows());
                    assertion::assert_text_in_region(
                        &offer_frame,
                        "Watch Review Request",
                        &offer_full,
                    );
                    assertion::assert_not_visible(&offer_frame, "Publishing review request...");
                    assertion::assert_not_visible(&offer_frame, "[Review Request] Created PR");
                    assertion::assert_text_in_region(
                        &offer_frame,
                        "Auto-resolve checks, comments, conflicts?",
                        &offer_full,
                    );

                    let enabled_frame = common::frame_from_capture(&report.captures[1]);
                    let enabled_full = Region::full(enabled_frame.cols(), enabled_frame.rows());
                    assertion::assert_text_in_region(
                        &enabled_frame,
                        "[Review Watch] Watching pull request #42",
                        &enabled_full,
                    );
                    assertion::assert_text_in_region(
                        &enabled_frame,
                        "[Review Request] Created PR",
                        &enabled_full,
                    );
                    assertion::assert_not_visible(&enabled_frame, "Watch Review Request");
                    assertion::assert_not_visible(&enabled_frame, "Fixing 1 failing check");

                    // The follow-up turn keeps appending output, so assert
                    // its start on the capture taken when it appeared.
                    let follow_up_frame = common::frame_from_capture(&report.captures[2]);
                    let follow_up_full =
                        Region::full(follow_up_frame.cols(), follow_up_frame.rows());
                    assertion::assert_text_in_region(
                        &follow_up_frame,
                        "[Review Watch] Fixing 1 failing check: unit-tests",
                        &follow_up_full,
                    );
                })
            },
        )
        .await?;

    Ok(())
}

/// Seeds a watched linked pull request whose GitHub Actions check keeps
/// failing at the session worktree's head commit, with the follow-ups whose
/// handled-key kinds are named by `handled_key_kinds` already attempted at
/// that commit.
async fn seed_watched_review_request_with_unfixed_check(
    env: &BuilderEnv,
    handled_key_kinds: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session_with_review_request(env).await?;

    let session_head = run_git_stdout(
        &env.agentty_root.join("wt").join("review-s"),
        &["rev-parse", "HEAD"],
    )?;
    let database = common::open_database(env).await?;
    database
        .reviews()
        .enable_session_review_watch("review-shortcut-0001")
        .await?;
    database
        .reviews()
        .update_session_review_watch_handled_keys(
            "review-shortcut-0001",
            handled_key_kinds
                .iter()
                .map(|kind| format!("{kind}:{session_head}"))
                .collect(),
        )
        .await?;

    let gh_path = env.stub_bin.join("gh");
    let check_lookup = r#"  *"repos/agentty-xyz/agentty/actions/runs/7/rerun-failed-jobs"*)
    touch "${0}.rerun-requested"
    printf '%s\n' '{}'
    ;;
  *"headRefOid"*)
    printf '%s\n' '[{"data":{"repository":{"pullRequest":{"headRefOid":"abc1234","mergeable":"MERGEABLE","statusCheckRollup":{"contexts":{"nodes":[{"__typename":"CheckRun","name":"unit-tests","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/agentty-xyz/agentty/actions/runs/7","checkSuite":{"app":{"slug":"github-actions"}}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}}]'
    ;;
  *"isOutdated"*)
    printf '%s\n' '[{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}]'
    ;;
  *"mergeStateStatus"*)"#
        .replace("abc1234", &session_head);
    let gh_script =
        std::fs::read_to_string(&gh_path)?.replace("  *\"mergeStateStatus\"*)", &check_lookup);
    std::fs::write(&gh_path, gh_script)?;

    Ok(())
}

/// Verify that a watched check still failing at the head commit its fix turn
/// left unchanged is rerun through the forge instead of starting another fix
/// turn.
#[tokio::test]
async fn review_watch_reruns_a_check_its_fix_turn_left_failing() -> E2eResult {
    // Arrange
    let rerun_requested_marker = Arc::new(Mutex::new(None::<PathBuf>));
    let setup_rerun_requested_marker = Arc::clone(&rerun_requested_marker);

    FeatureTest::new("review_watch_unfixed_check_rerun")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(move |env| {
            Box::pin(async move {
                setup_rerun_requested_marker
                    .lock()
                    .expect("rerun marker capture should remain available")
                    .replace(env.stub_bin.join("gh.rerun-requested"));

                seed_watched_review_request_with_unfixed_check(env, &["checks"]).await
            })
        })
        .run(
            move |scenario| {
                // Act
                let rerun_requested_marker = rerun_requested_marker
                    .lock()
                    .expect("rerun marker capture should remain available")
                    .clone()
                    .expect("setup should capture the rerun marker path");

                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text(
                        "[Review Watch] Rerunning 1 failing check: unit-tests",
                        20000,
                    )
                    .eventually(
                        Duration::from_secs(10),
                        Duration::from_millis(100),
                        move |frame| {
                            if rerun_requested_marker.is_file() {
                                return Ok(());
                            }

                            let full = Region::full(frame.cols(), frame.rows());
                            assertion::match_text_in_region(
                                frame,
                                "forge rerun requested through the gh stub",
                                &full,
                            )
                        },
                    )
                    .wait_for_stable_frame(500, 5000)
                    .capture_labeled(
                        "review_watch_unfixed_check_rerun",
                        "Check left failing by its fix turn rerun through the forge",
                    )
            },
            |frame, _report| {
                // Assert
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                        frame,
                        "[Review Watch] Rerunning 1 failing check: unit-tests",
                        &full,
                    );
                    assertion::assert_not_visible(frame, "Could not rerun");
                    assertion::assert_not_visible(frame, "Fixing 1 failing check");
                    assertion::assert_not_visible(frame, "Leaving 1 failing check");
                })
            },
        )
        .await?;

    Ok(())
}

/// Verify that a watched check still failing after its fix turn and its
/// rerun is left to the user with one notice instead of another attempt.
#[tokio::test]
async fn review_watch_leaves_a_check_failing_after_its_rerun_to_the_user() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_watch_unfixed_check_left")
        .with_git()
        .with_terminal_size(120, 40)
        .setup(|env| {
            Box::pin(async move {
                seed_watched_review_request_with_unfixed_check(
                    env,
                    &["checks", "rerun", "rerun-started"],
                )
                .await
            })
        })
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::open_selected_session_view())
                    .wait_for_text("[Review Watch] Leaving 1 failing check to you", 20000)
                    .capture_labeled(
                        "review_watch_unfixed_check_left",
                        "Check still failing after its rerun left to the user",
                    )
            },
            |frame, _report| {
                Box::pin(async move {
                    let full = Region::full(frame.cols(), frame.rows());
                    assertion::assert_text_in_region(
                        frame,
                        "[Review Watch] Leaving 1 failing check to you after automatic attempts: \
                         unit-tests",
                        &full,
                    );
                    assertion::assert_not_visible(frame, "Rerunning");
                    assertion::assert_not_visible(frame, "Fixing 1 failing check");
                })
            },
        )
        .await?;

    Ok(())
}
