//! Opt-in OTLP trace export owned by the application composition root.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, Sampler, SdkTracerProvider,
};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};
use url::Url;

/// Owns the exporter and diagnostics until background work has stopped.
pub struct Telemetry {
    diagnostics: Arc<AtomicU64>,
    provider: SdkTracerProvider,
}

impl Telemetry {
    /// Starts OTLP HTTP/protobuf export only when the CLI supplied an endpoint.
    /// Environment variables alone never initialize tracing.
    ///
    /// # Errors
    /// Returns a bounded configuration error before the terminal UI starts.
    pub async fn start(endpoint: Option<&str>) -> Result<Option<Self>, String> {
        let Some(endpoint) = endpoint else {
            return Ok(None);
        };
        let endpoint = Self::validate_endpoint(endpoint)?.to_string();

        Self::initialize(move || Self::build(&endpoint))
            .await
            .map(Some)
    }

    /// Installs the application-owned provider and a terminal-safe diagnostic
    /// subscriber. Diagnostic text and authentication headers are not retained.
    ///
    /// # Errors
    /// Returns an error when the process already has a tracing subscriber.
    pub fn install(&self) -> Result<(), String> {
        Registry::default()
            .with(Diagnostics(Arc::clone(&self.diagnostics)))
            .try_init()
            .map_err(|_| "Could not install OTLP diagnostics subscriber".to_string())?;
        global::set_tracer_provider(self.provider.clone());

        Ok(())
    }

    /// Flushes finished spans after terminal restoration. Export failure never
    /// changes the application's result. Returns a coalesced diagnostic count.
    pub async fn shutdown(self) -> u64 {
        let diagnostics = Arc::clone(&self.diagnostics);
        let result = tokio::task::spawn_blocking(move || {
            self.provider.shutdown_with_timeout(Duration::from_secs(3))
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            diagnostics.fetch_add(1, Ordering::Relaxed);
        }

        diagnostics.load(Ordering::Relaxed)
    }

    /// Isolates blocking exporter initialization and bounds worker failures.
    async fn initialize(
        build: impl FnOnce() -> Result<Self, String> + Send + 'static,
    ) -> Result<Self, String> {
        tokio::task::spawn_blocking(build)
            .await
            .map_err(|_| "OTLP exporter initialization failed".to_string())?
    }

    fn validate_endpoint(endpoint: &str) -> Result<Url, String> {
        let invalid = || {
            "--otlp-endpoint requires a complete HTTP(S) traces URL, such as \
             http://localhost:4318/v1/traces"
                .to_string()
        };
        let url = Url::parse(endpoint).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid());
        }

        Ok(url)
    }

    fn build(endpoint: &str) -> Result<Self, String> {
        let exporter = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .with_timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| "Could not configure OTLP HTTP/protobuf export".to_string())?;
        let processor = BatchSpanProcessor::builder(exporter)
            .with_batch_config(
                BatchConfigBuilder::default()
                    .with_max_queue_size(2048)
                    .with_max_export_batch_size(512)
                    .with_scheduled_delay(Duration::from_secs(1))
                    .build(),
            )
            .build();
        let resource = Resource::builder_empty()
            .with_service_name("agentty")
            .with_attributes([
                KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                KeyValue::new("service.instance.id", uuid::Uuid::new_v4().to_string()),
            ])
            .build();

        Ok(Self {
            diagnostics: Arc::new(AtomicU64::new(0)),
            provider: SdkTracerProvider::builder()
                .with_resource(resource)
                .with_sampler(Sampler::AlwaysOn)
                .with_span_processor(processor)
                .build(),
        })
    }
}

struct Diagnostics(Arc<AtomicU64>);

impl<S: Subscriber> Layer<S> for Diagnostics {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target().starts_with("opentelemetry")
            && matches!(*metadata.level(), Level::WARN | Level::ERROR)
        {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
#[path = "telemetry_test.rs"]
mod tests;
