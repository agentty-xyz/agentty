//! Launch-time availability of in-process harness sessions.

use crate::domain::agent::AgentModel;

/// Whether the session-type picker offers Harness sessions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HarnessAvailability {
    /// Agentty started without `--experimental-harness`; the row is hidden.
    #[default]
    Hidden,
    /// The flag is set but no provider has a key and endpoint; the row is
    /// shown disabled.
    MissingCredentials,
    /// Harness sessions can be created and start on the contained model,
    /// whose provider is configured.
    Available(AgentModel),
}

impl HarnessAvailability {
    /// Resolves availability from the launch flag and the first Harness model
    /// whose provider credentials are configured.
    #[must_use]
    pub fn resolve(experimental_harness: bool, default_model: Option<AgentModel>) -> Self {
        match (experimental_harness, default_model) {
            (false, _) => Self::Hidden,
            (true, None) => Self::MissingCredentials,
            (true, Some(model)) => Self::Available(model),
        }
    }

    /// Returns whether the picker shows the Harness row.
    #[must_use]
    pub fn is_visible(self) -> bool {
        self != Self::Hidden
    }

    /// Returns whether Harness sessions can be created.
    #[must_use]
    pub fn is_available(self) -> bool {
        self.default_model().is_some()
    }

    /// Returns the model new Harness sessions start on, when available.
    #[must_use]
    pub fn default_model(self) -> Option<AgentModel> {
        match self {
            Self::Available(model) => Some(model),
            Self::Hidden | Self::MissingCredentials => None,
        }
    }
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;
