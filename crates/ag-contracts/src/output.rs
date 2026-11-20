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
    /// Item `id` finished, and `text` replaces its fragments. A tool call
    /// finishes once its arguments are complete and carries no text.
    Completed {
        /// Identifier of the item, as [`Self::Started`] reported it.
        id: u64,
        /// The authoritative item text.
        text: String,
    },
    /// The turn's validated final answer.
    Answer {
        /// Message item that held the answer as it streamed, which the answer
        /// supersedes, or `None` when the answer did not stream.
        replaces: Option<u64>,
        /// The answer text.
        text: String,
    },
    /// Items that belong to a discarded response, or that the turn left
    /// unfinished, are void.
    Discard {
        /// Identifiers of the void items.
        ids: Vec<u64>,
    },
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
