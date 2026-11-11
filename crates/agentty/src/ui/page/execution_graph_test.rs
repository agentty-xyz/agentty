use ag_protocol::{ProtocolRequestProfile, QuestionItem};
use ag_session::{
    ExecutionGraphStatus, GraphBlockedReason, GraphCompletionGate, GraphDependency,
    GraphNodeStatus, GraphWorkspaceAccess,
};
use ag_tui_text::text_util::wrap_lines_breaking_long_words;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::CellWidth;
use ratatui::style::Modifier;

use super::{actions, details, gate_label, render, render_details};
use crate::domain::input::InputState;
use crate::presentation::execution_graph::tests::graph_view;
use crate::presentation::execution_graph::{GraphEditKind, GraphEditor, GraphViewOutput};
use crate::ui::markdown::MarkdownRenderCache;

#[test]
fn proposal_inspector_renders_scrollable_details_and_available_actions() {
    // Arrange
    let mut view = graph_view();
    let mut proposal = ag_session::test_support::graph_proposal_state("preview", "sess");
    proposal.version.outcome = ag_session::GraphProposalVersionOutcome::Generated {
        launch: Some(Box::new(ag_session::StartExecutionGraphRequest {
            graph: view.graphs[0].clone(),
            provider_call_limits: std::collections::BTreeMap::default(),
        })),
        response: ag_protocol::GraphProposalResponse {
            outcome: ag_protocol::GraphProposalOutcome::Proposal {
                proposal: ag_session::test_support::graph_proposal(),
            },
        },
    };
    view.proposal = Some(proposal);
    view.progress = None;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    let markdown = MarkdownRenderCache::default();

    // Act
    terminal
        .draw(|frame| render(frame, frame.area(), &view, &markdown))
        .expect("render");

    // Assert
    let rendered = (0..30)
        .map(|row| {
            (1..99)
                .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let buffer = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(buffer.contains("Proposal preview v1"));
    let hint = "reply in chat to approve, revise, or cancel";
    assert!(buffer.contains(hint), "{buffer}");
    assert!(!buffer.contains("approve displayed version"), "{buffer}");
    assert!(details(&view).contains("Captured instructions"));
    assert_eq!(
        actions(&view),
        [
            "q back",
            "[/] execution graphs",
            "r refresh",
            "J/K scroll",
            hint
        ]
    );
    view.proposal
        .as_mut()
        .expect("proposal")
        .version
        .is_cancelled = true;
    assert!(!actions(&view).contains(&hint));
    view.is_proposal_selected = false;
    assert!(actions(&view).contains(&"p proposal"));
}

#[test]
fn nodes_awaiting_answers_point_to_chat_instead_of_an_editor() {
    // Arrange
    let mut view = graph_view();
    let node = &mut view.progress.as_mut().expect("progress").nodes[0];
    node.status = GraphNodeStatus::AwaitingAnswers;
    node.questions = vec![QuestionItem::new("Which target?")];

    // Act
    let awaiting = actions(&view);
    view.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Succeeded;
    let settled = actions(&view);

    // Assert
    assert!(
        awaiting.contains(&"reply in chat to answer"),
        "{awaiting:?}"
    );
    assert!(!awaiting.contains(&"e reply/revise"), "{awaiting:?}");
    assert!(!settled.contains(&"reply in chat to answer"), "{settled:?}");
    assert!(settled.contains(&"e reply/revise"), "{settled:?}");
}

#[test]
fn output_markdown_wraps_once_and_scrolls_styled_rendered_lines() {
    // Arrange
    let mut view = graph_view();
    let result = view.outputs.remove(0).result;
    view.outputs.push(GraphViewOutput::new(result, r###"{"answer":"## Readable heading\n\n**Bold answer**\n\n- A list item with enough words to wrap in the narrow pane and remain visible.\n\n```rust\n    let ready = true;\n```"}"###.into()));
    let markdown = MarkdownRenderCache::default();

    // Act / Assert
    for width in [80, 28] {
        let mut terminal = Terminal::new(TestBackend::new(width + 2, 8)).expect("terminal");
        let header_lines =
            wrap_lines_breaking_long_words(&details(&view), usize::from(width)).len();
        let output_lines = markdown.render(view.outputs[0].display(), usize::from(width));
        let mut saw_bold = false;
        let mut saw_code = false;
        for (offset, expected) in output_lines.iter().enumerate() {
            view.scroll = u16::try_from(header_lines + offset).expect("offset");
            terminal
                .draw(|frame| render_details(frame, frame.area(), &view, &markdown))
                .expect("render output");
            let buffer = terminal.backend().buffer();
            let line: String = (1..=width)
                .map(|column| buffer[(column, 1)].symbol())
                .collect();
            assert_eq!(
                line.trim_end(),
                expected.to_string().trim_end(),
                "width {width}, offset {offset}"
            );
            saw_bold |=
                (1..=width).any(|column| buffer[(column, 1)].modifier.contains(Modifier::BOLD));
            saw_code |= line.contains("let ready = true;");
            assert!(!line.contains(r#"{"answer""#));
            assert!(!line.contains(r"\n"));
        }
        assert!(saw_bold, "Markdown styles must reach the terminal");
        assert!(saw_code, "code must remain reachable by scrolling");
    }
}

#[test]
fn long_metadata_tokens_remain_complete_when_scrolling_narrow_panes() {
    // Arrange
    let mut view = graph_view();
    let objective = "crates/agentty/src/ui/page/a_very_long_objective_path.rs";
    let error = "https://example.test/a/very/long/error/reference/ERROR_SUFFIX";
    let question = "界界界界界界界界界界界界界界界界界界QUESTION_SUFFIX";
    view.graphs[0].nodes[0].objective = objective.into();
    let progress = &mut view.progress.as_mut().expect("progress").nodes[0];
    progress.last_error = Some(error.into());
    progress.questions = vec![QuestionItem::new(question)];
    view.outputs.clear();
    let markdown = MarkdownRenderCache::default();

    for width in [16, 23] {
        let mut terminal = Terminal::new(TestBackend::new(width + 2, 5)).expect("terminal");
        let metadata = details(&view);
        let line_count = wrap_lines_breaking_long_words(&metadata, usize::from(width)).len();
        let mut scrolled_text = String::new();

        // Act
        for offset in 0..line_count {
            view.scroll = u16::try_from(offset).expect("offset");
            terminal
                .draw(|frame| render_details(frame, frame.area(), &view, &markdown))
                .expect("render metadata");
            let buffer = terminal.backend().buffer();
            let mut line = String::new();
            let mut column = 1;
            while column <= width {
                let symbol = buffer[(column, 1)].symbol();
                line.push_str(symbol);
                column += symbol.cell_width().max(1);
            }
            scrolled_text.push_str(line.trim_end());
        }

        // Assert
        for token in [objective, error, question] {
            assert!(
                scrolled_text.contains(token),
                "width {width}: {scrolled_text}"
            );
        }
    }
}

#[test]
fn long_answer_tokens_remain_complete_when_scrolling_narrow_panes() {
    // Arrange
    let markdown = MarkdownRenderCache::default();
    let cases = [
        (
            "",
            "crates/agentty/src/ui/page/a_very_long_answer_path/ANSWER_SUFFIX",
            "",
        ),
        (
            "`",
            "https://example.test/a/very/long/answer/reference/INLINE_SUFFIX",
            "`",
        ),
        (
            "**",
            "界界e\u{301}👩‍💻界界e\u{301}👩‍💻界界e\u{301}👩‍💻UNICODE_SUFFIX",
            "**",
        ),
        ("```text\n", "a👩‍💻b👩‍💻CODE_SUFFIX", "\n```"),
    ];

    for (prefix, token, suffix) in cases {
        for width in [3, 16, 23] {
            let mut view = graph_view();
            let result = view.outputs.remove(0).result;
            let body = serde_json::json!({ "answer": format!("{prefix}{token}{suffix}") });
            view.outputs
                .push(GraphViewOutput::new(result, body.to_string()));
            let header_lines =
                wrap_lines_breaking_long_words(&details(&view), usize::from(width)).len();
            let mut terminal = Terminal::new(TestBackend::new(width + 2, 5)).expect("terminal");
            let mut scrolled_text = String::new();

            // Act
            for offset in 0..token.chars().count() + 8 {
                view.scroll = u16::try_from(header_lines + offset).expect("offset");
                terminal
                    .draw(|frame| render_details(frame, frame.area(), &view, &markdown))
                    .expect("render answer");
                let buffer = terminal.backend().buffer();
                let mut line = String::new();
                let mut column = 1;
                while column <= width {
                    let symbol = buffer[(column, 1)].symbol();
                    assert!(
                        !symbol.contains('👩') || symbol == "👩‍💻",
                        "partial joined emoji: {symbol:?}"
                    );
                    line.push_str(symbol);
                    column += symbol.cell_width().max(1);
                }
                scrolled_text.push_str(line.trim_end());
                if scrolled_text.contains(token) {
                    break;
                }
            }

            // Assert
            assert!(
                scrolled_text.contains(token),
                "width {width}, prefix {prefix}: {scrolled_text}"
            );
        }
    }
}

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
    let text = details(&view);

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
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    for kind in [
        None,
        Some(GraphEditKind::Reply),
        Some(GraphEditKind::Cancel),
        Some(GraphEditKind::Resume),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
        view.editor = kind.map(|kind| GraphEditor {
            input: InputState::with_text("Graph input text".into()),
            kind,
            needs_reconciliation: matches!(kind, GraphEditKind::Reply),
        });
        terminal
            .draw(|frame| render(frame, frame.area(), &view, &MarkdownRenderCache::default()))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let rendered: String = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(rendered.contains("Execution graphs"));
        assert!(rendered.contains("Exact output body"));
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
        let text = details(&view);
        if scenario == "focused" {
            assert!(text.contains("new generation"));
        }
        if scenario == "no-output" {
            assert!(!text.contains("Output 1/"));
        }
        for (width, height) in [(80, 24), (30, 12), (5, 3)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
            terminal
                .draw(|frame| render(frame, frame.area(), &view, &MarkdownRenderCache::default()))
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
                .draw(|frame| render(frame, frame.area(), &view, &MarkdownRenderCache::default()))
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
            .draw(|frame| render(frame, frame.area(), &view, &MarkdownRenderCache::default()))
            .expect("small render");
    }
}
