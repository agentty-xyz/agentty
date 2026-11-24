//! Tool handlers, the built-in repository tools, and their typed arguments.
//!
//! A turn offers exactly the tools of its [`ToolSet`]: host
//! [`ToolHandler`]s and the built-in `read`, `write`, and `bash` handlers.
//! Every call runs through the handler of its name; a turn whose set holds
//! no tool offers none. Repository access goes through a validated
//! `Repository` and an injectable [`FileSystem`].

use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value, json};
use thiserror::Error;

use crate::bash::{BashArguments, BashConfig, Enforcement};
pub use crate::file_system::{FileSystem, LocalFileSystem};
pub use crate::read::{ReadError, ReadOutput};
pub use crate::tool_handler::{
    ToolContext, ToolEffects, ToolError, ToolHandler, ToolOutput, ToolSet, ToolSetBuilder,
    ToolSetError,
};
pub use crate::write::{WriteError, WriteOutput};
use crate::{ComparisonBase, model, schema_contract, write};

/// `bash` isolation clause when the executor enforces nothing.
const BASH_UNSANDBOXED: &str = "Run Bash directly on the host without a sandbox. Commands run \
                                with the host process's own access; writes and network access are \
                                not restricted by the harness.";
/// `bash` isolation clause when only the filesystem policy is enforced.
const BASH_FILE_SYSTEM_SANDBOX: &str = "Run Bash inside a host filesystem sandbox. Workspace \
                                        reads are allowed; writes require host grants. Network \
                                        access is not restricted by the harness.";
/// `bash` isolation clause when filesystem and network policy are enforced.
const BASH_FULL_SANDBOX: &str = "Run Bash inside the host sandbox. Workspace reads are allowed; \
                                 writes require host grants. Network access is denied.";
/// Start of the `bash` clause naming programs excluded from the sandbox.
const BASH_EXCLUDED_PREFIX: &str = "Programs excluded from the sandbox:";
/// End of the `bash` clause naming programs excluded from the sandbox.
const BASH_EXCLUDED_SUFFIX: &str = "A command that starts with one of them and consists only of \
                                    plain words, with no quotes, `$`, globs, `~`, or shell \
                                    operators, runs without a shell outside the sandbox with the \
                                    host's full access once the user explicitly approves it; \
                                    otherwise it runs inside the sandbox.";
/// `bash` limits that hold for every executor.
const BASH_LIMITS: &str = "Output and duration are bounded; long output keeps each stream's start \
                           and end. Applied writes are not rolled back.";
const READ_DESCRIPTION: &str = concat!(
    "Inspect the repository with one bounded read-only action. Use `file` with `path` and ",
    "optional `offset`/`limit` for worktree text; `list` with optional `path`/`limit`; ",
    "`search` with `query` and optional `path`/`limit`; `diff` with optional `path` for ",
    "changes from the host-selected commit; or `show` with `path`, `side` (`base` or `head`), ",
    "and optional `offset`/`limit`. Search queries are literal strings, not regular expressions. ",
    "File/show offsets are one-based line numbers and limits count lines. If truncated, ",
    "continue file/show at next_offset; narrow list/search/diff when no continuation is supplied."
);
const BASH_NAME: &str = "bash";
const READ_NAME: &str = "read";
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const MAX_QUERY_BYTES: usize = 4 * 1024;
const MAX_TOOL_CALL_ID_BYTES: usize = 1024;
/// Corrective feedback for a read path that is not a safe repository path.
const INVALID_READ_PATH: &str = "path must be repository-relative without empty, `.`, `..`, or \
                                 `.git` components; omit it to inspect the repository root";
pub(crate) const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const WRITE_DESCRIPTION: &str = concat!(
    "Apply one unified diff to one repository-relative text file. Hunk line counts are taken ",
    "from the hunk body. A hunk whose context and removed lines do not match at its `@@` start ",
    "line applies only where they match exactly one place in the file; copy those lines exactly ",
    "and add context when they repeat. A hunk with only `+` lines needs a `-N,0` header to ",
    "insert after line N. To create an empty file, use only `--- /dev/null` and `+++ b/<path>` ",
    "headers."
);
const WRITE_NAME: &str = "write";

/// A built-in tool, bound to a turn's repository and options when the turn
/// starts and dispatched, like every tool, through its handler. Variants
/// are declared, and so ordered, in their advertised order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Builtin {
    Read,
    Write,
    Bash,
}

impl Builtin {
    /// The built-in tools in their advertised order.
    pub(crate) const ALL: [Self; 3] = [Self::Read, Self::Write, Self::Bash];

    /// Returns the built-in tool named `name`, if any.
    pub(crate) fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|builtin| builtin.name() == name)
    }

    /// Returns the function name advertised to models.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Bash => BASH_NAME,
            Self::Read => READ_NAME,
            Self::Write => WRITE_NAME,
        }
    }

    /// Decodes this tool's JSON arguments text.
    fn decode(self, arguments: &str) -> Result<ToolArguments, serde_json::Error> {
        match self {
            Self::Bash => serde_json::from_str(arguments).map(ToolArguments::Bash),
            Self::Read => serde_json::from_str(arguments).map(ToolArguments::Read),
            Self::Write => serde_json::from_str(arguments).map(ToolArguments::Write),
        }
    }
}

/// Provider-neutral definition of a native model tool.
///
/// Definitions describe only the wire contract advertised to a model. They do
/// not execute tools or access the filesystem; a [`ToolHandler`] pairs one
/// with its execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDefinition {
    description: String,
    name: Cow<'static, str>,
    parameters: Value,
}

/// Why [`ToolDefinition::new`] rejected a definition.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum ToolDefinitionError {
    /// The name is not 1 to 64 ASCII letters, digits, `_`, or `-`, the
    /// function names every supported provider accepts and replays.
    #[error("tool name `{name}` must be 1 to 64 ASCII letters, digits, `_`, or `-`")]
    InvalidName {
        /// The rejected name, bounded.
        name: String,
    },
    /// The parameters are not a JSON Schema object with `"type": "object"`.
    #[error("tool parameters must be a JSON Schema object with \"type\": \"object\"")]
    InvalidParameters,
}

impl ToolDefinition {
    /// Defines a host-provided function tool named `name`, described to the
    /// model by `description`, whose arguments follow the JSON Schema
    /// `parameters`.
    ///
    /// The harness sends the definition as given and does not validate
    /// call arguments against `parameters`; the handler decodes them.
    ///
    /// # Errors
    /// Returns [`ToolDefinitionError`] for a name providers cannot replay or
    /// parameters that are not an object schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Result<Self, ToolDefinitionError> {
        let name = name.into();
        if !ag_router::is_valid_function_name(&name) {
            return Err(ToolDefinitionError::InvalidName {
                name: schema_contract::bounded_diagnostic(name),
            });
        }
        if parameters.get("type") != Some(&json!("object")) {
            return Err(ToolDefinitionError::InvalidParameters);
        }

        Ok(Self {
            description: description.into(),
            name: Cow::Owned(name),
            parameters,
        })
    }

    /// Defines shell source; the host fixes executable, grants, and limits.
    /// The description states exactly the isolation `enforcement` provides.
    pub fn bash(enforcement: Enforcement) -> Self {
        let isolation = match enforcement {
            Enforcement::None => BASH_UNSANDBOXED,
            Enforcement::FileSystem => BASH_FILE_SYSTEM_SANDBOX,
            Enforcement::FileSystemAndNetwork => BASH_FULL_SANDBOX,
        };

        Self {
            description: format!("{isolation} {BASH_LIMITS}"),
            name: Cow::Borrowed(BASH_NAME),
            parameters: json!({"type":"object", "properties":{"command":{"type":"string","minLength":1,"maxLength":65536}}, "required":["command"], "additionalProperties":false}),
        }
    }

    /// Defines shell source as [`Self::bash`] does for `configuration`'s
    /// enforcement, also naming the programs whose simple commands run
    /// outside the sandbox once approved, so the description never
    /// presents them as sandboxed.
    pub(crate) fn bash_for(configuration: &BashConfig) -> Self {
        let mut definition = Self::bash(configuration.enforcement());
        let programs = configuration
            .snapshot
            .excluded_commands
            .keys()
            .map(|program| format!("`{program}`"))
            .collect::<Vec<_>>();
        if configuration.enforcement() != Enforcement::None && !programs.is_empty() {
            definition.description = format!(
                "{} {BASH_EXCLUDED_PREFIX} {}. {BASH_EXCLUDED_SUFFIX}",
                definition.description,
                programs.join(", ")
            );
        }

        definition
    }

    /// Defines the native `read` function tool.
    pub fn read() -> Self {
        Self {
            description: READ_DESCRIPTION.to_string(),
            name: Cow::Borrowed(READ_NAME),
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

    /// Defines the read capabilities available with the turn's selected base.
    pub fn read_with_comparison_base(base: Option<&ComparisonBase>) -> Self {
        let mut definition = Self::read();
        if let Some(base) = base {
            definition.description =
                format!("{READ_DESCRIPTION} Comparison base OID: {}.", base.oid());
        } else {
            definition.parameters["properties"]["action"]["enum"] =
                json!(["file", "list", "search", "show"]);
            definition.parameters["properties"]["side"]["enum"] = json!(["head", null]);
            definition.description =
                "Inspect repository worktree text with file, list, or search, or use show with \
                 side head for committed text. No comparison base is configured; diff and \
                 show(base) are unavailable. File and show accept path and optional offset/limit; \
                 list accepts optional path/limit; search requires a literal query (not a regex) \
                 and accepts optional path/limit. File/show offsets are one-based lines. On \
                 truncation follow next_offset for file/show; narrow other requests."
                    .to_string();
        }

        definition
    }

    /// Defines the native `write` function tool.
    pub fn write() -> Self {
        Self {
            description: WRITE_DESCRIPTION.to_string(),
            name: Cow::Borrowed(WRITE_NAME),
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
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the native function name.
    pub fn name(&self) -> &str {
        &self.name
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
    assistant_text: Option<String>,
    id: String,
    reasoning_content: Option<String>,
}

impl ToolCall {
    /// Decodes a tool call returned by a model adapter.
    ///
    /// The call identifier must contain non-whitespace text and be at most
    /// 1,024 UTF-8 bytes. Accepted identifiers are preserved verbatim.
    /// Arguments and optional provider reasoning are bounded before decoding.
    /// Arguments sent as a JSON string that holds the arguments object are
    /// decoded from that string. Arguments the tool cannot decode, such as
    /// invalid JSON, an unknown
    /// field, an oversized read path, or an unsafe write path, are kept
    /// verbatim as [`MalformedArguments`] so the harness answers the call
    /// with an error result instead of running it. Structurally valid
    /// read-action mistakes, including unsafe read paths, are retained for
    /// corrective tool feedback. The harness separately enforces tool
    /// permissions, repository containment, batch identifiers, and execution
    /// limits. A call naming no built-in tool, such as a tool the model
    /// invented, is kept as an [`UnknownToolCall`] so the harness answers it
    /// with a correctable result listing the available tools; a name that
    /// providers could not replay, such as `container.exec`, is kept with
    /// each other character replaced by `_` and cut to 64 characters.
    ///
    /// # Errors
    /// Returns [`crate::ModelError`] for an invalid call identifier, an empty
    /// name, or oversized content.
    pub fn from_json(
        id: String,
        name: &str,
        arguments: &str,
        reasoning_content: Option<String>,
    ) -> Result<Self, model::ModelError> {
        if id.len() > MAX_TOOL_CALL_ID_BYTES || id.trim().is_empty() {
            return Err(model::ModelError::InvalidToolCallId);
        }

        schema_contract::ensure_content_size(arguments)
            .map_err(model::ModelError::from_validation)?;
        if let Some(reasoning_content) = &reasoning_content {
            schema_contract::ensure_content_size(reasoning_content)
                .map_err(model::ModelError::from_validation)?;
        }
        let tool = match Builtin::named(name) {
            Some(builtin) => builtin,
            None if let Some(replayable) = ag_router::replayable_function_name(name) => {
                return Ok(Self {
                    arguments: ToolArguments::Unknown(UnknownToolCall {
                        renamed: replayable != name,
                        name: replayable,
                        raw: arguments.to_string(),
                    }),
                    assistant_text: None,
                    id,
                    reasoning_content,
                });
            }
            None => {
                return Err(model::ModelError::UnsupportedToolName {
                    name: schema_contract::bounded_diagnostic(name),
                });
            }
        };
        // Some models send the arguments object JSON-encoded as a string; it
        // is decoded once more, and a failure reports the original error.
        let decoded = tool.decode(arguments).or_else(|error| {
            serde_json::from_str::<String>(arguments)
                .ok()
                .and_then(|inner| tool.decode(&inner).ok())
                .ok_or(error)
        });
        let arguments = decoded.unwrap_or_else(|error| {
            ToolArguments::Malformed(MalformedArguments {
                raw: arguments.to_string(),
                reason: schema_contract::bounded_diagnostic(error),
                tool,
            })
        });

        Ok(Self {
            arguments,
            assistant_text: None,
            id,
            reasoning_content,
        })
    }

    /// Attaches the assistant text the model wrote beside its calls, such as
    /// running notes on what it found so far. Text that is empty or only
    /// whitespace is not kept.
    ///
    /// A response's text belongs to its first call, like its reasoning. It
    /// is stored with the call, replayed beside it, and weighed in its
    /// message; context fitting keeps it while the call's round is sent.
    ///
    /// # Errors
    /// Returns [`crate::ModelError`] for oversized text.
    pub fn with_assistant_text(mut self, text: Option<String>) -> Result<Self, model::ModelError> {
        if let Some(text) = &text {
            schema_contract::ensure_content_size(text)
                .map_err(model::ModelError::from_validation)?;
        }
        self.assistant_text = text.filter(|text| !text.trim().is_empty());

        Ok(self)
    }

    /// Returns the typed arguments for this native tool call.
    pub fn arguments(&self) -> ToolCallArguments<'_> {
        match &self.arguments {
            ToolArguments::Bash(arguments) => ToolCallArguments::Bash(arguments),
            ToolArguments::Read(arguments) => ToolCallArguments::Read(arguments),
            ToolArguments::Write(arguments) => ToolCallArguments::Write(arguments),
            ToolArguments::Malformed(arguments) => ToolCallArguments::Malformed(arguments),
            ToolArguments::Unknown(call) => ToolCallArguments::Unknown(call),
        }
    }

    /// Returns the undecodable arguments when this call is malformed.
    pub fn malformed_arguments(&self) -> Option<&MalformedArguments> {
        match &self.arguments {
            ToolArguments::Malformed(arguments) => Some(arguments),
            ToolArguments::Bash(_)
            | ToolArguments::Read(_)
            | ToolArguments::Write(_)
            | ToolArguments::Unknown(_) => None,
        }
    }

    /// Returns the verbatim call when it names no built-in tool.
    pub fn unknown_tool(&self) -> Option<&UnknownToolCall> {
        match &self.arguments {
            ToolArguments::Unknown(call) => Some(call),
            ToolArguments::Bash(_)
            | ToolArguments::Read(_)
            | ToolArguments::Write(_)
            | ToolArguments::Malformed(_) => None,
        }
    }

    /// Returns typed `read` arguments when this is a `read` call.
    pub fn read_arguments(&self) -> Option<&ReadArguments> {
        match &self.arguments {
            ToolArguments::Read(arguments) => Some(arguments),
            ToolArguments::Write(_)
            | ToolArguments::Bash(_)
            | ToolArguments::Malformed(_)
            | ToolArguments::Unknown(_) => None,
        }
    }

    /// Returns typed `write` arguments when this is a `write` call.
    pub fn write_arguments(&self) -> Option<&WriteArguments> {
        match &self.arguments {
            ToolArguments::Read(_)
            | ToolArguments::Bash(_)
            | ToolArguments::Malformed(_)
            | ToolArguments::Unknown(_) => None,
            ToolArguments::Write(arguments) => Some(arguments),
        }
    }

    /// Returns the provider-assigned call identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the requested function name; for an [`UnknownToolCall`] it
    /// names no built-in tool.
    pub fn name(&self) -> &str {
        match &self.arguments {
            ToolArguments::Bash(_) => BASH_NAME,
            ToolArguments::Read(_) => READ_NAME,
            ToolArguments::Write(_) => WRITE_NAME,
            ToolArguments::Malformed(arguments) => arguments.tool.name(),
            ToolArguments::Unknown(call) => &call.name,
        }
    }

    /// Serializes the validated arguments for replay to a provider.
    ///
    /// # Errors
    /// Returns an error if the argument values cannot be serialized as JSON.
    pub fn arguments_json(&self) -> Result<String, serde_json::Error> {
        match &self.arguments {
            ToolArguments::Bash(arguments) => serde_json::to_string(arguments),
            ToolArguments::Read(arguments) => serde_json::to_string(arguments),
            ToolArguments::Write(arguments) => serde_json::to_string(arguments),
            ToolArguments::Malformed(arguments) => Ok(arguments.raw.clone()),
            ToolArguments::Unknown(call) => Ok(call.raw.clone()),
        }
    }

    /// Returns the arguments as the JSON object a [`ToolHandler`] receives.
    /// Blank verbatim arguments, which some providers send for a tool
    /// without parameters, read as an empty object.
    ///
    /// # Errors
    /// Returns why verbatim arguments are not a JSON object.
    pub(crate) fn arguments_value(&self) -> Result<Value, String> {
        let value = match &self.arguments {
            ToolArguments::Bash(arguments) => serde_json::to_value(arguments),
            ToolArguments::Read(arguments) => serde_json::to_value(arguments),
            ToolArguments::Write(arguments) => serde_json::to_value(arguments),
            ToolArguments::Malformed(arguments) => serde_json::from_str(&arguments.raw),
            ToolArguments::Unknown(call) if call.raw.trim().is_empty() => Ok(json!({})),
            ToolArguments::Unknown(call) => serde_json::from_str(&call.raw),
        }
        .map_err(|error| format!("failed to parse function arguments: {error}"))?;
        if !value.is_object() {
            return Err("function arguments must be a JSON object".to_string());
        }

        Ok(value)
    }

    /// Returns optional provider reasoning needed to replay the assistant
    /// tool call. This sensitive content is redacted from debug output.
    pub fn reasoning_content(&self) -> Option<&str> {
        self.reasoning_content.as_deref()
    }

    /// Returns the assistant text written beside this call, if any. This
    /// model content is redacted from debug output.
    pub fn assistant_text(&self) -> Option<&str> {
        self.assistant_text.as_deref()
    }

    /// Returns this call with its identifier and arguments, replayed with
    /// `reasoning_content` and `assistant_text` instead of its own, as a
    /// request to a model of another reasoning family replays it.
    pub(crate) fn replayed_as(
        &self,
        reasoning_content: Option<String>,
        assistant_text: Option<String>,
    ) -> Self {
        Self {
            arguments: self.arguments.clone(),
            assistant_text,
            id: self.id.clone(),
            reasoning_content,
        }
    }

    #[cfg(any(feature = "sqlite", test))]
    pub(crate) fn read(
        id: String,
        arguments: ReadArguments,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            arguments: ToolArguments::Read(arguments),
            assistant_text: None,
            id,
            reasoning_content,
        }
    }

    #[cfg(any(feature = "sqlite", test))]
    pub(crate) fn write(
        id: String,
        arguments: WriteArguments,
        reasoning_content: Option<String>,
    ) -> Self {
        Self {
            arguments: ToolArguments::Write(arguments),
            assistant_text: None,
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
            .field(
                "assistant_text",
                &self.assistant_text.as_ref().map(|_| "[REDACTED]"),
            )
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
    Bash(BashArguments),
    Read(ReadArguments),
    Write(WriteArguments),
    Malformed(MalformedArguments),
    Unknown(UnknownToolCall),
}

/// Arguments of a built-in tool call that the tool could not decode.
///
/// The raw text is kept verbatim, so the call is journaled and replayed as
/// the model sent it, and is answered with an error result naming
/// [`Self::reason`] instead of running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MalformedArguments {
    raw: String,
    reason: String,
    tool: Builtin,
}

impl MalformedArguments {
    /// Returns the arguments text exactly as the model sent it.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Returns the bounded decoding failure.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// Returns the name of the built-in tool the model called.
    pub fn name(&self) -> &'static str {
        self.tool.name()
    }
}

/// A call naming no built-in tool: a host [`ToolHandler`]'s, or one the
/// model invented.
///
/// The arguments text is kept verbatim and the name as the model sent it
/// when providers can replay it, so the call is journaled and replayed as
/// sent. A call whose name had to be rewritten never runs a handler; like
/// a call to a tool the turn does not offer, it is answered with a
/// correctable result naming the turn's available tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownToolCall {
    name: String,
    raw: String,
    /// Whether `name` differs from the name the model sent.
    renamed: bool,
}

impl UnknownToolCall {
    /// Returns whether the name had to be rewritten for providers to
    /// replay it.
    pub(crate) fn renamed(&self) -> bool {
        self.renamed
    }

    /// Returns the function name as the model sent it, with characters
    /// providers cannot replay replaced by `_`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the arguments text exactly as the model sent it.
    pub fn raw(&self) -> &str {
        &self.raw
    }
}

/// Borrowed typed arguments for one native tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolCallArguments<'a> {
    /// Arguments for sandboxed Bash.
    Bash(&'a BashArguments),
    /// Arguments for a repository read.
    Read(&'a ReadArguments),
    /// Arguments for a repository patch write.
    Write(&'a WriteArguments),
    /// Arguments the called tool could not decode; the call never runs.
    Malformed(&'a MalformedArguments),
    /// A call naming no built-in tool; the call never runs.
    Unknown(&'a UnknownToolCall),
}

/// Structurally validated arguments for one native read-only repository action.
///
/// Omitting `action` preserves the original file-read contract. The other
/// fields are action-specific. Unsafe paths and field combinations rejected by
/// an action are retained so the harness can return corrective tool feedback
/// instead of executing them. `offset` and `limit` are advertised as integers
/// but also decode from strings holding a canonical decimal integer, such as
/// `"500"`, under the same range checks.
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
    #[serde(default, deserialize_with = "deserialize_optional_bounded_path")]
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
        let path_error = arguments
            .path
            .as_deref()
            .is_some_and(|path| validate_repository_path(path).is_err())
            .then_some(INVALID_READ_PATH);
        let validation_error = path_error.or(match arguments.action {
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
        });

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
#[non_exhaustive]
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
#[non_exhaustive]
pub enum ReadSide {
    /// Host-selected comparison commit, fixed for the turn.
    Base,
    /// Current `HEAD` commit.
    Head,
}

/// Validated arguments for the native `write` function.
///
/// `path` names exactly one repository-relative text file and `patch` is a
/// unified diff that creates or updates that same file. A `path` left out
/// or `null` is taken from the patch's file headers when they name one (see
/// the `write` tool for the patch forms it accepts).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WriteArguments {
    patch: String,
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

/// Wire form of [`WriteArguments`], whose `path` may be left out for the
/// patch headers to name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgumentsWire {
    #[serde(deserialize_with = "deserialize_bounded_patch")]
    patch: String,
    #[serde(default)]
    path: Option<String>,
}

impl<'de> Deserialize<'de> for WriteArguments {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let WriteArgumentsWire { patch, path } = WriteArgumentsWire::deserialize(deserializer)?;
        let path = match path {
            Some(path) => path,
            None => write::header_target(&patch)
                .ok_or_else(|| {
                    de::Error::custom(
                        "missing field `path`, and the patch has no `+++` header naming the file",
                    )
                })?
                .to_string(),
        };
        validate_repository_path(&path).map_err(de::Error::custom)?;

        Ok(Self { patch, path })
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

/// Accepts a positive integer as a JSON number or as a string holding a
/// canonical decimal integer, which some models (for example `qwen3.8-max`)
/// send for line offsets and limits. Strings with signs, whitespace, leading
/// zeros, fractions, or exponents are rejected.
fn deserialize_optional_positive_integer<'de, D>(
    deserializer: D,
) -> Result<Option<NonZeroU64>, D::Error>
where
    D: Deserializer<'de>,
{
    let integer = match Option::<IntegerArgument>::deserialize(deserializer)? {
        None => return Ok(None),
        Some(IntegerArgument::Number(number)) => parse_positive_json_integer(&number.to_string()),
        Some(IntegerArgument::Text(text)) => {
            if !is_canonical_decimal_integer(&text) {
                return Err(de::Error::custom(
                    "integer string must contain only decimal digits without leading zeros",
                ));
            }

            text.parse::<u64>().ok()
        }
    };

    integer
        .and_then(NonZeroU64::new)
        .map(Some)
        .ok_or_else(|| de::Error::custom("number must be an integer from 1 through u64::MAX"))
}

/// Integer tool argument sent either as a JSON number or as a string.
#[derive(Deserialize)]
#[serde(untagged)]
enum IntegerArgument {
    Number(Number),
    Text(String),
}

/// Bounds a read path before it can be echoed in corrective feedback; other
/// unsafe paths are retained for that feedback.
fn deserialize_optional_bounded_path<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let path = Option::<String>::deserialize(deserializer)?;
    if path
        .as_ref()
        .is_some_and(|path| path.len() > MAX_PATH_BYTES)
    {
        return Err(de::Error::custom("path exceeds the byte limit"));
    }

    Ok(path)
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

/// Returns whether `text` is `0` or a digit string without leading zeros.
fn is_canonical_decimal_integer(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'))
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

fn validate_repository_path(path: &str) -> Result<(), &'static str> {
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

    Ok(())
}

#[cfg(test)]
#[path = "tool_test.rs"]
mod tests;
