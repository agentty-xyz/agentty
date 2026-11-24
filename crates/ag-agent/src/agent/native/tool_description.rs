//! Describes a native harness tool call for display: its activity kind and
//! what it acts on, shared by session and utility tool rows.

use ag_contracts::{ActivityKind, LineRange};
use ag_harness::tool::{ReadAction, ToolCall, ToolCallArguments};

use crate::agent::activity_content::{line_range, one_line};

/// Longest command a tool row's input preview keeps, in characters.
const INPUT_LIMIT: usize = 4096;

/// What a tool row shows about a call before it is answered.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct ToolDescription {
    /// Command text for a `bash` call's input preview, bounded.
    pub(super) input: Option<String>,
    pub(super) kind: ActivityKind,
    /// Line range a file read selects.
    pub(super) lines: Option<LineRange>,
    /// Path, query, or command flattened to one line.
    pub(super) target: Option<String>,
}

impl ToolDescription {
    /// Describes the call to `name` from its `arguments`, decoded as the
    /// harness decodes them. Arguments that are absent or that the tool
    /// cannot decode describe the call by its name alone.
    pub(super) fn new(call_id: &str, name: &str, arguments: Option<&str>) -> Self {
        let call = arguments.and_then(|arguments| {
            ToolCall::from_json(call_id.to_string(), name, arguments, None).ok()
        });
        let mut description = Self {
            input: None,
            kind: Self::kind_of(name),
            lines: None,
            target: None,
        };
        match call.as_ref().map(ToolCall::arguments) {
            Some(ToolCallArguments::Bash(arguments)) => {
                description.input = Some(arguments.command().chars().take(INPUT_LIMIT).collect());
                description.target = one_line(arguments.command());
            }
            Some(ToolCallArguments::Read(arguments)) => {
                let (kind, target) = match arguments.action() {
                    ReadAction::Search => (ActivityKind::Search, arguments.query()),
                    ReadAction::List => (ActivityKind::List, arguments.path_filter()),
                    _ => {
                        description.lines = line_range(arguments.offset(), arguments.limit());

                        (ActivityKind::Read, arguments.path_filter())
                    }
                };
                description.kind = kind;
                description.target = target.and_then(one_line);
            }
            Some(ToolCallArguments::Write(arguments)) => {
                description.target = one_line(arguments.path());
            }
            _ => {}
        }

        description
    }

    /// Classifies a call by its tool's name: a `read` without decodable
    /// arguments counts as a file read, its default action.
    fn kind_of(name: &str) -> ActivityKind {
        match name {
            "bash" => ActivityKind::Command,
            "read" => ActivityKind::Read,
            "write" => ActivityKind::FileChange,
            _ => ActivityKind::Tool,
        }
    }
}

#[cfg(test)]
#[path = "tool_description_test.rs"]
mod tests;
