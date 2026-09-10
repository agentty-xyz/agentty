use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use super::{OrchestrationPage, campaign_board_height, campaign_page_areas};
use crate::domain::agent::ReasoningLevel;
use crate::domain::session::{SessionRole, Status};
use crate::presentation::app_mode::AppMode;
use crate::presentation::frame_time::FrameTime;
use crate::ui::Page;
use crate::ui::component::session_output::SessionOutputLayoutCache;
use crate::ui::markdown::MarkdownRenderCache;
use crate::ui::page::session_chat::SessionChatPageInput;

fn render_campaign(progress: Option<&str>) -> String {
    let mut session = crate::test_support::SessionFixtureBuilder::new()
        .role(SessionRole::Orchestrator)
        .status(Status::Review)
        .build();
    session.orchestration_progress = progress.map(str::to_string);
    let sessions = [session];
    let mode = AppMode::View {
        scroll_offset: None,
        session_id: sessions[0].id.clone(),
    };
    let markdown_render_cache = MarkdownRenderCache::default();
    let output_layout_cache = SessionOutputLayoutCache::default();
    let input = SessionChatPageInput {
        active_prompt_output: None,
        active_progress: None,
        resources: None,
        host_cpu_temperature_celsius: None,
        default_reasoning_level: ReasoningLevel::default(),
        frame_time: FrameTime::new(0, 0, 0),
        has_merge_conflict: false,
        markdown_render_cache: &markdown_render_cache,
        mode: &mode,
        output_layout_cache: &output_layout_cache,
        review_text: None,
        scroll_offset: None,
        session_index: 0,
        session_update_version: 0,
        sessions: &sessions,
    };
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| {
            OrchestrationPage::new(input)
                .can_open_worktree(true)
                .render(frame, frame.area());
        })
        .expect("failed to render campaign");

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn campaign_board_height_has_readable_minimum() {
    // Arrange, Act
    let height = campaign_board_height(None);

    // Assert
    assert_eq!(height, 6);
}

#[test]
fn campaign_board_height_is_bounded_for_large_campaigns() {
    // Arrange
    let progress = (0..20)
        .map(|index| format!("task-{index}"))
        .collect::<Vec<_>>()
        .join("\n");

    // Act
    let height = campaign_board_height(Some(&progress));

    // Assert
    assert_eq!(height, 12);
}

#[test]
fn campaign_page_areas_reserve_the_board_above_chat() {
    // Arrange
    let page_area = Rect::new(2, 3, 80, 24);
    let progress = "Phase: Running\n1. api\n2. ui\n3. docs";

    // Act
    let [board_area, chat_area] = campaign_page_areas(page_area, Some(progress));

    // Assert
    assert_eq!(board_area, Rect::new(2, 3, 80, 8));
    assert_eq!(chat_area, Rect::new(2, 11, 80, 16));
}

#[test]
fn campaign_page_renders_planning_approval_and_integration_boards() {
    // Arrange
    let cases = [
        (None, "Discuss the goal"),
        (
            Some("Phase: AwaitingApproval\nParallel workers: 3 (global setting)"),
            "a approve  Enter discuss/revise",
        ),
        (
            Some("Phase: AwaitingIntegration\n1. api - ready"),
            "a approve integration",
        ),
    ];

    // Act
    let rendered = cases
        .iter()
        .map(|(progress, _)| render_campaign(*progress))
        .collect::<Vec<_>>();

    // Assert
    for ((_, expected), frame) in cases.iter().zip(rendered) {
        assert!(frame.contains(expected));
        assert!(frame.contains("Campaign:"));
    }
}
