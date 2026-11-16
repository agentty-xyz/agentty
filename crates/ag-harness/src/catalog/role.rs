//! Open set of model roles and the selectors that resolve a catalog model.

use std::borrow::Cow;
use std::fmt;

use super::spec::{Capabilities, ModelId};

/// Purpose a host selects a model for, such as [`Self::CHEAP`] for titles
/// and commit messages.
///
/// Roles are an open set: the constants name the roles the built-in
/// defaults assign, and hosts may define any other role with [`Self::new`].
/// A [`super::ModelCatalog`] maps each role to ordered candidate models.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Role(Cow<'static, str>);

impl Role {
    /// Low-cost model for simple, high-volume tasks.
    pub const CHEAP: Self = Self(Cow::Borrowed("cheap"));
    /// Low-latency model for interactive, small tasks.
    pub const FAST: Self = Self(Cow::Borrowed("fast"));
    /// Most capable model for demanding work.
    pub const SMART: Self = Self(Cow::Borrowed("smart"));
    /// Capable model that reads image input.
    pub const VISION: Self = Self(Cow::Borrowed("vision"));

    /// Creates a host-defined role.
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self(name.into())
    }

    /// Returns the role name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Which catalog model to use, resolved by
/// [`super::ModelCatalog::resolve`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ModelSelector {
    /// The model with this catalog key.
    Id(ModelId),
    /// The first candidate assigned to this role.
    Role(Role),
    /// The first candidate assigned to `role` with every capability in
    /// `needs`.
    Require {
        /// Role whose candidates are searched in order.
        role: Role,
        /// Capabilities the selected model must have.
        needs: Capabilities,
    },
}

impl From<ModelId> for ModelSelector {
    fn from(id: ModelId) -> Self {
        Self::Id(id)
    }
}

impl From<Role> for ModelSelector {
    fn from(role: Role) -> Self {
        Self::Role(role)
    }
}

#[cfg(test)]
#[path = "role_test.rs"]
mod tests;
