//! Opt-in OTLP HTTP/protobuf trace export for host composition roots.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, Sampler, SdkTracerProvider,
};
use thiserror::Error;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};
use url::Url;

/// Resource identity attached to every exported span.
#[derive(Clone, Copy, Debug)]
pub struct Service {
    /// Exported as `service.name`.
    pub name: &'static str,
    /// Exported as `service.version`.
    pub version: &'static str,
}

/// Bounded export setup failures that never echo the endpoint or credentials.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum OtlpError {
    /// The exporter rejected the validated endpoint.
    #[error("Could not configure OTLP HTTP/protobuf export")]
    Configuration,
    /// The endpoint is not a complete HTTP(S) traces URL.
    #[error(
        "--otlp-endpoint requires a complete HTTP(S) traces URL, such as \
         http://localhost:4318/v1/traces"
    )]
    Endpoint,
    /// The blocking exporter construction worker failed.
    #[error("OTLP exporter initialization failed")]
    Initialization,
}

/// Owns the exporter and its diagnostics until host work has stopped.
pub struct OtlpExport {
    diagnostics: Arc<AtomicU64>,
    provider: SdkTracerProvider,
}

impl OtlpExport {
    /// Starts export only when the host supplied an endpoint. Environment
    /// variables alone never enable tracing or override the endpoint;
    /// `OTEL_EXPORTER_OTLP_TRACES_HEADERS` and `OTEL_EXPORTER_OTLP_HEADERS`
    /// still supply authentication headers.
    ///
    /// # Errors
    /// Returns a bounded error for an invalid endpoint or exporter setup.
    pub async fn start(
        endpoint: Option<&str>,
        service: Service,
    ) -> Result<Option<Self>, OtlpError> {
        let Some(endpoint) = endpoint else {
            return Ok(None);
        };
        let endpoint = Self::validate_endpoint(endpoint)?.to_string();

        Self::initialize(move || Self::build(&endpoint, service))
            .await
            .map(Some)
    }

    /// Installs the process-wide provider and, when no subscriber exists yet,
    /// a diagnostic subscriber that counts exporter warnings without retaining
    /// their text. Repeated installs replace the provider and never fail;
    /// without the subscriber only shutdown failures are counted.
    pub fn install(&self) {
        let _ = Registry::default()
            .with(Diagnostics(Arc::clone(&self.diagnostics)))
            .try_init();
        global::set_tracer_provider(self.provider.clone());
    }

    /// Flushes finished spans. Export failure never changes the host result.
    /// Returns a coalesced diagnostic count.
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

    /// Isolates blocking exporter construction from the async runtime.
    async fn initialize(
        build: impl FnOnce() -> Result<Self, OtlpError> + Send + 'static,
    ) -> Result<Self, OtlpError> {
        tokio::task::spawn_blocking(build)
            .await
            .map_err(|_| OtlpError::Initialization)?
    }

    fn validate_endpoint(endpoint: &str) -> Result<Url, OtlpError> {
        let url = Url::parse(endpoint).map_err(|_| OtlpError::Endpoint)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.path() == "/"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(OtlpError::Endpoint);
        }

        Ok(url)
    }

    fn build(endpoint: &str, service: Service) -> Result<Self, OtlpError> {
        let exporter = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .with_timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| OtlpError::Configuration)?;
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
            .with_service_name(service.name)
            .with_attributes([
                KeyValue::new("service.version", service.version),
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
#[path = "otlp_test.rs"]
mod tests;
