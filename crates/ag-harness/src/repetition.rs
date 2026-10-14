//! Detection of a model repeating the same tool call within one turn.

use std::collections::{HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use crate::tool::ToolCall;

/// Recent handled calls of a turn among which identical calls are counted.
/// A model reading or searching across a repository rarely repeats one call
/// within this many, while a loop repeats the same call several times in it.
pub(crate) const REPETITION_WINDOW: usize = 12;

/// Identical calls within [`REPETITION_WINDOW`] after which the next request
/// carries a request-only nudge to stop repeating it.
pub(crate) const REPETITION_NUDGE_AT: usize = 3;

/// Identical calls within [`REPETITION_WINDOW`] that fail the turn with
/// [`crate::TurnError::RepeatedToolCalls`]; the model ignored the nudge.
pub(crate) const REPETITION_STOP_AT: usize = 5;

/// What the latest handled call means for the turn's repetition, ordered
/// from no action to stopping the turn.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Repetition {
    /// The call repeats no earlier call often enough to act on.
    None,
    /// The call reached [`REPETITION_NUDGE_AT`] identical calls for the first
    /// time since the window last reset.
    Nudge { repeats: usize, tool: &'static str },
    /// The call reached [`REPETITION_STOP_AT`] identical calls.
    Stop { repeats: usize, tool: &'static str },
}

/// Identical tool calls of the current turn: the same tool with the same
/// canonical arguments that returned the same result.
///
/// Keying on the result keeps polling a changing command or re-reading a
/// changed file from counting. A `write` or `bash` call that differs from
/// every call in the window changes what later calls may return, so it
/// clears the window, and counting restarts from it.
#[derive(Default)]
pub(crate) struct RepeatedCalls {
    nudged: HashSet<u64>,
    window: VecDeque<u64>,
}

impl RepeatedCalls {
    /// Records one handled `call` whose result digest is `result_digest` and
    /// returns how often it now repeats. A call with malformed arguments has
    /// its own bound and is never recorded.
    pub(crate) fn record(&mut self, call: &ToolCall, result_digest: u64) -> Repetition {
        if call.malformed_arguments().is_some() {
            return Repetition::None;
        }
        let key = Self::key(call, result_digest);
        let changes_state = call.read_arguments().is_none();
        if changes_state && !self.window.contains(&key) {
            self.window.clear();
            self.nudged.clear();
        }
        if self.window.len() == REPETITION_WINDOW {
            self.window.pop_front();
        }
        self.window.push_back(key);
        let repeats = self.window.iter().filter(|seen| **seen == key).count();
        let tool = call.name();

        if repeats >= REPETITION_STOP_AT {
            Repetition::Stop { repeats, tool }
        } else if repeats >= REPETITION_NUDGE_AT && self.nudged.insert(key) {
            Repetition::Nudge { repeats, tool }
        } else {
            Repetition::None
        }
    }

    fn key(call: &ToolCall, result_digest: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        call.name().hash(&mut hasher);
        // Typed arguments serialize deterministically; a call whose arguments
        // fail to serialize keys on its tool and result alone.
        call.arguments_json().ok().hash(&mut hasher);
        result_digest.hash(&mut hasher);

        hasher.finish()
    }
}

/// Returns the digest of one tool result's content.
pub(crate) fn result_digest(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);

    hasher.finish()
}

/// Renders the request-only feedback that asks the model to stop repeating
/// `tool` after `repeats` identical calls.
pub(crate) fn repetition_feedback(tool: &str, repeats: usize) -> String {
    format!(
        "[harness] You have called `{tool}` {repeats} times among your recent tool calls with the \
         same arguments, and it returned the same result each time. Repeating it will not return \
         anything new. Use the results you already have, try a different approach, or give your \
         final response. The turn stops if the same call reaches {REPETITION_STOP_AT} repeats."
    )
}

#[cfg(test)]
#[path = "repetition_test.rs"]
mod tests;
