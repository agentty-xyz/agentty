//! Bounded, terminal-safe previews of provider tool content.

use ag_contracts::{ActivityEvent, ActivityFacts};
use serde_json::Value;

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
    /// pipeline statuses, signals, or rejection reasons, so those stay unset.
    pub(super) fn apply(self, event: &mut ActivityEvent) {
        let facts = &mut event.facts;
        facts.diff_stat = self.facts.diff_stat.or(facts.diff_stat);
        facts.duration = self.facts.duration.or(facts.duration);
        facts.lines = self.facts.lines.or(facts.lines);
        facts.target = self.facts.target.or(facts.target.take());
        event.input = self.input.or(event.input.take());
        event.output = self.output.or(event.output.take());
        event.exit_code = self.exit_code.or(event.exit_code);
    }
}

/// Returns the bounded first line of what an operation's arguments act on,
/// or of the arguments themselves when they are a plain string.
fn target(input: &Value) -> Option<String> {
    let value = if input.is_object() {
        TARGET_KEYS.iter().find_map(|key| input.get(*key))?
    } else {
        input
    };

    value.as_str().and_then(ActivityFacts::bounded_line)
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
