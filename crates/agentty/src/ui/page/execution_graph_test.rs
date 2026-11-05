use ag_protocol::{ProtocolRequestProfile, QuestionItem};
use ag_session::{
    ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate, GraphDependency,
    GraphNodeStatus, GraphWorkspaceAccess,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::{details, gate_label, render};
use crate::domain::input::InputState;
use crate::presentation::execution_graph::tests::graph_view;
use crate::presentation::execution_graph::{GraphEditKind, GraphEditor};

#[test]
fn inspector_renders_blocks_exact_output_and_supported_controls() {
    // Arrange
    let mut view = graph_view();
    view.graphs[0].nodes[0].dependencies = vec![GraphDependency {
        node_id: "upstream".into(),
        gate: GraphCompletionGate::ResultValidated {
            output_name: "data".into(),
        },
    }];
    let node = &mut view.progress.as_mut().expect("progress").nodes[0];
    node.blocked = vec![
        GraphBlockedReason::Dependency {
            gate: GraphCompletionGate::ResultAccepted {
                output_name: "plan".into(),
            },
            node_id: "planner".into(),
        },
        GraphBlockedReason::EnvironmentBusy,
        GraphBlockedReason::AwaitingDispatch,
        GraphBlockedReason::BudgetExhausted("calls".into()),
        GraphBlockedReason::ResumeRequired,
    ];
    node.questions = vec![QuestionItem::new("Which target?")];
    node.last_error = Some("Previous failure".into());

    // Act
    let text = details(&view).to_string();

    // Assert
    assert_eq!(
        gate_label(&GraphCompletionGate::ExecutionSucceeded),
        "successful execution without questions"
    );
    for expected in [
        "Inspect first",
        "validated data",
        "planner: accepted plan",
        "Workspace is busy",
        "Waiting for worker capacity",
        "Provider-call budget exhausted: calls",
        "resume required",
        "Previous failure",
        "Which target?",
        "response v1 (Validated)",
        "Exact output body",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    for kind in [
        None,
        Some(GraphEditKind::Reply),
        Some(GraphEditKind::Answers),
        Some(GraphEditKind::Cancel),
        Some(GraphEditKind::Resume),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
        view.editor = kind.map(|kind| GraphEditor {
            answers: Vec::new(),
            input: InputState::with_text("Graph input text".into()),
            kind,
            needs_reconciliation: matches!(kind, GraphEditKind::Reply),
        });
        terminal
            .draw(|frame| render(frame, frame.area(), &view))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let rendered: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("Execution graphs"));
        assert!(rendered.contains(if kind.is_some() {
            "Graph input"
        } else {
            "a accept displayed version"
        }));
        if kind == Some(GraphEditKind::Reply) {
            assert!(rendered.contains("Ctrl+r reconcile draft"));
        }
    }
}

#[test]
fn inspector_renders_empty_failed_and_host_coordinated_graphs_at_small_sizes() {
    // Arrange
    let original = graph_view();

    // Act / Assert
    for scenario in [
        "none",
        "no-progress",
        "no-output",
        "node-missing",
        "failed",
        "focused",
    ] {
        let mut view = original.clone();
        match scenario {
            "none" => {
                view.graphs.clear();
                view.progress = None;
            }
            "no-progress" => view.progress = None,
            "no-output" => view.outputs.clear(),
            "node-missing" => view.selected_node = 9,
            "failed" => {
                view.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Failed;
                view.progress.as_mut().expect("progress").status = ExecutionGraphStatus::Failed;
            }
            _ => {
                view.graphs[0].nodes[0].execution.response_profile =
                    ProtocolRequestProfile::FocusedReview;
                view.graphs[0].nodes[0].execution.workspace_access =
                    GraphWorkspaceAccess::BackgroundRead;
            }
        }
        let text = details(&view).to_string();
        if scenario == "focused" {
            assert!(text.contains("new generation"));
        }
        if scenario == "no-output" {
            assert!(!text.contains("Exact output body"));
        }
        for (width, height) in [(80, 24), (30, 12), (5, 3)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            terminal
                .draw(|frame| render(frame, frame.area(), &view))
                .expect("small render");
            if scenario == "no-progress" && width == 80 {
                let rendered: String = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect();
                assert!(rendered.contains("Unavailable"));
            }
        }
    }
}

#[test]
fn editor_notices_wrap_and_scroll_without_hiding_the_draft() {
    // Arrange
    let mut view = graph_view();
    view.editor = Some(GraphEditor {
        answers: Vec::new(),
        input: InputState::with_text("Preserved draft".into()),
        kind: GraphEditKind::Reply,
        needs_reconciliation: true,
    });
    view.notice = format!(
        "Draft retained. Permission denied. {} FINAL_REASON. Ctrl+r to reconcile; Esc to discard.",
        "The persisted execution can no longer accept this revision. ".repeat(8)
    );

    // Act / Assert
    for (width, height) in [(80, 24), (120, 40), (30, 18)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        let mut reached_end = false;
        for offset in 0..40 {
            view.notice_scroll = offset;
            terminal
                .draw(|frame| render(frame, frame.area(), &view))
                .expect("render");
            let rendered: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect();
            assert!(rendered.contains("Preserved draft"));
            if offset == 0 {
                assert!(rendered.contains("Permission"));
                assert!(rendered.contains("denied."));
            }
            reached_end |= rendered.contains("FINAL_REASON");
        }
        assert!(
            reached_end,
            "complete notice should be reachable at width {width}"
        );
    }
    for (width, height) in [(5, 3), (0, 0)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| render(frame, frame.area(), &view))
            .expect("small render");
    }
}
