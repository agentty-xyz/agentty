use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use thiserror::Error;

use crate::{ExecutionIdentity, Model, ModelMetadata};

/// Host-declared capabilities of a registered adapter, not tool permissions.
///
/// These describe the configured implementation; they do not enable features or
/// replace provider request validation. Text and locally validated structured
/// output are required by the model contract. Images are not supported.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ModelCapabilities {
    /// Whether the adapter can resume provider-native continuation identifiers.
    pub native_continuation: bool,
    /// Whether the adapter supports native tool calls and normalized tool
    /// history.
    pub tool_calls: bool,
}

/// Immutable registration retained by harnesses independently of the registry.
#[derive(Clone)]
pub struct ModelRegistration {
    capabilities: ModelCapabilities,
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

    pub(crate) fn model(&self) -> Arc<dyn Model> {
        Arc::clone(&self.model)
    }
}

/// Host model catalog keyed by [`ExecutionIdentity::key`].
///
/// Register built-in clients or injected [`Model`] implementations. Keys are
/// unique regardless of revision; construct a new registry to revise a
/// registration. Existing harnesses retain their selected model and identity.
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
    /// behavior changes.
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::DuplicateKey`] without replacing an
    /// existing registration when the key is already present, even with
    /// another revision.
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
    /// Returns [`ModelRegistryError::DuplicateKey`] without replacing an
    /// existing registration when the key is already present.
    pub fn register_shared(
        &mut self,
        identity: ExecutionIdentity,
        model: Arc<dyn Model>,
        capabilities: ModelCapabilities,
    ) -> Result<(), ModelRegistryError> {
        match self.models.entry(identity.key().to_string()) {
            Entry::Occupied(entry) => Err(ModelRegistryError::DuplicateKey {
                key: entry.key().clone(),
            }),
            Entry::Vacant(entry) => {
                entry.insert(ModelRegistration {
                    capabilities,
                    identity,
                    model,
                });

                Ok(())
            }
        }
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
}

/// Failure to register or select a host model key.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ModelRegistryError {
    /// A registration already owns this key.
    #[error("model key `{key}` is already registered")]
    DuplicateKey {
        /// Conflicting host key.
        key: String,
    },
    /// No registration owns this key.
    #[error("unknown model key `{key}`")]
    UnknownKey {
        /// Requested host key.
        key: String,
    },
}
