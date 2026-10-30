//! Harness models this launch offers and availability of Harness sessions.

use crate::domain::agent::{AgentModel, HarnessModelOption};

/// Whether the session-type picker offers Harness sessions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HarnessAvailability {
    /// Agentty started without `--experimental-harness`; the row is hidden.
    #[default]
    Hidden,
    /// The flag is set but no harness model can run; the row is shown
    /// disabled.
    MissingCredentials,
    /// Harness sessions can be created and start on the contained model.
    Available(AgentModel),
}

impl HarnessAvailability {
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

/// Harness models discovered for this launch and the user's last explicit
/// harness model choice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HarnessModelCatalog {
    enabled: bool,
    options: Vec<HarnessModelOption>,
    remembered: Option<AgentModel>,
}

impl HarnessModelCatalog {
    /// Creates the catalog of a launch with `--experimental-harness`
    /// `enabled`, its discovered `options`, and the `remembered` choice.
    #[must_use]
    pub fn new(
        enabled: bool,
        options: Vec<HarnessModelOption>,
        remembered: Option<AgentModel>,
    ) -> Self {
        Self {
            enabled,
            options,
            remembered,
        }
    }

    /// Returns whether and on which model new Harness sessions start, per
    /// [`ag_session::default_harness_model`].
    #[must_use]
    pub fn availability(&self) -> HarnessAvailability {
        if !self.enabled {
            return HarnessAvailability::Hidden;
        }

        ag_session::default_harness_model(&self.options, self.remembered)
            .map_or(HarnessAvailability::MissingCredentials, |model| {
                HarnessAvailability::Available(model)
            })
    }

    /// Returns whether this launch offers Harness sessions at all.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the discovered models in picker order.
    #[must_use]
    pub fn options(&self) -> &[HarnessModelOption] {
        &self.options
    }

    /// Replaces the discovered models after a refresh.
    pub fn replace_options(&mut self, options: Vec<HarnessModelOption>) {
        self.options = options;
    }

    /// Records the user's explicit harness model choice.
    pub fn remember(&mut self, model: AgentModel) {
        self.remembered = Some(model);
    }
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;
