//! Session lifecycle and prompt E2E tests.
//!
//! Tests cover session creation via `a` key, opening sessions with `Enter`,
//! list navigation with `j`/`k`, deletion with confirmation, prompt input
//! basics (typing, multiline via Alt+Enter, cancel via Esc), and returning
//! to the session list from session view.

use agentty::db::{DB_DIR, DB_FILE, Database};
use testty::assertion;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Builds a temporary E2E environment for session tests.
fn session_env(
    with_git: bool,
) -> Result<(tempfile::TempDir, BuilderEnv), Box<dyn std::error::Error>> {
    let temp = tempfile::TempDir::new()?;
    let env = BuilderEnv::new(temp.path())?;
    if with_git {
        env.init_git()?;
    }

    Ok((temp, env))
}

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
            .update_session_title(
                "review-shortcut-0001",
                "Refresh review request from session view",
            )
            .await?;
        database
            .update_session_diff_stats(12, 3, "review-shortcut-0001", "M")
            .await
    })?;

    std::fs::create_dir_all(env.agentty_root.join("wt").join("review-s"))?;

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
    // Arrange
    let (_temp, env) = session_env(false)?;

    let scenario = Scenario::new("session_empty")
        .compose(&common::wait_for_agentty_startup())
        .viewing_pause_ms(1500)
        // Navigate to Sessions tab.
        .compose(&common::switch_to_tab("Sessions"))
        .viewing_pause_ms(1500)
        .capture_labeled("sessions_tab", "Sessions tab with no sessions");

    // Act
    let (frame, report) = scenario.run_with_proof(env.builder())?;

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(&scenario, &report, &env, "session_empty");

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

/// Verify that pressing `Esc` in an empty prompt for a new non-draft
/// session deletes it and returns to the empty Sessions list.
#[test]
fn session_prompt_cancel_returns_to_empty_list() -> E2eResult {
    // Arrange
    let (_temp, env) = session_env(true)?;

    let scenario = Scenario::new("prompt_cancel")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .viewing_pause_ms(1500)
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .viewing_pause_ms(1500)
        .press_key("Esc")
        .wait_for_stable_frame(300, 5000)
        .viewing_pause_ms(1500)
        .capture_labeled("back_to_list", "Sessions list after cancel");

    // Act
    let (frame, report) = scenario.run_with_proof(env.builder())?;

    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "No sessions", &full);

    common::save_feature_gif(&scenario, &report, &env, "prompt_cancel");

    Ok(())
}

/// Verify that pressing `Enter` on a session opens the session view and
/// pressing `q` returns to the session list.
#[test]
fn session_open_and_return_to_list() -> E2eResult {
    // Arrange
    let (_temp, env) = session_env(true)?;

    let scenario = Scenario::new("open_and_return")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::create_session_and_return_to_list())
        // Open the session with Enter.
        .press_key("Enter")
        // Use fixed sleep because the agent may produce continuous output.
        .sleep(std::time::Duration::from_secs(2))
        .capture_labeled("session_view", "Session view after Enter")
        // Return to list with q.
        .press_key("q")
        .sleep(std::time::Duration::from_secs(1))
        .capture_labeled("back_to_list", "Sessions list after q");

    // Act
    let (frame, report) = scenario.run_with_proof(env.builder())?;

    // Assert — intermediate capture shows session view with back action.
    let session_view_frame = common::frame_from_capture(&report.captures[0]);
    let view_full = Region::full(session_view_frame.cols(), session_view_frame.rows());
    assertion::assert_text_in_region(&session_view_frame, "q: back", &view_full);

    // Assert — final frame is back on the list with the session visible.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "test", &full);

    common::save_feature_gif(&scenario, &report, &env, "session_open");

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
            },
        )?;

    Ok(())
}

/// Verify that `j` and `k` navigate the session list when multiple
/// sessions exist.
#[test]
fn session_list_jk_navigation() -> E2eResult {
    // Arrange
    let (_temp, env) = session_env(true)?;

    let scenario = Scenario::new("jk_navigation")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .compose(&common::create_session_with_prompt_and_return_to_list(
            "alpha",
        ))
        .compose(&common::create_session_with_prompt_and_return_to_list(
            "beta",
        ))
        .capture_labeled("two_sessions", "Two sessions in list")
        // Navigate down with j, open the selection, and capture the session.
        .press_key("j")
        .wait_for_stable_frame(300, 3000)
        .press_key("Enter")
        .sleep(std::time::Duration::from_secs(2))
        .capture_labeled("opened_after_j", "Session opened after pressing j")
        .press_key("q")
        .sleep(std::time::Duration::from_secs(1))
        // Navigate back up with k, open the selection, and capture it.
        .press_key("k")
        .wait_for_stable_frame(300, 3000)
        .press_key("Enter")
        .sleep(std::time::Duration::from_secs(2))
        .capture_labeled("opened_after_k", "Session opened after pressing k");

    // Act
    let (_frame, report) = scenario.run_with_proof(env.builder())?;

    // Assert — both sessions are visible in the list capture.
    let initial_frame = common::frame_from_capture(&report.captures[0]);
    let frame_after_down_open = common::frame_from_capture(&report.captures[1]);
    let frame_after_up_open = common::frame_from_capture(&report.captures[2]);

    let initial_matches = initial_frame.find_text("alpha");
    let initial_beta_matches = initial_frame.find_text("beta");
    let text_after_down_open = frame_after_down_open.text_in_region(&Region::full(
        frame_after_down_open.cols(),
        frame_after_down_open.rows(),
    ));
    let text_after_up_open = frame_after_up_open.text_in_region(&Region::full(
        frame_after_up_open.cols(),
        frame_after_up_open.rows(),
    ));

    assert!(
        !initial_matches.is_empty(),
        "Expected at least 1 'alpha' match in initial frame, found {}",
        initial_matches.len()
    );
    assert!(
        !initial_beta_matches.is_empty(),
        "Expected at least 1 'beta' match in initial frame, found {}",
        initial_beta_matches.len()
    );

    // Assert — `j` and `k` navigate to different sessions when opening them.
    assert!(
        text_after_down_open.contains("alpha") || text_after_down_open.contains("beta"),
        "Expected the session opened after j to contain either alpha or beta"
    );
    assert!(
        text_after_up_open.contains("alpha") || text_after_up_open.contains("beta"),
        "Expected the session opened after k to contain either alpha or beta"
    );
    assert_ne!(
        text_after_down_open.contains("alpha"),
        text_after_up_open.contains("alpha"),
        "Expected j and k to open different sessions"
    );

    common::save_feature_gif(&scenario, &report, &env, "session_navigation");

    Ok(())
}
/// Verify that typed text appears in the prompt input.
#[test]
fn prompt_typing_shows_text() -> E2eResult {
    // Arrange
    let (_temp, env) = session_env(true)?;

    let scenario = Scenario::new("prompt_typing")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .viewing_pause_ms(1500)
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .viewing_pause_ms(1000)
        .write_text("hello world")
        .wait_for_text("hello world", 3000)
        .viewing_pause_ms(1500)
        .capture_labeled("typed_text", "Prompt input with typed text");

    // Act
    let (frame, report) = scenario.run_with_proof(env.builder())?;

    // Assert — typed text is visible in the prompt.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "hello world", &full);

    common::save_feature_gif(&scenario, &report, &env, "prompt_typing");

    Ok(())
}

/// Verify that Alt+Enter inserts a newline in the prompt input,
/// producing multiline content.
///
/// Alt+Enter is sent as ESC (0x1b) followed by CR (0x0d) which crossterm
/// interprets as `KeyCode::Enter` with `KeyModifiers::ALT`.
#[test]
fn prompt_multiline_via_alt_enter() -> E2eResult {
    // Arrange
    let (_temp, env) = session_env(true)?;

    let scenario = Scenario::new("prompt_multiline")
        .compose(&common::wait_for_agentty_startup())
        .compose(&common::switch_to_tab("Sessions"))
        .viewing_pause_ms(1500)
        .press_key("a")
        .wait_for_stable_frame(300, 5000)
        .viewing_pause_ms(1000)
        .write_text("first line")
        .wait_for_text("first line", 3000)
        .viewing_pause_ms(1000)
        // Alt+Enter: ESC (0x1b) followed by CR (0x0d).
        .write_text("\x1b\r")
        .wait_for_stable_frame(300, 3000)
        .write_text("second line")
        .wait_for_text("second line", 3000)
        .viewing_pause_ms(1500)
        .capture_labeled("multiline", "Prompt input with multiline text");

    // Act
    let (frame, report) = scenario.run_with_proof(env.builder())?;

    // Assert — both lines are visible in the prompt.
    let full = Region::full(frame.cols(), frame.rows());
    assertion::assert_text_in_region(&frame, "first line", &full);
    assertion::assert_text_in_region(&frame, "second line", &full);

    common::save_feature_gif(&scenario, &report, &env, "prompt_multiline");

    Ok(())
}
