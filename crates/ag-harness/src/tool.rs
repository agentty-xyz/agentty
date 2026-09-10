use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value, json};

use crate::{model, schema_contract};

const READ_DESCRIPTION: &str = concat!(
    "Inspect the repository with one bounded read-only action. Use `file` with `path` and ",
    "optional `offset`/`limit` for worktree text; `list` with optional `path`/`limit`; ",
    "`search` with `query` and optional `path`/`limit`; `diff` with optional `path` for ",
    "changes from `main`; or `show` with `path`, `side` (`base` for `main` or `head`), ",
    "and optional `offset`/`limit`."
);
const READ_NAME: &str = "read";
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_TOOL_CALL_ID_BYTES: usize = 1024;
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const WRITE_DESCRIPTION: &str = concat!(
    "Apply one unified diff to one repository-relative text file. To create an empty file, use ",
    "only `--- /dev/null` and `+++ b/<path>` headers."
);
const WRITE_NAME: &str = "write";

/// Built-in tool that can be enabled for a harness run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Read-only repository inspection.
    Read,
    /// Repository-relative patch writes.
    Write,
}

/// Provider-neutral definition of a native model tool.
///
/// Definitions describe only the wire contract advertised to a model. They do
/// not execute tools or access the filesystem.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    description: &'static str,
    name: &'static str,
    parameters: Value,
}

impl ToolDefinition {
    /// Defines the native `read` function tool.
    pub fn read() -> Self {
        Self {
            description: READ_DESCRIPTION,
            name: READ_NAME,
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["file", "list", "search", "diff", "show"]
                    },
                    "path": nullable_schema(repository_path_schema()),
                    "query": {
                        "type": ["string", "null"],
                        "minLength": 1,
                        "maxLength": MAX_QUERY_BYTES,
                        "pattern": "^[^\\u0000]*$"
                    },
                    "side": {
                        "type": ["string", "null"],
                        "enum": ["base", "head", null]
                    },
                    "offset": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": u64::MAX
                    },
                    "limit": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": u64::MAX
                    }
                },
                "additionalProperties": false
            }),
        }
    }

    /// Defines the native `write` function tool.
    pub fn write() -> Self {
        Self {
            description: WRITE_DESCRIPTION,
            name: WRITE_NAME,
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": repository_path_schema(),
                    "patch": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_PATCH_BYTES
                    }
                },
                "required": ["path", "patch"],
                "additionalProperties": false
            }),
        }
    }

    /// Returns the description sent with the native function definition.
    pub fn description(&self) -> &'static str {
        self.description
    }

    /// Returns the native function name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the JSON Schema for the native function arguments.
    pub fn parameters(&self) -> &Value {
        &self.parameters
    }
}

fn repository_path_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_PATH_BYTES,
        "pattern": "^(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+)(?:/(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+))*$",
        "not": {
            "type": "string",
            "pattern": "(^|/)\\.[gG][iI][tT](/|$)"
        }
    })
}

fn nullable_schema(mut schema: Value) -> Value {
    schema["type"] = json!(["string", "null"]);

    schema
}

/// Provider-neutral model request for one native tool invocation.
#[derive(Clone, Eq, PartialEq)]
pub struct ToolCall {
    arguments: ToolArguments,
    id: String,
    reasoning_content: Option<String>,
}

impl ToolCall {
    /// Decodes a built-in tool call returned by a model adapter.
    ///
    /// The call identifier must contain non-whitespace text and be at most
    /// 1,024 UTF-8 bytes. Accepted identifiers are preserved verbatim.
    /// Arguments and optional provider reasoning are bounded before decoding.
    /// Structurally valid read-action mistakes are retained for corrective
    /// tool feedback. The harness separately enforces tool permissions,
    /// repository containment, batch identifiers, and execution limits.
    ///
    /// # Errors
    /// Returns [`crate::ModelError`] for an invalid call identifier,
    /// unsupported tool name, oversized content, or invalid JSON arguments,
    /// including unsafe repository paths.
    pub fn from_json(
        id: String,
        name: &str,
        arguments: &str,
        reasoning_content: Option<String>,
    ) -> Result<Self, model::ModelError> {
        if id.len() > MAX_TOOL_CALL_ID_BYTES || id.trim().is_empty() {
            return Err(model::ModelError::InvalidToolCallId);
        }

        schema_contract::ensure_content_size(arguments).map_err(model::ModelError::from)?;
        if let Some(reasoning_content) = &reasoning_content {
            schema_contract::ensure_content_size(reasoning_content)
                .map_err(model::ModelError::from)?;
        }
        let arguments = match name {
            READ_NAME => serde_json::from_str(arguments).map(ToolArguments::Read),
            WRITE_NAME => serde_json::from_str(arguments).map(ToolArguments::Write),
            _ => {
                return Err(model::ModelError::UnsupportedToolName {
                    name: schema_contract::bounded_diagnostic(name),
                });
            }
        }
        .map_err(|error| model::ModelError::InvalidToolArguments {
            reason: schema_contract::bounded_diagnostic(error),
        })?;

        Ok(Self {
            arguments,
            id,
            reasoning_content,
        })
    }

    /// Returns the typed arguments for this native tool call.
    pub fn arguments(&self) -> ToolCallArguments<'_> {
        match &self.arguments {
            ToolArguments::Read(arguments) => ToolCallArguments::Read(arguments),
            ToolArguments::Write(arguments) => ToolCallArguments::Write(arguments),
        }
    }

    /// Returns typed `read` arguments when this is a `read` call.
    pub fn read_arguments(&self) -> Option<&ReadArguments> {
        match &self.arguments {
            ToolArguments::Read(arguments) => Some(arguments),
            ToolArguments::Write(_) => None,
        }
    }

    /// Returns typed `write` arguments when this is a `write` call.
    pub fn write_arguments(&self) -> Option<&WriteArguments> {
        match &self.arguments {
            ToolArguments::Read(_) => None,
            ToolArguments::Write(arguments) => Some(arguments),
        }
    }

    /// Returns the provider-assigned call identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the requested native function name.
    pub fn name(&self) -> &'static str {
        match self.arguments {
            ToolArguments::Read(_) => READ_NAME,
            ToolArguments::Write(_) => WRITE_NAME,
        }
    }

    /// Serializes the validated arguments for replay to a provider.
    ///
    /// # Errors
    /// Returns an error if the argument values cannot be serialized as JSON.
    pub fn arguments_json(&self) -> Result<String, serde_json::Error> {
        match &self.arguments {
            ToolArguments::Read(arguments) => serde_json::to_string(arguments),
            ToolArguments::Write(arguments) => serde_json::to_string(arguments),
        }
    }

    /// Returns optional provider reasoning needed to replay the assistant
    /// tool call. This sensitive content is redacted from debug output.
    pub fn reasoning_content(&self) -> Option<&str> {
        self.reasoning_content.as_deref()
    }

    pub(crate) fn read(
        id: String,
        arguments: ReadArguments,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            arguments: ToolArguments::Read(arguments),
            id,
            reasoning_content,
        }
    }

    pub(crate) fn write(
        id: String,
        arguments: WriteArguments,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            arguments: ToolArguments::Write(arguments),
            id,
            reasoning_content,
        }
    }
}

impl fmt::Debug for ToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCall")
            .field("arguments", &self.arguments)
            .field("id", &self.id)
            .field("name", &self.name())
            .field(
                "reasoning_content",
                &self.reasoning_content.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ToolArguments {
    Read(ReadArguments),
    Write(WriteArguments),
}

/// Borrowed typed arguments for one native tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCallArguments<'a> {
    /// Arguments for a repository read.
    Read(&'a ReadArguments),
    /// Arguments for a repository patch write.
    Write(&'a WriteArguments),
}

/// Structurally validated arguments for one native read-only repository action.
///
/// Omitting `action` preserves the original file-read contract. The other
/// fields are action-specific. Schema-valid field combinations rejected by an
/// action are retained so the harness can return corrective tool feedback.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadArguments {
    #[serde(default, skip_serializing_if = "is_default")]
    action: ReadAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<ReadSide>,
    #[serde(skip)]
    validation_error: Option<&'static str>,
}

impl ReadArguments {
    /// Returns the selected repository inspection action.
    pub fn action(&self) -> ReadAction {
        self.action
    }

    /// Returns the optional positive maximum line count.
    pub fn limit(&self) -> Option<u64> {
        self.limit.map(NonZeroU64::get)
    }

    /// Returns the optional one-based starting line.
    pub fn offset(&self) -> Option<u64> {
        self.offset.map(NonZeroU64::get)
    }

    /// Returns the repository-relative path to read.
    pub fn path(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }

    /// Returns an optional path filter for actions that do not require a path.
    pub fn path_filter(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Returns the literal search query for a `search` action.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    /// Returns the selected revision side for a `show` action.
    pub fn side(&self) -> Option<ReadSide> {
        self.side
    }

    pub(crate) fn validation_error(&self) -> Option<&'static str> {
        self.validation_error
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgumentsWire {
    #[serde(default)]
    action: ReadAction,
    #[serde(default, deserialize_with = "deserialize_optional_positive_integer")]
    limit: Option<NonZeroU64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_integer")]
    offset: Option<NonZeroU64>,
    #[serde(default, deserialize_with = "deserialize_optional_repository_path")]
    path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_query")]
    query: Option<String>,
    side: Option<ReadSide>,
}

impl<'de> Deserialize<'de> for ReadArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let arguments = ReadArgumentsWire::deserialize(deserializer)?;
        let validation_error = match arguments.action {
            ReadAction::Diff
                if arguments.limit.is_none()
                    && arguments.offset.is_none()
                    && arguments.query.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::File
                if arguments.path.is_some()
                    && arguments.query.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::List
                if arguments.offset.is_none()
                    && arguments.query.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::Search
                if arguments.query.is_some()
                    && arguments.offset.is_none()
                    && arguments.side.is_none() =>
            {
                None
            }
            ReadAction::Show
                if arguments.path.is_some()
                    && arguments.side.is_some()
                    && arguments.query.is_none() =>
            {
                None
            }
            ReadAction::Diff => Some("diff accepts only an optional path"),
            ReadAction::File => Some("file requires a path and accepts only offset and limit"),
            ReadAction::List => Some("list accepts only an optional path and limit"),
            ReadAction::Search => {
                Some("search requires a query and accepts only an optional path and limit")
            }
            ReadAction::Show => {
                Some("show requires a path and side and accepts only offset and limit")
            }
        };

        Ok(Self {
            action: arguments.action,
            limit: arguments.limit,
            offset: arguments.offset,
            path: arguments.path,
            query: arguments.query,
            side: arguments.side,
            validation_error,
        })
    }
}

/// Read-only operation selected within the built-in `read` tool.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadAction {
    /// Host-bound review diff.
    Diff,
    /// Current worktree file content.
    #[default]
    File,
    /// Repository path discovery.
    List,
    /// Literal repository text search.
    Search,
    /// File content from the base or `HEAD` revision.
    Show,
}

impl ReadAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::File => "file",
            Self::List => "list",
            Self::Search => "search",
            Self::Show => "show",
        }
    }
}

/// Revision side available to the `show` read action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSide {
    /// Built-in `main` review base.
    Base,
    /// Current `HEAD` commit.
    Head,
}

/// Validated arguments for the native `write` function.
///
/// `path` names exactly one repository-relative text file and `patch` is a
/// standard unified diff that creates or updates that same file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteArguments {
    #[serde(deserialize_with = "deserialize_bounded_patch")]
    patch: String,
    #[serde(deserialize_with = "deserialize_repository_path")]
    path: String,
}

impl WriteArguments {
    /// Returns the unified diff supplied by the model.
    pub fn patch(&self) -> &str {
        &self.patch
    }

    /// Returns the repository-relative path to write.
    pub fn path(&self) -> &str {
        &self.path
    }
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    value == &T::default()
}

fn deserialize_bounded_patch<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let patch = String::deserialize(deserializer)?;
    if patch.is_empty() {
        return Err(de::Error::custom("patch must not be empty"));
    }
    if patch.len() > MAX_PATCH_BYTES {
        return Err(de::Error::custom("patch exceeds the byte limit"));
    }

    Ok(patch)
}

fn deserialize_optional_positive_integer<'de, D>(
    deserializer: D,
) -> Result<Option<NonZeroU64>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(number) = Option::<Number>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_positive_json_integer(&number.to_string())
        .and_then(NonZeroU64::new)
        .map(Some)
        .ok_or_else(|| de::Error::custom("number must be an integer from 1 through u64::MAX"))
}

fn deserialize_optional_repository_path<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(validate_repository_path)
        .transpose()
        .map_err(de::Error::custom)
}

fn deserialize_optional_query<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(validate_query)
        .transpose()
        .map_err(de::Error::custom)
}

fn validate_query(query: String) -> Result<String, &'static str> {
    if query.is_empty() {
        return Err("query must not be empty");
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err("query exceeds the byte limit");
    }
    if query.contains('\0') {
        return Err("query must not contain NUL");
    }

    Ok(query)
}

fn parse_positive_json_integer(number: &str) -> Option<u64> {
    let (mantissa, exponent) = match number.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i64>().ok()?),
        None => (number, 0),
    };
    if mantissa.starts_with('-') {
        return None;
    }
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = String::with_capacity(whole.len() + fraction.len());
    digits.push_str(whole);
    digits.push_str(fraction);
    let scale = exponent.checked_sub(i64::try_from(fraction.len()).ok()?)?;
    let appended_zeros = if scale < 0 {
        let removed_digits = usize::try_from(scale.unsigned_abs()).ok()?;
        if removed_digits >= digits.len()
            || !digits[digits.len() - removed_digits..]
                .bytes()
                .all(|digit| digit == b'0')
        {
            return None;
        }
        digits.truncate(digits.len() - removed_digits);

        0
    } else {
        usize::try_from(scale).ok()?
    };
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() || digits.len().checked_add(appended_zeros)? > 20 {
        return None;
    }
    let value = digits.parse::<u64>().ok()?;

    (0..appended_zeros).try_fold(value, |value, _| value.checked_mul(10))
}

fn deserialize_repository_path<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let path = String::deserialize(deserializer)?;
    validate_repository_path(path).map_err(de::Error::custom)
}

fn validate_repository_path(path: String) -> Result<String, &'static str> {
    if path.is_empty() {
        return Err("path must not be empty");
    }
    if path.len() > MAX_PATH_BYTES {
        return Err("path exceeds the byte limit");
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err("path must be repository-relative");
    }
    if path.contains('\0') {
        return Err("path must not contain NUL");
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(
            "path must not contain empty, current-directory, or parent-directory components",
        );
    }
    if path
        .split('/')
        .any(|component| component.eq_ignore_ascii_case(".git"))
    {
        return Err("path must not access Git administrative state");
    }

    Ok(path)
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
