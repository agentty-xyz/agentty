//! Bounded activity projection from provider protocols, independent of tracing.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use ag_contracts::{
    ActivityEvent, ActivityFacts, ActivityKind, ActivityStatus, DiffStat, LineRange,
};
use ag_session::AgentKind;
use serde_json::Value;

use super::activity_content::{ActivityDetails, append_output};

const MAX_ACTIVITIES: usize = 512;

/// Emits changed lifecycle and content snapshots for each operation.
/// Dropping the attempt settles unfinished calls without claiming success.
pub(crate) struct ActivityObserver {
    attempt_id: String,
    calls: BTreeMap<String, ActivityEvent>,
    emit: Box<dyn Fn(ActivityEvent) + Send + Sync>,
    excluded_ids: BTreeSet<String>,
}

impl ActivityObserver {
    pub(crate) fn new(emit: impl Fn(ActivityEvent) + Send + Sync + 'static) -> Self {
        Self {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            calls: BTreeMap::new(),
            emit: Box::new(emit),
            excluded_ids: BTreeSet::new(),
        }
    }

    pub(crate) fn observe(&mut self, kind: AgentKind, payload: &Value) {
        match kind {
            AgentKind::Codex => self.codex(payload),
            AgentKind::Claude => self.claude(payload),
            AgentKind::Antigravity => self.antigravity(payload),
            AgentKind::Harness => {}
        }
    }

    /// Call only after the transport validates the completed turn identity.
    pub(crate) fn codex_completed_turn(&mut self, payload: &Value) {
        if let Some(items) = payload
            .pointer("/params/turn/items")
            .and_then(Value::as_array)
        {
            for item in items {
                self.codex_item(item, true);
            }
        }
    }

    fn codex(&mut self, payload: &Value) {
        let completed = match payload.get("method").and_then(Value::as_str) {
            Some("item/started") => false,
            Some("item/completed") => true,
            Some("item/commandExecution/outputDelta") => {
                self.codex_output(payload);

                return;
            }
            _ => return,
        };
        if let Some(item) = payload.pointer("/params/item") {
            self.codex_item(item, completed);
        }
    }

    fn codex_item(&mut self, item: &Value, completed: bool) {
        let (kind, name) = match item.get("type").and_then(Value::as_str) {
            Some("commandExecution" | "command_execution") => (ActivityKind::Command, "shell"),
            Some("fileChange" | "file_change") => (ActivityKind::FileChange, "apply_patch"),
            Some("webSearch" | "web_search") => (ActivityKind::WebSearch, "web_search"),
            Some("mcpToolCall" | "mcp_tool_call") => (
                ActivityKind::Mcp,
                item.get("tool").and_then(Value::as_str).unwrap_or("tool"),
            ),
            Some("dynamicToolCall" | "dynamic_tool_call") => (
                ActivityKind::Tool,
                item.get("tool").and_then(Value::as_str).unwrap_or("tool"),
            ),
            Some("collabAgentToolCall" | "collab_agent_tool_call") => (ActivityKind::Tool, "agent"),
            Some("imageGeneration" | "image_generation") => {
                (ActivityKind::Tool, "image_generation")
            }
            _ => return,
        };
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            return;
        };
        let status = if !completed {
            ActivityStatus::Running
        } else if matches!(
            item.get("status").and_then(Value::as_str),
            Some("interrupted" | "canceled" | "cancelled")
        ) {
            ActivityStatus::Interrupted
        } else if matches!(
            item.get("status").and_then(Value::as_str),
            Some("failed" | "declined" | "denied")
        ) || item.get("success").and_then(Value::as_bool) == Some(false)
            || item
                .get("exitCode")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
            || item.get("error").is_some_and(|value| !value.is_null())
        {
            ActivityStatus::Failed
        } else {
            ActivityStatus::Completed
        };
        let name = match item.get("server").and_then(Value::as_str) {
            Some(server) => format!("{server}/{name}"),
            None => name.to_string(),
        };
        let mut details = ActivityDetails::new(
            item.get("command")
                .or_else(|| item.get("arguments"))
                .or_else(|| item.get("query")),
            item.get("aggregatedOutput")
                .filter(|value| !value.is_null())
                .or_else(|| item.get("result").filter(|value| !value.is_null()))
                .or_else(|| item.get("contentItems").filter(|value| !value.is_null()))
                .or_else(|| item.get("error")),
            item.get("exitCode").and_then(Value::as_i64),
        );
        details.facts.duration = item
            .get("durationMs")
            .and_then(Value::as_u64)
            .map(Duration::from_millis);
        let kind = match kind {
            ActivityKind::Command => match Self::codex_exploration(item) {
                Some((exploration, target)) => {
                    details.facts.target = target.or(details.facts.target);

                    exploration
                }
                None => kind,
            },
            ActivityKind::FileChange => {
                details.facts.diff_stat = Self::codex_diff_stat(item);
                details.facts.target = item
                    .pointer("/changes/0/path")
                    .and_then(Value::as_str)
                    .and_then(ActivityFacts::bounded_line);

                kind
            }
            _ => kind,
        };
        self.record(id, kind, &name, None, status, details);
    }

    /// Returns the exploration kind of a Codex command whose parsed actions
    /// all read, search, or list files, with the first action's target.
    fn codex_exploration(item: &Value) -> Option<(ActivityKind, Option<String>)> {
        let actions = item.get("commandActions")?.as_array()?;
        let action_type = actions.first()?.get("type")?.as_str()?;
        let (kind, target_key) = match action_type {
            "read" => (ActivityKind::Read, "path"),
            "search" => (ActivityKind::Search, "query"),
            "listFiles" => (ActivityKind::List, "path"),
            _ => return None,
        };
        if actions
            .iter()
            .any(|action| action.get("type").and_then(Value::as_str) != Some(action_type))
        {
            return None;
        }
        let target = actions[0]
            .get(target_key)
            .and_then(Value::as_str)
            .and_then(ActivityFacts::bounded_line);

        Some((kind, target))
    }

    /// Counts the lines a Codex file change adds and removes: every line of
    /// an added or deleted file, and the `+` and `-` lines of an update's
    /// diff. Approximate, since diff headers are recognized by prefix.
    fn codex_diff_stat(item: &Value) -> Option<DiffStat> {
        let changes = item.get("changes")?.as_array()?;
        let mut stat = DiffStat::default();
        for change in changes {
            let diff = change
                .get("diff")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match change.pointer("/kind/type").and_then(Value::as_str) {
                Some("add") => stat.added += line_count(diff),
                Some("delete") => stat.removed += line_count(diff),
                _ => {
                    for line in diff.lines() {
                        if line.starts_with('+') && !line.starts_with("+++ ") {
                            stat.added += 1;
                        } else if line.starts_with('-') && !line.starts_with("--- ") {
                            stat.removed += 1;
                        }
                    }
                }
            }
        }

        (!changes.is_empty()).then_some(stat)
    }

    fn codex_output(&mut self, payload: &Value) {
        let Some(id) = payload.pointer("/params/itemId").and_then(Value::as_str) else {
            return;
        };
        let Some(delta) = payload.pointer("/params/delta").and_then(Value::as_str) else {
            return;
        };
        let Some(previous) = self.calls.get(id) else {
            return;
        };
        if previous.status != ActivityStatus::Running {
            return;
        }
        let output = append_output(previous.output.as_deref(), delta);
        self.record(
            id,
            ActivityKind::Command,
            "shell",
            None,
            ActivityStatus::Running,
            ActivityDetails {
                output: Some(output),
                ..ActivityDetails::default()
            },
        );
    }

    fn antigravity(&mut self, payload: &Value) {
        if payload.get("event").and_then(Value::as_str) != Some("step_update") {
            return;
        }
        let Some(step) = payload.get("step_update") else {
            return;
        };
        if !step
            .get("step_type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("tool"))
        {
            return;
        }
        let Some(index) = step.get("step_index").and_then(Value::as_u64) else {
            return;
        };
        let info = step.get("tool_info").unwrap_or(step);
        let name = info
            .get("name")
            .or_else(|| step.get("tool_name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let completed = step
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("DONE"));
        let exit_code = info.get("exit_code").and_then(Value::as_i64);
        let status = if info.get("error").is_some_and(|error| !error.is_null())
            || (completed && exit_code.is_some_and(|code| code != 0))
        {
            ActivityStatus::Failed
        } else if completed {
            ActivityStatus::Completed
        } else {
            ActivityStatus::Running
        };
        let kind = match name {
            "run_command" => ActivityKind::Command,
            "view_file" => ActivityKind::Read,
            "grep_search" => ActivityKind::Search,
            "list_dir" => ActivityKind::List,
            _ => ActivityKind::Tool,
        };
        self.record(
            &index.to_string(),
            kind,
            name,
            None,
            status,
            ActivityDetails::new(
                info.get("parameters"),
                info.get("output")
                    .filter(|value| !value.is_null())
                    .or_else(|| info.get("error")),
                exit_code,
            ),
        );
    }

    fn claude(&mut self, payload: &Value) {
        let Some(blocks) = payload
            .get("message")
            .unwrap_or(payload)
            .get("content")
            .and_then(Value::as_array)
        else {
            return;
        };
        let parent = payload.get("parent_tool_use_id").and_then(Value::as_str);
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let Some(id) = block.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(name) = block.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    // StructuredOutput carries the answer, not user-facing tool
                    // usage.
                    if name == "StructuredOutput" {
                        if Self::valid_id(id)
                            && self.calls.len() + self.excluded_ids.len() < MAX_ACTIVITIES
                        {
                            self.excluded_ids.insert(id.to_string());
                        }
                        continue;
                    }
                    let mcp_name;
                    let (kind, name) = if name == "Skill" {
                        (
                            ActivityKind::Skill,
                            block
                                .pointer("/input/skill")
                                .and_then(Value::as_str)
                                .unwrap_or("Skill"),
                        )
                    } else if let Some(kind) = Self::claude_builtin_kind(name) {
                        (kind, name)
                    } else if let Some((server, tool)) = name
                        .strip_prefix("mcp__")
                        .and_then(|qualified| qualified.split_once("__"))
                    {
                        // Claude qualifies MCP tools as
                        // `mcp__<server>__<tool>`.
                        mcp_name = format!("{server}/{tool}");

                        (ActivityKind::Mcp, mcp_name.as_str())
                    } else {
                        (ActivityKind::Tool, name)
                    };
                    let mut details = ActivityDetails::new(block.get("input"), None, None);
                    if let Some(input) = block.get("input") {
                        Self::claude_input_facts(name, input, &mut details.facts);
                    }
                    self.record(id, kind, name, parent, ActivityStatus::Running, details);
                }
                Some("tool_result") => {
                    let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let status = if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                        ActivityStatus::Failed
                    } else {
                        ActivityStatus::Completed
                    };
                    self.record(
                        id,
                        ActivityKind::Tool,
                        "tool",
                        parent,
                        status,
                        ActivityDetails::new(None, block.get("content"), None),
                    );
                }
                _ => {}
            }
        }
    }

    /// Classifies a Claude built-in tool whose kind is more specific than a
    /// generic tool.
    fn claude_builtin_kind(name: &str) -> Option<ActivityKind> {
        match name {
            "Bash" => Some(ActivityKind::Command),
            "Edit" | "MultiEdit" | "NotebookEdit" | "Write" => Some(ActivityKind::FileChange),
            "Glob" | "LS" => Some(ActivityKind::List),
            "Grep" => Some(ActivityKind::Search),
            "Read" => Some(ActivityKind::Read),
            _ => None,
        }
    }

    /// Fills the facts a Claude tool's input implies: a read's line range,
    /// and an edit's or write's approximate line counts, which count every
    /// replaced line as removed and every replacement line as added.
    fn claude_input_facts(name: &str, input: &Value, facts: &mut ActivityFacts) {
        let text = |value: &Value, key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let edit_stat = |edit: &Value| DiffStat {
            added: line_count(&text(edit, "new_string")),
            removed: line_count(&text(edit, "old_string")),
        };
        match name {
            "Edit" => facts.diff_stat = Some(edit_stat(input)),
            "MultiEdit" => {
                let edits = input.get("edits").and_then(Value::as_array);
                facts.diff_stat = edits.map(|edits| {
                    edits
                        .iter()
                        .map(edit_stat)
                        .fold(DiffStat::default(), |total, stat| DiffStat {
                            added: total.added + stat.added,
                            removed: total.removed + stat.removed,
                        })
                });
            }
            "Write" => {
                facts.diff_stat = Some(DiffStat {
                    added: line_count(&text(input, "content")),
                    removed: 0,
                });
            }
            "Read" => {
                let number = |key: &str| input.get(key).and_then(Value::as_u64);
                facts.lines = LineRange::from_window(number("offset"), number("limit"));
            }
            _ => {}
        }
    }

    fn record(
        &mut self,
        id: &str,
        kind: ActivityKind,
        name: &str,
        parent: Option<&str>,
        status: ActivityStatus,
        details: ActivityDetails,
    ) {
        if !Self::valid_id(id) || self.excluded_ids.contains(id) {
            return;
        }
        let event = if let Some(previous) = self.calls.get(id) {
            if previous.status != ActivityStatus::Running && previous.status != status {
                return;
            }
            let mut event = previous.clone();
            event.status = status;
            if name != "tool" {
                event.name = Self::display_name(name);
                event.kind = kind;
            }
            details.apply(&mut event);
            if event == *previous {
                return;
            }
            event.observed_at = SystemTime::now();

            event
        } else {
            if self.calls.len() + self.excluded_ids.len() >= MAX_ACTIVITIES {
                return;
            }
            ActivityEvent {
                attempt_id: self.attempt_id.clone(),
                exit_code: details.exit_code,
                facts: Box::new(details.facts),
                id: id.to_string(),
                input: details.input,
                kind,
                name: Self::display_name(name),
                observed_at: SystemTime::now(),
                output: details.output,
                parent_id: parent.filter(|id| Self::valid_id(id)).map(str::to_string),
                status,
            }
        };
        self.calls.insert(id.to_string(), event.clone());
        (self.emit)(event);
    }

    fn display_name(name: &str) -> String {
        name.chars()
            .filter(|character| character.is_alphanumeric() || "_-./:".contains(*character))
            .take(128)
            .collect()
    }

    fn valid_id(id: &str) -> bool {
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
    }
}

/// Counts the lines of `text`, so a file of three lines counts 3 with or
/// without a final newline.
fn line_count(text: &str) -> u64 {
    u64::try_from(text.lines().count()).unwrap_or(u64::MAX)
}

impl Drop for ActivityObserver {
    fn drop(&mut self) {
        for event in self
            .calls
            .values()
            .filter(|event| event.status == ActivityStatus::Running)
        {
            (self.emit)(ActivityEvent {
                status: ActivityStatus::Interrupted,
                observed_at: SystemTime::now(),
                ..event.clone()
            });
        }
    }
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
