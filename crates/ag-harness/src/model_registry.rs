//! Registry of keyed model registrations with their declared capabilities
//! and execution identities.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use thiserror::Error;

use crate::catalog::{ModelHandle, ModelSpec};
use crate::model::{ContextBudget, ContextBudgetError, ModelMetadata, ModelProfile};
use crate::recovery::ExecutionIdentity;
use crate::{Model, TurnInput};

/// Host-declared capabilities of a registered adapter, not tool permissions.
///
/// These describe the configured implementation; they do not enable features or
/// replace provider request validation. Text and locally validated structured
/// output are required by the model contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelCapabilities {
    /// Whether the adapter accepts image-bearing user input and history.
    pub image_input: bool,
    /// Declared model context, from which registration derives the request
    /// budget bounding every provider request.
    pub profile: ModelProfile,
    /// Whether the adapter supports native tool calls and normalized tool
    /// history.
    pub tool_calls: bool,
}

impl ModelCapabilities {
    /// Declares a text-only adapter without native tool calls under the
    /// required model profile.
    pub fn new(profile: ModelProfile) -> Self {
        Self {
            image_input: false,
            profile,
            tool_calls: false,
        }
    }
}

/// Immutable registration retained by harnesses independently of the registry.
#[derive(Clone)]
pub struct ModelRegistration {
    capabilities: ModelCapabilities,
    /// Catalog entry the registration came from, whose prices, concurrency
    /// cap, and maximum output harnesses adopt with the model.
    catalog_entry: Option<ModelHandle>,
    context_budget: ContextBudget,
    identity: ExecutionIdentity,
    model: Arc<dyn Model>,
}

impl ModelRegistration {
    /// Returns the host identity retained by sessions and request fingerprints.
    pub fn identity(&self) -> &ExecutionIdentity {
        &self.identity
    }

    /// Returns the capabilities declared by the configuring host.
    pub fn capabilities(&self) -> ModelCapabilities {
        self.capabilities
    }

    /// Returns the adapter's provider/model metadata, when available.
    pub fn metadata(&self) -> Option<ModelMetadata> {
        self.model.metadata()
    }

    /// Returns the catalog specification of a registration made from a
    /// catalog entry, or `None` for one made from a bare model.
    pub fn catalog_spec(&self) -> Option<&ModelSpec> {
        self.catalog_entry.as_ref().map(ModelHandle::spec)
    }

    /// Returns the registration of the catalog entry `handle` under its
    /// catalog identifier and `revision`, declaring the entry's tool calls,
    /// image input, and context profile.
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::InvalidIdentity`] when the identifier
    /// and `revision` do not form an [`ExecutionIdentity`], and
    /// [`ModelRegistryError::ProfileRequired`] for an entry without a
    /// context profile.
    pub(crate) fn from_catalog_entry(
        handle: &ModelHandle,
        revision: &str,
    ) -> Result<Self, ModelRegistryError> {
        let key = handle.id().as_str();
        let identity = ExecutionIdentity::new(key, revision).map_err(|_| {
            ModelRegistryError::InvalidIdentity {
                key: key.to_string(),
                revision: revision.to_string(),
            }
        })?;
        // The catalog derives an entry's budget from its profile when it
        // builds, so both are present or both absent.
        let (Some(profile), Some(context_budget)) = (handle.profile(), handle.context_budget())
        else {
            return Err(ModelRegistryError::ProfileRequired {
                key: key.to_string(),
            });
        };
        let declared = handle.spec().capabilities();

        Ok(Self {
            capabilities: ModelCapabilities {
                image_input: declared.image_input(),
                profile,
                tool_calls: declared.tool_calls(),
            },
            catalog_entry: Some(handle.clone()),
            context_budget,
            identity,
            model: handle.model(),
        })
    }

    /// Returns the declared capabilities narrowed by what the adapter accepts,
    /// so a declaration alone never admits image history the adapter rejects.
    pub(crate) fn history_capabilities(&self) -> ModelCapabilities {
        let mut capabilities = self.capabilities;
        capabilities.image_input = capabilities.image_input
            && self.model.validate_input(&TurnInput::image_probe()).is_ok();

        capabilities
    }

    /// Returns the request budget derived from the declared profile.
    pub(crate) fn context_budget(&self) -> ContextBudget {
        self.context_budget
    }

    pub(crate) fn model(&self) -> Arc<dyn Model> {
        Arc::clone(&self.model)
    }

    /// Returns the catalog entry the registration came from, if any.
    pub(crate) fn catalog_entry(&self) -> Option<&ModelHandle> {
        self.catalog_entry.as_ref()
    }
}

/// Host model catalog keyed by [`ExecutionIdentity::key`].
///
/// Register built-in clients, injected [`Model`] implementations, or
/// [`crate::catalog::ModelCatalog`] entries. Keys are unique regardless of
/// revision; construct a new registry to revise a registration. Existing
/// harnesses retain their selected model and identity.
#[derive(Default)]
pub struct ModelRegistry {
    models: HashMap<String, ModelRegistration>,
}

impl ModelRegistry {
    /// Creates an empty host model catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a model and its host-asserted execution identity/capabilities.
    ///
    /// Revise the identity whenever model configuration, capability
    /// declarations, endpoints, credential scope, or injected execution
    /// behavior changes. A revised [`ModelProfile`] alone needs no new
    /// revision: it only shapes how new requests are fitted.
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::InvalidProfile`] when the declared
    /// profile leaves no request capacity, and
    /// [`ModelRegistryError::DuplicateKey`] without replacing an existing
    /// registration when the key is already present, even with another
    /// revision.
    pub fn register(
        &mut self,
        identity: ExecutionIdentity,
        model: impl Model + 'static,
        capabilities: ModelCapabilities,
    ) -> Result<(), ModelRegistryError> {
        self.register_shared(identity, Arc::new(model), capabilities)
    }

    /// Registers an already shared model without wrapping or copying it.
    ///
    /// A boxed trait object can be passed as `Arc::from(boxed_model)`.
    /// The identity and capability contract is the same as [`Self::register`].
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::InvalidProfile`] for a profile without
    /// request capacity, and [`ModelRegistryError::DuplicateKey`] without
    /// replacing an existing registration when the key is already present.
    pub fn register_shared(
        &mut self,
        identity: ExecutionIdentity,
        model: Arc<dyn Model>,
        capabilities: ModelCapabilities,
    ) -> Result<(), ModelRegistryError> {
        let context_budget = capabilities.profile.context_budget().map_err(|source| {
            ModelRegistryError::InvalidProfile {
                key: identity.key().to_string(),
                source,
            }
        })?;

        self.insert(ModelRegistration {
            capabilities,
            catalog_entry: None,
            context_budget,
            identity,
            model,
        })
    }

    /// Registers the catalog entry `handle` under its
    /// [`crate::catalog::ModelId`] as key and the host's `revision`.
    ///
    /// The registration declares the entry's
    /// [`crate::catalog::Capabilities::tool_calls`],
    /// [`crate::catalog::Capabilities::image_input`], and context profile,
    /// exactly as [`Self::register_shared`] would with those
    /// [`ModelCapabilities`], so a session recorded on an equal manual
    /// registration resumes on this one and the reverse. Unlike a bare
    /// model, it also keeps the entry's prices, concurrency cap, and
    /// maximum output, which [`crate::Harness::from_registry`] and
    /// [`crate::Session::switch_model`] adopt with the model. An entry
    /// without a context profile cannot be registered.
    /// [`crate::HarnessBuilder::registration_revision`] records the same
    /// registration for a harness built from the catalog. Revise
    /// `revision` as [`Self::register`] describes.
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::InvalidIdentity`] when the identifier
    /// and `revision` do not form an [`ExecutionIdentity`],
    /// [`ModelRegistryError::ProfileRequired`] for an entry without a
    /// context profile, and [`ModelRegistryError::DuplicateKey`] without
    /// replacing an existing registration when the key is already present.
    pub fn register_catalog_entry(
        &mut self,
        handle: &ModelHandle,
        revision: impl Into<String>,
    ) -> Result<(), ModelRegistryError> {
        self.insert(ModelRegistration::from_catalog_entry(
            handle,
            &revision.into(),
        )?)
    }

    /// Resolves an exact host key without contacting a provider.
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::UnknownKey`] for an unregistered key.
    pub fn resolve(&self, key: &str) -> Result<&ModelRegistration, ModelRegistryError> {
        self.models
            .get(key)
            .ok_or_else(|| ModelRegistryError::UnknownKey {
                key: key.to_string(),
            })
    }

    /// Adds `registration` unless its key is already registered.
    fn insert(&mut self, registration: ModelRegistration) -> Result<(), ModelRegistryError> {
        match self.models.entry(registration.identity.key().to_string()) {
            Entry::Occupied(entry) => Err(ModelRegistryError::DuplicateKey {
                key: entry.key().clone(),
            }),
            Entry::Vacant(entry) => {
                entry.insert(registration);

                Ok(())
            }
        }
    }
}

/// Failure to register or select a host model key.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum ModelRegistryError {
    /// A registration already owns this key.
    #[error("model key `{key}` is already registered")]
    DuplicateKey {
        /// Conflicting host key.
        key: String,
    },
    /// A catalog identifier and revision do not form an execution identity.
    #[error("model key `{key}` with revision `{revision}` is not a valid execution identity")]
    InvalidIdentity {
        /// Rejected catalog identifier.
        key: String,
        /// Rejected host revision.
        revision: String,
    },
    /// The declared profile leaves no request capacity.
    #[error("model key `{key}` declares an invalid context profile: {source}")]
    InvalidProfile {
        /// Rejected host key.
        key: String,
        /// Budget derivation failure.
        #[source]
        source: ContextBudgetError,
    },
    /// A catalog entry declares no context profile, which a registration
    /// needs to bound its requests.
    #[error("model key `{key}` declares no context profile")]
    ProfileRequired {
        /// Catalog identifier of the entry.
        key: String,
    },
    /// No registration owns this key.
    #[error("unknown model key `{key}`")]
    UnknownKey {
        /// Requested host key.
        key: String,
    },
}
