//! Streamed model output of one turn, accumulated by the turn producer and
//! formatted by the foreground at most once per frame.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_contracts::{OutputEvent, OutputKind};
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::session::SessionId;

/// Most recent items kept, so a long turn's output stays cheap to render.
const LIVE_OUTPUT_ITEM_LIMIT: usize = 16;
/// Line shown while the model writes a tool call's arguments.
const TOOL_CALL_PREPARATION: &str = "_Preparing a tool call…_";

/// A replaceable streamed-output snapshot shared by a turn producer and the
/// foreground. Equality identifies the mailbox, independent of its changing
/// contents.
///
/// The producer queues one [`AppEvent::SessionOutputReady`] notification
/// while none is armed. The foreground keeps the notification armed after a
/// [`Self::snapshot`] and applies later changes through [`Self::refresh`]
/// once per frame, so a fast stream redraws at most once per frame instead
/// of waking the event loop for every fragment.
#[derive(Clone, Debug, Default)]
pub(crate) struct LiveOutput {
    state: Arc<Mutex<OutputState>>,
}

impl LiveOutput {
    /// Returns the current output as Markdown and keeps the notification
    /// armed until a [`Self::refresh`] finds nothing new.
    pub(crate) fn snapshot(&self) -> String {
        let mut state = self.lock();
        state.changed = false;

        state.markdown()
    }

    /// Returns the output as Markdown when it changed since the last
    /// snapshot. Otherwise releases the notification, so the next change
    /// queues a new one, and returns `None`.
    pub(crate) fn refresh(&self) -> Option<String> {
        let mut state = self.lock();
        if !state.changed {
            state.pending = false;

            return None;
        }
        state.changed = false;

        Some(state.markdown())
    }

    /// Applies one streamed change and queues a notification when none is
    /// armed.
    pub(super) fn observe(
        &self,
        event: OutputEvent,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        let mut state = self.lock();
        if !state.apply(event) {
            return;
        }
        self.mark_changed(&mut state, sender, session_id);
    }

    /// Removes the output from the session view once the turn's saved
    /// result replaces it; later calls, or a turn that streamed nothing,
    /// send nothing.
    pub(super) fn clear(&self, sender: &mpsc::UnboundedSender<AppEvent>, session_id: &SessionId) {
        if std::mem::take(&mut self.lock().streamed) {
            let _ = sender.send(AppEvent::SessionOutputCleared {
                session_id: session_id.clone(),
            });
        }
    }

    /// Records a change and queues a notification when none is armed.
    fn mark_changed(
        &self,
        state: &mut OutputState,
        sender: &mpsc::UnboundedSender<AppEvent>,
        session_id: &SessionId,
    ) {
        state.changed = true;
        state.streamed = true;
        if !state.pending {
            state.pending = sender
                .send(AppEvent::SessionOutputReady {
                    output: self.clone(),
                    session_id: session_id.clone(),
                })
                .is_ok();
        }
    }

    /// Locks the shared state; it stays valid after a panic.
    fn lock(&self) -> MutexGuard<'_, OutputState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl PartialEq for LiveOutput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for LiveOutput {}

/// Streamed items in start order, with the notification state.
#[derive(Debug, Default)]
struct OutputState {
    /// Whether the items changed since the foreground last formatted them.
    changed: bool,
    items: Vec<OutputItem>,
    /// Whether a notification is queued or its handle is still armed.
    pending: bool,
    /// Whether the turn streamed any output.
    streamed: bool,
}

impl OutputState {
    /// Applies `event`, returning whether the items changed.
    fn apply(&mut self, event: OutputEvent) -> bool {
        match event {
            OutputEvent::Started { id, kind } => self.push(OutputItem {
                id: Some(id),
                kind,
                open: true,
                text: String::new(),
            }),
            OutputEvent::Delta { id, text } => {
                let Some(item) = self.open_item(id) else {
                    return false;
                };
                item.text.push_str(&text);
            }
            OutputEvent::Completed { id, text } => return self.complete(id, text),
            OutputEvent::Answer { replaces, text } => self.answer(replaces, text),
            OutputEvent::Discard { ids } => {
                let item_count = self.items.len();
                self.items
                    .retain(|item| !item.id.is_some_and(|id| ids.contains(&id)));

                return self.items.len() != item_count;
            }
        }

        true
    }

    /// Appends `item`, dropping the oldest item at the limit.
    fn push(&mut self, item: OutputItem) {
        if self.items.len() >= LIVE_OUTPUT_ITEM_LIMIT {
            self.items.remove(0);
        }
        self.items.push(item);
    }

    /// Returns the open item `id`.
    fn open_item(&mut self, id: u64) -> Option<&mut OutputItem> {
        self.items
            .iter_mut()
            .rev()
            .find(|item| item.open && item.id == Some(id))
    }

    /// Replaces an item's fragments with its completed text, returning
    /// whether the item exists. A tool call's line goes once its arguments
    /// are complete, when its activity row takes over.
    fn complete(&mut self, id: u64, text: String) -> bool {
        let Some(index) = self.items.iter().rposition(|item| item.id == Some(id)) else {
            return false;
        };
        if self.items[index].kind == OutputKind::ToolCall {
            self.items.remove(index);

            return true;
        }
        let item = &mut self.items[index];
        item.open = false;
        item.text = text;

        true
    }

    /// Shows the validated answer in place of the message it streamed as,
    /// or after the other items when it did not stream.
    fn answer(&mut self, replaces: Option<u64>, text: String) {
        if let Some(item) =
            replaces.and_then(|id| self.items.iter_mut().rev().find(|item| item.id == Some(id)))
        {
            item.open = false;
            item.text = text;

            return;
        }
        self.push(OutputItem {
            id: None,
            kind: OutputKind::Message,
            open: false,
            text,
        });
    }

    /// Formats the items as one Markdown block: reasoning quoted, messages
    /// verbatim, and tool calls as a one-line preparation summary.
    fn markdown(&self) -> String {
        let mut markdown = String::new();
        for item in &self.items {
            let block = item.markdown();
            if block.is_empty() {
                continue;
            }
            if !markdown.is_empty() {
                markdown.push_str("\n\n");
            }
            markdown.push_str(&block);
        }

        markdown
    }
}

/// One streamed item.
#[derive(Debug)]
struct OutputItem {
    /// Identifier from the turn, or `None` for an answer that streamed no
    /// message.
    id: Option<u64>,
    kind: OutputKind,
    /// Whether fragments may still arrive.
    open: bool,
    text: String,
}

impl OutputItem {
    /// Formats this item, or returns an empty string when it shows nothing
    /// yet.
    fn markdown(&self) -> String {
        match self.kind {
            OutputKind::Message => self.text.trim().to_string(),
            OutputKind::Reasoning => {
                let mut quoted = String::new();
                for line in self.text.trim().lines() {
                    if !quoted.is_empty() {
                        quoted.push('\n');
                    }
                    let _ = write!(quoted, "> {line}");
                }

                quoted
            }
            OutputKind::ToolCall => TOOL_CALL_PREPARATION.to_string(),
        }
    }
}

#[cfg(test)]
#[path = "output_test.rs"]
mod tests;
