//! Readable views of the provider-neutral reasoning content each wire
//! protocol stores for replay.

use crate::{anthropic, responses};

/// Readable view of the provider-neutral reasoning content a model
/// returned, as [`reasoning_view`] reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReasoningView<'a> {
    /// Content that is neither Responses reasoning items nor Anthropic
    /// thinking blocks, such as the plain-text reasoning Chat Completions
    /// providers return, as written.
    Text(&'a str),
    /// The summary text of Responses reasoning items, whose reasoning itself
    /// is encrypted, or the summarized thinking of Anthropic thinking
    /// blocks, with parts separated by blank lines.
    Summary(String),
    /// Responses reasoning items or Anthropic thinking blocks without any
    /// summary text: encrypted, redacted, or omitted only.
    Opaque,
}

/// Returns the readable view of `reasoning_content`, recognizing Responses
/// reasoning items and Anthropic thinking blocks exactly as requests replay
/// them.
pub fn reasoning_view(reasoning_content: &str) -> ReasoningView<'_> {
    anthropic::reasoning_view(reasoning_content)
        .or_else(|| responses::reasoning_view(reasoning_content))
        .unwrap_or(ReasoningView::Text(reasoning_content))
}
