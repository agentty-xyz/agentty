//! Versioned turn-options snapshots independent of persistence backends.

use std::num::NonZeroUsize;

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::comparison::{ComparisonBase, ComparisonIdentity};
use crate::{OutputSchema, OutputSchemaError, ToolPolicy, TurnOptions};

/// Versioned durable metadata, independent of live repository validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredTurnOptions {
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
    pub(crate) fn encode(options: &TurnOptions) -> String {
        let stored = Self {
            comparison_base: options
                .comparison_base()
                .map(|base| base.identity().clone()),
            fingerprint: None,
            max_tool_calls: options.limits().max_tool_calls(),
            output_schema: options.schema().value().clone(),
            tool_policy: options.tool_policy(),
            version: 3,
        };
        let mut snapshot = stored.effective_options();
        snapshot["fingerprint"] = json!(stored.fingerprint());

        snapshot.to_string()
    }

    pub(crate) fn decode(snapshot: &str) -> Result<Self, StoredTurnOptionsError> {
        let stored: Self = serde_json::from_str(snapshot).map_err(StoredTurnOptionsError::Json)?;
        if !matches!(stored.version, 1..=3) {
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

    pub(crate) fn continuation_compatible(&self, options: &TurnOptions) -> bool {
        matches!(self.version, 2 | 3)
            && self.output_schema == *options.schema().value()
            && self.tool_policy == options.tool_policy()
            && self.comparison_base.as_ref()
                == options.comparison_base().map(ComparisonBase::identity)
    }

    fn effective_options(&self) -> Value {
        json!({
            "comparison_base": self.comparison_base,
            "max_tool_calls": self.max_tool_calls,
            "output_schema": self.output_schema,
            "tool_policy": self.tool_policy,
            "version": self.version,
        })
    }

    fn fingerprint(&self) -> String {
        let mut options = self.effective_options();
        if self.version == 3 {
            options.sort_all_objects();
        }

        format!("{:x}", Sha256::digest(options.to_string()))
    }
}

/// Invalid historical options, before a persistence backend adds its context.
#[derive(Debug)]
pub(crate) enum StoredTurnOptionsError {
    InvalidData { reason: String },
    Json(serde_json::Error),
    Schema(OutputSchemaError),
}

#[cfg(test)]
#[path = "turn_options_snapshot_test.rs"]
mod tests;
