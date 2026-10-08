//! Provider operation timing and bounded tool metadata for one execution
//! attempt.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime};

use ag_session::AgentKind;
use ag_telemetry::{CaptureToolContent, Context, KeyValue, Outcome, Span, TraceContextExt as _};
use serde_json::Value;

use super::tool_trace::{ToolMetadata, safe_identity};

/// Bounds provider-controlled operation identities retained by one attempt.
const MAX_OPERATIONS: usize = 4096;

/// Retains child spans until matching completion or attempt cancellation.
pub(crate) struct OperationTrace {
    completed: HashSet<String>,
    context: Context,
    operations: HashMap<String, Span>,
    started_at: SystemTime,
}

impl OperationTrace {
    /// Captures the attempt context before reading provider events.
    pub(crate) fn new() -> Self {
        Self {
            completed: HashSet::new(),
            context: Context::current(),
            operations: HashMap::new(),
            started_at: SystemTime::now(),
        }
    }

    /// Reads allowlisted operation metadata. Tool content is exported only
    /// under the host's explicit content-capture policy.
    pub(crate) fn observe(&mut self, kind: AgentKind, payload: &Value) {
        if !self.context.span().is_recording() {
            return;
        }

        match kind {
            AgentKind::Codex => self.codex(payload),
            AgentKind::Gemini => self.gemini(payload),
            AgentKind::Antigravity => self.antigravity(payload),
            AgentKind::Claude => self.claude(payload),
            AgentKind::Harness => {}
        }
    }

    /// Observes a raw CLI line without changing its delivery or retained bytes.
    pub(crate) fn observe_line(&mut self, kind: AgentKind, line: &[u8]) {
        if self.context.span().is_recording()
            && let Ok(payload) = serde_json::from_slice(line)
        {
            self.observe(kind, &payload);
        }
    }

    /// Records terminal items after the caller validates a successful
    /// completion for the active Codex turn and thread. Item identities
    /// deduplicate snapshots against standalone completion notifications.
    pub(crate) fn observe_codex_completed_turn(&mut self, payload: &Value) {
        if !self.context.span().is_recording() {
            return;
        }
        let Some(items) = payload
            .get("params")
            .and_then(|params| params.get("turn"))
            .and_then(|turn| turn.get("items"))
            .and_then(Value::as_array)
        else {
            return;
        };

        for item in items {
            self.codex_item(item, true);
        }
    }

    fn codex(&mut self, payload: &Value) {
        let Some(method) = payload.get("method").and_then(Value::as_str) else {
            return;
        };
        if !matches!(method, "item/started" | "item/completed") {
            return;
        }
        let Some(item) = payload.get("params").and_then(|params| params.get("item")) else {
            return;
        };

        self.codex_item(item, method == "item/completed");
    }

    fn codex_item(&mut self, item: &Value, completed: bool) {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            return;
        };
        let category = match item.get("type").and_then(Value::as_str) {
            Some("commandExecution" | "command_execution") => "command",
            Some("fileChange" | "file_change") => "file_change",
            Some("mcpToolCall" | "mcp_tool_call") => "mcp",
            Some("dynamicToolCall" | "dynamic_tool_call") => "dynamic_tool",
            Some("webSearch" | "web_search") => "web_search",
            Some("reasoning") => "reasoning",
            Some("agentMessage" | "agent_message") => "response",
            Some("contextCompaction" | "context_compaction") => "compaction",
            Some("collabAgentToolCall" | "collab_agent_tool_call") => "subagent",
            Some("imageGeneration" | "image_generation") => "image_generation",
            _ => return,
        };
        let outcome = completed.then(|| Self::outcome(item));
        let mut attributes = Vec::new();
        if let Some(exit_code) = item.get("exitCode").and_then(Value::as_i64) {
            attributes.push(KeyValue::new("agentty.provider.exit_code", exit_code));
        }
        if matches!(category, "command" | "mcp" | "dynamic_tool") {
            attributes.extend(
                ToolMetadata {
                    command: item.get("command").and_then(Value::as_str),
                    input: item.get("arguments"),
                    name: item.get("tool").and_then(Value::as_str),
                    output: ["aggregatedOutput", "result", "contentItems", "error"]
                        .into_iter()
                        .filter_map(|field| item.get(field))
                        .find(|value| !value.is_null()),
                    ..ToolMetadata::default()
                }
                .attributes(self.capture_content()),
            );
        }
        self.record(
            id,
            category,
            outcome,
            item.get("durationMs").and_then(Value::as_f64),
            attributes,
        );
    }

    fn gemini(&mut self, payload: &Value) {
        if payload.get("method").and_then(Value::as_str) != Some("session/update") {
            return;
        }
        let Some(update) = payload
            .get("params")
            .and_then(|params| params.get("update"))
        else {
            return;
        };
        if !matches!(
            update.get("sessionUpdate").and_then(Value::as_str),
            Some("tool_call" | "tool_call_update")
        ) {
            return;
        }
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return;
        };
        let category = match update.get("kind").and_then(Value::as_str) {
            Some("read") => "read",
            Some("edit") => "file_change",
            Some("execute") => "command",
            Some("search") => "search",
            Some("fetch") => "fetch",
            _ => "tool",
        };
        let outcome = match update.get("status").and_then(Value::as_str) {
            Some("completed") => Some(Outcome::Completed),
            Some("failed") => Some(Outcome::Failed),
            Some("canceled" | "cancelled") => Some(Outcome::Canceled),
            _ => None,
        };
        let input = update.get("rawInput");
        let output = update.get("rawOutput");
        let attributes = ToolMetadata {
            command: input
                .and_then(|input| input.get("command"))
                .and_then(Value::as_str),
            input,
            name: update.get("name").and_then(Value::as_str),
            output: output
                .filter(|value| !value.is_null())
                .or_else(|| update.get("content")),
            stderr: output.and_then(|output| output.get("stderr")),
            stdout: output.and_then(|output| output.get("stdout")),
        }
        .attributes(self.capture_content());
        self.record(id, category, outcome, None, attributes);
    }

    fn antigravity(&mut self, payload: &Value) {
        if payload.get("event").and_then(Value::as_str) != Some("step_update") {
            return;
        }
        let Some(step) = payload.get("step_update") else {
            return;
        };
        let Some(index) = step.get("step_index").and_then(Value::as_u64) else {
            return;
        };
        let category = match step.get("step_type").and_then(Value::as_str) {
            Some("tool")
                if step
                    .get("subagent_info")
                    .is_some_and(|info| !info.is_null()) =>
            {
                "subagent"
            }
            Some("tool") => "tool",
            Some("agent_response") => "response",
            Some("reasoning" | "thought") => "reasoning",
            Some("context_compaction" | "context-compaction" | "context_compression") => {
                "compaction"
            }
            Some("checkpoint") => "checkpoint",
            _ => return,
        };
        let outcome = match step.get("state").and_then(Value::as_str) {
            Some(state) if state.eq_ignore_ascii_case("active") => None,
            Some(state) if state.eq_ignore_ascii_case("done") => {
                Some(Self::outcome(step.get("tool_info").unwrap_or(step)))
            }
            _ => return,
        };
        let mut attributes = Self::usage(step.get("usage"));
        if matches!(category, "tool" | "subagent") {
            attributes.extend(
                ToolMetadata {
                    name: step.get("tool_name").and_then(Value::as_str),
                    ..ToolMetadata::default()
                }
                .attributes(self.capture_content()),
            );
        }
        self.record(
            &index.to_string(),
            category,
            outcome,
            step.get("duration_seconds")
                .and_then(Value::as_f64)
                .map(|seconds| seconds * 1000.0),
            attributes,
        );
    }

    fn claude(&mut self, payload: &Value) {
        let message = if payload.get("type").and_then(Value::as_str) == Some("message") {
            payload
        } else if let Some(message) = payload.get("message") {
            message
        } else {
            return;
        };
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return;
        };
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    if let Some(id) = block.get("id").and_then(Value::as_str) {
                        let category = match block.get("name").and_then(Value::as_str) {
                            Some("Bash") => "command",
                            Some("Read") => "read",
                            Some("Edit" | "Write" | "MultiEdit") => "file_change",
                            Some("Grep" | "Glob") => "search",
                            Some("WebFetch") => "fetch",
                            Some("WebSearch") => "web_search",
                            Some("Agent" | "Task") => "subagent",
                            _ => "tool",
                        };
                        let input = if category == "subagent"
                            || block.get("name").and_then(Value::as_str) == Some("StructuredOutput")
                        {
                            None
                        } else {
                            block.get("input")
                        };
                        let attributes = ToolMetadata {
                            command: input
                                .and_then(|input| input.get("command"))
                                .and_then(Value::as_str),
                            input,
                            name: block.get("name").and_then(Value::as_str),
                            ..ToolMetadata::default()
                        }
                        .attributes(self.capture_content());
                        self.record(id, category, None, None, attributes);
                    }
                }
                Some("tool_result") => {
                    if let Some(id) = block.get("tool_use_id").and_then(Value::as_str) {
                        let outcome =
                            if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                                Outcome::Failed
                            } else {
                                Outcome::Completed
                            };
                        let attributes = ToolMetadata {
                            output: block.get("content"),
                            ..ToolMetadata::default()
                        }
                        .attributes(self.capture_content());
                        self.record(id, "tool", Some(outcome), None, attributes);
                    }
                }
                _ => {}
            }
        }
    }

    fn record(
        &mut self,
        id: &str,
        category: &'static str,
        outcome: Option<Outcome>,
        duration_ms: Option<f64>,
        attributes: Vec<KeyValue>,
    ) {
        if id.is_empty() || id.len() > 256 || self.completed.contains(id) {
            return;
        }
        let duration_ms = duration_ms.filter(|duration| duration.is_finite() && *duration >= 0.0);
        let span = self.operations.remove(id);
        if span.is_none() && self.operations.len() + self.completed.len() >= MAX_OPERATIONS {
            return;
        }
        let span = span.unwrap_or_else(|| {
            let now = SystemTime::now();
            let reported = outcome
                .and(duration_ms)
                .and_then(|duration| Duration::try_from_secs_f64(duration / 1000.0).ok());
            let start = reported.map_or(now, |duration| {
                now - duration.min(now.duration_since(self.started_at).unwrap_or_default())
            });
            let _guard = self.context.clone().attach();
            let name = match category {
                "response" => "agent.response",
                "reasoning" => "agent.reasoning",
                "compaction" => "agent.compaction",
                "checkpoint" => "agent.checkpoint",
                "subagent" => "agent.subagent",
                _ => "agent.tool",
            };
            let span = Span::child_at(name, start);
            span.attribute("agentty.provider.operation.type", category);
            if (name == "agent.tool" || name == "agent.subagent")
                && let Some(id) = safe_identity(id)
            {
                span.attribute("gen_ai.tool.call.id", id.to_string());
            }
            span.attribute(
                "agentty.timing.source",
                if reported.is_some() {
                    "provider"
                } else if outcome.is_some() {
                    "completion"
                } else {
                    "lifecycle"
                },
            );
            span
        });
        if let Some(duration) = duration_ms {
            span.attribute("agentty.provider.duration_ms", duration);
        }
        for attribute in attributes {
            span.context().span().set_attribute(attribute);
        }
        if let Some(outcome) = outcome {
            span.finish(outcome);
            self.completed.insert(id.to_string());
        } else {
            self.operations.insert(id.to_string(), span);
        }
    }

    fn outcome(value: &Value) -> Outcome {
        if matches!(
            value.get("status").and_then(Value::as_str),
            Some("canceled" | "cancelled" | "interrupted")
        ) {
            Outcome::Canceled
        } else if matches!(
            value.get("status").and_then(Value::as_str),
            Some("failed" | "declined" | "denied")
        ) || value.get("error").is_some_and(|error| !error.is_null())
            || value.get("success").and_then(Value::as_bool) == Some(false)
            || value
                .get("exitCode")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
        {
            Outcome::Failed
        } else {
            Outcome::Completed
        }
    }

    fn capture_content(&self) -> bool {
        self.context
            .get::<CaptureToolContent>()
            .is_some_and(|policy| policy.0)
    }

    fn usage(usage: Option<&Value>) -> Vec<KeyValue> {
        let Some(usage) = usage else {
            return Vec::new();
        };

        [
            ("input_tokens", "gen_ai.usage.input_tokens"),
            ("output_tokens", "gen_ai.usage.output_tokens"),
            ("cache_read_tokens", "gen_ai.usage.cache_read.input_tokens"),
            ("thinking_tokens", "gen_ai.usage.reasoning.output_tokens"),
        ]
        .into_iter()
        .filter_map(|(field, key)| {
            usage
                .get(field)
                .and_then(Value::as_u64)
                .and_then(|tokens| i64::try_from(tokens).ok())
                .map(|tokens| KeyValue::new(key, tokens))
        })
        .collect()
    }
}

#[cfg(test)]
#[path = "trace_test.rs"]
mod tests;
