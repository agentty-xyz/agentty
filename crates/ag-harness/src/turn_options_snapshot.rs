//! Versioned turn-options snapshots independent of persistence backends.

use std::num::{NonZeroU32, NonZeroUsize};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::comparison::ComparisonIdentity;
use crate::{OutputSchema, OutputSchemaError, ToolPolicy, TurnOptions};

/// Versioned durable metadata, independent of live repository validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredTurnOptions {
    #[serde(default)]
    bash: Option<crate::bash::BashPolicySnapshot>,
    #[serde(default)]
    comparison_base: Option<ComparisonIdentity>,
    #[serde(default)]
    fingerprint: Option<String>,
    /// Host instructions of the turn, recorded from version 6.
    #[serde(default)]
    instructions: Option<String>,
    /// Output-token bound of the turn, recorded from version 6.
    #[serde(default)]
    max_output_tokens: Option<NonZeroU32>,
    /// Removed per-turn tool-call limit, retained only so legacy v1-v4
    /// snapshots keep validating against their recorded fingerprints.
    #[serde(default)]
    max_tool_calls: Option<NonZeroUsize>,
    /// Output schema, or, from version 6, `null` for a plain-text turn.
    output_schema: Value,
    tool_policy: ToolPolicy,
    version: u8,
}

impl StoredTurnOptions {
    /// Encodes effective options with the current version and fingerprint.
    pub fn encode(options: &TurnOptions) -> String {
        let stored = Self {
            bash: options.bash().map(|config| config.snapshot.clone()),
            comparison_base: options
                .comparison_base()
                .map(|base| base.identity().clone()),
            fingerprint: None,
            instructions: options.instructions().map(str::to_string),
            max_output_tokens: options.generation().max_output_tokens,
            max_tool_calls: None,
            output_schema: options
                .schema()
                .map_or(Value::Null, |schema| schema.value().clone()),
            tool_policy: options.tool_policy(),
            version: 6,
        };
        let mut snapshot = stored.effective_options();
        snapshot["fingerprint"] = json!(stored.fingerprint());

        snapshot.to_string()
    }

    /// Validates a historical snapshot without accessing Git or a repository.
    ///
    /// # Errors
    /// Returns an error for unsupported versions, malformed data, or invalid
    /// schemas, comparison identities, or fingerprints.
    pub fn decode(snapshot: &str) -> Result<Self, StoredTurnOptionsError> {
        let stored: Self = serde_json::from_str(snapshot).map_err(StoredTurnOptionsError::Json)?;
        let text_output = stored.output_schema.is_null();
        if !matches!(stored.version, 1..=6)
            || (stored.version < 4 && stored.bash.is_some())
            || (stored.version < 5) != stored.max_tool_calls.is_some()
            || (stored.version < 6
                && (text_output
                    || stored.instructions.is_some()
                    || stored.max_output_tokens.is_some()))
        {
            return Err(StoredTurnOptionsError::InvalidData {
                reason: format!("unsupported turn options version {}", stored.version),
            });
        }
        if !text_output {
            OutputSchema::new(stored.output_schema.clone())
                .map_err(StoredTurnOptionsError::Schema)?;
        }
        let valid = if stored.version == 1 {
            stored.comparison_base.is_none() && stored.fingerprint.is_none()
        } else {
            stored
                .comparison_base
                .as_ref()
                .is_none_or(ComparisonIdentity::is_valid)
                && stored.fingerprint.as_deref() == Some(stored.fingerprint().as_str())
        };
        if !valid {
            return Err(StoredTurnOptionsError::InvalidData {
                reason: "invalid comparison identity or effective-options fingerprint".to_string(),
            });
        }

        Ok(stored)
    }

    fn effective_options(&self) -> Value {
        // Legacy v2 fingerprints hash unsorted keys, so the limit keeps its
        // original position for builds that preserve insertion order.
        let mut value = match self.max_tool_calls {
            Some(max_tool_calls) => json!({
                "comparison_base": self.comparison_base,
                "max_tool_calls": max_tool_calls,
                "output_schema": self.output_schema,
                "tool_policy": self.tool_policy,
                "version": self.version,
            }),
            None => json!({
                "comparison_base": self.comparison_base,
                "output_schema": self.output_schema,
                "tool_policy": self.tool_policy,
                "version": self.version,
            }),
        };
        if self.version >= 4 {
            value["bash"] = json!(self.bash);
        }
        if self.version >= 6 {
            value["instructions"] = json!(self.instructions);
            value["max_output_tokens"] = json!(self.max_output_tokens);
        }

        value
    }

    fn fingerprint(&self) -> String {
        let mut options = self.effective_options();
        if self.version >= 3 {
            options.sort_all_objects();
        }

        hex::encode(Sha256::digest(options.to_string()))
    }
}

/// Invalid historical options, before a persistence backend adds its context.
#[derive(Debug, Error)]
pub enum StoredTurnOptionsError {
    /// The snapshot violates its versioned contract.
    #[error("invalid stored options: {reason}")]
    InvalidData {
        /// Validation failure without repository contents.
        reason: String,
    },
    /// The snapshot could not be decoded.
    #[error(transparent)]
    Json(serde_json::Error),
    /// The stored output schema is invalid.
    #[error(transparent)]
    Schema(OutputSchemaError),
}

#[cfg(test)]
#[path = "turn_options_snapshot_test.rs"]
mod tests;
