//! Settings page E2E tests: content rendering, navigation, and editing.

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use agentty::db::{DB_DIR, DB_FILE, Database};
use agentty::domain::agent::ReasoningLevel;
use agentty::test_support::persist_project_reasoning_levels_for_test;
use testty::assertion;
use testty::journey::Journey;
use testty::region::Region;
use testty::step::Step;

use crate::common;
use crate::common::{BuilderEnv, FeatureTest};

const DEFAULT_SMART_MODEL_ROW_OFFSET: usize = 3;
const LAUNCH_CONFIGURATIONS_ROW_OFFSET: usize = 7;

/// Moves from the initial Theme row to a known settings row.
fn move_to_settings_row(name: &str, row_offset: usize) -> Journey {
    let mut journey = Journey::new(format!("move_to_{name}"));
    for _ in 0..row_offset {
        journey = journey.step(Step::press_key("j"));
    }

    journey
}

/// Adds a Gemini CLI stub so the settings page exposes a real Gemini-owned
/// model option.
fn seed_gemini_settings_cli_stub(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let stub_agent_path = env.stub_bin.join("gemini");
    fs::write(&stub_agent_path, "#!/bin/sh\nexit 1\n")?;

    #[cfg(unix)]
    {
        fs::set_permissions(&stub_agent_path, fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

/// Seeds deterministic selector values for the settings navigation test.
///
/// Persists the three model selectors to `claude-opus-4-6` so the test can
/// verify Agentty upgrades retired stored model ids to `claude-opus-5`
/// before row navigation changes the visible selector values.
fn seed_settings_navigation_models(env: &BuilderEnv) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .projects()
            .upsert_project(&canonical_workdir.to_string_lossy(), None)
            .await?;

        for setting_name in [
            "DefaultSmartModel",
            "DefaultFastModel",
            "DefaultReviewModel",
        ] {
            sqlx::query!(
                r"
INSERT INTO project_setting (project_id, name, value)
VALUES (?, ?, ?)
ON CONFLICT(project_id, name) DO UPDATE
SET value = excluded.value
",
                project_id,
                setting_name,
                "claude-opus-4-6"
            )
            .execute(database.pool())
            .await?;
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Seeds distinct persisted role reasoning levels for model-selector coverage.
fn seed_settings_model_reasoning_levels(
    env: &BuilderEnv,
) -> Result<(), Box<dyn std::error::Error>> {
    let canonical_workdir = env.workdir.canonicalize()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let db_path = env.agentty_root.join(DB_DIR).join(DB_FILE);
        let database = Database::open(&db_path).await?;
        let project_id = database
            .projects()
            .upsert_project(&canonical_workdir.to_string_lossy(), None)
            .await?;

        persist_project_reasoning_levels_for_test(
            &database,
            project_id,
            ReasoningLevel::High,
            ReasoningLevel::Low,
            ReasoningLevel::XHigh,
        )
        .await?;

        Ok::<(), Box<dyn std::error::Error>>(())
    })?;

    Ok(())
}

/// Verify that the Settings tab renders all setting rows with labels.
///
/// Navigates to the Settings tab and asserts that the settings table
/// appears with expected role model rows and `Launch Configurations`. It also
/// verifies the Agentty coauthor trailer starts disabled for new projects.
#[test]
fn settings_tab_shows_content() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_content")
        .setup(seed_gemini_settings_cli_stub)
        .with_stub_only_path()
        .zola(
            "Settings tab",
            "View and configure agent settings like reasoning level and models.",
            150,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .viewing_pause_ms(1500)
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(3000)
                    .capture_labeled("settings_tab", "Settings tab with all rows")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Settings", &full);
                assertion::assert_text_in_region(frame, "Global settings", &full);
                assertion::assert_text_in_region(frame, "Default Smart Model", &full);
                assertion::assert_text_in_region(frame, "Default Fast Model", &full);
                assertion::assert_text_in_region(frame, "Default Review Model", &full);
                assertion::assert_text_in_region(frame, "gemini/gemini-3.1-pro-preview", &full);
                assertion::assert_text_in_region(frame, "[high]", &full);
                assertion::assert_text_in_region(frame, "Disabled", &full);
                assertion::assert_text_in_region(frame, "Launch Configurations", &full);
                assertion::assert_text_in_region(frame, "Theme", &full);
                assertion::assert_text_in_region(frame, "Agentty Default", &full);
                assertion::assert_text_in_region(frame, "Orchestrator Parallelism", &full);
                assertion::assert_text_in_region(frame, "Auto-approve Research", &full);
            },
        )
        .expect("feature test failed");
}

/// Verify that `j` and `k` navigate through settings rows.
///
/// Opens the Settings tab and presses `j` multiple times to move the
/// selection down, then `k` to move back up. The test confirms the selected
/// row by seeding deterministic retired Opus model values, observing startup
/// migration to `claude-opus-5`, and then checking which role reasoning
/// value advances after each dropdown selection.
#[test]
fn settings_jk_navigation() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_navigation")
        .setup(seed_settings_navigation_models)
        .zola(
            "Settings navigation",
            "Navigate settings rows with j/k keys.",
            152,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("initial", "Settings tab at first row")
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("moved_down", "Selection moved down five rows")
                    .press_key("k")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("moved_up", "Selection moved back up one row")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Settings", &full);

                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected 3 captures (initial, moved_down, moved_up)"
                );

                let initial_frame = common::frame_from_capture(&report.captures[0]);
                assertion::assert_match_count(&initial_frame, "claude-opus-4-6", 0);
                assertion::assert_match_count(&initial_frame, "claude/claude-opus-5", 3);

                let moved_down_frame = common::frame_from_capture(&report.captures[1]);
                assertion::assert_match_count(&moved_down_frame, "claude-opus-4-6", 0);
                assertion::assert_match_count(&moved_down_frame, "claude/claude-opus-5", 3);
                assertion::assert_match_count(&moved_down_frame, "[high]", 2);
                assertion::assert_match_count(&moved_down_frame, "[xhigh]", 1);

                let moved_up_frame = common::frame_from_capture(&report.captures[2]);
                assertion::assert_match_count(&moved_up_frame, "claude-opus-4-6", 0);
                assertion::assert_match_count(&moved_up_frame, "claude/claude-opus-5", 3);
                assertion::assert_match_count(&moved_up_frame, "[high]", 1);
                assertion::assert_match_count(&moved_up_frame, "[xhigh]", 2);
            },
        )
        .expect("feature test failed");
}

/// Verify that selector settings are edited through dropdowns.
///
/// Opens the Settings tab, moves to the Smart role selector, opens the
/// model dropdown, advances to the reasoning dropdown, and saves maximum
/// reasoning for the current model. Captures each stage for the GIF.
#[test]
fn settings_dropdown_selects_value() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_edit")
        .zola(
            "Settings editing",
            "Open dropdowns to select setting values.",
            154,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .compose(&move_to_settings_row(
                        "default_smart_model",
                        DEFAULT_SMART_MODEL_ROW_OFFSET,
                    ))
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("before_edit", "Smart model and reasoning before selection")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("model_dropdown", "Choose the Smart model")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(1500)
                    .capture_labeled("reasoning_dropdown", "Choose Smart model reasoning")
                    .press_key("j")
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled("after_edit", "Smart model with maximum reasoning")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Default Smart Model", &full);

                assert_eq!(
                    report.captures.len(),
                    4,
                    "Expected captures before, at both selector stages, and after selection"
                );

                let before_frame = common::frame_from_capture(&report.captures[0]);
                let before_full = Region::full(before_frame.cols(), before_frame.rows());
                assertion::assert_text_in_region(&before_frame, "[high]", &before_full);

                let model_frame = common::frame_from_capture(&report.captures[1]);
                let model_full = Region::full(model_frame.cols(), model_frame.rows());
                assertion::assert_text_in_region(&model_frame, "Select model", &model_full);
                assertion::assert_text_in_region(&model_frame, "codex/", &model_full);

                let reasoning_frame = common::frame_from_capture(&report.captures[2]);
                let reasoning_full = Region::full(reasoning_frame.cols(), reasoning_frame.rows());
                assertion::assert_text_in_region(
                    &reasoning_frame,
                    "Select reasoning level",
                    &reasoning_full,
                );
                assertion::assert_text_in_region(&reasoning_frame, "xhigh", &reasoning_full);
                assertion::assert_text_in_region(&reasoning_frame, "max", &reasoning_full);

                let after_frame = common::frame_from_capture(&report.captures[3]);
                let after_full = Region::full(after_frame.cols(), after_frame.rows());
                assertion::assert_text_in_region(&after_frame, "[max]", &after_full);
            },
        )
        .expect("feature test failed");
}

/// Verify that each role persists and shows its own default reasoning level.
///
/// Opens the smart-model selector, confirms models appear once without a
/// model-reasoning cross product, then advances to the reasoning selector.
#[test]
fn settings_model_selector_uses_two_steps() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_model_reasoning")
        .setup(seed_settings_model_reasoning_levels)
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .compose(&move_to_settings_row(
                        "default_smart_model",
                        DEFAULT_SMART_MODEL_ROW_OFFSET,
                    ))
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled(
                        "model_rows",
                        "Model setting rows with persisted reasoning levels",
                    )
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled(
                        "model_selector",
                        "Choose a model without repeated reasoning combinations",
                    )
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("reasoning_selector", "Choose the default reasoning level")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Select reasoning level", &full);
                for reasoning_level in ["low", "medium", "high", "xhigh", "max"] {
                    assertion::assert_text_in_region(frame, reasoning_level, &full);
                }
                assert_eq!(
                    report.captures.len(),
                    3,
                    "Expected model rows, model selector, and reasoning selector captures"
                );

                let rows_frame = common::frame_from_capture(&report.captures[0]);
                assertion::assert_match_count(&rows_frame, "[high]", 1);
                assertion::assert_match_count(&rows_frame, "[low]", 1);
                assertion::assert_match_count(&rows_frame, "[xhigh]", 1);

                let model_frame = common::frame_from_capture(&report.captures[1]);
                let model_full = Region::full(model_frame.cols(), model_frame.rows());
                assertion::assert_text_in_region(&model_frame, "Select model", &model_full);
                assertion::assert_match_count(
                    &model_frame,
                    "gemini/gemini-3.1-pro-preview [max]",
                    0,
                );
            },
        )
        .expect("feature test failed");
}

/// Verify that `Launch Configurations` are edited as discrete list entries.
///
/// Opens the command-list editor, adds one command, edits it, adds another
/// command, reorders the entries, deletes the selected command, and confirms
/// the settings row summarizes the remaining command.
#[test]
fn settings_launch_configurations_list_editor() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_launch_configurations")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(1500)
                    .compose(&move_to_settings_row(
                        "launch_configurations",
                        LAUNCH_CONFIGURATIONS_ROW_OFFSET,
                    ))
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled(
                        "empty_editor",
                        "Launch Configurations list editor with no commands",
                    )
                    .press_key("a")
                    .wait_for_stable_frame(200, 3000)
                    .write_text("nvim .")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("add_input", "Adding a launch configuration")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Backspace")
                    .press_key("Backspace")
                    .press_key("Backspace")
                    .press_key("Backspace")
                    .press_key("Backspace")
                    .press_key("Backspace")
                    .write_text("lazygit")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("edit_input", "Editing a launch configuration")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("a")
                    .wait_for_stable_frame(200, 3000)
                    .write_text("npm run dev")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("two_commands", "Two configured launch configurations")
                    .write_text("K")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("reordered", "Selected command moved up")
                    .press_key("d")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("deleted", "Selected command deleted")
                    .press_key("Escape")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("summary", "Launch Configurations row summary after editing")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Launch Configurations", &full);
                assertion::assert_text_in_region(frame, "lazygit", &full);

                assert_eq!(
                    report.captures.len(),
                    7,
                    "Expected 7 captures for the launch-configuration list editor workflow"
                );

                let empty_frame = common::frame_from_capture(&report.captures[0]);
                let empty_full = Region::full(empty_frame.cols(), empty_frame.rows());
                assertion::assert_text_in_region(
                    &empty_frame,
                    "(no commands configured)",
                    &empty_full,
                );

                let add_frame = common::frame_from_capture(&report.captures[1]);
                let add_full = Region::full(add_frame.cols(), add_frame.rows());
                assertion::assert_text_in_region(&add_frame, "Add command", &add_full);
                assertion::assert_text_in_region(&add_frame, "nvim .|", &add_full);

                let edit_frame = common::frame_from_capture(&report.captures[2]);
                let edit_full = Region::full(edit_frame.cols(), edit_frame.rows());
                assertion::assert_text_in_region(&edit_frame, "Edit command", &edit_full);
                assertion::assert_text_in_region(&edit_frame, "lazygit|", &edit_full);

                let two_command_frame = common::frame_from_capture(&report.captures[3]);
                let two_command_full =
                    Region::full(two_command_frame.cols(), two_command_frame.rows());
                assertion::assert_text_in_region(
                    &two_command_frame,
                    "npm run dev",
                    &two_command_full,
                );

                let deleted_frame = common::frame_from_capture(&report.captures[5]);
                assertion::assert_match_count(&deleted_frame, "npm run dev", 0);
            },
        )
        .expect("feature test failed");
}

/// Verify shared input word deletion, undo, and redo in a single-line editor.
#[test]
fn test_input_undo_redo() {
    // Arrange, Act, Assert
    FeatureTest::new("input_undo_redo")
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .compose(&move_to_settings_row(
                        "launch_configurations",
                        LAUNCH_CONFIGURATIONS_ROW_OFFSET,
                    ))
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("a")
                    .write_text("hello brave world")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("typed", "Text before shared editing shortcuts")
                    .press_key("ctrl+w")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("word_deleted", "Previous word deleted with Ctrl+w")
                    .press_key("ctrl+z")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("undone", "Deleted word restored with Ctrl+z")
                    .press_key("ctrl+y")
                    .wait_for_stable_frame(200, 3000)
                    .capture_labeled("redone", "Word deletion restored with Ctrl+y")
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "hello brave|", &full);
                assert_eq!(
                    report.captures.len(),
                    4,
                    "Expected four input edit captures"
                );

                let typed_frame = common::frame_from_capture(&report.captures[0]);
                let typed_full = Region::full(typed_frame.cols(), typed_frame.rows());
                assertion::assert_text_in_region(&typed_frame, "hello brave world|", &typed_full);

                let deleted_frame = common::frame_from_capture(&report.captures[1]);
                let deleted_full = Region::full(deleted_frame.cols(), deleted_frame.rows());
                assertion::assert_text_in_region(&deleted_frame, "hello brave|", &deleted_full);

                let undone_frame = common::frame_from_capture(&report.captures[2]);
                let undone_full = Region::full(undone_frame.cols(), undone_frame.rows());
                assertion::assert_text_in_region(&undone_frame, "hello brave world|", &undone_full);
            },
        )
        .expect("feature test failed");
}

/// Verify that the `Theme` settings row dropdown selects `Agentty Default`,
/// `Agentty Green`, and `Dark Horizon` and wraps back to `Agentty Default`.
#[test]
fn settings_theme_switch() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_theme_switch")
        .zola(
            "Settings theme switch",
            "Select Agentty Default, Agentty Green, and Dark Horizon themes from settings.",
            155,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .capture_labeled("before_theme_switch", "Theme setting before switching")
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled(
                        "after_theme_switch_agentty_green",
                        "Agentty Green theme selected",
                    )
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled(
                        "after_theme_switch_dark_horizon",
                        "Dark Horizon theme selected",
                    )
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("j")
                    .wait_for_stable_frame(200, 3000)
                    .press_key("Enter")
                    .wait_for_stable_frame(200, 3000)
                    .viewing_pause_ms(2500)
                    .capture_labeled(
                        "after_theme_switch_wrap_agentty_default",
                        "Theme wraps back to Agentty Default",
                    )
            },
            |frame, report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Theme", &full);
                assertion::assert_text_in_region(frame, "Agentty Default", &full);

                assert_eq!(
                    report.captures.len(),
                    4,
                    "Expected 4 captures (initial Agentty Default, Agentty Green, Dark Horizon, \
                     wrapped Agentty Default)"
                );

                let before_frame = common::frame_from_capture(&report.captures[0]);
                let before_full = Region::full(before_frame.cols(), before_frame.rows());
                assertion::assert_text_in_region(&before_frame, "Agentty Default", &before_full);

                let agentty_green_frame = common::frame_from_capture(&report.captures[1]);
                let agentty_green_full =
                    Region::full(agentty_green_frame.cols(), agentty_green_frame.rows());
                assertion::assert_text_in_region(
                    &agentty_green_frame,
                    "Agentty Green",
                    &agentty_green_full,
                );

                let dark_horizon_frame = common::frame_from_capture(&report.captures[2]);
                let dark_horizon_full =
                    Region::full(dark_horizon_frame.cols(), dark_horizon_frame.rows());
                assertion::assert_text_in_region(
                    &dark_horizon_frame,
                    "Dark Horizon",
                    &dark_horizon_full,
                );

                let wrapped_frame = common::frame_from_capture(&report.captures[3]);
                let wrapped_full = Region::full(wrapped_frame.cols(), wrapped_frame.rows());
                assertion::assert_text_in_region(&wrapped_frame, "Agentty Default", &wrapped_full);
            },
        )
        .expect("feature test failed");
}

/// Verify that the help overlay on the Settings tab shows settings-specific
/// keybinding hints.
#[test]
fn settings_help_shows_edit_hint() {
    // Arrange, Act, Assert
    FeatureTest::new("settings_help")
        .zola(
            "Settings help",
            "Press ? on the Settings tab to see settings-specific keybindings.",
            156,
        )
        .run(
            |scenario| {
                scenario
                    .compose(&common::wait_for_agentty_startup())
                    .compose(&common::switch_to_tab("Sessions"))
                    .compose(&common::switch_to_tab("Settings"))
                    .viewing_pause_ms(2000)
                    .compose(&common::open_help_overlay())
                    .viewing_pause_ms(3000)
                    .capture_labeled("settings_help", "Help overlay on Settings tab")
            },
            |frame, _report| {
                let full = Region::full(frame.cols(), frame.rows());
                assertion::assert_text_in_region(frame, "Keybindings", &full);
                assertion::assert_text_in_region(frame, "edit", &full);
            },
        )
        .expect("feature test failed");
}
