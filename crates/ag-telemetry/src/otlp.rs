//! Opt-in OTLP HTTP/protobuf trace and event-log export for host composition
//! roots.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{LogExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
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

/// Emits content-free host events as OTLP log records.
#[derive(Clone)]
pub struct EventLog(SdkLogger);

impl EventLog {
    /// Emits one named event at the current time, with the name as both the
    /// record's event name and body. Values must be static strings, which
    /// limits callers to fixed vocabularies rather than runtime text such as
    /// prompts, paths or error messages.
    pub fn emit(&self, name: &'static str, attributes: &[(&'static str, &'static str)]) {
        let mut record = self.0.create_log_record();
        record.set_event_name(name);
        record.set_body(AnyValue::from(name));
        record.set_severity_number(Severity::Info);
        record.set_severity_text("INFO");
        record.set_timestamp(SystemTime::now());
        record.add_attributes(attributes.iter().copied());
        self.0.emit(record);
    }
}

/// Owns the exporters and their diagnostics until host work has stopped.
pub struct OtlpExport {
    diagnostics: Arc<AtomicU64>,
    events: Option<SdkLoggerProvider>,
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
        Self::start_signals(endpoint, service, false).await
    }

    /// Starts trace export like [`Self::start`] and, when the traces URL path
    /// ends in `/v1/traces`, also exports [`EventLog`] records to the sibling
    /// `/v1/logs` path with the same resource identity. Other traces URLs
    /// export traces only. Event-log authentication headers come from
    /// `OTEL_EXPORTER_OTLP_LOGS_HEADERS` or `OTEL_EXPORTER_OTLP_HEADERS`.
    ///
    /// # Errors
    /// Returns a bounded error for an invalid endpoint or exporter setup.
    pub async fn start_with_events(
        endpoint: Option<&str>,
        service: Service,
    ) -> Result<Option<Self>, OtlpError> {
        Self::start_signals(endpoint, service, true).await
    }

    /// Returns the event log when this export includes one.
    pub fn event_log(&self) -> Option<EventLog> {
        self.events
            .as_ref()
            .map(|events| EventLog(events.logger("ag-telemetry")))
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

    /// Flushes finished spans and events concurrently. Export failure never
    /// changes the host result. Returns a coalesced diagnostic count.
    pub async fn shutdown(self) -> u64 {
        let provider = self.provider;
        let traces = tokio::task::spawn_blocking(move || {
            provider.shutdown_with_timeout(Duration::from_secs(3))
        });
        let events = self.events.map(|events| {
            tokio::task::spawn_blocking(move || {
                events.shutdown_with_timeout(Duration::from_secs(3))
            })
        });
        let mut failures = u64::from(!matches!(traces.await, Ok(Ok(()))));
        if let Some(events) = events {
            failures += u64::from(!matches!(events.await, Ok(Ok(()))));
        }

        self.diagnostics.fetch_add(failures, Ordering::Relaxed) + failures
    }

    async fn start_signals(
        endpoint: Option<&str>,
        service: Service,
        events: bool,
    ) -> Result<Option<Self>, OtlpError> {
        let Some(endpoint) = endpoint else {
            return Ok(None);
        };
        let endpoint = Self::validate_endpoint(endpoint)?;
        let events_endpoint = events.then(|| Self::events_endpoint(&endpoint)).flatten();
        let endpoint = endpoint.to_string();

        Self::initialize(move || {
            Self::build(
                &endpoint,
                events_endpoint.as_ref().map(Url::as_str),
                service,
            )
        })
        .await
        .map(Some)
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

    /// Maps a standard signal path to its logs sibling, keeping any prefix
    /// and query; other paths have no unambiguous logs URL.
    fn events_endpoint(traces: &Url) -> Option<Url> {
        let prefix = traces.path().strip_suffix("/v1/traces")?;
        let mut events = traces.clone();
        events.set_path(&format!("{prefix}/v1/logs"));

        Some(events)
    }

    fn build(
        endpoint: &str,
        events_endpoint: Option<&str>,
        service: Service,
    ) -> Result<Self, OtlpError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
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
        let events = events_endpoint
            .map(|events_endpoint| {
                LogExporter::builder()
                    .with_http()
                    .with_protocol(Protocol::HttpBinary)
                    .with_endpoint(events_endpoint)
                    .with_timeout(Duration::from_secs(2))
                    .build()
            })
            .transpose()
            .map_err(|_| OtlpError::Configuration)?
            .map(|exporter| {
                SdkLoggerProvider::builder()
                    .with_resource(resource.clone())
                    .with_batch_exporter(exporter)
                    .build()
            });

        Ok(Self {
            diagnostics: Arc::new(AtomicU64::new(0)),
            events,
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
