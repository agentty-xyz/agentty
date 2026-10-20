//! Terminal interaction state for explicitly accepted planning workflows.

use crate::domain::input::InputState;

/// An explicit user action admitted by the shared coordinator API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanWorkflowIntent {
    Accept,
    Cancel,
    Close,
    Refresh,
    Submit,
}

/// A rendered workflow snapshot; durable decisions remain in the coordinator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlanWorkflowItem {
    pub(crate) accept_plan: Option<(String, String)>,
    pub(crate) agent_label: String,
    pub(crate) body: String,
    pub(crate) can_cancel: bool,
    pub(crate) continuation: Option<PlanEditorKind>,
    pub(crate) current_attempt_id: String,
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) user_request: String,
}

/// Explicit intent displayed above the workflow's isolated composer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlanEditorKind {
    New,
    RevisePlan,
    AnswerPlanner,
    AnswerImplementer,
    RetryPlanner,
    RetryImplementer,
}

impl PlanEditorKind {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::New => "New plan · original request",
            Self::RevisePlan => "Revise plan · instructions for Planner",
            Self::AnswerPlanner => "Answer Planner questions",
            Self::AnswerImplementer => "Answer Implementer questions",
            Self::RetryPlanner => "Retry Planner · optional clarification",
            Self::RetryImplementer => "Retry Implementer · optional clarification",
        }
    }

    pub(crate) fn requires_text(self) -> bool {
        !matches!(self, Self::RetryPlanner | Self::RetryImplementer)
    }
}

/// Composer kept separate from the ordinary session prompt and history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlanWorkflowEditor {
    pub(crate) chain_id: Option<String>,
    pub(crate) expected_attempt_id: String,
    pub(crate) input: InputState,
    pub(crate) kind: PlanEditorKind,
}

/// Foreground-owned state for one session's planning panel.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlanWorkflowState {
    pub(crate) chains: Vec<PlanWorkflowItem>,
    pub(crate) editor: Option<PlanWorkflowEditor>,
    pub(crate) error: Option<String>,
    pub(crate) is_loading: bool,
    pub(crate) is_submitting: bool,
    pub(crate) request_id: String,
    pub(crate) scroll_offset: u16,
    pub(crate) selected_index: usize,
    pub(crate) update_version: u64,
}

impl PlanWorkflowState {
    pub(crate) fn selected(&self) -> Option<&PlanWorkflowItem> {
        self.chains.get(self.selected_index)
    }

    /// Retains selection by identity when a newly loaded snapshot arrives.
    pub(crate) fn replace_chains(&mut self, chains: Vec<PlanWorkflowItem>) {
        let selected = self.selected();
        let selected_index = selected
            .map(|chain| chain.id.as_str())
            .and_then(|id| chains.iter().position(|chain| chain.id == id))
            .unwrap_or_default();
        if selected.map(|chain| (&chain.accept_plan, &chain.current_attempt_id))
            != chains
                .get(selected_index)
                .map(|chain| (&chain.accept_plan, &chain.current_attempt_id))
        {
            self.scroll_offset = 0;
        }
        self.selected_index = selected_index;
        self.chains = chains;
    }

    pub(crate) fn begin_editing(&mut self, kind: PlanEditorKind) {
        let selected = self.selected().filter(|_| kind != PlanEditorKind::New);
        self.editor = Some(PlanWorkflowEditor {
            chain_id: selected.map(|chain| chain.id.clone()),
            expected_attempt_id: selected.map_or_default(|chain| chain.current_attempt_id.clone()),
            input: InputState::default(),
            kind,
        });
        self.error = None;
    }

    pub(crate) fn select_next(&mut self) {
        if !self.chains.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.chains.len();
            self.scroll_offset = 0;
            self.error = None;
        }
    }
}

#[cfg(test)]
#[path = "plan_workflow_test.rs"]
mod tests;
