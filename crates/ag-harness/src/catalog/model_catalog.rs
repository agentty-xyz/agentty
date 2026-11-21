//! Model catalog: cataloged models with their specifications, role
//! assignments, and selector resolution.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use std::env;
use std::fmt;
use std::sync::Arc;

#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use ag_router::chatgpt::ChatGptAuthError;
use thiserror::Error;
use tokio::sync::Semaphore;

use super::role::{ModelSelector, Role};
use super::spec::{Capabilities, ModelId, ModelSpec, Pricing, PricingSource};
use crate::context::{ContextBudget, ContextBudgetError};
use crate::model::Model;
use crate::model_profile::ModelProfile;
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
use crate::provider::{
    CHATGPT_GPT_6_1_SOL, CHATGPT_GPT_6_LUNA, CLAUDE_HAIKU_5_5, CLAUDE_OPUS_5_5, GPT_6_1_SOL,
    GPT_6_LUNA, KIMI_K2_6, KIMI_K3, MUSE_SPARK_1_3, ModelConfiguration, ModelConfigurationError,
    ModelProvider, QWEN3_7_FLASH, QWEN3_8_FLASH, QWEN3_8_MAX,
};

/// Built-in models, by provider and model identifier, in preference order.
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
type Candidates = &'static [(ModelProvider, &'static str)];

/// Candidates of [`Role::SMART`] and [`Role::VISION`] in the environment
/// catalog, plan-billed first.
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
const SMART_CANDIDATES: Candidates = &[
    (ModelProvider::ChatGpt, CHATGPT_GPT_6_1_SOL),
    (ModelProvider::OpenAi, GPT_6_1_SOL),
    (ModelProvider::Muse, MUSE_SPARK_1_3),
    (ModelProvider::Kimi, KIMI_K3),
    (ModelProvider::Qwen, QWEN3_8_MAX),
    (ModelProvider::Anthropic, CLAUDE_OPUS_5_5),
];
/// Candidates of [`Role::CHEAP`] in the environment catalog.
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
const CHEAP_CANDIDATES: Candidates = &[
    (ModelProvider::ChatGpt, CHATGPT_GPT_6_LUNA),
    (ModelProvider::OpenAi, GPT_6_LUNA),
    (ModelProvider::Qwen, QWEN3_8_FLASH),
    (ModelProvider::Qwen, QWEN3_7_FLASH),
    (ModelProvider::Kimi, KIMI_K2_6),
    (ModelProvider::Anthropic, CLAUDE_HAIKU_5_5),
];
/// Candidates of [`Role::FAST`] in the environment catalog.
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
const FAST_CANDIDATES: Candidates = &[
    (ModelProvider::ChatGpt, CHATGPT_GPT_6_LUNA),
    (ModelProvider::OpenAi, GPT_6_LUNA),
    (ModelProvider::Qwen, QWEN3_8_FLASH),
    (ModelProvider::Qwen, QWEN3_7_FLASH),
    (ModelProvider::Anthropic, CLAUDE_HAIKU_5_5),
];
/// Default role candidates of the environment catalog, in order.
#[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
const DEFAULT_ROLES: &[(Role, Candidates)] = &[
    (Role::SMART, SMART_CANDIDATES),
    (Role::CHEAP, CHEAP_CANDIDATES),
    (Role::FAST, FAST_CANDIDATES),
    (Role::VISION, SMART_CANDIDATES),
];

/// Models a host can select by identifier, role, or required capability.
///
/// A catalog is a resolver for hosts, not a harness dependency: resolve a
/// [`ModelSelector`] to a [`ModelHandle`] and hand its model to
/// [`crate::HarnessBuilder::model`], or give the catalog to
/// [`crate::HarnessBuilder::catalog`]. Each role maps to ordered candidate
/// models, every one of which has the capabilities the role requires (see
/// [`CatalogBuilder::require`]); resolving a role picks its first
/// candidate, and [`ModelSelector::Require`] its first candidate with the
/// additionally needed capabilities.
///
/// With a `providers-*` feature, [`Self::from_environment`] catalogs every
/// built-in model whose provider the build includes and the environment
/// configures, with these default role candidates:
///
/// | Role             | Candidates, in order                                                                                                                  |
/// | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
/// | [`Role::SMART`]  | `chatgpt/gpt-6.1-sol`, `openai/gpt-6.1-sol`, `muse/muse-spark-1.3`, `kimi/kimi-k3`, `qwen/qwen3.8-max`, `anthropic/claude-opus-5-5`   |
/// | [`Role::CHEAP`]  | `chatgpt/gpt-6-luna`, `openai/gpt-6-luna`, `qwen/qwen3.8-flash`, `qwen/qwen3.7-flash`, `kimi/kimi-k2.6`, `anthropic/claude-haiku-5-5` |
/// | [`Role::FAST`]   | `chatgpt/gpt-6-luna`, `openai/gpt-6-luna`, `qwen/qwen3.8-flash`, `qwen/qwen3.7-flash`, `anthropic/claude-haiku-5-5`                   |
/// | [`Role::VISION`] | the [`Role::SMART`] candidates that read images                                                                                       |
///
/// A role keeps only the candidates that are cataloged, declare a context
/// profile, and have the role's required capabilities, so every default
/// resolves to a model a harness can run without an explicit
/// [`crate::HarnessBuilder::context_budget`]. The `qwen/qwen3.8-max` entry
/// declares no profile yet, so it is cataloged but not assigned. Plan-billed
/// `ChatGPT` models come first because they have no per-token charge. Change
/// any assignment with [`Self::into_builder`] and [`CatalogBuilder::assign`].
pub struct ModelCatalog {
    entries: BTreeMap<ModelId, CatalogEntry>,
    requirements: BTreeMap<Role, Capabilities>,
    roles: BTreeMap<Role, Vec<ModelId>>,
}

impl ModelCatalog {
    /// Starts an empty catalog whose [`Role::VISION`] candidates must read
    /// image input.
    pub fn builder() -> CatalogBuilder {
        CatalogBuilder {
            models: Vec::new(),
            pricing: Vec::new(),
            requirements: BTreeMap::from([(
                Role::VISION,
                Capabilities::NONE.with_image_input(true),
            )]),
            roles: BTreeMap::new(),
        }
    }

    /// Catalogs the built-in models of every provider the process
    /// environment configures; see [`Self::from_environment_with`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] like [`Self::from_environment_with`].
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
    pub fn from_environment() -> Result<Self, CatalogError> {
        Self::from_environment_with(|name| env::var(name))
    }

    /// Catalogs every known model of each built-in provider that
    /// `environment` configures, under its [`ModelId::cataloged`] key, with
    /// the default role candidates documented on [`ModelCatalog`].
    ///
    /// A provider whose API key, base URL, or `ChatGPT` sign-in is absent, or
    /// whose `providers-*` feature this build omits, is skipped. Tiers whose
    /// prompts the provider trains on, such as Muse's contributor tier, are
    /// never cataloged here; a host that accepts that adds them with
    /// [`CatalogBuilder::model`]. OpenAI-compatible servers name their own
    /// models, so hosts add those the same way.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::Configuration`] when a provider is configured
    /// but broken, such as an API key or base URL that is not valid Unicode,
    /// or a `ChatGPT` credential file that exists but cannot be read.
    ///
    /// `ChatGPT` models read the credential file `CHATGPT_AUTH_FILE` names;
    /// a host with its own sign-in location passes it to
    /// [`Self::from_environment_with_auth_file`] instead, so its plan-billed
    /// models keep their place ahead of per-token ones.
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
    pub fn from_environment_with(
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> Result<Self, CatalogError> {
        Self::catalog_environment(environment, None)
    }

    /// Catalogs like [`Self::from_environment_with`], reading the `ChatGPT`
    /// sign-in from the host-chosen `auth_file`, such as the one
    /// [`crate::provider::chatgpt_auth_file_in`] locates, in place of
    /// `CHATGPT_AUTH_FILE`; `None` catalogs exactly like
    /// [`Self::from_environment_with`].
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] like [`Self::from_environment_with`].
    #[cfg(feature = "providers-openai")]
    pub fn from_environment_with_auth_file(
        environment: impl FnMut(&str) -> Result<String, env::VarError>,
        auth_file: Option<&std::path::Path>,
    ) -> Result<Self, CatalogError> {
        Self::catalog_environment(environment, auth_file)
    }

    /// Catalogs the configured providers' models, passing `auth_file` to
    /// each `ChatGPT` model's configuration when the host chose one.
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
    fn catalog_environment(
        mut environment: impl FnMut(&str) -> Result<String, env::VarError>,
        auth_file: Option<&std::path::Path>,
    ) -> Result<Self, CatalogError> {
        let mut builder = Self::builder();
        for provider in ModelProvider::all()
            .iter()
            .filter(|provider| provider.is_enabled())
        {
            let cataloged = provider
                .known_models()
                .iter()
                .filter(|model| !provider.trains_on_prompts(model))
                .filter_map(|model| Some((*model, provider.model_spec(model)?)));
            for (model, spec) in cataloged {
                let configuration = ModelConfiguration::new(*provider, model);
                #[cfg(feature = "providers-openai")]
                let configuration = match auth_file {
                    Some(auth_file) if provider.requires_sign_in() => {
                        configuration.auth_file(auth_file)
                    }
                    Some(_) | None => configuration,
                };
                #[cfg(not(feature = "providers-openai"))]
                let _ = auth_file;
                match configuration.client_from_environment(&mut environment) {
                    Ok(client) => builder = builder.model(spec, Arc::new(client)),
                    Err(source) if Self::is_unconfigured(&source, &mut environment) => break,
                    Err(source) => {
                        return Err(CatalogError::Configuration {
                            provider: *provider,
                            source,
                        });
                    }
                }
            }
        }

        Self::assign_default_roles(builder.build()?, DEFAULT_ROLES).build()
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

    /// Returns the capabilities every candidate of `role` has.
    pub fn requirement(&self, role: &Role) -> Capabilities {
        self.requirements
            .get(role)
            .copied()
            .unwrap_or(Capabilities::NONE)
    }

    /// Returns a builder holding this catalog's models, role requirements,
    /// and role assignments, so a host can adjust them.
    pub fn into_builder(self) -> CatalogBuilder {
        CatalogBuilder {
            models: self
                .entries
                .into_values()
                .map(|entry| (entry.spec, entry.model))
                .collect(),
            pricing: Vec::new(),
            requirements: self.requirements,
            roles: self.roles,
        }
    }

    /// Returns whether `error` means the provider is not configured at all,
    /// as opposed to configured but broken.
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
    fn is_unconfigured(
        error: &ModelConfigurationError,
        environment: &mut impl FnMut(&str) -> Result<String, env::VarError>,
    ) -> bool {
        match error {
            ModelConfigurationError::ApiKey { name } => {
                matches!(environment(name), Err(env::VarError::NotPresent))
            }
            ModelConfigurationError::AuthFile(_)
            | ModelConfigurationError::BaseUrl { .. }
            | ModelConfigurationError::SignIn(ChatGptAuthError::NotSignedIn { .. }) => true,
            _ => false,
        }
    }

    /// Returns a builder of `catalog` assigning each role of `roles` its
    /// candidates that `catalog` holds with a context profile and the
    /// role's required capabilities.
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
    fn assign_default_roles(catalog: Self, roles: &[(Role, Candidates)]) -> CatalogBuilder {
        let assignments = roles
            .iter()
            .map(|(role, candidates)| {
                let needs = catalog.requirement(role);
                let usable = candidates
                    .iter()
                    .map(|(provider, model)| ModelId::cataloged(*provider, model))
                    .filter(|id| {
                        catalog.spec(id).is_some_and(|spec| {
                            spec.profile().is_some() && spec.capabilities().satisfies(needs)
                        })
                    })
                    .collect::<Vec<_>>();

                (role.clone(), usable)
            })
            .collect::<Vec<_>>();

        assignments
            .into_iter()
            .fold(catalog.into_builder(), |builder, (role, usable)| {
                builder.assign(role, usable)
            })
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
            .field("requirements", &self.requirements)
            .field("roles", &self.roles)
            .finish()
    }
}

/// Configuration of a [`ModelCatalog`], checked by [`Self::build`].
#[must_use = "a builder does nothing until built"]
pub struct CatalogBuilder {
    models: Vec<(ModelSpec, Arc<dyn Model>)>,
    pricing: Vec<(ModelId, Pricing)>,
    requirements: BTreeMap<Role, Capabilities>,
    roles: BTreeMap<Role, Vec<ModelId>>,
}

impl CatalogBuilder {
    /// Catalogs `model` under its specification's identifier.
    ///
    /// A specification without a profile adopts the model's declared
    /// [`Model::profile`], if any.
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

    /// Requires every candidate of `role` to have `needs`, replacing an
    /// earlier requirement; [`Capabilities::NONE`] removes it.
    /// [`ModelCatalog::builder`] starts with [`Role::VISION`] requiring
    /// image input.
    pub fn require(mut self, role: Role, needs: Capabilities) -> Self {
        if needs == Capabilities::NONE {
            self.requirements.remove(&role);
        } else {
            self.requirements.insert(role, needs);
        }

        self
    }

    /// Replaces the prices of the model `id`, such as with a negotiated
    /// rate, attributing them to [`PricingSource::Host`].
    pub fn pricing(mut self, id: impl Into<ModelId>, pricing: Pricing) -> Self {
        self.pricing
            .push((id.into(), pricing.with_source(PricingSource::Host)));

        self
    }

    /// Checks the configuration and returns the catalog.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::DuplicateModel`] for an identifier cataloged
    /// twice, [`CatalogError::InvalidProfile`] for a profile that leaves no
    /// request capacity, [`CatalogError::UnknownModel`] for a role
    /// candidate or price override naming an uncataloged model, and
    /// [`CatalogError::IncapableCandidate`] for a role candidate without
    /// the role's required capabilities.
    pub fn build(self) -> Result<ModelCatalog, CatalogError> {
        let mut entries = BTreeMap::new();
        for (mut spec, model) in self.models {
            if spec.profile().is_none()
                && let Some(profile) = model.profile()
            {
                spec = spec.with_profile(profile);
            }
            let concurrency = spec.max_concurrency().map(|permits| {
                Arc::new(Semaphore::new(
                    usize::try_from(permits.get())
                        .unwrap_or(usize::MAX)
                        .min(Semaphore::MAX_PERMITS),
                ))
            });
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
                        concurrency,
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
        for (role, candidates) in &self.roles {
            let needs = self
                .requirements
                .get(role)
                .copied()
                .unwrap_or(Capabilities::NONE);
            for id in candidates {
                let entry = entries
                    .get(id)
                    .ok_or_else(|| CatalogError::UnknownModel { id: id.clone() })?;
                if !entry.spec.capabilities().satisfies(needs) {
                    return Err(CatalogError::IncapableCandidate {
                        id: id.clone(),
                        needs,
                        role: role.clone(),
                    });
                }
            }
        }

        Ok(ModelCatalog {
            entries,
            requirements: self.requirements,
            roles: self.roles,
        })
    }
}

/// One resolved catalog model: the model, its specification, and the
/// request budget its profile declares.
#[derive(Clone)]
#[non_exhaustive]
pub struct ModelHandle {
    concurrency: Option<Arc<Semaphore>>,
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

    /// Returns the effective context profile, from the specification or
    /// else the model's declared [`Model::profile`], if known.
    pub fn profile(&self) -> Option<ModelProfile> {
        self.spec.profile()
    }

    /// Returns the request budget derived from [`Self::profile`], if
    /// declared.
    pub fn context_budget(&self) -> Option<ContextBudget> {
        self.context_budget
    }

    /// Returns the permits of [`ModelSpec::max_concurrency`], shared by
    /// every handle of this catalog entry, or `None` without a cap.
    pub(crate) fn concurrency(&self) -> Option<Arc<Semaphore>> {
        self.concurrency.clone()
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
    #[cfg(any(feature = "providers-compat", feature = "providers-openai"))]
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
    /// A role candidate lacks capabilities the role requires.
    #[error("model `{id}` lacks the capabilities {needs} the `{role}` role requires")]
    IncapableCandidate {
        /// Candidate without the capabilities.
        id: ModelId,
        /// Capabilities the role requires.
        needs: Capabilities,
        /// Role the candidate was assigned.
        role: Role,
    },
    /// A model's declared profile leaves no request capacity.
    #[error("model `{id}` declares an invalid context profile: {source}")]
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

/// One cataloged model with its validated request budget and the permits
/// of its concurrency cap.
struct CatalogEntry {
    concurrency: Option<Arc<Semaphore>>,
    context_budget: Option<ContextBudget>,
    model: Arc<dyn Model>,
    spec: ModelSpec,
}

impl CatalogEntry {
    fn handle(&self) -> ModelHandle {
        ModelHandle {
            concurrency: self.concurrency.clone(),
            context_budget: self.context_budget,
            model: Arc::clone(&self.model),
            spec: self.spec.clone(),
        }
    }
}

#[cfg(test)]
#[path = "model_catalog_test.rs"]
mod tests;
