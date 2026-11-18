//! Model request metrics for builds without the `otel` feature, which
//! record nothing.

use crate::model::{CompletionMetadata, ModelError, ModelMetadata};

/// Records nothing for one model request.
pub(crate) struct RequestMetrics;

impl RequestMetrics {
    /// Starts recording one model request.
    pub(crate) fn start(_metadata: &ModelMetadata) -> Self {
        Self
    }

    /// Ends the request without recording it.
    pub(crate) fn completed(self, _metadata: Option<&CompletionMetadata>) {
        let Self = self;
    }

    /// Ends the failed request without recording it.
    pub(crate) fn failed(self, _error: &ModelError, _metadata: Option<&CompletionMetadata>) {
        let Self = self;
    }
}
