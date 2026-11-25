//! Bounded, terminal-safe previews of provider tool content.

use ag_contracts::{ActivityEvent, ActivityFacts, LineRange};
use serde_json::Value;

/// Longest text one activity fact keeps, in characters.
const FACT_LIMIT: usize = 240;
const PREVIEW_LIMIT: usize = 4096;
const TRUNCATED: &str = "\n… [truncated]";
/// Argument keys naming what an operation acts on, most specific first: a
/// search pattern before the path it searches.
const TARGET_KEYS: [&str; 12] = [
    "pattern",
    "query",
    "Query",
    "command",
    "CommandLine",
    "file_path",
    "path",
    "AbsolutePath",
    "DirectoryPath",
    "TargetFile",
    "target_file",
    "SearchPath",
];

/// Optional fields in a provider update; absent fields preserve prior values.
#[derive(Default)]
pub(super) struct ActivityDetails {
    pub exit_code: Option<i64>,
    pub facts: ActivityFacts,
    pub input: Option<String>,
    pub output: Option<String>,
}

impl ActivityDetails {
    pub(super) fn new(
        input: Option<&Value>,
        output: Option<&Value>,
        exit_code: Option<i64>,
    ) -> Self {
        let input = input.filter(|value| !value.is_null());

        Self {
            exit_code,
            facts: ActivityFacts {
                target: input.and_then(target),
                ..ActivityFacts::default()
            },
            input: input.map(|value| {
                // Select useful fields before bounding large edit/write
                // payloads.
                let value = [
                    "command",
                    "CommandLine",
                    "file_path",
                    "path",
                    "TargetFile",
                    "target_file",
                ]
                .iter()
                .find_map(|key| value.get(*key))
                .unwrap_or(value);
                match value.as_str() {
                    Some(text) => bounded(text),
                    None => bounded(&value.to_string()),
                }
            }),
            output: output.and_then(preview),
        }
    }

    /// Applies the reported fields to `event`, keeping its earlier values
    /// for the fields this update leaves out. Provider protocols report no
    /// pipeline statuses or signals, so those stay unset.
    pub(super) fn apply(self, event: &mut ActivityEvent) {
        let facts = &mut event.facts;
        facts.diff_stat = self.facts.diff_stat.or(facts.diff_stat);
        facts.duration = self.facts.duration.or(facts.duration);
        facts.failure = self.facts.failure.or(facts.failure);
        facts.lines = self.facts.lines.or(facts.lines);
        facts.reason = self.facts.reason.or(facts.reason.take());
        facts.target = self.facts.target.or(facts.target.take());
        event.input = self.input.or(event.input.take());
        event.output = self.output.or(event.output.take());
        event.exit_code = self.exit_code.or(event.exit_code);
    }
}

/// Returns the first nonblank line of `text` as a bounded fact, such as a
/// failure's reason, or `None` when `text` is blank.
pub(super) fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(bounded_fact)
}

/// Returns `text` with its whitespace runs, line breaks included, joined by
/// single spaces as a bounded fact, such as a multi-line command's target,
/// or `None` when `text` is blank.
pub(super) fn one_line(text: &str) -> Option<String> {
    bounded_fact(&text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Returns the line range a read starting at one-based line `offset` and
/// reading at most `limit` lines covers, or `None` when the read named
/// neither and covered the whole file. A zero counts as absent.
pub(super) fn line_range(offset: Option<u64>, limit: Option<u64>) -> Option<LineRange> {
    let offset = offset.filter(|offset| *offset > 0);
    let limit = limit.filter(|limit| *limit > 0);
    if offset.is_none() && limit.is_none() {
        return None;
    }
    let first = offset.unwrap_or(1);

    Some(LineRange {
        first,
        last: limit.map(|limit| first.saturating_add(limit - 1)),
    })
}

/// Returns what an operation's arguments act on, or the arguments
/// themselves when they are a plain string, as a one-line fact.
fn target(input: &Value) -> Option<String> {
    let value = if input.is_object() {
        TARGET_KEYS.iter().find_map(|key| input.get(*key))?
    } else {
        input
    };

    value.as_str().and_then(one_line)
}

/// Strips control characters from one line and cuts it to [`FACT_LIMIT`]
/// characters with a trailing `…`; `None` when nothing is left.
fn bounded_fact(line: &str) -> Option<String> {
    let mut characters = line.chars().filter(|character| !character.is_control());
    let mut bounded: String = characters.by_ref().take(FACT_LIMIT).collect();
    if characters.next().is_some() {
        bounded.push('…');
    }

    (!bounded.is_empty()).then_some(bounded)
}

/// Appends a streamed chunk without growing beyond the preview limit.
pub(super) fn append_output(previous: Option<&str>, delta: &str) -> String {
    let previous = previous.unwrap_or_default();
    if previous.ends_with(TRUNCATED) {
        return previous.to_string();
    }

    bounded(&format!("{previous}{}", bounded(delta)))
}

fn preview(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(bounded(text)),
        Value::Array(blocks) if blocks.iter().all(is_content_block) => {
            let mut output = String::new();
            for block in blocks {
                let text = block
                    .as_str()
                    .or_else(|| block.get("text").and_then(Value::as_str))
                    .or_else(|| block.pointer("/content/text").and_then(Value::as_str));
                if let Some(text) = text {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output = append_output(Some(&output), text);
                }
            }

            (!output.is_empty()).then_some(output)
        }
        _ => Some(bounded(&value.to_string())),
    }
}

/// Recognizes provider text/media blocks without dropping arbitrary JSON
/// arrays.
fn is_content_block(value: &Value) -> bool {
    value.is_string()
        || value.get("text").is_some_and(Value::is_string)
        || value.pointer("/content/text").is_some_and(Value::is_string)
        || matches!(
            value.get("type").and_then(Value::as_str),
            Some("image" | "audio" | "resource" | "resource_link")
        )
}

fn bounded(text: &str) -> String {
    let mut characters = text
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'));
    let mut output: String = characters.by_ref().take(PREVIEW_LIMIT).collect();
    if characters.next().is_some() {
        output.push_str(TRUNCATED);
    }

    output
}

#[cfg(test)]
#[path = "activity_content_test.rs"]
mod tests;
