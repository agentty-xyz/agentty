//! Pure routing decisions for ordinary session messages.
//!
//! Plans select an existing workflow, not a new lifecycle transition. Payload
//! validation, preparation, persistence, and execution-time eligibility remain
//! with the adapters and workflows that already own them.

use ag_session::SessionStatus;

use crate::app::prompt_intent::PromptSessionMode;

/// The workflow selected from submission intent and foreground session state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MessagePlan {
    /// Save a terminal draft for an explicit start later.
    Stage,
    /// Save an API draft and start it only after staging succeeds.
    StageAndStart,
    /// Submit the first prompt of a regular session.
    Start,
    /// Append ordinary chat behind the active turn or rebase.
    Queue,
    /// Attempt a follow-up through the existing reply workflow.
    Resume,
}

impl MessagePlan {
    /// Preserves composer intent even when the session is missing or its status
    /// has changed. The selected workflow retains responsibility for rejection.
    pub(super) fn for_prompt(mode: PromptSessionMode, status: Option<SessionStatus>) -> Self {
        match mode {
            PromptSessionMode::NewDraft => Self::Stage,
            PromptSessionMode::NewDeletable | PromptSessionMode::NewRegular => Self::Start,
            PromptSessionMode::Existing => Self::for_existing(status),
        }
    }

    /// Routes a validated API message, retaining automatic draft startup.
    pub(super) fn for_api(status: SessionStatus, is_draft: bool) -> Self {
        if status == SessionStatus::Draft {
            return if is_draft {
                Self::StageAndStart
            } else {
                Self::Start
            };
        }

        Self::for_existing(Some(status))
    }

    /// Defers non-busy eligibility to reply handling, including preparation
    /// prompts that may be accepted before the first worker turn.
    fn for_existing(status: Option<SessionStatus>) -> Self {
        if matches!(
            status,
            Some(SessionStatus::InProgress | SessionStatus::Rebasing)
        ) {
            Self::Queue
        } else {
            Self::Resume
        }
    }
}

#[cfg(test)]
#[path = "message_admission_test.rs"]
mod tests;
