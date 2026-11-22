//! Projects the content-bearing events of a harness run onto the streamed
//! output Agentty shows while a session turn runs.

use std::collections::{BTreeSet, HashMap, HashSet};

use ag_contracts::{OutputEvent, OutputKind, TurnNotice};
use ag_harness::event::{CompactionOutcome, Delta, Item, ItemKind};
use ag_harness::{RunEventKind, RunOutput};
use serde_json::Value;

use super::config::ModelSelection;

/// Field of the protocol response that carries the user-facing answer.
const ANSWER_FIELD: &str = "answer";

/// Turns one run's item events into [`OutputEvent`]s, decoding the answer of
/// a streamed protocol response as it arrives.
///
/// A model request that completes reports `ModelRequestCompleted` before its
/// response's items complete. A streamed response whose items complete
/// without it was discarded, such as a failed response or an invalid final
/// answer requested again, so its items are dropped when the next response
/// starts streaming, a request is retried, or the run ends.
#[derive(Default)]
pub(crate) struct OutputProjector {
    /// Committed protocol answer that the run's final output replaces.
    answer: Option<u64>,
    /// Whether the streaming response's items completed without its request
    /// completing.
    discarded: bool,
    /// Decoders of the open message items.
    messages: HashMap<u64, AnswerDecoder>,
    /// Items shown and not yet completed or dropped.
    open: HashSet<u64>,
    /// Tool calls whose arguments are streaming.
    preparing: BTreeSet<u64>,
    /// Items that streamed into the response whose request has not
    /// completed.
    streaming: Vec<u64>,
    /// Tool calls whose arguments have not streamed, which stay unshown.
    unstreamed_tools: HashSet<u64>,
}

impl OutputProjector {
    /// Returns the output changes `event` makes, in order.
    pub(crate) fn project(&mut self, event: RunEventKind) -> Vec<OutputEvent> {
        match event {
            RunEventKind::ItemStarted { id, item_kind } => self.start(id.get(), item_kind),
            RunEventKind::ItemDelta { delta, id } => self.delta(id.get(), delta),
            RunEventKind::ItemCompleted { id, item } => {
                self.complete(id.get(), item).into_iter().collect()
            }
            RunEventKind::ModelRequestCompleted { .. } => self.commit(),
            // A retried request discards the response it was streaming.
            RunEventKind::Retrying { .. } => self.drop_items(Vec::new()),
            // A finished run never completes what it left open.
            RunEventKind::Completed(_) => {
                let open = self.open.iter().copied().collect();

                self.drop_items(open)
            }
            _ => Vec::new(),
        }
    }

    /// Opens an item. A tool call is shown once its arguments stream, and
    /// the final output once it completes.
    fn start(&mut self, id: u64, item_kind: ItemKind) -> Vec<OutputEvent> {
        let kind = match item_kind {
            ItemKind::Message => {
                self.messages.insert(id, AnswerDecoder::default());

                OutputKind::Message
            }
            ItemKind::Reasoning => OutputKind::Reasoning,
            ItemKind::ToolCall => {
                self.unstreamed_tools.insert(id);

                return Vec::new();
            }
            _ => return Vec::new(),
        };
        self.open.insert(id);

        vec![OutputEvent::Started { id, kind }]
    }

    /// Applies one fragment, dropping a discarded response first. Message
    /// fragments that add no answer text yet, such as the opening of a
    /// protocol response, emit nothing.
    fn delta(&mut self, id: u64, delta: Delta) -> Vec<OutputEvent> {
        let mut events = if self.discarded {
            self.drop_items(Vec::new())
        } else {
            Vec::new()
        };
        if self.streaming.is_empty() {
            self.answer = None;
        }
        if !self.streaming.contains(&id) {
            self.streaming.push(id);
        }
        let text = match delta {
            Delta::Text { text } => self
                .messages
                .get_mut(&id)
                .map(|decoder| decoder.push(&text)),
            Delta::Reasoning { text } => Some(text),
            _ => {
                events.extend(self.prepare_tool_call(id));

                return events;
            }
        };
        events.extend(
            text.filter(|text| !text.is_empty())
                .map(|text| OutputEvent::Delta { id, text }),
        );

        events
    }

    /// Shows a tool call at its first argument fragment; its arguments
    /// themselves stay unshown.
    fn prepare_tool_call(&mut self, id: u64) -> Option<OutputEvent> {
        self.unstreamed_tools.remove(&id).then(|| {
            self.open.insert(id);
            self.preparing.insert(id);

            OutputEvent::Started {
                id,
                kind: OutputKind::ToolCall,
            }
        })
    }

    /// Completes an item with its authoritative text, or reports the run's
    /// final answer. A streamed item completing before its request
    /// completed marks its response discarded.
    fn complete(&mut self, id: u64, item: Item) -> Option<OutputEvent> {
        let discarded = self.streaming.contains(&id);
        let text = match item {
            Item::Output { output } => {
                return Some(OutputEvent::Answer {
                    replaces: self.answer.take(),
                    text: answer(&output),
                });
            }
            Item::Message { text } => {
                self.messages.remove(&id);
                let mut decoder = AnswerDecoder::default();
                let text = decoder.push(&text);
                if decoder.is_answer() && !discarded {
                    self.answer = Some(id);
                }

                text
            }
            Item::Reasoning { text } => text,
            _ => {
                // A tool call ran, so the newest message was no answer. A
                // streamed call's line already completed with its request.
                self.unstreamed_tools.remove(&id);
                self.answer = None;

                return None;
            }
        };
        self.discarded |= discarded;
        self.open.remove(&id);

        Some(OutputEvent::Completed { id, text })
    }

    /// Accepts the streamed response of a completed request; its tool calls'
    /// arguments are complete, so their preparation lines complete.
    fn commit(&mut self) -> Vec<OutputEvent> {
        self.streaming.clear();
        self.discarded = false;

        std::mem::take(&mut self.preparing)
            .into_iter()
            .map(|id| {
                self.open.remove(&id);

                OutputEvent::Completed {
                    id,
                    text: String::new(),
                }
            })
            .collect()
    }

    /// Drops `ids` and the streaming response's items.
    fn drop_items(&mut self, mut ids: Vec<u64>) -> Vec<OutputEvent> {
        ids.append(&mut self.streaming);
        ids.sort_unstable();
        ids.dedup();
        self.discarded = false;
        for id in &ids {
            self.messages.remove(id);
            self.open.remove(id);
            self.preparing.remove(id);
        }
        if ids.is_empty() {
            return Vec::new();
        }

        vec![OutputEvent::Discard { ids }]
    }
}

/// Returns the notice a run event warrants: a compaction that published a
/// checkpoint, or one that failed so the turn continued without its oldest
/// turns. Any other event warrants none, including a compaction that found
/// nothing to cover.
pub(crate) fn turn_notice(event: &RunEventKind) -> Option<TurnNotice> {
    let RunEventKind::Compacted(report) = event else {
        return None;
    };

    match &report.outcome {
        CompactionOutcome::Compacted { model, .. } => Some(TurnNotice::ContextCompacted {
            model: model
                .as_ref()
                .map(|model| ModelSelection::model_id_of(model.as_str())),
        }),
        CompactionOutcome::Failed(failure) => {
            // The notice names only the failure's kind; its message can be a
            // long provider response.
            tracing::warn!(kind = %failure.kind, message = %failure.message, "compaction failed");

            Some(TurnNotice::ContextCompactionFailed {
                reason: failure.kind.as_str().replace('_', " "),
            })
        }
        _ => None,
    }
}

/// Returns the answer text of a validated protocol response.
fn answer(output: &RunOutput) -> String {
    output
        .parse::<Value>()
        .ok()
        .as_ref()
        .and_then(|response| response.get(ANSWER_FIELD))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Incremental decoder of one assistant message's display text.
///
/// A message whose first non-whitespace character opens a JSON object is a
/// protocol response, shown as the `answer` string decoded so far; any other
/// message is shown verbatim.
#[derive(Default)]
struct AnswerDecoder {
    mode: DecoderMode,
}

impl AnswerDecoder {
    /// Consumes `fragment` and returns the display text it adds.
    fn push(&mut self, fragment: &str) -> String {
        let mut text = String::new();
        for character in fragment.chars() {
            match &mut self.mode {
                DecoderMode::Undecided if character.is_whitespace() => {}
                DecoderMode::Undecided if character == '{' => {
                    let mut scanner = AnswerScanner::default();
                    scanner.push(character, &mut text);
                    self.mode = DecoderMode::Json(scanner);
                }
                DecoderMode::Undecided => {
                    text.push(character);
                    self.mode = DecoderMode::Plain;
                }
                DecoderMode::Plain => text.push(character),
                DecoderMode::Json(scanner) => scanner.push(character, &mut text),
            }
        }

        text
    }

    /// Returns whether the message is a JSON protocol response.
    fn is_answer(&self) -> bool {
        matches!(self.mode, DecoderMode::Json(_))
    }
}

/// How an [`AnswerDecoder`] reads its message.
#[derive(Default)]
enum DecoderMode {
    /// Only whitespace has arrived.
    #[default]
    Undecided,
    /// The message is plain text.
    Plain,
    /// The message is a JSON protocol response.
    Json(AnswerScanner),
}

/// Scanner of a streamed JSON object that emits the top-level `answer`
/// string's decoded characters as they arrive.
#[derive(Default)]
struct AnswerScanner {
    /// Whether the latest top-level key read was `answer`.
    answer_key: bool,
    /// Nesting depth of objects and arrays.
    depth: usize,
    /// Pending escape inside the current string.
    escape: Escape,
    /// Whether the next top-level string is a key.
    expect_key: bool,
    /// Whether the scanner is inside a string.
    in_string: bool,
    /// Text of the top-level key being read.
    key: String,
    /// What the current string is.
    role: StringRole,
    /// UTF-16 code units of an escaped surrogate pair awaiting its low half.
    surrogates: Vec<u16>,
}

impl AnswerScanner {
    /// Consumes `character`, appending decoded answer text to `text`.
    fn push(&mut self, character: char, text: &mut String) {
        if self.in_string {
            self.push_string(character, text);

            return;
        }
        match character {
            '"' => {
                self.in_string = true;
                self.role = if self.depth != 1 {
                    StringRole::Other
                } else if self.expect_key {
                    self.key.clear();

                    StringRole::Key
                } else if self.answer_key {
                    StringRole::Answer
                } else {
                    StringRole::Other
                };
            }
            '{' | '[' => {
                self.depth += 1;
                self.expect_key = character == '{';
            }
            '}' | ']' => self.depth = self.depth.saturating_sub(1),
            ',' if self.depth == 1 => {
                self.answer_key = false;
                self.expect_key = true;
            }
            ':' if self.depth == 1 => self.expect_key = false,
            _ => {}
        }
    }

    /// Consumes one character inside a string.
    fn push_string(&mut self, character: char, text: &mut String) {
        match std::mem::take(&mut self.escape) {
            Escape::Backslash => match character {
                'b' => self.emit('\u{8}', text),
                'f' => self.emit('\u{c}', text),
                'n' => self.emit('\n', text),
                'r' => self.emit('\r', text),
                't' => self.emit('\t', text),
                'u' => self.escape = Escape::Unicode(String::new()),
                other => self.emit(other, text),
            },
            Escape::Unicode(mut hex) => {
                hex.push(character);
                if hex.len() < 4 {
                    self.escape = Escape::Unicode(hex);

                    return;
                }
                let unit = u16::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                self.push_unit(unit, text);
            }
            Escape::None if character == '\\' => self.escape = Escape::Backslash,
            Escape::None if character == '"' => {
                self.in_string = false;
                if self.role == StringRole::Key {
                    self.answer_key = self.key == ANSWER_FIELD;
                }
            }
            Escape::None => self.emit(character, text),
        }
    }

    /// Decodes one escaped UTF-16 code unit, holding a high surrogate until
    /// its low half arrives.
    fn push_unit(&mut self, unit: u16, text: &mut String) {
        self.surrogates.push(unit);
        if self.surrogates.len() == 1 && (0xD800..0xDC00).contains(&unit) {
            return;
        }
        let characters = char::decode_utf16(std::mem::take(&mut self.surrogates))
            .map(|character| character.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<Vec<_>>();
        for character in characters {
            self.emit(character, text);
        }
    }

    /// Records one decoded string character: answer text is emitted, and a
    /// top-level key is remembered.
    fn emit(&mut self, character: char, text: &mut String) {
        match self.role {
            StringRole::Answer => text.push(character),
            StringRole::Key => self.key.push(character),
            StringRole::Other => {}
        }
    }
}

/// Escape sequence in progress inside a JSON string.
#[derive(Default)]
enum Escape {
    /// No escape is pending.
    #[default]
    None,
    /// A backslash arrived.
    Backslash,
    /// A `\u` escape with the hexadecimal digits read so far.
    Unicode(String),
}

/// What the JSON string being scanned is.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum StringRole {
    /// The value of the top-level `answer` field.
    Answer,
    /// A top-level key.
    Key,
    /// Any other string.
    #[default]
    Other,
}

#[cfg(test)]
#[path = "output_test.rs"]
mod tests;
