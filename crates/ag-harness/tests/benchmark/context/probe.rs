//! A model wrapper that records which messages every reported request
//! sent, so a lost prompt-cache prefix is charged to the harness only when
//! it rewrote earlier messages.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::sync::{Arc, Mutex, PoisonError};

use ag_harness::model::{ModelClient, ModelCompletion, ModelMessage, ModelMetadata, ModelRequest};
use ag_harness::{Model, ModelError, OutputSchema, TurnInput};
use async_trait::async_trait;

/// Message digests of the requests a [`ProbedModel`] sent, one list per
/// request the turn report records, in order.
#[derive(Clone, Default)]
pub(super) struct SentRequests(Arc<Mutex<Vec<Vec<u64>>>>);

impl SentRequests {
    /// Takes the digests recorded since the last call.
    pub(super) fn take(&self) -> Vec<Vec<u64>> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Delegates to a provider client and digests each request's messages.
pub(super) struct ProbedModel {
    inner: ModelClient,
    sent: SentRequests,
}

impl ProbedModel {
    /// Wraps `inner`, returning the handle its sent requests are read from.
    pub(super) fn new(inner: ModelClient) -> (Self, SentRequests) {
        let sent = SentRequests::default();

        (
            Self {
                inner,
                sent: sent.clone(),
            },
            sent,
        )
    }
}

#[async_trait]
impl Model for ProbedModel {
    fn metadata(&self) -> Option<ModelMetadata> {
        Model::metadata(&self.inner)
    }

    fn validate_schema(&self, schema: &OutputSchema) -> Result<(), ModelError> {
        Model::validate_schema(&self.inner, schema)
    }

    fn validate_input(&self, input: &TurnInput) -> Result<(), ModelError> {
        Model::validate_input(&self.inner, input)
    }

    fn keeps_prefix_across_schemas(&self) -> bool {
        Model::keeps_prefix_across_schemas(&self.inner)
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelCompletion, ModelError> {
        let digests = request.messages().iter().map(digest).collect();
        let completion = Model::complete(&self.inner, request).await;
        // The turn report records answered requests and rejected outputs.
        let reported = match &completion {
            Ok(_) => true,
            Err(error) => matches!(
                error,
                ModelError::InvalidJson { .. } | ModelError::SchemaViolation { .. }
            ),
        };
        if reported {
            self.sent
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(digests);
        }

        completion
    }
}

/// Digests one message's full content, so equal digests mean the provider
/// received the same message.
fn digest(message: &ModelMessage) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{message:?}").hash(&mut hasher);

    hasher.finish()
}
