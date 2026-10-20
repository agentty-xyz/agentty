use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Cell;

use crate::presentation::plan_workflow::{PlanEditorKind, PlanWorkflowItem, PlanWorkflowState};
use crate::ui::markdown::MarkdownRenderCache;
use crate::ui::page::plan_workflow::render;

#[test]
fn panel_renders_role_attribution_scroll_and_explicit_editor_intent() {
    // Arrange
    let mut state = PlanWorkflowState {
        chains: vec![PlanWorkflowItem {
            accept_plan: Some(("planner".into(), "attempt".into())),
            agent_label: "Captured model".into(),
            body: "# Planner\nInspect this plan.\n\n# Implementer\nWaiting for acceptance.".into(),
            can_cancel: true,
            continuation: Some(PlanEditorKind::RevisePlan),
            current_attempt_id: "attempt".into(),
            id: "workflow".into(),
            status: "Awaiting plan acceptance".into(),
            user_request: "Add search".into(),
        }],
        ..PlanWorkflowState::default()
    };
    let cache = MarkdownRenderCache::default();
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("terminal");

    for variant in 0..5 {
        if variant == 1 {
            state.begin_editing(PlanEditorKind::RevisePlan);
        }
        if variant == 2 {
            state.is_loading = true;
        }
        if variant == 3 {
            state.error = Some("Plan changed; inspect the latest version.".into());
        }
        if variant == 4 {
            state.scroll_offset = 1;
        }

        // Act
        terminal
            .draw(|frame| render(frame, frame.area(), &state, &cache))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let text = buffer
            .content()
            .iter()
            .map(Cell::symbol)
            .collect::<String>();

        // Assert
        assert!(text.contains("Plan then implement"));
        assert!(text.contains("Awaiting plan acceptance"));
        assert!(text.contains("Captured model"));
        assert!(text.contains("Inspect this plan."));
        assert!(text.contains("Implementer"));
        if variant > 0 {
            assert!(text.contains("Revise plan · instructions for Planner"));
        }
        if variant > 2 {
            assert!(text.contains("Plan changed; inspect the latest version."));
        }
    }
}

#[test]
fn empty_panel_handles_tiny_terminals_and_loading_without_a_composer() {
    // Arrange
    let cache = MarkdownRenderCache::default();
    for is_loading in [false, true] {
        let state = PlanWorkflowState {
            is_loading,
            ..PlanWorkflowState::default()
        };
        for (width, height) in [(120, 24), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");

            // Act
            terminal
                .draw(|frame| render(frame, frame.area(), &state, &cache))
                .expect("render");

            // Assert
            if width > 1 {
                let buffer = terminal.backend().buffer();
                let text = buffer
                    .content()
                    .iter()
                    .map(Cell::symbol)
                    .collect::<String>();
                assert!(text.contains("No workflows yet."));
                assert!(text.contains(if is_loading {
                    "Loading workflow"
                } else {
                    "n new"
                }));
            }
        }
    }
}

#[test]
fn failed_start_keeps_the_request_visible_with_actionable_workspace_guidance() {
    // Arrange
    let mut state = PlanWorkflowState::default();
    state.begin_editing(PlanEditorKind::New);
    state
        .editor
        .as_mut()
        .expect("editor")
        .input
        .insert_text("Add search without losing this request");
    state.error =
        Some("Workspace is not prepared. Create a Regular session to start planning.".into());
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("terminal");
    let cache = MarkdownRenderCache::default();

    // Act
    terminal
        .draw(|frame| render(frame, frame.area(), &state, &cache))
        .expect("render");
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(Cell::symbol)
        .collect::<String>();

    // Assert
    assert!(text.contains("Add search without losing this request"));
    assert!(
        text.contains("Workspace is not prepared. Create a Regular session to start planning.")
    );
}
