use std::str::FromStr;

/// Supported reasoning-effort levels for task execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReasoningLevel {
    /// Low reasoning effort for faster responses.
    Low,
    /// Medium reasoning effort.
    Medium,
    /// High reasoning effort for deeper reasoning.
    #[default]
    High,
    /// Extra-high reasoning effort for deeper analysis.
    XHigh,
    /// Maximum reasoning effort for the hardest tasks.
    Max,
}

impl ReasoningLevel {
    /// All selectable reasoning-effort levels in UI display order.
    pub const ALL: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::XHigh, Self::Max];

    /// Returns the stable persisted identifier for this level.
    ///
    /// This value is stored in the database and remains independent from any
    /// provider-specific transport string changes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Returns a short UI description for this reasoning level.
    pub fn description(self) -> &'static str {
        match self {
            Self::Low => "Fastest responses with lighter reasoning.",
            Self::Medium => "Balanced speed and reasoning depth.",
            Self::High => "Deeper reasoning for tougher tasks.",
            Self::XHigh => "Extra-high reasoning for complex tasks.",
            Self::Max => "Maximum reasoning effort for the hardest tasks.",
        }
    }
}

impl FromStr for ReasoningLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(format!("unknown reasoning level: {other}")),
        }
    }
}

#[cfg(test)]
#[path = "reasoning_test.rs"]
mod tests;
