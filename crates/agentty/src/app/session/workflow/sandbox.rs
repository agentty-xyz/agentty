//! Session notices for what a Harness turn reports about the native sandbox
//! its commands run in.

use crate::domain::session_message::{SessionMessage, SessionMessageKind};
use crate::domain::transcript_notice::TranscriptNotice;

/// Hint shown to the user after a turn in which the sandbox may have
/// blocked a command.
const LIKELY_DENIAL_HINT: &str =
    "A command may have been blocked by the sandbox. Session commands can write only the session \
     worktree, its Git metadata, and their temporary directory, and cannot read credential stores \
     such as `~/.ssh`. You can run commands that need more access outside the session.";

/// What one turn reported about the sandbox.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct TurnSandbox {
    /// Whether a command failed with a denial the sandbox may have caused.
    likely_denial: bool,
    /// The latest warning that commands run without the full sandbox.
    unavailable: Option<String>,
}

impl TurnSandbox {
    /// Records that the turn's commands ran without the full sandbox, for
    /// the reason `message`, fixed as `remedy` says.
    pub(super) fn observe_unavailable(&mut self, message: &str, remedy: &str) {
        self.unavailable =
            Some(TranscriptNotice::SandboxWarning.format(format_args!("{message} {remedy}")));
    }

    /// Records that a command failed with a denial the sandbox may have
    /// caused.
    pub(super) fn observe_likely_denial(&mut self) {
        self.likely_denial = true;
    }

    /// Returns the workflow notices to append after a turn whose session
    /// transcript holds `messages`: the warning once per session, or when it
    /// differs from the latest one shown, then the denial hint for this turn.
    pub(super) fn notices(&self, messages: &[SessionMessage]) -> Vec<String> {
        let shown = messages
            .iter()
            .rev()
            .filter(|message| message.kind == SessionMessageKind::WorkflowNotice)
            .map(|message| message.content.trim())
            .find(|content| content.starts_with(TranscriptNotice::SandboxWarning.prefix()));
        let warning = self
            .unavailable
            .clone()
            .filter(|warning| shown != Some(warning.trim()));
        let hint = self
            .likely_denial
            .then(|| TranscriptNotice::Sandbox.format(LIKELY_DENIAL_HINT));

        warning.into_iter().chain(hint).collect()
    }
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;
