use ag_session::test_support as model_fixture;
use ratatui::style::Modifier;

use super::{
    prompt_session_status, session_header_lines, session_output_queued_lines,
    session_output_status_icon, session_output_status_message, session_output_uses_tachyon_loader,
    session_resources_line, session_speed_display, session_started_label,
};
use crate::domain::agent::{AgentModel, ResponseStyle};
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Session, Status,
};
use crate::presentation::frame_time::FrameTime;
use crate::test_support::SessionFixtureBuilder;
use crate::ui::icon::Icon;
use crate::ui::style;

#[test]
fn queued_preview_compacts_and_truncates_to_terminal_columns() {
    // Arrange
    let message = " \n修复\tbuild\n\nwith context \n";

    // Act
    let full = session_output_queued_lines(message, "queued › ", 80);
    let narrow = session_output_queued_lines(message, "queued › ", 18);
    let blank = session_output_queued_lines(" \n\t", "queued › ", 80);
    let zero_width = session_output_queued_lines(message, "", 0);

    // Assert
    assert_eq!(full.len(), 1);
    assert_eq!(full[0].to_string(), "≡ queued › 修复 build with context");
    assert_eq!(narrow.len(), 1);
    assert_eq!(narrow[0].to_string(), "≡ queued › 修复...");
    assert_eq!(narrow[0].width(), 18);
    assert_eq!(full[0].style.fg, Some(style::palette::text_subtle()));
    assert!(full[0].style.add_modifier.contains(Modifier::ITALIC));
    assert_eq!(blank, Vec::<ratatui::text::Line<'_>>::new());
    assert_eq!(zero_width, Vec::<ratatui::text::Line<'_>>::new());
}

#[test]
fn resource_row_formats_values_unavailable_and_narrow_widths() {
    // Arrange
    let resources = SessionResources {
        cpu_percent: 128.5,
        process_count: 3,
        resident_memory_kib: 3584,
    };

    // Act
    let row = session_resources_line(Some(resources), None, 80);
    let unavailable = session_resources_line(None, None, 80);
    let narrow = session_resources_line(Some(resources), None, 15);

    // Assert
    assert_eq!(
        row.to_string(),
        "Processes: 3  CPU: 128.5%  Memory: 3.5 MiB  Host CPU temp: --"
    );
    assert_eq!(
        unavailable.to_string(),
        "Processes: --  CPU: --  Memory: --  Host CPU temp: --"
    );
    assert!(narrow.width() <= 15);
    assert_eq!(session_resources_line(Some(resources), None, 0).width(), 0);
    assert!(
        session_resources_line(Some(SessionResources::default()), None, 80)
            .to_string()
            .contains("Processes: 0  CPU: 0.0%  Memory: 0.0 MiB")
    );
}

#[test]
fn resource_row_shows_host_cpu_temperature_in_celsius() {
    // Arrange
    let resources = SessionResources::default();

    // Act
    let row = session_resources_line(Some(resources), Some(64.5), 80);

    // Assert
    assert_eq!(
        row.to_string(),
        "Processes: 0  CPU: 0.0%  Memory: 0.0 MiB  Host CPU temp: 64.5°C"
    );
}

fn session_with_review_request(url: &str) -> Session {
    let mut session = SessionFixtureBuilder::new().build();
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 1,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "main".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Update workflow".to_string(),
            web_url: url.to_string(),
        },
    });

    session
}

#[test]
fn test_session_header_lines_shows_review_request_url_below_title() {
    // Arrange
    let session = session_with_review_request("https://github.com/agentty-xyz/agentty/pull/42");
    let header_width = 180;

    // Act
    let header_lines = session_header_lines(&session, header_width, false);
    let metadata_line = header_lines[1].to_string();

    // Assert
    assert_eq!(header_lines.len(), 2);
    assert_eq!(
        metadata_line,
        "https://github.com/agentty-xyz/agentty/pull/42"
    );
}

#[test]
fn test_session_header_lines_truncates_review_url_and_omits_empty_urls() {
    // Arrange
    for (url, expected) in [
        (
            " https://example.test/pull/42 ",
            Some("https://example.t..."),
        ),
        ("", None),
        ("  ", None),
    ] {
        let session = session_with_review_request(url);

        // Act
        let header = session_header_lines(&session, 20, false);

        // Assert
        assert_eq!(header.get(1).map(ToString::to_string).as_deref(), expected);
    }
}

#[test]
fn test_session_header_lines_show_red_merge_conflict_alert() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.base_branch = "develop".to_string();

    // Act
    let header_lines = session_header_lines(&session, 100, true);

    // Assert
    assert_eq!(header_lines[1].to_string(), "Merge conflict with develop");
    assert_eq!(
        header_lines[1].spans[0].style.fg,
        Some(style::palette::danger())
    );
    assert!(
        header_lines[1].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn session_header_keeps_in_progress_highlight_in_every_status() {
    for status in Status::ALL {
        // Arrange
        let mut session = SessionFixtureBuilder::new().build();
        session.status = status;
        session.title = Some("Consistent session title".to_string());

        // Act
        let header = session_header_lines(&session, 80, false);

        // Assert
        for span in &header[0].spans[..2] {
            assert_eq!(
                span.style.fg,
                Some(style::palette::warning()),
                "title and size should keep the in-progress color for {status:?}"
            );
            assert!(span.style.add_modifier.contains(Modifier::BOLD));
        }
        assert!(header[0].to_string().contains("Consistent session title"));
    }
}

#[test]
fn test_session_header_shows_colored_totals_without_composer_fields() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.agent = model_fixture::codex_selection();
    session.stats.added_lines = 12;
    session.stats.deleted_lines = 3;

    // Act
    let header = session_header_lines(&session, 160, false);
    let title = &header[0];

    // Assert
    assert_eq!(header.len(), 1);
    assert_eq!(title.width(), 160);
    assert!(title.to_string().ends_with("(+12 -3)"));
    for (text, color) in [
        ("(", style::palette::warning()),
        ("+12", style::palette::success()),
        ("-3", style::palette::danger()),
        (")", style::palette::warning()),
    ] {
        let span = title
            .spans
            .iter()
            .find(|span| span.content == text)
            .expect("change total span should be present");
        assert_eq!(span.style.fg, Some(color));
    }
    for field in [
        "Agent:",
        "Model:",
        "Size:",
        "Style:",
        "Speed:",
        "Reasoning:",
        "Timer:",
        "Tokens:",
    ] {
        assert!(!title.to_string().contains(field));
    }
}

#[test]
fn test_session_header_keeps_speed_in_prompt_only() {
    for speed in [
        crate::domain::agent::SpeedMode::Normal,
        crate::domain::agent::SpeedMode::Fast,
    ] {
        // Arrange
        let mut session = SessionFixtureBuilder::new().build();
        session.agent = model_fixture::codex_selection();
        session.speed_mode = speed;

        // Act
        let header = session_header_lines(&session, 160, false);
        let prompt_status = prompt_session_status(&session);

        // Assert
        assert!(header[0].to_string().ends_with("(+0 -0)"));
        assert!(!header[0].to_string().contains("Speed:"));
        assert_eq!(
            prompt_status,
            format!("Balanced · {} · Auto Edit", speed.name())
        );
    }
}

#[test]
fn test_session_header_omits_speed_for_provider_without_speed_control() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Gemini,
        AgentModel::Gemini31Pro,
    );
    session.speed_mode = crate::domain::agent::SpeedMode::Fast;

    // Act
    let header = session_header_lines(&session, 160, false);

    // Assert
    assert!(header[0].to_string().ends_with("(+0 -0)"));
    assert!(!header[0].to_string().contains("Speed:"));
}

#[test]
fn test_prompt_status_shows_every_response_style() {
    for (response_style, label) in [
        (ResponseStyle::Concise, "Concise"),
        (ResponseStyle::Balanced, "Balanced"),
        (ResponseStyle::Detailed, "Detailed"),
    ] {
        // Arrange
        let mut session = SessionFixtureBuilder::new().build();
        session.response_style = response_style;

        // Act
        let prompt_status_without_speed = prompt_session_status(&session);
        session.agent = model_fixture::codex_selection();
        let prompt_status = prompt_session_status(&session);

        // Assert
        assert_eq!(prompt_status_without_speed, format!("{label} · Auto Edit"));
        assert_eq!(prompt_status, format!("{label} · Normal · Auto Edit"));
    }
}

#[test]
fn header_preserves_totals_at_constrained_widths() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.agent = model_fixture::codex_selection();
    session.stats.input_tokens = 123;
    session.stats.output_tokens = 456;
    session.title = Some("修复 **wide title** with a long description".to_string());

    for width in 0..=200 {
        // Act
        let header = session_header_lines(&session, width, false);

        // Assert
        assert_eq!(header.len(), 1);
        assert!(header[0].width() <= usize::from(width));
        if width >= 7 {
            assert!(header[0].to_string().ends_with("(+0 -0)"));
            assert_eq!(header[0].width(), usize::from(width));
        }
    }
}

#[test]
fn test_session_speed_display_reports_speed_only_for_supported_provider() {
    // Arrange
    let mut codex_session = SessionFixtureBuilder::new().build();
    codex_session.agent = model_fixture::codex_selection();
    let mut antigravity_session = SessionFixtureBuilder::new().build();
    antigravity_session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Antigravity,
        AgentModel::Gemini31Pro,
    );

    // Act
    let codex_speed = session_speed_display(&codex_session);
    let antigravity_speed = session_speed_display(&antigravity_session);

    // Assert
    assert_eq!(codex_speed, Some("Normal"));
    assert_eq!(antigravity_speed, None);
}

#[test]
fn test_session_output_uses_tachyon_loader_for_animated_statuses() {
    // Arrange
    let animated_statuses = [
        Status::InProgress,
        Status::AgentReview,
        Status::Rebasing,
        Status::Merging,
        Status::Merged,
    ];
    let static_statuses = [
        Status::Draft,
        Status::Review,
        Status::Question,
        Status::Queued,
        Status::Done,
        Status::Canceled,
    ];

    // Act
    let animated_results = animated_statuses.map(session_output_uses_tachyon_loader);
    let static_results = static_statuses.map(session_output_uses_tachyon_loader);

    // Assert
    assert!(animated_results.into_iter().all(|uses_loader| uses_loader));
    assert!(static_results.into_iter().all(|uses_loader| !uses_loader));
}

#[test]
fn merged_session_output_explains_manual_sync_wait() {
    // Arrange
    let status = Status::Merged;

    // Act
    let message = session_output_status_message(status, None, None, None);
    let icon = session_output_status_icon(status);

    // Assert
    assert_eq!(message, "Waiting for manual local target sync...");
    assert!(matches!(icon, Icon::TachyonLoader));
}

#[test]
fn test_session_started_label_formats_age() {
    // Arrange
    let created_at = 1_782_864_000;
    let utc_offset_seconds = 2 * 3_600;

    // Act
    let just_started = session_started_label(created_at, FrameTime::new(created_at + 59, 0, 0));
    let minutes = session_started_label(created_at, FrameTime::new(created_at + 12 * 60, 0, 0));
    let hours = session_started_label(
        created_at,
        FrameTime::new(created_at + 3 * 3_600 + 5 * 60, 0, utc_offset_seconds),
    );
    let days = session_started_label(
        created_at,
        FrameTime::new(created_at + 2 * 86_400 + 4 * 3_600 + 59, 0, 0),
    );
    let future = session_started_label(created_at, FrameTime::new(created_at - 600, 0, 0));

    // Assert
    assert_eq!(just_started, "Started just now");
    assert_eq!(minutes, "Started 12m ago");
    assert_eq!(hours, "Started 3h 5m ago");
    assert_eq!(days, "Started 2d 4h ago");
    assert_eq!(future, "Started just now");
}

#[test]
fn test_session_started_label_is_independent_of_daylight_saving_offsets() {
    // Arrange
    let created_at = 1_792_843_200;
    let now = created_at + 2 * 86_400;

    // Act
    let winter = session_started_label(created_at, FrameTime::new(now, 0, 3_600));
    let summer = session_started_label(created_at, FrameTime::new(now, 0, 2 * 3_600));

    // Assert
    assert_eq!(winter, "Started 2d 0h ago");
    assert_eq!(summer, winter);
}

#[test]
fn test_session_started_label_handles_extreme_timestamps_and_offsets() {
    // Arrange
    let frame_time = FrameTime::new(1_782_864_000, 0, i64::MAX);

    // Act
    let invalid_offset = session_started_label(1_782_864_000 - 600, frame_time);
    let invalid_timestamp = session_started_label(i64::MIN, FrameTime::new(0, 0, 0));

    // Assert
    assert_eq!(invalid_offset, "Started 10m ago");
    assert!(invalid_timestamp.starts_with("Started "));
    assert!(invalid_timestamp.ends_with("h ago"));
}
