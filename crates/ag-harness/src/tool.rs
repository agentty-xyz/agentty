use std::num::NonZeroU64;

use serde::{Deserialize, Deserializer, de};
use serde_json::{Number, Value, json};

const READ_DESCRIPTION: &str =
    "Read a repository-relative file, optionally selecting a line range.";
const READ_NAME: &str = "read";

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
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "pattern": "^(?:[^./\\u0000][^/\\u0000]*|\\.[^./\\u0000][^/\\u0000]*|\\.\\.[^/\\u0000]+)(?:/(?:[^./\\u0000][^/\\u0000]*|\\.[^./\\u0000][^/\\u0000]*|\\.\\.[^/\\u0000]+))*$"
                    },
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

/// Provider-neutral model request for one native tool invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCall {
    arguments: ReadArguments,
    id: String,
    name: String,
}

impl ToolCall {
    /// Returns the typed arguments supplied to the `read` function.
    pub fn arguments(&self) -> &ReadArguments {
        &self.arguments
    }

    /// Returns the provider-assigned call identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the requested native function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn read(id: String, arguments: ReadArguments) -> Self {
        Self {
            arguments,
            id,
            name: READ_NAME.to_string(),
        }
    }
}

/// Validated arguments for the native `read` function.
///
/// `path` is a non-empty repository-relative POSIX path. `offset`, when
/// present, is a one-based line number, and `limit`, when present, is a
/// positive maximum line count.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReadArguments {
    #[serde(default, deserialize_with = "deserialize_optional_positive_integer")]
    limit: Option<NonZeroU64>,
    #[serde(default, deserialize_with = "deserialize_optional_positive_integer")]
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
    if path.starts_with('/') {
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
    fn read_arguments_reject_invalid_repository_paths() {
        // Arrange
        let invalid_paths = [
            "",
            "/Cargo.toml",
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
