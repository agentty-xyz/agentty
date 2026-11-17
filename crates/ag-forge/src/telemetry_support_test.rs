//! Crate-wide span capture for telemetry assertions.

use std::future::Future;

use ag_telemetry::{Span, TraceContextExt};
use opentelemetry::trace::SpanId;
use opentelemetry::{Value, global};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tokio::sync::{Mutex, MutexGuard};

/// Serializes tests that replace the global tracer provider.
static TRACER_PROVIDER_LOCK: Mutex<()> = Mutex::const_new(());

/// Installs an in-memory exporter for the lifetime of one test.
pub(crate) struct SpanCapture {
    _guard: MutexGuard<'static, ()>,
    exporter: InMemorySpanExporter,
}

impl SpanCapture {
    /// Replaces the global tracer provider with an in-memory exporter.
    pub(crate) async fn install() -> Self {
        let guard = TRACER_PROVIDER_LOCK.lock().await;
        let exporter = InMemorySpanExporter::default();
        global::set_tracer_provider(
            SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build(),
        );

        Self {
            _guard: guard,
            exporter,
        }
    }

    /// Runs `work` beneath a fresh root span and returns its result with the
    /// spans finished in that root's trace.
    pub(crate) async fn trace<T>(&self, work: impl Future<Output = T>) -> (T, CapturedTrace) {
        let root = Span::root("test.root", Vec::new());
        let root_context = root.context();
        let root_id = root_context.span().span_context().span_id();
        let trace_id = root_context.span().span_context().trace_id();
        let value = root.scope(work).await;
        let spans = self
            .finished_spans()
            .into_iter()
            .filter(|span| span.span_context.trace_id() == trace_id)
            .collect();

        (value, CapturedTrace { root_id, spans })
    }

    /// Returns every span finished since installation.
    pub(crate) fn finished_spans(&self) -> Vec<SpanData> {
        self.exporter.get_finished_spans().expect("finished spans")
    }
}

/// Spans finished beneath one test root.
pub(crate) struct CapturedTrace {
    /// Span id of the test root.
    pub(crate) root_id: SpanId,
    /// Finished spans that belong to the root's trace.
    pub(crate) spans: Vec<SpanData>,
}

impl CapturedTrace {
    /// Returns every captured span named `name`.
    pub(crate) fn named(&self, name: &str) -> Vec<&SpanData> {
        self.spans.iter().filter(|span| span.name == name).collect()
    }
}

/// Returns the value recorded for `key` on `span`.
pub(crate) fn attribute<'span>(span: &'span SpanData, key: &str) -> Option<&'span Value> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| &attribute.value)
}

/// Returns the sorted attribute keys recorded on `span`.
pub(crate) fn attribute_keys(span: &SpanData) -> Vec<&str> {
    let mut keys: Vec<&str> = span
        .attributes
        .iter()
        .map(|attribute| attribute.key.as_str())
        .collect();
    keys.sort_unstable();

    keys
}
