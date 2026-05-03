//! Clarification question model shared across domain, UI, and protocol code.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One extracted question with predefined answer choices.
///
/// The UI, persistence, and structured agent protocol all use this as the
/// canonical clarification question representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    title = "QuestionItem",
    description = "One clarification question emitted by the assistant protocol payload. Keep \
                   each item focused to one actionable decision."
)]
pub struct QuestionItem {
    /// Predefined answer choices the user can select from.
    #[serde(default)]
    #[schemars(
        title = "options",
        description = "Predefined answer choices the user can select from. Keep this list focused \
                       to 1-3 likely answers, put the recommended choice first, and omit deferral \
                       or non-answer choices. Defaults to an empty list when omitted."
    )]
    pub options: Vec<String>,
    /// The clarification question text.
    #[schemars(
        title = "text",
        description = "Human-readable markdown text for this question. Ask one specific \
                       actionable question instead of bundling multiple decisions into one item."
    )]
    pub text: String,
}

impl QuestionItem {
    /// Constructs one clarification question without predefined answer
    /// options.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            options: Vec::new(),
            text: text.into(),
        }
    }

    /// Constructs one clarification question with predefined answer options.
    pub fn with_options(text: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            options,
            text: text.into(),
        }
    }
}
