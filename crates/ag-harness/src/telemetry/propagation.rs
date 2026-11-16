//! Carries the caller's OpenTelemetry context into spawned work.

use std::future::Future;

#[cfg(feature = "otel")]
use opentelemetry::context::FutureExt as _;

/// Binds `future` to the caller's current OpenTelemetry context, so spans it
/// records keep the caller's span as their parent after it is spawned.
#[cfg(feature = "otel")]
pub(crate) fn with_current_context<F: Future>(future: F) -> impl Future<Output = F::Output> {
    future.with_current_context()
}

/// Returns `future` unchanged: without the `otel` feature there is no
/// OpenTelemetry context to carry.
#[cfg(not(feature = "otel"))]
pub(crate) fn with_current_context<F: Future>(future: F) -> F {
    future
}
