use crate::presentation::plan_workflow::{PlanEditorKind, PlanWorkflowItem, PlanWorkflowState};

fn item(id: &str) -> PlanWorkflowItem {
    PlanWorkflowItem {
        accept_plan: None,
        agent_label: "Captured model".into(),
        body: String::new(),
        can_cancel: false,
        continuation: None,
        id: id.into(),
        status: "Planning".into(),
        user_request: "Original request".into(),
    }
}

#[test]
fn workflow_selection_survives_reload_and_wraps_without_losing_editor() {
    // Arrange
    let mut state = PlanWorkflowState::default();
    state.select_next();
    assert!(state.selected().is_none());
    state.replace_chains(vec![item("first"), item("second")]);
    state.begin_editing(PlanEditorKind::RevisePlan);

    // Act
    state.select_next();
    state.replace_chains(vec![item("second"), item("first")]);

    // Assert
    assert_eq!(state.selected().expect("selection").id, "second");
    assert_eq!(
        state.editor.as_ref().expect("editor").kind,
        PlanEditorKind::RevisePlan
    );
    state.select_next();
    state.select_next();
    assert_eq!(state.selected_index, 0);
    state.replace_chains(vec![item("replacement")]);
    assert_eq!(state.selected_index, 0);
}

#[test]
fn editor_intents_distinguish_revision_answers_and_explicit_retries() {
    // Arrange
    let cases = [
        (PlanEditorKind::New, "New plan · original request", true),
        (
            PlanEditorKind::RevisePlan,
            "Revise plan · instructions for Planner",
            true,
        ),
        (
            PlanEditorKind::AnswerPlanner,
            "Answer Planner questions",
            true,
        ),
        (
            PlanEditorKind::AnswerImplementer,
            "Answer Implementer questions",
            true,
        ),
        (
            PlanEditorKind::RetryPlanner,
            "Retry Planner · optional clarification",
            false,
        ),
        (
            PlanEditorKind::RetryImplementer,
            "Retry Implementer · optional clarification",
            false,
        ),
    ];
    let mut state = PlanWorkflowState {
        error: Some("old failure".into()),
        ..PlanWorkflowState::default()
    };

    for (kind, title, requires_text) in cases {
        // Act
        state.begin_editing(kind);

        // Assert
        assert_eq!(kind.title(), title);
        assert_eq!(kind.requires_text(), requires_text);
        assert!(state.editor.as_ref().expect("editor").input.is_empty());
        assert!(state.error.is_none());
    }
}
