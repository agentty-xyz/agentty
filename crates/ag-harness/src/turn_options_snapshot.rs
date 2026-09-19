//! Versioned turn-options snapshots independent of persistence backends.

use std::num::NonZeroUsize;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::comparison::{ComparisonBase, ComparisonIdentity};
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
    max_tool_calls: NonZeroUsize,
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
            max_tool_calls: options.limits().max_tool_calls(),
            output_schema: options.schema().value().clone(),
            tool_policy: options.tool_policy(),
            version: if options.bash().is_some() { 4 } else { 3 },
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
        if !matches!(stored.version, 1..=4) || (stored.version < 4 && stored.bash.is_some()) {
            return Err(StoredTurnOptionsError::InvalidData {
                reason: format!("unsupported turn options version {}", stored.version),
            });
        }
        OutputSchema::new(stored.output_schema.clone()).map_err(StoredTurnOptionsError::Schema)?;
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

    /// Whether native continuation can reuse this snapshot's context policy.
    /// Legacy v1 snapshots cannot establish compatibility; budget alone does
    /// not invalidate a continuation. This is not a host-request fingerprint.
    pub fn continuation_compatible(&self, options: &TurnOptions) -> bool {
        matches!(self.version, 2..=4)
            && self.bash.as_ref() == options.bash().map(|config| &config.snapshot)
            && self.output_schema == *options.schema().value()
            && self.tool_policy == options.tool_policy()
            && self.comparison_base.as_ref()
                == options.comparison_base().map(ComparisonBase::identity)
    }

    fn effective_options(&self) -> Value {
        let mut value = json!({
            "comparison_base": self.comparison_base,
            "max_tool_calls": self.max_tool_calls,
            "output_schema": self.output_schema,
            "tool_policy": self.tool_policy,
            "version": self.version,
        });
        if self.version >= 4 {
            value["bash"] = json!(self.bash);
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
