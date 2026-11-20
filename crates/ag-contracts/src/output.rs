//! Streamed model output of one in-process agent turn.

/// One change to the model output an agent streams while its turn runs.
///
/// Items are numbered in the order they start. Hosts render them transiently
/// and replace them with the persisted turn result when the turn ends; the
/// output is session content, never telemetry, and is never replayed to a
/// provider.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OutputEvent {
    /// Item `id` started; its fragments and completion follow.
    Started {
        /// Identifier of the item within its turn.
        id: u64,
        /// What the item holds.
        kind: OutputKind,
    },
    /// A fragment of display text appended to message or reasoning item
    /// `id`; tool calls stream no fragments.
    Delta {
        /// Identifier of the item, as [`Self::Started`] reported it.
        id: u64,
        /// The fragment.
        text: String,
    },
    /// Item `id` finished; `text` replaces its streamed fragments. A
    /// finished tool call carries no text, since its activity row reports
    /// it.
    Completed {
        /// Identifier of the item, as [`Self::Started`] reported it.
        id: u64,
        /// The authoritative item text.
        text: String,
    },
    /// The turn's validated final answer, shown in place of the latest
    /// message when that message is the newest item.
    Answer(String),
    /// Items still open will never complete, such as the tool calls of a
    /// discarded response, so their partial text is dropped.
    DiscardOpen,
}

/// What one streamed output item holds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OutputKind {
    /// Assistant text.
    Message,
    /// Provider reasoning, such as a reasoning summary.
    Reasoning,
    /// A tool call whose arguments the model is still writing.
    ToolCall,
}
