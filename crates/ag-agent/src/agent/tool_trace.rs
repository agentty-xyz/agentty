//! Allowlisted tool metadata and opt-in, bounded content previews.

use std::io::{self, Write};

use ag_telemetry::KeyValue;
use serde_json::Value;

/// Maximum UTF-8 bytes exported for any one content preview.
const PREVIEW_BYTES: usize = 4096;

/// Provider fields whose meaning is known by the owning adapter.
#[derive(Default)]
pub(super) struct ToolMetadata<'a> {
    pub(super) command: Option<&'a str>,
    pub(super) input: Option<&'a Value>,
    pub(super) name: Option<&'a str>,
    pub(super) output: Option<&'a Value>,
    pub(super) stderr: Option<&'a Value>,
    pub(super) stdout: Option<&'a Value>,
}

impl ToolMetadata<'_> {
    /// Exports identities and sizes by default; content requires host opt-in.
    pub(super) fn attributes(&self, capture: bool) -> Vec<KeyValue> {
        let mut attributes = Vec::new();
        if let Some(name) = self.name.and_then(safe_identity) {
            attributes.push(KeyValue::new("gen_ai.tool.name", name.to_string()));
        }
        if let Some(executable) = self.command.and_then(executable_name) {
            attributes.push(KeyValue::new(
                "process.executable.name",
                executable.to_string(),
            ));
        }
        for (value, key, size_key, truncated_key) in [
            (
                self.output,
                "gen_ai.tool.call.result",
                "agentty.tool.output.bytes",
                "agentty.tool.output.truncated",
            ),
            (
                self.stdout,
                "agentty.tool.stdout",
                "agentty.tool.stdout.bytes",
                "agentty.tool.stdout.truncated",
            ),
            (
                self.stderr,
                "agentty.tool.stderr",
                "agentty.tool.stderr.bytes",
                "agentty.tool.stderr.truncated",
            ),
        ] {
            if let Some(value) = value.filter(|value| !value.is_null()) {
                let preview = Preview::new(value, capture);
                attributes.push(KeyValue::new(
                    size_key,
                    i64::try_from(preview.bytes).unwrap_or(i64::MAX),
                ));
                if capture {
                    preview.export(key, truncated_key, &mut attributes);
                }
            }
        }
        if capture {
            if let Some(command) = self.command.filter(|command| !command.trim().is_empty()) {
                Preview::text(command, true).export(
                    "agentty.tool.command",
                    "agentty.tool.command.truncated",
                    &mut attributes,
                );
            }
            if let Some(input) = self.input.filter(|value| !value.is_null()) {
                Preview::new(input, true).export(
                    "gen_ai.tool.call.arguments",
                    "agentty.tool.input.truncated",
                    &mut attributes,
                );
            }
        }

        attributes
    }
}

/// Restricts provider-controlled identities to short identifiers, not content.
pub(super) fn safe_identity(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        && !sensitive_text(value))
    .then_some(value)
}

fn executable_name(command: &str) -> Option<&str> {
    let word = command.split_ascii_whitespace().next()?;
    if word.contains('=')
        || word.starts_with('\'') && !word.ends_with('\'')
        || word.starts_with('"') && !word.ends_with('"')
    {
        return None;
    }
    let executable = word.trim_matches(['\'', '"']);
    let basename = executable.rsplit(['/', '\\']).next()?;

    safe_identity(basename)
}

/// Conservatively suppresses whole values containing common credential markers.
/// Arbitrary secrets cannot be recognized reliably; capture remains opt-in.
fn sensitive_text(text: &str) -> bool {
    const MARKERS: [&str; 15] = [
        "password",
        "passwd",
        "secret",
        "token",
        "api_key",
        "api-key",
        "apikey",
        "authorization",
        "bearer ",
        "private key",
        "sk-",
        "ghp_",
        "github_pat_",
        "AKIA",
        "credential",
    ];

    MARKERS.iter().any(|marker| {
        text.as_bytes()
            .windows(marker.len())
            .any(|window| window.eq_ignore_ascii_case(marker.as_bytes()))
    }) || text.split_whitespace().any(|word| {
        word.split_once("://").is_some_and(|(_, rest)| {
            rest.split('/')
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    })
}

fn sensitive_value(value: &Value) -> bool {
    match value {
        Value::String(text) => sensitive_text(text),
        Value::Array(values) => values.iter().any(sensitive_value),
        Value::Object(fields) => fields
            .iter()
            .any(|(key, value)| sensitive_text(key) || sensitive_value(value)),
        _ => false,
    }
}

/// Counts output bytes without serializing when capture is disabled, and
/// retains at most one preview when enabled.
struct Preview {
    bytes: usize,
    content: Vec<u8>,
    retain: bool,
    structured: bool,
}

impl Preview {
    fn new(value: &Value, capture: bool) -> Self {
        if let Some(text) = value.as_str() {
            return Self::text(text, capture);
        }
        if !capture {
            return Self {
                bytes: Self::json_bytes(value),
                content: Vec::new(),
                retain: false,
                structured: true,
            };
        }
        let redacted = sensitive_value(value);
        let mut preview = Self {
            bytes: 0,
            content: Vec::new(),
            retain: !redacted,
            structured: true,
        };
        // JSON values and this in-memory writer have no fallible I/O.
        let _ = serde_json::to_writer(&mut preview, value);
        if redacted {
            preview.content.extend_from_slice(br#""[REDACTED]""#);
        }

        preview
    }

    /// Exact compact JSON length, including escaping, without encoding values.
    fn json_bytes(value: &Value) -> usize {
        match value {
            Value::Null => 4,
            Value::Bool(value) => {
                if *value {
                    4
                } else {
                    5
                }
            }
            Value::Number(value) => value.to_string().len(),
            Value::String(text) => Self::json_string_bytes(text),
            Value::Array(values) => values.iter().fold(
                2_usize.saturating_add(values.len().saturating_sub(1)),
                |bytes, value| bytes.saturating_add(Self::json_bytes(value)),
            ),
            Value::Object(fields) => fields.iter().fold(
                2_usize.saturating_add(fields.len().saturating_sub(1)),
                |bytes, (key, value)| {
                    bytes
                        .saturating_add(Self::json_string_bytes(key))
                        .saturating_add(1)
                        .saturating_add(Self::json_bytes(value))
                },
            ),
        }
    }

    fn json_string_bytes(text: &str) -> usize {
        text.bytes()
            .fold(text.len().saturating_add(2), |bytes, byte| {
                bytes.saturating_add(match byte {
                    b'"' | b'\\' | b'\x08' | b'\x0c' | b'\n' | b'\r' | b'\t' => 1,
                    0..=0x1f => 5,
                    _ => 0,
                })
            })
    }

    fn text(text: &str, capture: bool) -> Self {
        let redacted = capture && sensitive_text(text);
        let mut preview = Self {
            bytes: text.len(),
            content: Vec::new(),
            retain: capture && !redacted,
            structured: false,
        };
        if redacted {
            preview.content.extend_from_slice(b"[REDACTED]");
        } else if capture {
            preview
                .content
                .extend_from_slice(&text.as_bytes()[..text.len().min(PREVIEW_BYTES)]);
        }

        preview
    }

    fn export(
        self,
        key: &'static str,
        truncated_key: &'static str,
        attributes: &mut Vec<KeyValue>,
    ) {
        let truncated = self.retain && self.bytes > self.content.len();
        let valid_bytes = match std::str::from_utf8(&self.content) {
            Ok(_) => self.content.len(),
            Err(error) => error.valid_up_to(),
        };
        let content = String::from_utf8_lossy(&self.content[..valid_bytes]).into_owned();
        // A JSON prefix is useful as text, but must not occupy an attribute
        // whose consumers expect a complete structured value.
        let attribute = if truncated && self.structured {
            KeyValue::new(
                format!("{}.preview", truncated_key.trim_end_matches(".truncated")),
                content,
            )
        } else {
            KeyValue::new(key, content)
        };
        attributes.push(attribute);
        attributes.push(KeyValue::new(truncated_key, truncated));
    }
}

impl Write for Preview {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        if self.retain {
            let remaining = PREVIEW_BYTES.saturating_sub(self.content.len());
            self.content
                .extend_from_slice(&buffer[..buffer.len().min(remaining)]);
        }

        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tool_trace_test.rs"]
mod tests;
