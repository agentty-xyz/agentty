//! Session lifecycle and prompt E2E tests.
//!
//! Tests cover session creation via `a` key, opening sessions with `Enter`,
//! list navigation with `j`/`k`, deletion with confirmation, prompt input
//! basics (typing, multiline via Alt+Enter, cancel via Esc), and returning
//! to the session list from session view.

use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
};
use agentty::test_support;
use testty::assertion;
use testty::region::Region;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

type E2eResult = Result<(), Box<dyn std::error::Error>>;
const LOADER_SESSION_ID: &str = "loader-session-0001";

/// Stable id for the seeded running session used by stop-turn tests.
const RUNNING_STOP_SESSION_ID: &str = "running-stop-0001";

/// Seeds one review-ready session and propagates setup errors to the caller.
fn seed_review_ready_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .upsert_project(&canonical_workdir.to_string_lossy(), Some("main"))
            .await?;

        database.touch_project_last_opened(project_id).await?;
        database
            .insert_session(
                "review-shortcut-0001",
                "gpt-5.4",
                "main",
                "Review",
                project_id,
            )
            .await?;
        database
            .update_session_title("review-shortcut-0001", "Review-ready session shortcuts")
            .await?;
        database
            .update_session_diff_stats(12, 3, "review-shortcut-0001", "M")
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("review-s"))?;

    Ok(())
}

/// Seeds one running session so `Ctrl+c` can exercise the turn-stop path
/// without needing a live agent backend.
fn seed_in_progress_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .upsert_project(&canonical_workdir.to_string_lossy(), Some("main"))
            .await?;

        database.touch_project_last_opened(project_id).await?;
        database
            .insert_session(
                RUNNING_STOP_SESSION_ID,
                "gpt-5.4",
                "main",
                "InProgress",
                project_id,
            )
            .await?;
        database
            .update_session_title(RUNNING_STOP_SESSION_ID, "Running session stop")
            .await
    })?;

    // Match `session_folder()` so the seeded row has the worktree path the
    // runtime expects for this session id.
    let worktree_name = &RUNNING_STOP_SESSION_ID[..8];
    std::fs::create_dir_all(env.agentty_root.join("wt").join(worktree_name))?;

    Ok(())
}

/// Seeds one review-ready session with a linked review request.
fn seed_review_ready_session_with_review_request(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    seed_review_ready_session(env)?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let review_request = ReviewRequest {
            last_refreshed_at: 55,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/review-s".to_string(),
                state: ReviewRequestState::Open,
                status_summary: Some("Checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Review-ready session shortcuts".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            },
        };

        database
            .update_session_review_request("review-shortcut-0001", Some(&review_request))
            .await
    })?;

    Ok(())
}

/// Seeds one draft-session lookup target file into the temporary project.
fn seed_draft_at_lookup_project(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(
        env.workdir.join("draft_lookup_target.txt"),
        "draft lookup target\n",
    )?;

    Ok(())
}

/// Seeds one done session whose merged commit hash can drive the continuation
/// draft flow.
fn seed_done_session_for_continuation(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let merged_commit_hash = "704de31d0f4b5a1234567890abcdef1234567890";
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .upsert_project(&canonical_workdir.to_string_lossy(), Some("main"))
            .await?;

        database.touch_project_last_opened(project_id).await?;
        database
            .insert_session("done-continue-0001", "gpt-5.4", "main", "Done", project_id)
            .await?;
        database
            .update_session_title("done-continue-0001", "Continue terminal session")
            .await?;
        database
            .update_session_merged_commit_hash("done-continue-0001", Some(merged_commit_hash))
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds one in-progress session so the session view can show the active
/// Tachyonfx loader without launching a live agent backend.
fn seed_in_progress_session(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .upsert_project(&canonical_workdir.to_string_lossy(), Some("main"))
            .await?;

        database.touch_project_last_opened(project_id).await?;
        database
            .insert_session(
                LOADER_SESSION_ID,
                "gpt-5.4",
                "main",
                "InProgress",
                project_id,
            )
            .await?;
        database
            .update_session_title(LOADER_SESSION_ID, "Loader session")
            .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    std::fs::create_dir_all(test_support::session_folder(
        &env.agentty_root.join("wt"),
        LOADER_SESSION_ID,
    ))?;

    Ok(())
}

/// Verify that the Sessions tab shows an empty-state message when no
/// sessions exist.
///
/// Starts agentty in a fresh temp directory (no database, no sessions),
/// switches to the Sessions tab, and asserts that the placeholder text
/// is visible.
#[test]
fn session_list_empty_state() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_empty")
        .zola(
            "Empty session state",
            "A clean slate when no sessions exist yet.",
            40,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(2000)
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("sessions_tab", "Sessions tab with no sessions")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that `Ctrl+c` in a running session stops only the current turn and
/// returns the session to review-ready controls.
#[test]
fn session_stop_turn_returns_to_review() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_stop_turn_returns_to_review")
        .with_git()
        .setup(seed_in_progress_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("Ctrl+c: stop", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "running_session",
                        "Running session before stopping the turn",
                    )
                    .press_key("ctrl+c")
                    .wait_for_text("Enter: reply", 5000)
                    .viewing_pause_ms(1200)
                    .capture_labeled(
                        "review_after_stop",
                        "Session view after Ctrl+c stops only the active turn",
                    )
            },
            |frame, report| {
                let running_frame = common::frame_from_capture(&report.captures[0]);
                let running_full = Region::full(running_frame.cols(), running_frame.rows());
                assertion::assert_text_in_region(&running_frame, "Ctrl+c: stop", &running_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: reply", &full);
                assertion::assert_not_visible(frame, "Ctrl+c: stop");
            },
        )?;

    Ok(())
}

/// Verify that pressing `a` on the Sessions tab creates a session and
/// opens prompt mode with the submit footer.
#[test]
fn session_creation_opens_prompt_mode() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_creation")
        .with_git()
        .zola(
            "Session creation",
            "Start a new agent session with a single keypress.",
            30,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .press_key("a")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("prompt_mode", "Prompt mode after pressing a")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: submit", &full);
                assertion::assert_text_in_region(frame, "Esc: cancel", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `Shift+A` opens draft-session staging with explicit
/// draft guidance before any message is staged.
#[test]
fn draft_session_creation_opens_staging_mode() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_creation")
        .with_git()
        .zola(
            "Draft session creation",
            "Create a draft session that clearly starts in local staging mode before the bundle \
             runs.",
            31,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .write_text("A")
                    .wait_for_text("Draft Session", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_session_prompt_mode",
                        "Draft-session prompt mode immediately after creation",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Draft Session", &full);
                assertion::assert_text_in_region(frame, "No draft messages staged yet.", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `Esc` in an empty prompt for a new non-draft
/// session deletes it and returns to the empty Sessions list.
#[test]
fn session_prompt_cancel_returns_to_empty_list() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_cancel")
        .with_git()
        .zola(
            "Prompt cancel",
            "Cancel prompt input with Esc to return to the session view.",
            120,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("prompt_open", "Prompt mode opened")
                    .press_key("Esc")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_list", "Sessions list after cancel")
            },
            |frame, report| {
                let prompt_frame = common::frame_from_capture(&report.captures[0]);
                let prompt_full = Region::full(prompt_frame.cols(), prompt_frame.rows());
                assertion::assert_text_in_region(&prompt_frame, "Esc: cancel", &prompt_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "No sessions", &full);
            },
        )?;

    Ok(())
}

/// Verify that draft-session prompt mode can open `@` file lookup suggestions
/// before the deferred worktree exists.
#[test]
fn draft_session_at_lookup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("draft_session_at_lookup")
        .with_git()
        .setup(seed_draft_at_lookup_project)
        .zola(
            "Draft session @ lookup",
            "Browse project files with `@` before a draft session materializes its worktree.",
            121,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(1500)
                    .write_text("A")
                    .wait_for_text("Enter: stage draft", 3000)
                    .viewing_pause_ms(1000)
                    .write_text("@draft_lookup")
                    .wait_for_text("draft_lookup_target.txt", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "draft_at_lookup",
                        "Draft-session prompt mode with an active @ lookup",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "draft_lookup_target.txt", &full);
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `Enter` on a session opens the session view and
/// pressing `q` returns to the session list.
#[test]
fn session_open_and_return_to_list() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_open")
        .with_git()
        .zola(
            "Session open and return",
            "Open a session with Enter and return to the list with q.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_and_return_to_list())
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(2))
                    .viewing_pause_ms(2000)
                    .capture_labeled("session_view", "Session view after Enter")
                    .press_key("q")
                    .sleep(std::time::Duration::from_secs(1))
                    .viewing_pause_ms(2000)
                    .capture_labeled("back_to_list", "Sessions list after q")
            },
            |frame, report| {
                let session_view_frame = common::frame_from_capture(&report.captures[0]);
                let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
                assertion::assert_text_in_region(&session_view_frame, "q: back", &view_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "test", &full);
            },
        )?;

    Ok(())
}

/// Verify that active session output uses the Tachyonfx loader glyph instead
/// of dot-based working copy.
#[test]
fn session_active_loader_uses_tachyonfx_glyph() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_active_loader")
        .setup(seed_in_progress_session)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "session_active_loader",
                        "Active session view with Tachyonfx loader",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "▌▌▌ Working...", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `c` in a terminal session opens a confirmation and,
/// after acceptance, stages the continuation message before focusing an empty
/// draft composer.
#[test]
fn terminal_session_continue_opens_seeded_prompt() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("terminal_session_continue")
        .with_git()
        .setup(seed_done_session_for_continuation)
        .zola(
            "Continue terminal session",
            "Confirm continuation from a done session and stage a merged-commit summary message \
             before focusing an empty draft composer.",
            45,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .wait_for_text("q: back", 5000)
                    .press_key("c")
                    .wait_for_text("Confirm Continue", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "continue_confirmation",
                        "Continuation confirmation for the selected done session",
                    )
                    .press_key("y")
                    .wait_for_stable_frame(500, 15000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "terminal_session_continue",
                        "Continuation draft composer with the staged merged-commit summary",
                    )
            },
            |frame, report| {
                let confirmation_frame = common::frame_from_capture(&report.captures[0]);
                let confirmation_full =
                    Region::full(confirmation_frame.cols(), confirmation_frame.rows());
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Confirm Continue",
                    &confirmation_full,
                );
                assertion::assert_text_in_region(
                    &confirmation_frame,
                    "Create a new draft sess...",
                    &confirmation_full,
                );

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Enter: stage draft", &full);
                assertion::assert_text_in_region(frame, "Summarize changes from", &full);
                assertion::assert_text_in_region(frame, "704de31d0f4b5a12", &full);
                assertion::assert_text_in_region(frame, "as an initial context", &full);
                assertion::assert_text_in_region(frame, "Type your message", &full);
            },
        )?;

    Ok(())
}

/// Verify that pressing `p` in a review-ready session opens the review-request
/// publish popup.
#[test]
fn review_request_publish_shortcut_opens_publish_popup() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_publish_shortcut")
        .with_git()
        .setup(seed_review_ready_session)
        .zola(
            "Review request publish shortcut",
            "Open the review-request publish popup directly from session view with `p`.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(1))
                    .press_key("p")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_publish_popup",
                        "Review-request publish popup after pressing p",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Publish Review Request", &full);
                assertion::assert_text_in_region(frame, "Enter: publish review request", &full);
                assertion::assert_text_in_region(
                    frame,
                    "Leave blank to push as `wt/review-s`",
                    &full,
                );
            },
        )?;

    Ok(())
}

/// Verify that review-ready sessions no longer expose a manual review-request
/// sync shortcut because linked review requests refresh in the background.
#[test]
fn review_request_sync_runs_in_background() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_request_background_sync")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .zola(
            "Background review-request sync",
            "Review sessions track linked pull requests in the background instead of exposing a \
             manual sync shortcut.",
            43,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(1))
                    .press_key("?")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_request_background_sync",
                        "Review session help overlay without a manual sync shortcut",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                let view_text = frame.text_in_region(&full);

                assertion::assert_text_in_region(frame, "p: Create or refresh", &full);
                assert!(
                    !view_text.contains("s: Sync"),
                    "manual sync help action should be absent"
                );
            },
        )?;

    Ok(())
}

/// Verify that opening the diff page from a review-ready session and pressing
/// `c` toggles the right panel to review-request comments. The panel renders
/// the "Waiting for review-request comments to sync..." empty-state banner
/// before the background sync has populated the cache.
#[test]
fn review_comments_preview_opens_from_diff_page() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("review_comments_preview")
        .with_git()
        .setup(seed_review_ready_session_with_review_request)
        .zola(
            "Review comments preview",
            "From a review-ready session press `d` to open the diff page, then `c` to toggle the \
             right panel to inline review-request comments grouped by file.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(1))
                    .press_key("d")
                    .wait_for_stable_frame(300, 5000)
                    .press_key("c")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "review_comments_preview",
                        "Review comments preview after pressing d then c",
                    )
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Comments", &full);
                assertion::assert_text_in_region(frame, "Review request #42", &full);
            },
        )?;

    Ok(())
}

/// Verify that typing `/apply` in a review-ready session surfaces the
/// slash-command suggestion and, when submitted without a cached focused
/// review, shows the `Run a focused review first (f key)` guidance.
#[test]
fn apply_slash_command_guides_user_when_no_focused_review_cached() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("apply_slash_command_no_review")
        .with_git()
        .setup(seed_review_ready_session)
        .zola(
            "Apply slash command",
            "Submit `/apply` in a review-ready session and see guidance to run focused review \
             first.",
            42,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(1))
                    .press_key("/")
                    .wait_for_text("/apply", 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "apply_suggestion_visible",
                        "Slash command suggestion list shows /apply",
                    )
                    .write_text("apply")
                    .wait_for_stable_frame(300, 3000)
                    .press_key("Enter")
                    .wait_for_text("Run a focused review first", 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled(
                        "apply_guidance_shown",
                        "Session transcript shows focused-review guidance after /apply",
                    )
            },
            |frame, report| {
                let suggestion_frame = common::frame_from_capture(&report.captures[0]);
                let suggestion_full =
                    Region::full(suggestion_frame.cols(), suggestion_frame.rows());
                assertion::assert_text_in_region(&suggestion_frame, "/apply", &suggestion_full);

                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Run a focused review first", &full);
            },
        )?;

    Ok(())
}

/// Verify that `j` and `k` navigate the session list and that `Enter`
/// opens the currently selected session.
///
/// Creates two sessions ("alpha" and "beta"), navigates down with `j`,
/// opens the selection with `Enter`, returns with `q`, navigates back
/// up with `k`, and opens again. Asserts that both navigations still land on
/// openable session views after moving the cursor in the list.
#[test]
fn session_list_jk_navigation() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("session_navigation")
        .with_git()
        .zola(
            "Session list navigation",
            "Navigate sessions with j/k keys to select and open different entries.",
            44,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "alpha",
                    ))
                    .compose(&common::create_session_with_prompt_and_return_to_list(
                        "beta",
                    ))
                    .viewing_pause_ms(2000)
                    .capture_labeled("two_sessions", "Two sessions in list")
                    // Navigate down with j, open the selection, and capture.
                    .press_key("j")
                    .wait_for_stable_frame(300, 3000)
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(2))
                    .viewing_pause_ms(2000)
                    .capture_labeled("opened_after_j", "Session opened after pressing j")
                    .press_key("q")
                    .sleep(std::time::Duration::from_secs(1))
                    // Navigate back up with k, open the selection, and capture.
                    .press_key("k")
                    .wait_for_stable_frame(300, 3000)
                    .press_key("Enter")
                    .sleep(std::time::Duration::from_secs(2))
                    .viewing_pause_ms(2000)
                    .capture_labeled("opened_after_k", "Session opened after pressing k")
            },
            |_frame, report| {
                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (list, opened_after_j, opened_after_k)"
                );

                // Both sessions visible in the initial list.
                let initial_frame = common::frame_from_capture(&report.captures[0]);
                let initial_full = Region::full(initial_frame.cols(), initial_frame.rows());
                let initial_text = initial_frame.text_in_region(&initial_full);
                assert!(
                    initial_text.contains("alpha") && initial_text.contains("beta"),
                    "Expected both session prompts visible in list"
                );

                // Extract text from the two opened-session captures.
                let session_after_down_navigation_frame =
                    common::frame_from_capture(&report.captures[1]);
                let session_after_up_navigation_frame =
                    common::frame_from_capture(&report.captures[2]);

                let down_navigation_full = Region::full(
                    session_after_down_navigation_frame.cols(),
                    session_after_down_navigation_frame.rows(),
                );
                let up_navigation_full = Region::full(
                    session_after_up_navigation_frame.cols(),
                    session_after_up_navigation_frame.rows(),
                );

                let down_navigation_text =
                    session_after_down_navigation_frame.text_in_region(&down_navigation_full);
                let up_navigation_text =
                    session_after_up_navigation_frame.text_in_region(&up_navigation_full);

                // Each opened view must contain one of the session prompts.
                assert!(
                    down_navigation_text.contains("alpha") || down_navigation_text.contains("beta"),
                    "Session opened after j must contain alpha or beta"
                );
                assert!(
                    up_navigation_text.contains("alpha") || up_navigation_text.contains("beta"),
                    "Session opened after k must contain alpha or beta"
                );
            },
        )?;

    Ok(())
}
/// Verify that typed text appears in the prompt input.
#[test]
fn prompt_typing_shows_text() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_typing")
        .with_git()
        .zola(
            "Prompt typing",
            "Type text into the prompt input and see it appear in real time.",
            115,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("empty_prompt", "Empty prompt input")
                    .write_text("hello world")
                    .wait_for_text("hello world", 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("typed_text", "Prompt input with typed text")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "hello world", &full);
            },
        )?;

    Ok(())
}

/// Verify that Alt+Enter inserts a newline in the prompt input,
/// producing multiline content.
///
/// Alt+Enter is sent as ESC (0x1b) followed by CR (0x0d) which crossterm
/// interprets as `KeyCode::Enter` with `KeyModifiers::ALT`.
#[test]
fn prompt_multiline_via_alt_enter() -> E2eResult {
    // Arrange, Act, Assert
    FeatureTest::new("prompt_multiline")
        .with_git()
        .zola(
            "Multiline prompt",
            "Insert newlines with Alt+Enter to compose multiline prompts.",
            125,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .viewing_pause_ms(2000)
                    .press_key("a")
                    .wait_for_stable_frame(300, 5000)
                    .viewing_pause_ms(1500)
                    .write_text("first line")
                    .wait_for_text("first line", 3000)
                    .viewing_pause_ms(2000)
                    .capture_labeled("first_line", "First line typed")
                    // Alt+Enter: ESC (0x1b) followed by CR (0x0d).
                    .write_text("\x1b\r")
                    .wait_for_stable_frame(300, 3000)
                    .write_text("second line")
                    .wait_for_text("second line", 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("multiline", "Multiline prompt with both lines")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "first line", &full);
                assertion::assert_text_in_region(frame, "second line", &full);
            },
        )?;

    Ok(())
}
