//! Record-time truncation of oversized tool results.
//!
//! Every tool result is cut once, when the engine records it, to a fixed
//! weight independent of how full the request is. The canonical message,
//! the durable journal, and replayed history all keep the cut form, so later
//! requests repeat the same bytes and a provider prompt cache keeps matching.

use serde_json::{Map, Value};

use crate::context::{ContextBudget, ContextEstimator};
use crate::model::ModelMessage;
use crate::tool::ToolCall;

/// Most weight one recorded tool result keeps under a large budget, about
/// the ten thousand tokens Codex keeps of each tool output.
pub(crate) const TOOL_OUTPUT_WEIGHT_LIMIT: u64 = 10_000;

/// Share, in percent, of the request capacity left after reserved output
/// that one recorded tool result keeps under a small budget. Two results of
/// this size stay within the fitting target beside instructions and older
/// notices.
pub(crate) const TOOL_OUTPUT_CAPACITY_PERCENT: u64 = 25;

/// Field holding the truncation warning in a cut tool result.
pub(crate) const TRUNCATION_KEY: &str = "output_truncated";

/// Field that carries a cut result that was not a JSON object.
const RAW_OUTPUT_KEY: &str = "output";

/// Returns the most weight one tool result keeps when it is recorded under
/// `budget`.
pub(crate) fn tool_output_limit(budget: ContextBudget) -> u64 {
    let capacity = budget
        .max_request_weight()
        .get()
        .saturating_sub(budget.reserved_output_weight());

    (capacity.saturating_mul(TOOL_OUTPUT_CAPACITY_PERCENT) / 100).min(TOOL_OUTPUT_WEIGHT_LIMIT)
}

/// Returns `content`, the result of `call`, cut to weigh at most `limit`,
/// and whether it was cut.
///
/// A JSON object keeps every field; its largest string fields, largest
/// first, keep their head and tail around a marker counting the cut lines
/// and bytes, and an [`TRUNCATION_KEY`] field states the original weight and
/// how to request the cut part. Any other content, including an object that
/// already has that field, is cut the same way inside one `output` field. When
/// even empty excerpts outweigh `limit`, the smallest form is returned.
pub(crate) fn truncate_tool_output(
    estimator: &dyn ContextEstimator,
    call: &ToolCall,
    content: String,
    limit: u64,
) -> (String, bool) {
    let weigh = |content: &str| {
        estimator.message_weight(&ModelMessage::ToolResult {
            call_id: call.id().to_string(),
            content: content.to_string(),
            name: call.name().to_string(),
        })
    };
    let original_weight = weigh(&content);
    if original_weight <= limit {
        return (content, false);
    }
    let fields = match serde_json::from_str::<Value>(&content) {
        Ok(Value::Object(fields))
            if fields.values().any(Value::is_string) && !fields.contains_key(TRUNCATION_KEY) =>
        {
            fields
        }
        _ => Map::from_iter([(RAW_OUTPUT_KEY.to_string(), Value::String(content))]),
    };
    let truncation = Truncation {
        original_weight,
        read: call.name() == "read",
    };

    (truncation.cut(fields, &weigh, limit), true)
}

/// What the warning of one cut result reports.
struct Truncation {
    original_weight: u64,
    read: bool,
}

impl Truncation {
    /// Cuts the largest uncut string field of `fields` to the longest
    /// excerpt that brings the result within `limit`, moving on to the next
    /// largest while even an empty excerpt is too heavy.
    fn cut(
        &self,
        mut fields: Map<String, Value>,
        weigh: &dyn Fn(&str) -> u64,
        limit: u64,
    ) -> String {
        let mut cut = Vec::new();
        loop {
            let largest = fields
                .iter()
                .filter(|(key, _)| !cut.iter().any(|(cut_key, _)| cut_key == *key))
                .filter_map(|(key, value)| Some((key, value.as_str()?)))
                .max_by_key(|(_, text)| text.len())
                .map(|(key, text)| (key.clone(), text.to_string()));
            let Some((key, text)) = largest else {
                return self.render(&fields, &cut);
            };
            let render = |kept: usize| {
                let excerpt = Excerpt::new(&text, kept);
                let mut fields = fields.clone();
                fields.insert(key.clone(), excerpt.text().into());
                let mut cut = cut.clone();
                cut.push((key.clone(), excerpt.summary(&key, &fields)));

                (self.render(&fields, &cut), fields, cut)
            };
            let (mut low, mut high) = (0, text.len());
            while low < high {
                let middle = low + (high - low).div_ceil(2);
                if weigh(&render(middle).0) <= limit {
                    low = middle;
                } else {
                    high = middle - 1;
                }
            }
            let (rendered, kept_fields, kept_cut) = render(low);
            if weigh(&rendered) <= limit {
                return rendered;
            }
            fields = kept_fields;
            cut = kept_cut;
        }
    }

    /// Renders `fields` with the warning naming each cut field.
    fn render(&self, fields: &Map<String, Value>, cut: &[(String, String)]) -> String {
        let summaries = cut
            .iter()
            .map(|(_, summary)| summary.as_str())
            .collect::<Vec<_>>()
            .join(". ");
        let hint = if self.read {
            "Read the cut lines again, narrowed with offset/limit, if you need them."
        } else {
            "Request a narrower result (a narrower path or search, or a command that filters its \
             output) if you need the cut part."
        };
        let mut fields = fields.clone();
        fields.insert(
            TRUNCATION_KEY.to_string(),
            format!(
                "Warning: truncated output (original weight about {} tokens). {summaries}. {hint}",
                self.original_weight
            )
            .into(),
        );

        Value::Object(fields).to_string()
    }
}

/// Head and tail of one string field around its cut middle.
struct Excerpt<'a> {
    field: &'a str,
    head_end: usize,
    tail_start: usize,
}

impl<'a> Excerpt<'a> {
    /// Splits `field` to keep about `kept` bytes, half from each end, ending
    /// the head and starting the tail on a line boundary when one lies within
    /// the outer half of each budget.
    fn new(field: &'a str, kept: usize) -> Self {
        let head_budget = field.floor_char_boundary(kept / 2);
        let head_end = field[..head_budget]
            .rfind('\n')
            .map(|newline| newline + 1)
            .filter(|end| *end >= head_budget / 2)
            .unwrap_or(head_budget);
        let tail_budget = kept - kept / 2;
        let tail_budget_start =
            field.ceil_char_boundary(field.len() - tail_budget.min(field.len()));
        let tail_start = field[tail_budget_start..]
            .find('\n')
            .map(|newline| tail_budget_start + newline + 1)
            .filter(|start| field.len() - start >= tail_budget / 2 && *start < field.len())
            .unwrap_or(tail_budget_start)
            .max(head_end);

        Self {
            field,
            head_end,
            tail_start,
        }
    }

    /// Returns the head, a marker counting the cut lines and bytes, and the
    /// tail.
    fn text(&self) -> String {
        let omitted = &self.field[self.head_end..self.tail_start];
        let lines = omitted.matches('\n').count();
        let marker = if lines == 0 {
            format!("…{} bytes truncated…", omitted.len())
        } else {
            format!(
                "…{} ({} bytes) truncated…",
                line_count(lines),
                omitted.len()
            )
        };

        format!(
            "{}{marker}{}",
            &self.field[..self.head_end],
            &self.field[self.tail_start..]
        )
    }

    /// Describes the cut of field `key` for the warning: its total lines
    /// and, for a read result's `content` with a known `start_line`, the
    /// one-based lines to request again.
    fn summary(&self, key: &str, fields: &Map<String, Value>) -> String {
        let total_lines = line_count(self.field.lines().count());
        let start_line = fields.get("start_line").and_then(Value::as_u64);
        match start_line.filter(|_| key == "content" && self.tail_start > self.head_end) {
            Some(start_line) => {
                let first = start_line + count_newlines(&self.field[..self.head_end]);
                let before_tail = &self.field[..self.tail_start];
                let last = start_line + count_newlines(before_tail)
                    - u64::from(before_tail.ends_with('\n'));

                format!(
                    "The middle of `{key}` ({total_lines}) was cut: lines {first}-{last} are \
                     missing (offset={first} limit={})",
                    last - first + 1
                )
            }
            None => format!("The middle of `{key}` ({total_lines}) was cut"),
        }
    }
}

fn line_count(lines: usize) -> String {
    if lines == 1 {
        "1 line".to_string()
    } else {
        format!("{lines} lines")
    }
}

fn count_newlines(text: &str) -> u64 {
    u64::try_from(text.matches('\n').count()).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[path = "truncation_test.rs"]
mod tests;
