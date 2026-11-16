//! Model catalog: cataloged models with their specifications, role
//! assignments, and selector resolution.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;
use std::{env, fmt};

use thiserror::Error;

use super::role::{ModelSelector, Role};
use super::spec::{Capabilities, ModelId, ModelSpec, Pricing};
use crate::context::{ContextBudget, ContextBudgetError};
use crate::model::Model;
use crate::model_profile::ModelProfile;
use crate::provider::{
    CHATGPT_GPT_6_1_SOL, CHATGPT_GPT_6_LUNA, GPT_6_1_SOL, GPT_6_LUNA, KIMI_K2_6, KIMI_K3,
    MUSE_SPARK_1_3, ModelConfiguration, ModelConfigurationError, ModelProvider, QWEN3_7_FLASH,
    QWEN3_8_FLASH, QWEN3_8_MAX,
};

/// Built-in models, by provider and model identifier, in preference order.
type Candidates = &'static [(ModelProvider, &'static str)];

/// Candidates of [`Role::SMART`] and [`Role::VISION`] in the environment
/// catalog, plan-billed first.
const SMART_CANDIDATES: Candidates = &[
    (ModelProvider::ChatGpt, CHATGPT_GPT_6_1_SOL),
    (ModelProvider::OpenAi, GPT_6_1_SOL),
    (ModelProvider::Muse, MUSE_SPARK_1_3),
    (ModelProvider::Kimi, KIMI_K3),
    (ModelProvider::Qwen, QWEN3_8_MAX),
];
/// Candidates of [`Role::CHEAP`] in the environment catalog.
const CHEAP_CANDIDATES: Candidates = &[
    (ModelProvider::ChatGpt, CHATGPT_GPT_6_LUNA),
    (ModelProvider::OpenAi, GPT_6_LUNA),
    (ModelProvider::Qwen, QWEN3_8_FLASH),
    (ModelProvider::Qwen, QWEN3_7_FLASH),
    (ModelProvider::Kimi, KIMI_K2_6),
];
/// Candidates of [`Role::FAST`] in the environment catalog.
const FAST_CANDIDATES: Candidates = &[
    (ModelProvider::ChatGpt, CHATGPT_GPT_6_LUNA),
    (ModelProvider::OpenAi, GPT_6_LUNA),
    (ModelProvider::Qwen, QWEN3_8_FLASH),
    (ModelProvider::Qwen, QWEN3_7_FLASH),
];
/// Default role assignments of the environment catalog: each role, the
/// capabilities its candidates need, and its candidates in order.
const DEFAULT_ROLES: &[(Role, Capabilities, Candidates)] = &[
    (Role::SMART, Capabilities::NONE, SMART_CANDIDATES),
    (Role::CHEAP, Capabilities::NONE, CHEAP_CANDIDATES),
    (Role::FAST, Capabilities::NONE, FAST_CANDIDATES),
    (
        Role::VISION,
        Capabilities::NONE.with_image_input(true),
        SMART_CANDIDATES,
    ),
];

/// Models a host can select by identifier, role, or required capability.
///
/// A catalog is a resolver for hosts, not a harness dependency: resolve a
/// [`ModelSelector`] to a [`ModelHandle`] and hand its model to
/// [`crate::HarnessBuilder::model`], or give the catalog to
/// [`crate::HarnessBuilder::catalog`]. Each role maps to ordered candidate
/// models; resolving a role picks its first candidate, and
/// [`ModelSelector::Require`] its first candidate with the needed
/// capabilities.
///
/// [`Self::from_environment`] catalogs every built-in model whose provider
/// the environment configures, with these default role candidates, keeping
/// those cataloged:
///
/// | Role             | Candidates, in order                                                                                       |
/// | ---------------- | ---------------------------------------------------------------------------------------------------------- |
/// | [`Role::SMART`]  | `chatgpt/gpt-6.1-sol`, `openai/gpt-6.1-sol`, `muse/muse-spark-1.3`, `kimi/kimi-k3`, `qwen/qwen3.8-max`     |
/// | [`Role::CHEAP`]  | `chatgpt/gpt-6-luna`, `openai/gpt-6-luna`, `qwen/qwen3.8-flash`, `qwen/qwen3.7-flash`, `kimi/kimi-k2.6`    |
/// | [`Role::FAST`]   | `chatgpt/gpt-6-luna`, `openai/gpt-6-luna`, `qwen/qwen3.8-flash`, `qwen/qwen3.7-flash`                      |
/// | [`Role::VISION`] | the [`Role::SMART`] candidates that read images                                                            |
///
/// Plan-billed `ChatGPT` models come first because they have no per-token
/// charge. No default selects Muse's contributor tier, whose prompts Meta
/// trains on. Change any assignment with [`Self::into_builder`] and
/// [`CatalogBuilder::assign`].
pub struct ModelCatalog {
    entries: BTreeMap<ModelId, CatalogEntry>,
    roles: BTreeMap<Role, Vec<ModelId>>,
}

impl ModelCatalog {
    /// Starts an empty catalog.
    pub fn builder() -> CatalogBuilder {
        CatalogBuilder {
            models: Vec::new(),
            pricing: Vec::new(),
            roles: BTreeMap::new(),
        }
    }

    /// Catalogs the built-in models of every provider the process
    /// environment configures; see [`Self::from_environment_with`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] like [`Self::from_environment_with`].
    pub fn from_environment() -> Result<Self, CatalogError> {
        Self::from_environment_with(|name| env::var(name))
    }

    /// Catalogs every known model of each built-in provider that
    /// `environment` configures, under its [`ModelId::cataloged`] key, with
    /// the default role candidates documented on [`ModelCatalog`].
    ///
    /// A provider whose API key, base URL, or `ChatGPT` sign-in is missing
    /// is skipped. OpenAI-compatible servers name their own models, so hosts
    /// add those with [`CatalogBuilder::model`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::Configuration`] when a configured provider's
    /// environment is otherwise invalid, such as a base URL that is not
    /// valid Unicode.
    pub fn from_environment_with(
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Self, CatalogError> {
        let mut builder = Self::builder();
        for provider in ModelProvider::all() {
            let cataloged = provider
                .known_models()
                .iter()
                .filter_map(|model| Some((*model, provider.model_spec(model)?)));
            for (model, spec) in cataloged {
                match ModelConfiguration::new(*provider, model)
                    .client_from_environment(&mut environment)
                {
                    Ok(client) => builder = builder.model(spec, Arc::new(client)),
                    Err(
                        ModelConfigurationError::ApiKey { .. }
                        | ModelConfigurationError::AuthFile(_)
                        | ModelConfigurationError::BaseUrl { .. }
                        | ModelConfigurationError::SignIn(_),
                    ) => break,
                    Err(source) => {
                        return Err(CatalogError::Configuration {
                            provider: *provider,
                            source,
                        });
                    }
                }
            }
        }
        for (role, needs, candidates) in DEFAULT_ROLES {
            let candidates = candidates
                .iter()
                .map(|(provider, model)| ModelId::cataloged(*provider, model))
                .filter(|id| {
                    builder
                        .models
                        .iter()
                        .any(|(spec, _)| spec.id() == id && spec.capabilities().satisfies(*needs))
                })
                .collect::<Vec<_>>();
            if !candidates.is_empty() {
                builder = builder.assign(role.clone(), candidates);
            }
        }

        builder.build()
    }

    /// Resolves `selector` to a cataloged model.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::UnknownModel`] for an uncataloged identifier,
    /// [`CatalogError::UnassignedRole`] for a role without candidates, and
    /// [`CatalogError::MissingCapability`] when no candidate of the role has
    /// the needed capabilities.
    pub fn resolve(&self, selector: &ModelSelector) -> Result<ModelHandle, CatalogError> {
        let id = match selector {
            ModelSelector::Id(id) => id,
            ModelSelector::Role(role) => &self.role_candidates(role)?[0],
            ModelSelector::Require { role, needs } => self
                .role_candidates(role)?
                .iter()
                .find(|id| self.entries[*id].spec.capabilities().satisfies(*needs))
                .ok_or_else(|| CatalogError::MissingCapability {
                    needs: *needs,
                    role: role.clone(),
                })?,
        };
        let entry = self
            .entries
            .get(id)
            .ok_or_else(|| CatalogError::UnknownModel { id: id.clone() })?;

        Ok(entry.handle())
    }

    /// Returns the specification of the model `id`, if cataloged.
    pub fn spec(&self, id: &ModelId) -> Option<&ModelSpec> {
        self.entries.get(id).map(|entry| &entry.spec)
    }

    /// Returns every cataloged specification, ordered by identifier.
    pub fn specs(&self) -> impl Iterator<Item = &ModelSpec> {
        self.entries.values().map(|entry| &entry.spec)
    }

    /// Returns the candidates assigned to `role`, in order, or none.
    pub fn candidates(&self, role: &Role) -> &[ModelId] {
        self.roles.get(role).map_or(&[], Vec::as_slice)
    }

    /// Returns a builder holding this catalog's models and role
    /// assignments, so a host can adjust them.
    pub fn into_builder(self) -> CatalogBuilder {
        CatalogBuilder {
            models: self
                .entries
                .into_values()
                .map(|entry| (entry.spec, entry.model))
                .collect(),
            pricing: Vec::new(),
            roles: self.roles,
        }
    }

    /// Returns the candidates of `role`, never empty because
    /// [`CatalogBuilder::assign`] removes a role assigned no candidates.
    fn role_candidates(&self, role: &Role) -> Result<&[ModelId], CatalogError> {
        self.roles
            .get(role)
            .map(Vec::as_slice)
            .ok_or_else(|| CatalogError::UnassignedRole { role: role.clone() })
    }
}

impl fmt::Debug for ModelCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelCatalog")
            .field("models", &self.entries.keys().collect::<Vec<_>>())
            .field("roles", &self.roles)
            .finish()
    }
}

/// Configuration of a [`ModelCatalog`], checked by [`Self::build`].
#[must_use = "a builder does nothing until built"]
pub struct CatalogBuilder {
    models: Vec<(ModelSpec, Arc<dyn Model>)>,
    pricing: Vec<(ModelId, Pricing)>,
    roles: BTreeMap<Role, Vec<ModelId>>,
}

impl CatalogBuilder {
    /// Catalogs `model` under its specification's identifier.
    pub fn model(mut self, spec: ModelSpec, model: Arc<dyn Model>) -> Self {
        self.models.push((spec, model));

        self
    }

    /// Assigns `role` its candidate models in order, replacing an earlier
    /// assignment. An empty list removes the role.
    pub fn assign<Id: Into<ModelId>>(
        mut self,
        role: Role,
        candidates: impl IntoIterator<Item = Id>,
    ) -> Self {
        let candidates = candidates.into_iter().map(Into::into).collect::<Vec<_>>();
        if candidates.is_empty() {
            self.roles.remove(&role);
        } else {
            self.roles.insert(role, candidates);
        }

        self
    }

    /// Replaces the prices of the model `id`, such as with a negotiated
    /// rate.
    pub fn pricing(mut self, id: impl Into<ModelId>, pricing: Pricing) -> Self {
        self.pricing.push((id.into(), pricing));

        self
    }

    /// Checks the configuration and returns the catalog.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::DuplicateModel`] for an identifier cataloged
    /// twice, [`CatalogError::InvalidProfile`] for a profile that leaves no
    /// request capacity, and [`CatalogError::UnknownModel`] for a role
    /// candidate or price override naming an uncataloged model.
    pub fn build(self) -> Result<ModelCatalog, CatalogError> {
        let mut entries = BTreeMap::new();
        for (spec, model) in self.models {
            let context_budget = spec
                .profile()
                .map(ModelProfile::context_budget)
                .transpose()
                .map_err(|source| CatalogError::InvalidProfile {
                    id: spec.id().clone(),
                    source,
                })?;
            match entries.entry(spec.id().clone()) {
                Entry::Occupied(occupied) => {
                    return Err(CatalogError::DuplicateModel {
                        id: occupied.key().clone(),
                    });
                }
                Entry::Vacant(vacant) => {
                    vacant.insert(CatalogEntry {
                        context_budget,
                        model,
                        spec,
                    });
                }
            }
        }
        for (id, pricing) in self.pricing {
            let entry = entries
                .get_mut(&id)
                .ok_or(CatalogError::UnknownModel { id })?;
            entry.spec = entry.spec.clone().with_pricing(pricing);
        }
        if let Some(id) = self
            .roles
            .values()
            .flatten()
            .find(|id| !entries.contains_key(*id))
        {
            return Err(CatalogError::UnknownModel { id: id.clone() });
        }

        Ok(ModelCatalog {
            entries,
            roles: self.roles,
        })
    }
}

/// One resolved catalog model: the model, its specification, and the
/// request budget its profile declares.
#[derive(Clone)]
#[non_exhaustive]
pub struct ModelHandle {
    context_budget: Option<ContextBudget>,
    model: Arc<dyn Model>,
    spec: ModelSpec,
}

impl ModelHandle {
    /// Returns the model.
    pub fn model(&self) -> Arc<dyn Model> {
        Arc::clone(&self.model)
    }

    /// Returns the model's specification.
    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// Returns the model's catalog key.
    pub fn id(&self) -> &ModelId {
        self.spec.id()
    }

    /// Returns the declared context profile, if known.
    pub fn profile(&self) -> Option<ModelProfile> {
        self.spec.profile()
    }

    /// Returns the request budget derived from the profile, if declared.
    pub fn context_budget(&self) -> Option<ContextBudget> {
        self.context_budget
    }
}

impl fmt::Debug for ModelHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModelHandle")
            .field("spec", &self.spec)
            .field("context_budget", &self.context_budget)
            .finish_non_exhaustive()
    }
}

/// Failure to build a [`ModelCatalog`] or resolve a [`ModelSelector`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// A configured built-in provider's environment is invalid.
    #[error("cannot catalog {provider} models")]
    Configuration {
        /// Provider whose configuration failed.
        provider: ModelProvider,
        /// Configuration failure.
        #[source]
        source: ModelConfigurationError,
    },
    /// Two models share one identifier.
    #[error("model `{id}` is cataloged twice")]
    DuplicateModel {
        /// Repeated identifier.
        id: ModelId,
    },
    /// A model's declared profile leaves no request capacity.
    #[error("model `{id}` declares an invalid context profile")]
    InvalidProfile {
        /// Model with the invalid profile.
        id: ModelId,
        /// Budget derivation failure.
        #[source]
        source: ContextBudgetError,
    },
    /// No candidate of a role has the needed capabilities.
    #[error("no `{role}` model has the capabilities {needs}")]
    MissingCapability {
        /// Capabilities no candidate has.
        needs: Capabilities,
        /// Role whose candidates were searched.
        role: Role,
    },
    /// A role has no candidates.
    #[error("no model is assigned to the `{role}` role")]
    UnassignedRole {
        /// Unassigned role.
        role: Role,
    },
    /// An identifier names no cataloged model.
    #[error("model `{id}` is not cataloged")]
    UnknownModel {
        /// Uncataloged identifier.
        id: ModelId,
    },
}

/// One cataloged model with its validated request budget.
struct CatalogEntry {
    context_budget: Option<ContextBudget>,
    model: Arc<dyn Model>,
    spec: ModelSpec,
}

impl CatalogEntry {
    fn handle(&self) -> ModelHandle {
        ModelHandle {
            context_budget: self.context_budget,
            model: Arc::clone(&self.model),
            spec: self.spec.clone(),
        }
    }
}

#[cfg(test)]
#[path = "model_catalog_test.rs"]
mod tests;
