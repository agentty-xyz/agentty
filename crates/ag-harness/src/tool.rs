use std::fmt;
use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Number, Value, json};

const READ_DESCRIPTION: &str =
    "Read a repository-relative file, optionally selecting a line range.";
const READ_NAME: &str = "read";
const MAX_PATCH_BYTES: usize = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4 * 1024;
const WRITE_DESCRIPTION: &str = concat!(
    "Apply one unified diff to one repository-relative text file. To create an empty file, use ",
    "only `--- /dev/null` and `+++ b/<path>` headers."
);
const WRITE_NAME: &str = "write";

/// Built-in tool that can be enabled for a harness run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tool {
    /// Repository-relative file reads.
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
                    "path": repository_path_schema(),
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": u64::MAX
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": u64::MAX
                    }
                },
                "required": ["path"],
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
        "pattern": "^(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+)(?:/(?:[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.[^./\\\\\\u0000][^/\\\\\\u0000]*|\\.\\.[^/\\\\\\u0000]+))*$"
    })
}

/// Provider-neutral model request for one native tool invocation.
#[derive(Clone, Eq, PartialEq)]
pub struct ToolCall {
    arguments: ToolArguments,
    id: String,
    reasoning_content: Option<String>,
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

impl ToolCall {
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

    pub(crate) fn arguments_json(&self) -> Result<String, serde_json::Error> {
        match &self.arguments {
            ToolArguments::Read(arguments) => serde_json::to_string(arguments),
            ToolArguments::Write(arguments) => serde_json::to_string(arguments),
        }
    }

    pub(crate) fn reasoning_content(&self) -> Option<&str> {
        self.reasoning_content.as_deref()
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

/// Validated arguments for the native `read` function.
///
/// `path` is a non-empty repository-relative POSIX path. `offset`, when
/// present, is a one-based line number, and `limit`, when present, is a
/// positive maximum line count.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReadArguments {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_positive_integer",
        skip_serializing_if = "Option::is_none"
    )]
    limit: Option<NonZeroU64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_positive_integer",
        skip_serializing_if = "Option::is_none"
    )]
    offset: Option<NonZeroU64>,
    #[serde(deserialize_with = "deserialize_repository_path")]
    path: String,
}

impl ReadArguments {
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
        &self.path
    }
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
    let number = Number::deserialize(deserializer)?;
    parse_positive_json_integer(&number.to_string())
        .and_then(NonZeroU64::new)
        .map(Some)
        .ok_or_else(|| de::Error::custom("number must be an integer from 1 through u64::MAX"))
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
    if path.is_empty() {
        return Err(de::Error::custom("path must not be empty"));
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(de::Error::custom("path exceeds the byte limit"));
    }
    if path.starts_with('/') || path.contains('\\') {
        return Err(de::Error::custom("path must be repository-relative"));
    }
    if path.contains('\0') {
        return Err(de::Error::custom("path must not contain NUL"));
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(de::Error::custom(
            "path must not contain empty, current-directory, or parent-directory components",
        ));
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use jsonschema::Validator;
    use serde_json::json;

    use super::*;

    #[test]
    fn read_definition_exposes_native_function_contract() {
        // Arrange and Act
        let definition = ToolDefinition::read();
        let validator =
            Validator::new(definition.parameters()).expect("read argument schema should compile");

        // Assert
        assert_eq!(definition.name(), "read");
        assert_eq!(
            definition.description(),
            "Read a repository-relative file, optionally selecting a line range."
        );
        assert!(validator.is_valid(&json!({ "path": "Cargo.toml" })));
        assert!(validator.is_valid(&json!({
            "path": "crates/ag-harness/src/lib.rs",
            "offset": 1,
            "limit": 12
        })));
        assert!(validator.is_valid(&json!({
            "path": "Cargo.toml",
            "offset": u64::MAX,
            "limit": u64::MAX
        })));
        assert!(validator.is_valid(&json!({
            "path": "Cargo.toml",
            "offset": 1.0,
            "limit": 1e0
        })));
    }

    #[test]
    fn read_definition_rejects_invalid_arguments() {
        // Arrange
        let definition = ToolDefinition::read();
        let validator =
            Validator::new(definition.parameters()).expect("read argument schema should compile");
        let offset_above_maximum =
            serde_json::from_str(r#"{"path":"Cargo.toml","offset":18446744073709551616}"#)
                .expect("out-of-range offset fixture should be valid JSON");
        let limit_above_maximum =
            serde_json::from_str(r#"{"path":"Cargo.toml","limit":18446744073709551616}"#)
                .expect("out-of-range limit fixture should be valid JSON");
        let invalid_arguments = [
            json!({}),
            json!({ "path": "" }),
            json!({ "path": "/Cargo.toml" }),
            json!({ "path": "C:\\Cargo.toml" }),
            json!({ "path": "../Cargo.toml" }),
            json!({ "path": "Cargo\0.toml" }),
            json!({ "path": "Cargo.toml", "offset": 0 }),
            json!({ "path": "Cargo.toml", "limit": 0 }),
            json!({ "path": "Cargo.toml", "offset": null }),
            json!({ "path": "Cargo.toml", "limit": null }),
            json!({ "path": "Cargo.toml", "unexpected": true }),
            offset_above_maximum,
            limit_above_maximum,
        ];

        // Act
        let results = invalid_arguments.map(|arguments| validator.is_valid(&arguments));

        // Assert
        assert!(results.into_iter().all(|is_valid| !is_valid));
    }

    #[test]
    fn write_definition_exposes_native_function_contract() {
        // Arrange and Act
        let definition = ToolDefinition::write();
        let validator =
            Validator::new(definition.parameters()).expect("write argument schema should compile");

        // Assert
        assert_eq!(definition.name(), "write");
        assert_eq!(
            definition.description(),
            concat!(
                "Apply one unified diff to one repository-relative text file. To create an empty ",
                "file, use only `--- /dev/null` and `+++ b/<path>` headers."
            )
        );
        assert!(validator.is_valid(&json!({
            "path": "src/lib.rs",
            "patch": "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"
        })));
    }

    #[test]
    fn write_definition_and_arguments_reject_invalid_input() {
        // Arrange
        let definition = ToolDefinition::write();
        let validator =
            Validator::new(definition.parameters()).expect("write argument schema should compile");
        let values = [
            json!({}),
            json!({ "path": "src/lib.rs" }),
            json!({ "path": "src/lib.rs", "patch": "" }),
            json!({ "path": "../lib.rs", "patch": "patch" }),
            json!({ "path": "src/lib.rs", "patch": "patch", "extra": true }),
            json!({ "path": "a".repeat(MAX_PATH_BYTES + 1), "patch": "patch" }),
            json!({ "path": "src/lib.rs", "patch": "x".repeat(MAX_PATCH_BYTES + 1) }),
        ];

        // Act
        let schema_results = values.clone().map(|value| validator.is_valid(&value));
        let decode_results = values.map(serde_json::from_value::<WriteArguments>);

        // Assert
        assert!(schema_results.into_iter().all(|valid| !valid));
        assert!(decode_results.into_iter().all(|result| result.is_err()));
    }

    #[test]
    fn tool_call_exposes_matching_typed_arguments_and_serialization() {
        // Arrange
        let read_arguments = serde_json::from_value(json!({ "path": "Cargo.toml" }))
            .expect("read arguments should decode");
        let write_arguments = serde_json::from_value(json!({
            "path": "src/lib.rs",
            "patch": "patch"
        }))
        .expect("write arguments should decode");
        let read = ToolCall::read(
            "read-id".to_string(),
            read_arguments,
            Some("secret".to_string()),
        );
        let write = ToolCall::write("write-id".to_string(), write_arguments, None);

        // Act
        let read_json = read.arguments_json().expect("read arguments should encode");
        let write_json = write
            .arguments_json()
            .expect("write arguments should encode");

        // Assert
        assert!(read.read_arguments().is_some());
        assert!(read.write_arguments().is_none());
        assert!(write.read_arguments().is_none());
        assert_eq!(read.name(), "read");
        assert_eq!(write.name(), "write");
        let write_arguments = write
            .write_arguments()
            .expect("write arguments should be exposed");
        assert_eq!(write_arguments.path(), "src/lib.rs");
        assert_eq!(write_arguments.patch(), "patch");
        assert_eq!(read_json, r#"{"path":"Cargo.toml"}"#);
        assert_eq!(write_json, r#"{"patch":"patch","path":"src/lib.rs"}"#);
        assert_eq!(read.reasoning_content(), Some("secret"));
        assert!(format!("{read:?}").contains("[REDACTED]"));
        assert!(matches!(read.arguments(), ToolCallArguments::Read(_)));
        assert!(matches!(write.arguments(), ToolCallArguments::Write(_)));
    }

    #[test]
    fn read_arguments_reject_invalid_repository_paths() {
        // Arrange
        let invalid_paths = [
            "",
            "/Cargo.toml",
            "C:\\Cargo.toml",
            "server\\share",
            "src//lib.rs",
            "src/./lib.rs",
            "../lib.rs",
            "Cargo\0.toml",
        ];

        // Act
        let errors = invalid_paths.map(|path| {
            serde_json::from_value::<ReadArguments>(json!({ "path": path }))
                .expect_err("invalid path should be rejected")
        });

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| !error.to_string().is_empty())
        );
    }

    #[test]
    fn read_arguments_distinguish_missing_ranges_from_null() {
        // Arrange
        let omitted = json!({ "path": "Cargo.toml" });
        let explicit_null = [
            json!({ "path": "Cargo.toml", "offset": null }),
            json!({ "path": "Cargo.toml", "limit": null }),
        ];

        // Act
        let arguments = serde_json::from_value::<ReadArguments>(omitted)
            .expect("omitted ranges should remain optional");
        let errors = explicit_null.map(|value| {
            serde_json::from_value::<ReadArguments>(value)
                .expect_err("explicit null range should be rejected")
        });

        // Assert
        assert_eq!(arguments.offset(), None);
        assert_eq!(arguments.limit(), None);
        assert!(
            errors
                .into_iter()
                .all(|error| !error.to_string().is_empty())
        );
    }

    #[test]
    fn read_arguments_accept_maximum_ranges() {
        // Arrange
        let value = json!({
            "path": "Cargo.toml",
            "offset": u64::MAX,
            "limit": u64::MAX
        });

        // Act
        let arguments = serde_json::from_value::<ReadArguments>(value)
            .expect("maximum u64 ranges should decode");

        // Assert
        assert_eq!(arguments.offset(), Some(u64::MAX));
        assert_eq!(arguments.limit(), Some(u64::MAX));
    }

    #[test]
    fn read_arguments_accept_integral_decimal_and_exponent_ranges() {
        // Arrange
        let values = [
            (r#"{"path":"Cargo.toml","offset":1.0,"limit":1e0}"#, (1, 1)),
            (
                r#"{"path":"Cargo.toml","offset":1e2,"limit":100e-2}"#,
                (100, 1),
            ),
            (
                r#"{"path":"Cargo.toml","offset":18446744073709551615.0,"limit":18446744073709551615e0}"#,
                (u64::MAX, u64::MAX),
            ),
        ];

        // Act
        let arguments = values.map(|(value, expected)| {
            serde_json::from_str::<ReadArguments>(value)
                .map(|arguments| (arguments, expected))
                .expect("integral numeric forms should decode")
        });

        // Assert
        assert!(arguments.iter().all(|(arguments, expected)| {
            arguments.offset() == Some(expected.0) && arguments.limit() == Some(expected.1)
        }));
    }

    #[test]
    fn read_arguments_reject_non_integral_or_out_of_range_numbers() {
        // Arrange
        let values = [
            r#"{"path":"Cargo.toml","offset":-1}"#,
            r#"{"path":"Cargo.toml","limit":1.5}"#,
            r#"{"path":"Cargo.toml","limit":1e-1}"#,
            r#"{"path":"Cargo.toml","offset":18446744073709551616}"#,
            r#"{"path":"Cargo.toml","offset":1e999999999999999999999}"#,
        ];

        // Act
        let errors = values.map(|value| {
            serde_json::from_str::<ReadArguments>(value)
                .expect_err("non-integral or out-of-range number should fail")
        });

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| !error.to_string().is_empty())
        );
    }
}
