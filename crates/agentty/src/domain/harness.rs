//! Harness models this launch offers and availability of Harness sessions.

use crate::domain::agent::{AgentModel, HarnessModelOption};

/// Whether new Harness sessions can start, and on which model.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HarnessAvailability {
    /// No harness model can run yet, such as when no provider key is set and
    /// no local server serves a model.
    #[default]
    MissingCredentials,
    /// Harness sessions can be created and start on the contained model.
    Available(AgentModel),
}

impl HarnessAvailability {
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
            Self::MissingCredentials => None,
        }
    }
}

/// Harness models discovered for this launch and the user's last explicit
/// harness model choice.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HarnessModelCatalog {
    options: Vec<HarnessModelOption>,
    remembered: Option<AgentModel>,
}

impl HarnessModelCatalog {
    /// Creates the catalog of the discovered `options` and the `remembered`
    /// choice.
    #[must_use]
    pub fn new(options: Vec<HarnessModelOption>, remembered: Option<AgentModel>) -> Self {
        Self {
            options,
            remembered,
        }
    }

    /// Returns whether and on which model new Harness sessions start, per
    /// [`ag_session::default_harness_model`].
    #[must_use]
    pub fn availability(&self) -> HarnessAvailability {
        ag_session::default_harness_model(&self.options, self.remembered)
            .map_or(HarnessAvailability::MissingCredentials, |model| {
                HarnessAvailability::Available(model)
            })
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
