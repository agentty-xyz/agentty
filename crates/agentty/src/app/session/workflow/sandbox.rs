//! Session notices for what a Harness turn reports about the native sandbox
//! its commands run in, shown as soon as the turn reports them.

use std::sync::{Arc, Mutex};

use ag_contracts::{TurnEvent, TurnNotice};
use tokio::sync::mpsc;

use super::SessionTaskService;
use crate::app::AppEvent;
use crate::app::service::SessionUpdateVersionMap;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::infra::db::AppRepositories;

/// Hint shown to the user after a command the sandbox may have blocked.
const LIKELY_DENIAL_HINT: &str =
    "A command may have been blocked by the sandbox. Session commands can write only the session \
     worktree's files and their temporary directory, not its Git metadata, which Agentty commits \
     to after each turn, and cannot read credential stores such as `~/.ssh`. You can run commands \
     that need more access outside the session.";
/// Detail of the notice shown when the sandbox runs commands again after a
/// warning.
const RESTORED: &str = "Commands run in the sandbox again.";

/// The session whose transcript receives a turn's sandbox notices.
pub(super) struct SandboxNoticeTarget<'a> {
    pub(super) app_event_tx: &'a mpsc::UnboundedSender<AppEvent>,
    pub(super) db: &'a AppRepositories,
    pub(super) session_id: &'a str,
    pub(super) session_update_versions: &'a SessionUpdateVersionMap,
    pub(super) transcript: &'a Arc<Mutex<SessionTranscript>>,
}

impl SandboxNoticeTarget<'_> {
    /// Forwards `events` to `forward` until the turn's event stream ends,
    /// except sandbox notices, which it appends to the session transcript as
    /// they arrive, so the user learns that commands run without the sandbox
    /// before they run.
    pub(super) async fn relay(
        &self,
        mut events: mpsc::UnboundedReceiver<TurnEvent>,
        forward: mpsc::UnboundedSender<TurnEvent>,
    ) {
        let mut sandbox = SandboxNotices::default();
        while let Some(event) = events.recv().await {
            let TurnEvent::Notice(
                notice @ (TurnNotice::SandboxUnavailable { .. }
                | TurnNotice::SandboxAvailable
                | TurnNotice::LikelySandboxDenial),
            ) = event
            else {
                let _ = forward.send(event);

                continue;
            };
            let text = self
                .transcript
                .lock()
                .map_or_default(|transcript| sandbox.notice(&notice, transcript.messages()));
            if let Some(text) = text {
                SessionTaskService::append_workflow_notice(
                    self.transcript,
                    self.db,
                    self.app_event_tx,
                    self.session_update_versions,
                    self.session_id,
                    &text,
                )
                .await;
            }
        }
    }
}

/// Decides which sandbox notices one turn adds to its session.
#[derive(Debug, Default)]
pub(super) struct SandboxNotices {
    /// Whether the turn already showed the denial hint.
    hinted: bool,
}

impl SandboxNotices {
    /// Returns the workflow notice `notice` adds to a session whose
    /// transcript holds `messages`: a warning unless the session's latest
    /// sandbox state is the same warning, a note that the sandbox works again
    /// when that state is a warning, and the denial hint once per turn.
    pub(super) fn notice(
        &mut self,
        notice: &TurnNotice,
        messages: &[SessionMessage],
    ) -> Option<String> {
        let latest = Self::latest_state(messages);
        match notice {
            TurnNotice::SandboxUnavailable { message, remedy } => {
                let warning =
                    TranscriptNotice::SandboxWarning.format(format_args!("{message} {remedy}"));

                (latest != Some(warning.trim())).then_some(warning)
            }
            TurnNotice::SandboxAvailable => latest
                .is_some_and(Self::is_warning)
                .then(|| TranscriptNotice::Sandbox.format(RESTORED)),
            TurnNotice::LikelySandboxDenial => (!std::mem::replace(&mut self.hinted, true))
                .then(|| TranscriptNotice::Sandbox.format(LIKELY_DENIAL_HINT)),
            TurnNotice::ContextCompacted { .. } | TurnNotice::ContextCompactionFailed { .. } => {
                None
            }
        }
    }

    /// Returns the session's latest sandbox state notice, trimmed: a
    /// warning or the note that the sandbox works again.
    fn latest_state(messages: &[SessionMessage]) -> Option<&str> {
        let restored = TranscriptNotice::Sandbox.format_line(RESTORED);

        messages
            .iter()
            .rev()
            .filter(|message| message.kind == SessionMessageKind::WorkflowNotice)
            .map(|message| message.content.trim())
            .find(|content| Self::is_warning(content) || *content == restored)
    }

    /// Returns whether `content` is a sandbox warning.
    fn is_warning(content: &str) -> bool {
        content.starts_with(&format!("{} ", TranscriptNotice::SandboxWarning.prefix()))
    }
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod tests;
