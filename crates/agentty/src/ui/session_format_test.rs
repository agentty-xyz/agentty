use ratatui::style::Modifier;

use super::{
    prompt_session_status, session_header_lines, session_metadata_text, session_output_status_icon,
    session_output_status_message, session_output_uses_tachyon_loader, session_resources_line,
    session_speed_display,
};
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::domain::resource::SessionResources;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, Session, SessionId,
    SessionRole, Status,
};
use crate::test_support::SessionFixtureBuilder;
use crate::ui::icon::Icon;
use crate::ui::style;

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
fn test_session_header_lines_keeps_review_request_url_on_same_line_if_it_fits() {
    // Arrange
    let session = session_with_review_request("https://github.com/agentty-xyz/agentty/pull/42");
    let header_width = 180;

    // Act
    let header_lines =
        session_header_lines(&session, header_width, ReasoningLevel::default(), 0, false);
    let metadata_line = header_lines[1].to_string();

    // Assert
    assert_eq!(header_lines.len(), 2);
    assert_eq!(metadata_line.chars().count(), usize::from(header_width));
    assert!(metadata_line.contains("Tokens: 0/0"));
    assert!(metadata_line.ends_with("https://github.com/agentty-xyz/agentty/pull/42"));
}

#[test]
fn test_session_header_lines_wraps_review_request_url_to_second_line_when_too_narrow() {
    // Arrange
    let session = session_with_review_request("https://example.test/pull/42");

    // Act
    let header_lines = session_header_lines(&session, 60, ReasoningLevel::default(), 0, false);
    let metadata_line = header_lines[1].to_string();
    let review_url_line = header_lines[2].to_string();

    // Assert
    assert_eq!(header_lines.len(), 3);
    assert!(metadata_line.contains("Size: XS"));
    assert!(review_url_line.starts_with("https://"));
    assert!(review_url_line.ends_with("https://example.test/pull/42"));
}

#[test]
fn test_session_header_lines_show_red_merge_conflict_alert() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.base_branch = "develop".to_string();

    // Act
    let header_lines = session_header_lines(&session, 100, ReasoningLevel::default(), 0, true);

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
fn test_session_metadata_text_omits_review_request_url() {
    // Arrange
    let session = session_with_review_request("https://example.test/pull/42");

    // Act
    let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);

    // Assert
    assert!(metadata_text.contains("Tokens: 0/0"));
    assert!(!metadata_text.contains("https://example.test/pull/42"));
}

#[test]
fn managed_session_header_identifies_its_controller() {
    // Arrange
    let mut session = SessionFixtureBuilder::new()
        .role(SessionRole::OrchestrationWorker)
        .build();
    session.controller_session_id = Some(SessionId::from("campaign-controller"));

    // Act
    let header_lines = session_header_lines(&session, 100, ReasoningLevel::default(), 0, false);

    // Assert
    assert!(
        header_lines[1]
            .to_string()
            .contains("Managed by campaign-controller — actions restricted")
    );
}

#[test]
fn test_session_metadata_text_prints_agent_before_model() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );

    // Act
    let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);

    // Assert
    assert!(metadata_text.contains("Agent: codex  Model: gpt-5.6-sol"));
}

#[test]
fn test_session_metadata_text_prints_speed_after_reasoning() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    session.speed_mode = crate::domain::agent::SpeedMode::Fast;

    // Act
    let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);

    // Assert
    assert!(metadata_text.contains("Reasoning: high  Speed: Fast  Tokens:"));
}

#[test]
fn test_session_metadata_text_omits_speed_for_provider_without_speed_control() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Gemini,
        AgentModel::Gemini31Pro,
    );
    session.speed_mode = crate::domain::agent::SpeedMode::Fast;

    // Act
    let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);

    // Assert
    assert!(metadata_text.contains("Reasoning: high  Tokens:"));
    assert!(!metadata_text.contains("Speed:"));
}

#[test]
fn test_session_metadata_and_prompt_status_show_non_default_response_style() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().build();
    session.response_style = crate::domain::agent::ResponseStyle::Detailed;

    // Act
    let prompt_status_without_speed = prompt_session_status(&session);
    session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
    let metadata_text = session_metadata_text(&session, 160, ReasoningLevel::default(), 0);
    let prompt_status = prompt_session_status(&session);

    // Assert
    assert!(metadata_text.contains("Style: Detailed"));
    assert_eq!(prompt_status_without_speed, "Detailed · Auto Edit");
    assert_eq!(prompt_status, "Detailed · Normal · Auto Edit");
}

#[test]
fn test_session_speed_display_reports_speed_only_for_supported_provider() {
    // Arrange
    let mut codex_session = SessionFixtureBuilder::new().build();
    codex_session.agent = crate::domain::agent::AgentSelection::new(
        crate::domain::agent::AgentKind::Codex,
        AgentModel::Gpt56Sol,
    );
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
