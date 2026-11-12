use ag_protocol::QuestionItem;
use ag_session::{
    ExecutionGraphStatus, GraphNodeContinuation, GraphNodeStatus, GraphProposalVersionOutcome,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{GraphKeyAction, open_editor, resolve_editor_key, resolve_key};
use crate::app::graph_view::GraphViewAction;
use crate::domain::input::InputState;
use crate::presentation::execution_graph::tests::graph_view;
use crate::presentation::execution_graph::{GraphEditKind, GraphView};

fn key(view: &mut GraphView, code: KeyCode) -> GraphKeyAction {
    resolve_key(view, KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn proposal_selection_is_read_only_and_ignores_former_action_keys() {
    // Arrange
    let mut view = graph_view();
    let mut proposal = ag_session::test_support::graph_proposal_state("preview", "sess");
    proposal.version.outcome = GraphProposalVersionOutcome::Generated {
        launch: None,
        response: ag_protocol::GraphProposalResponse {
            outcome: ag_protocol::GraphProposalOutcome::Clarification {
                questions: vec![QuestionItem::new("Target?")],
            },
        },
    };
    view.proposal = Some(proposal);
    view.progress = None;
    let inspected = view.clone();

    // Act
    let actions: Vec<GraphKeyAction> = ['a', 'g', 'n', 'x', 'e', 't', 's']
        .into_iter()
        .map(|code| key(&mut view, KeyCode::Char(code)))
        .collect();
    key(&mut view, KeyCode::Char(']'));
    let deselected = view.is_proposal_selected;
    let reselected = key(&mut view, KeyCode::Char('p'));

    // Assert
    assert!(
        actions
            .iter()
            .all(|action| matches!(action, GraphKeyAction::None))
    );
    assert!(inspected.graph().is_none());
    assert!(!deselected);
    assert!(matches!(reselected, GraphKeyAction::Refresh));
    assert!(view.is_proposal_selected);
    assert!(view.editor.is_none());
    assert!(!view.can_reconcile_editor(&view.clone()));
}

#[test]
fn editor_submission_without_an_active_editor_preserves_the_snapshot() {
    // Arrange
    let mut view = graph_view();
    let original = view.clone();

    // Act
    let action = resolve_editor_key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // Assert
    assert!(matches!(action, GraphKeyAction::None));
    assert_eq!(view, original);
}

#[test]
fn navigation_selects_graph_node_and_output_without_editing_chat() {
    // Arrange
    let mut view = graph_view();
    view.graphs.push(view.graphs[0].clone());
    view.outputs.push(view.outputs[0].clone());

    // Act / Assert
    assert!(matches!(
        resolve_key(
            &mut view,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        ),
        GraphKeyAction::None
    ));
    for (code, graph_index, node_index) in [
        (KeyCode::Char(']'), 1, 0),
        (KeyCode::Char('['), 0, 0),
        (KeyCode::Char('['), 1, 0),
        (KeyCode::Char(']'), 0, 0),
        (KeyCode::Char('j'), 0, 1),
        (KeyCode::Char('j'), 0, 0),
        (KeyCode::Up, 0, 1),
        (KeyCode::Down, 0, 0),
    ] {
        assert!(matches!(key(&mut view, code), GraphKeyAction::Refresh));
        assert_eq!(
            (view.selected_graph, view.selected_node),
            (graph_index, node_index)
        );
    }
    key(&mut view, KeyCode::Char('o'));
    assert_eq!(view.selected_output, 1);
    key(&mut view, KeyCode::PageDown);
    assert_eq!(view.scroll, 5);
    key(&mut view, KeyCode::PageUp);
    assert_eq!(view.scroll, 0);
    key(&mut view, KeyCode::Char('J'));
    key(&mut view, KeyCode::Home);
    assert_eq!(view.scroll, 0);
    assert!(matches!(
        key(&mut view, KeyCode::Char('r')),
        GraphKeyAction::Refresh
    ));
    assert!(matches!(
        key(&mut view, KeyCode::Char('?')),
        GraphKeyAction::None
    ));
    view.graphs.clear();
    view.progress = None;
    view.outputs.clear();
    for code in ['[', ']', 'j', 'k', 'o', 'a', 't', 'e', 'n', 'x', 's'] {
        assert!(matches!(
            key(&mut view, KeyCode::Char(code)),
            GraphKeyAction::None
        ));
    }
}

#[test]
fn notice_scrolling_keeps_editor_input_and_bindings_unchanged() {
    // Arrange
    let mut view = graph_view();
    open_editor(&mut view, GraphEditKind::Reply);
    view.editor.as_mut().expect("editor").input = InputState::with_text("Keep this draft".into());
    view.editor.as_mut().expect("editor").needs_reconciliation = true;
    let original = view.clone();

    // Act / Assert
    for (code, offset) in [
        (KeyCode::PageUp, 0),
        (KeyCode::PageDown, 1),
        (KeyCode::PageDown, 2),
        (KeyCode::PageUp, 1),
    ] {
        assert!(matches!(key(&mut view, code), GraphKeyAction::None));
        assert_eq!(view.notice_scroll, offset);
        assert_eq!(view.editor, original.editor);
        assert_eq!(view.progress, original.progress);
    }
    key(&mut view, KeyCode::Enter);
    assert_eq!(view.notice_scroll, 0);
    assert_eq!(view.editor, original.editor);

    // Act: rejected drafts remain editable without changing their bindings.
    key(&mut view, KeyCode::Char('!'));
    let edited = view.editor.clone().expect("editor");
    resolve_key(&mut view, KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE));

    // Assert
    assert_eq!(edited.input.text(), "Keep this draft!");
    assert!(edited.needs_reconciliation);
    assert_eq!(view.editor.as_ref(), Some(&edited));
    assert_eq!(view.progress, original.progress);
}

#[test]
fn actions_and_editors_keep_exact_node_intent() {
    // Arrange
    let mut view = graph_view();

    // Act / Assert
    assert!(matches!(
        key(&mut view, KeyCode::Char('a')),
        GraphKeyAction::Submit(GraphViewAction::Accept)
    ));
    view.progress.as_mut().expect("progress").nodes[0].status = GraphNodeStatus::Failed;
    assert!(matches!(
        key(&mut view, KeyCode::Char('t')),
        GraphKeyAction::Submit(GraphViewAction::Continue(GraphNodeContinuation::Retry))
    ));
    key(&mut view, KeyCode::Char('e'));
    assert_eq!(
        view.editor.as_ref().expect("editor").kind,
        GraphEditKind::Reply
    );
    key(&mut view, KeyCode::Enter);
    assert_eq!(view.notice, "Enter a reply");
    for character in "revise q".chars() {
        key(&mut view, KeyCode::Char(character));
    }
    resolve_key(&mut view, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    assert!(
        view.editor
            .as_ref()
            .expect("editor")
            .input
            .text()
            .ends_with('\n')
    );
    assert!(
        matches!(key(&mut view, KeyCode::Enter), GraphKeyAction::Submit(GraphViewAction::Continue(GraphNodeContinuation::Reply { message, attachments })) if message == "revise q" && attachments.is_empty())
    );
    key(&mut view, KeyCode::Esc);
    assert!(view.editor.is_none());
    key(&mut view, KeyCode::Char('x'));
    key(&mut view, KeyCode::Enter);
    assert!(view.notice.contains("Type cancel"));
    view.editor.as_mut().expect("editor").input = InputState::with_text("cancel".into());
    assert!(matches!(
        key(&mut view, KeyCode::Enter),
        GraphKeyAction::Submit(GraphViewAction::Cancel)
    ));
    key(&mut view, KeyCode::Esc);
    assert!(matches!(
        key(&mut view, KeyCode::Char('s')),
        GraphKeyAction::Submit(GraphViewAction::Resume)
    ));
    assert!(view.editor.is_none());
    view.progress.as_mut().expect("progress").status = ExecutionGraphStatus::Cancelled;
    assert!(matches!(
        key(&mut view, KeyCode::Char('s')),
        GraphKeyAction::None
    ));
}

#[test]
fn nodes_awaiting_answers_open_no_inspector_editor() {
    // Arrange
    let mut view = graph_view();
    let node = &mut view.progress.as_mut().expect("progress").nodes[0];
    node.status = GraphNodeStatus::AwaitingAnswers;
    node.questions = vec![QuestionItem::new("Which target?")];

    // Act
    let actions: Vec<GraphKeyAction> = ['n', 'e']
        .into_iter()
        .map(|code| key(&mut view, KeyCode::Char(code)))
        .collect();

    // Assert: chat replies answer the node's questions.
    assert!(
        actions
            .iter()
            .all(|action| matches!(action, GraphKeyAction::None))
    );
    assert!(!view.can_continue());
    assert!(view.editor.is_none());
}
