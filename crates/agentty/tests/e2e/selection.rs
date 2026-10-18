//! Selection visibility across list tabs, modal choices, and color modes.

use ag_session::{SettingName, test_support as model_fixture};
use agentty::app::Tab;
use agentty::domain::theme::ColorTheme;
use testty::frame::{CellColor, TerminalFrame};
use testty::proof::report::ProofReport;
use testty::region::Region;
use testty::scenario::Scenario;

use crate::common::{BuilderEnv, FeatureTest, SessionSeed};
use crate::{common, test_support};

type E2eResult = Result<(), Box<dyn std::error::Error>>;

/// Theme and color-mode pairs covering terminal wiring: a persisted
/// non-default theme reaches the terminal in color, and `NO_COLOR` keeps
/// selections visible. Per-theme selection colors are pinned by the
/// `ui::selection` and `ui::style` unit tests.
pub(crate) const SELECTION_COLOR_CASES: [(ColorTheme, bool); 2] =
    [(ColorTheme::Green, true), (ColorTheme::Current, false)];

#[tokio::test]
async fn selection_visibility_across_tabs_and_dialogs() -> E2eResult {
    for (theme, colors_enabled) in SELECTION_COLOR_CASES {
        // Arrange
        let next_theme = match theme {
            ColorTheme::Green => ColorTheme::Current,
            ColorTheme::Current | ColorTheme::DarkHorizon => ColorTheme::Green,
        };
        let theme_navigation = if theme == ColorTheme::Current {
            "j"
        } else {
            "k"
        };
        FeatureTest::new(format!(
            "selection_visibility_{}_{}",
            theme.as_str(),
            colors_enabled
        ))
        .with_git()
        .with_terminal_size(100, 30)
        .env("NO_COLOR", if colors_enabled { "" } else { "1" })
        .setup(move |env| Box::pin(async move { seed_selection_controls(env, theme).await }))
        .run(
            move |scenario| {
                // Act
                selection_scenario(scenario, theme_navigation)
            },
            move |_frame, report| {
                Box::pin(async move {
                    // Assert
                    assert_selection_report(report, theme, next_theme, colors_enabled);
                })
            },
        )
        .await?;
    }

    Ok(())
}

fn selection_scenario(scenario: Scenario, theme_navigation: &str) -> Scenario {
    // Act
    let scenario = scenario
        .compose(&common::wait_for_agentty_startup())
        .capture_labeled("projects", "Selected project")
        .press_key("j")
        .wait_for_stable_frame(200, 3000)
        .capture_labeled("next_project", "Highlight moved to another project")
        .compose(&common::switch_to_tab("Sessions"))
        .capture_labeled("sessions", "Selected session")
        .press_key("p")
        .wait_for_text("Switch project", 5000)
        .capture_labeled("project_picker", "Project choice")
        .press_key("q")
        .press_key("a")
        .wait_for_text("Select session type", 5000)
        .capture_labeled("creation", "Regular choice")
        .press_key("j")
        .wait_for_stable_frame(200, 3000)
        .capture_labeled("draft", "Draft choice")
        .press_key("q")
        .compose(&common::open_quit_dialog())
        .capture_labeled("confirmation", "Safe default choice")
        .press_key("Esc")
        .wait_for_stable_frame(200, 3000)
        .compose(&common::switch_to_tab("Settings"))
        .capture_labeled("settings", "Selected setting")
        .press_key("Enter")
        .wait_for_text("Enter: select", 5000)
        .capture_labeled("theme_picker", "Selected theme option")
        .press_key(theme_navigation)
        .wait_for_stable_frame(200, 3000)
        .capture_labeled("next_theme", "Highlight moved to another theme option")
        .press_key("Esc")
        .wait_for_stable_frame(200, 3000);
    let scenario = (0..5).fold(scenario, |scenario, _| scenario.press_key("j"));
    scenario
        .press_key("Enter")
        .wait_for_text("J/K: reorder", 5000)
        .capture_labeled("launch_editor", "Selected launch command")
        .press_key("j")
        .wait_for_stable_frame(200, 3000)
        .capture_labeled("next_command", "Highlight moved to another command")
}

fn assert_selection_report(
    report: &ProofReport,
    theme: ColorTheme,
    next_theme: ColorTheme,
    colors_enabled: bool,
) {
    let (foreground, background) = selection_colors(theme);
    // Assert
    let selected_controls = [
        ("test-project", Tab::Projects),
        ("zeta-project", Tab::Projects),
        ("Alpha", Tab::Sessions),
        ("test-project", Tab::Sessions),
        ("Regular", Tab::Sessions),
        ("Draft", Tab::Sessions),
        ("No", Tab::Sessions),
        ("Theme", Tab::Settings),
        (theme.label(), Tab::Settings),
        (next_theme.label(), Tab::Settings),
        ("cargo test", Tab::Settings),
        ("cargo run", Tab::Settings),
    ];
    assert_eq!(report.captures.len(), selected_controls.len());
    for (capture, (label, active_tab)) in report.captures.iter().zip(selected_controls) {
        let frame = common::frame_from_capture(capture);
        assert_active_tab(
            &frame,
            active_tab,
            colors_enabled.then_some((foreground, background)),
        );
        let label_foreground = if label == "Alpha" && theme == ColorTheme::Current {
            CellColor::white()
        } else {
            foreground
        };
        assert_selected_label(
            &frame,
            label,
            colors_enabled.then_some((label_foreground, background)),
        );
        if label == "Alpha" {
            assert_session_detail_colors(&frame, theme, colors_enabled, background);
        }
    }
    let colors = colors_enabled.then_some((foreground, background));
    for (before, after, initial, next) in [
        (0, 1, "test-project", "zeta-project"),
        (4, 5, "Regular", "Draft"),
        (8, 9, theme.label(), next_theme.label()),
        (10, 11, "cargo test", "cargo run"),
    ] {
        let before_frame = common::frame_from_capture(&report.captures[before]);
        let after_frame = common::frame_from_capture(&report.captures[after]);
        let option_region = if before == 4 {
            let title = before_frame.find_text("Select session type");
            assert_eq!(title.len(), 1, "session selector title must be visible");
            Region::new(0, title[0].rect.row + 2, before_frame.cols(), 5)
        } else if before == 8 {
            let footer = before_frame.find_text("j/k: move | Enter: select | Esc/q: close");
            assert_eq!(footer.len(), 1, "theme dropdown footer must be visible");
            Region::new(0, footer[0].rect.row - 4, before_frame.cols(), 3)
        } else {
            Region::full(before_frame.cols(), before_frame.rows())
        };
        assert_unselected_label(&before_frame, next, colors, option_region);
        assert_unselected_label(&after_frame, initial, colors, option_region);
        assert_selected_label_in_region(&before_frame, initial, colors, option_region);
        assert_selected_label_in_region(&after_frame, next, colors, option_region);
    }
}

/// Session metadata keeps semantic colors and regular weight inside the
/// highlight.
fn assert_session_detail_colors(
    frame: &TerminalFrame,
    theme: ColorTheme,
    colors_enabled: bool,
    background: CellColor,
) {
    let (text, muted, success) = match theme {
        ColorTheme::Current => (
            CellColor::white(),
            CellColor::new(192, 192, 192),
            CellColor::new(0, 128, 0),
        ),
        ColorTheme::Green => (
            CellColor::new(205, 245, 211),
            CellColor::new(126, 159, 120),
            CellColor::new(86, 184, 105),
        ),
        ColorTheme::DarkHorizon => (
            CellColor::new(214, 217, 232),
            CellColor::new(132, 136, 168),
            CellColor::new(41, 211, 152),
        ),
    };
    let title = frame.find_text("Alpha");
    let row = Region::new(0, title[0].rect.row, frame.cols(), 1);
    for (label, foreground) in [
        ("[XS]", success),
        ("Draft", muted),
        (model_fixture::CODEX_MODEL_ID, text),
    ] {
        let matches = frame.find_text_in_region(label, &row);
        assert_eq!(matches.len(), 1, "session detail {label} must be visible");
        let detail = &matches[0];
        if colors_enabled {
            assert_eq!(detail.foreground, Some(foreground), "color for {label}");
            assert_eq!(detail.background, Some(background));
            assert!(
                !detail.style.bold(),
                "metadata must retain regular weight: {label}"
            );
        } else {
            assert!(detail.style.inverse());
            assert!(detail.style.bold());
            assert_eq!(detail.foreground, None);
            assert_eq!(detail.background, None);
        }
    }
}

fn assert_active_tab(
    frame: &TerminalFrame,
    active_tab: Tab,
    colors: Option<(CellColor, CellColor)>,
) {
    let headers = frame.find_text(" | Project: ");
    assert_eq!(headers.len(), 1, "tab header must be visible");
    let header_row = Region::new(0, headers[0].rect.row, frame.cols(), 1);
    let tabs = frame.find_text_in_region(active_tab.title(), &header_row);
    assert_eq!(tabs.len(), 1, "active tab must be visible");
    assert!(!tabs[0].style.underline(), "tab must not be underlined");
    if let Some((foreground, background)) = colors {
        assert!(tabs[0].style.bold(), "active tab must remain bold");
        assert_eq!(tabs[0].foreground, Some(foreground));
        assert_eq!(tabs[0].background, Some(background));
    } else {
        assert!(tabs[0].style.bold(), "active monochrome tab must be bold");
        assert!(
            tabs[0].style.inverse(),
            "active monochrome tab must be inverted"
        );
        assert_eq!(tabs[0].foreground, None);
        assert_eq!(tabs[0].background, None);
    }
}

async fn seed_selection_controls(env: &BuilderEnv, theme: ColorTheme) -> E2eResult {
    common::seed_session(
        env,
        SessionSeed::draft(
            "alpha-session",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "Draft",
        )
        .with_title("Alpha"),
    )
    .await?;
    common::seed_second_project(env).await?;
    let database = common::open_database(env).await?;
    let project_id = database
        .projects()
        .upsert_project(
            &env.workdir.canonicalize()?.to_string_lossy(),
            Some("main".to_string()),
        )
        .await?;
    database
        .settings()
        .upsert_setting(SettingName::Theme, theme.as_str())
        .await?;
    database
        .settings()
        .upsert_project_setting(
            project_id,
            SettingName::LaunchConfiguration,
            "cargo test\ncargo run",
        )
        .await?;
    test_support::persist_active_tab_for_test(&database, Tab::Projects).await?;

    Ok(())
}

pub(crate) fn assert_selected_label(
    frame: &TerminalFrame,
    label: &str,
    colors: Option<(CellColor, CellColor)>,
) {
    assert_selected_label_in_region(
        frame,
        label,
        colors,
        Region::full(frame.cols(), frame.rows()),
    );
}

fn assert_selected_label_in_region(
    frame: &TerminalFrame,
    label: &str,
    colors: Option<(CellColor, CellColor)>,
    region: Region,
) {
    let matches = frame.find_text_in_region(label, &region);
    assert!(
        !matches.is_empty(),
        "selected label {label}: {}",
        frame.all_text()
    );
    assert!(
        frame
            .all_text()
            .lines()
            .all(|line| { !line.trim_start_matches([' ', '│']).starts_with("> ") }),
        "selection must not use a selector glyph: {}",
        frame.all_text()
    );
    if let Some((foreground, background)) = colors {
        assert!(
            matches.iter().any(|span| {
                span.foreground == Some(foreground)
                    && span.background == Some(background)
                    && span.style.bold()
            }),
            "highlight must identify {label}: {matches:?}\n{}",
            frame.all_text()
        );
    } else {
        assert!(
            matches.iter().any(|span| {
                span.style.inverse()
                    && span.style.bold()
                    && span.foreground.is_none()
                    && span.background.is_none()
            }),
            "monochrome highlight must identify {label}: {matches:?}\n{}",
            frame.all_text()
        );
    }
}

fn assert_unselected_label(
    frame: &TerminalFrame,
    label: &str,
    colors: Option<(CellColor, CellColor)>,
    region: Region,
) {
    let matches = frame.find_text_in_region(label, &region);
    assert!(
        !matches.is_empty(),
        "unselected option {label} must remain visible"
    );
    assert!(
        matches.iter().all(|span| {
            if let Some((_, background)) = colors {
                span.background != Some(background)
            } else {
                !span.style.inverse()
            }
        }),
        "highlight must leave {label}: {matches:?}"
    );
}

/// Expected foreground and background for an explicit child-process theme.
pub(crate) fn selection_colors(theme: ColorTheme) -> (CellColor, CellColor) {
    match theme {
        ColorTheme::Current => (CellColor::new(245, 245, 245), CellColor::new(50, 62, 78)),
        ColorTheme::Green => (CellColor::new(205, 245, 211), CellColor::new(44, 78, 54)),
        ColorTheme::DarkHorizon => (CellColor::new(214, 217, 232), CellColor::new(45, 50, 68)),
    }
}
