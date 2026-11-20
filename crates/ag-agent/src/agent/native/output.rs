//! Projects the content-bearing events of a harness run onto the streamed
//! output Agentty shows while a session turn runs.

use std::collections::HashMap;

use ag_contracts::{OutputEvent, OutputKind};
use ag_harness::event::{Delta, Item, ItemKind};
use ag_harness::{RunEventKind, RunOutput};
use serde_json::Value;

/// Field of the protocol response that carries the user-facing answer.
const ANSWER_FIELD: &str = "answer";

/// Turns one run's item events into [`OutputEvent`]s, decoding the answer of
/// a streamed protocol response as it arrives.
#[derive(Default)]
pub(crate) struct OutputProjector {
    /// Decoders of the open message items, by item.
    messages: HashMap<u64, AnswerDecoder>,
}

impl OutputProjector {
    /// Returns the output change `event` makes, or `None` for an event that
    /// carries no displayed output.
    pub(crate) fn project(&mut self, event: RunEventKind) -> Option<OutputEvent> {
        match event {
            RunEventKind::ItemStarted { id, item_kind } => self.start(id.get(), item_kind),
            RunEventKind::ItemDelta { delta, id } => self.delta(id.get(), delta),
            RunEventKind::ItemCompleted { id, item } => Some(self.complete(id.get(), item)),
            // A retried request discards the response it was streaming, and
            // a finished run never completes what it left open.
            RunEventKind::Retrying { .. } | RunEventKind::Completed(_) => {
                Some(OutputEvent::DiscardOpen)
            }
            _ => None,
        }
    }

    /// Opens an item; the final output is shown only once it completes.
    fn start(&mut self, id: u64, item_kind: ItemKind) -> Option<OutputEvent> {
        let kind = match item_kind {
            ItemKind::Message => {
                self.messages.insert(id, AnswerDecoder::default());

                OutputKind::Message
            }
            ItemKind::Reasoning => OutputKind::Reasoning,
            ItemKind::ToolCall => OutputKind::ToolCall,
            _ => return None,
        };

        Some(OutputEvent::Started { id, kind })
    }

    /// Appends a text or reasoning fragment, dropping message fragments that
    /// add no answer text yet, such as the opening of a protocol response.
    /// Tool arguments stay unshown: hosts summarize a call being prepared.
    fn delta(&mut self, id: u64, delta: Delta) -> Option<OutputEvent> {
        let text = match delta {
            Delta::Text { text } => self
                .messages
                .get_mut(&id)
                .map(|decoder| decoder.push(&text)),
            Delta::Reasoning { text } => Some(text),
            _ => None,
        };

        text.filter(|text| !text.is_empty())
            .map(|text| OutputEvent::Delta { id, text })
    }

    /// Completes an item with its authoritative text, or reports the run's
    /// final answer.
    fn complete(&mut self, id: u64, item: Item) -> OutputEvent {
        let text = match item {
            Item::Message { text } => {
                self.messages.remove(&id);

                AnswerDecoder::default().push(&text)
            }
            Item::Reasoning { text } => text,
            Item::Output { output } => return OutputEvent::Answer(answer(&output)),
            _ => String::new(),
        };

        OutputEvent::Completed { id, text }
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
