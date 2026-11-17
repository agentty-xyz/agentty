//! Opt-in OTLP HTTP/protobuf trace, metric, and event-log export for host
//! composition roots.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::{KeyValue, global};
use opentelemetry_otlp::{LogExporter, MetricExporter, Protocol, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{
    LogBatch, LogExporter as SdkLogExporter, SdkLogger, SdkLoggerProvider,
};
use opentelemetry_sdk::metrics::data::ResourceMetrics;
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;
use opentelemetry_sdk::metrics::{SdkMeterProvider, Temporality};
use opentelemetry_sdk::trace::{
    BatchConfigBuilder, BatchSpanProcessor, Sampler, SdkTracerProvider, SpanData,
    SpanExporter as SdkSpanExporter,
};
use thiserror::Error;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, Registry};
use url::Url;

/// Resource identity attached to every exported span and event.
#[derive(Clone, Debug)]
pub struct Service {
    /// Exported as `service.name`.
    pub name: &'static str,
    /// Exported as `user.id` when present, such as a pseudonymous
    /// installation ID shared with the host's product analytics.
    pub user_id: Option<String>,
    /// Exported as `service.version`.
    pub version: &'static str,
}

/// Bounded export setup failures that never echo the endpoint or credentials.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
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
    metric_rejections: Arc<AtomicU64>,
    metrics: Option<SdkMeterProvider>,
    provider: SdkTracerProvider,
}

impl OtlpExport {
    /// Starts export only when the host supplied an endpoint. Environment
    /// variables alone never enable tracing or override the endpoint;
    /// `OTEL_EXPORTER_OTLP_TRACES_HEADERS` and `OTEL_EXPORTER_OTLP_HEADERS`
    /// still supply authentication headers.
    ///
    /// When the traces URL path ends in `/v1/traces`, metrics recorded through
    /// the installed global meter provider are also exported to the sibling
    /// `/v1/metrics` path with the same resource identity. Their
    /// authentication headers come from `OTEL_EXPORTER_OTLP_METRICS_HEADERS`
    /// or `OTEL_EXPORTER_OTLP_HEADERS`.
    ///
    /// # Errors
    /// Returns a bounded error for an invalid endpoint or exporter setup.
    pub async fn start(
        endpoint: Option<&str>,
        service: Service,
    ) -> Result<Option<Self>, OtlpError> {
        Self::start_signals(endpoint, service, false).await
    }

    /// Starts trace and metric export like [`Self::start`] and, when the
    /// traces URL path ends in `/v1/traces`, also exports [`EventLog`] records
    /// to the sibling `/v1/logs` path with the same resource identity. Other
    /// traces URLs export traces only. Event-log authentication headers come
    /// from `OTEL_EXPORTER_OTLP_LOGS_HEADERS` or `OTEL_EXPORTER_OTLP_HEADERS`.
    ///
    /// # Errors
    /// Returns a bounded error for an invalid endpoint or exporter setup.
    pub async fn start_with_events(
        endpoint: Option<&str>,
        service: Service,
    ) -> Result<Option<Self>, OtlpError> {
        Self::start_signals(endpoint, service, true).await
    }

    /// Validates `endpoint` like [`Self::start`] without constructing an
    /// exporter, so hosts can reject it before startup work that must precede
    /// export, such as loading the [`Service`] identity.
    ///
    /// # Errors
    /// Returns [`OtlpError::Endpoint`] for an incomplete or credentialed URL.
    pub fn validate(endpoint: &str) -> Result<(), OtlpError> {
        Self::validate_endpoint(endpoint).map(|_| ())
    }

    /// Returns the event log when this export includes one.
    pub fn event_log(&self) -> Option<EventLog> {
        self.events
            .as_ref()
            .map(|events| EventLog(events.logger("ag-telemetry")))
    }

    /// Installs the process-wide tracer and meter providers and, when no
    /// subscriber exists yet, a diagnostic subscriber that counts exporter
    /// warnings without retaining their text. Repeated installs replace the
    /// providers and never fail. Rejected export batches and shutdown failures
    /// are always counted by this export itself. The subscriber adds only
    /// other SDK warnings and errors, such as dropped spans, and only for the
    /// first export installed in the process.
    pub fn install(&self) {
        let _ = Registry::default()
            .with(Diagnostics(Arc::clone(&self.diagnostics)))
            .try_init();
        global::set_tracer_provider(self.provider.clone());
        if let Some(metrics) = &self.metrics {
            global::set_meter_provider(metrics.clone());
        }
    }

    /// Flushes finished spans, events, and metrics concurrently. Export
    /// failure never changes the host result. Returns the number of rejected
    /// export batches, plus one per signal provider whose shutdown failed,
    /// plus the other SDK warnings and errors described on [`Self::install`].
    /// Each failure is counted once; zero means every export succeeded.
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
        let rejected_before_flush = self.metric_rejections.load(Ordering::Relaxed);
        let metrics = self.metrics.map(|metrics| {
            tokio::task::spawn_blocking(move || {
                metrics.shutdown_with_timeout(Duration::from_secs(3))
            })
        });
        let mut failures = u64::from(!matches!(traces.await, Ok(Ok(()))));
        if let Some(events) = events {
            failures += u64::from(!matches!(events.await, Ok(Ok(()))));
        }
        if let Some(metrics) = metrics {
            let shutdown_failed = !matches!(metrics.await, Ok(Ok(())));
            let rejections = self.metric_rejections.load(Ordering::Relaxed);
            // A rejected final flush also fails the meter provider's shutdown;
            // count that failure once, as the rejected export.
            failures +=
                rejections + u64::from(shutdown_failed && rejections == rejected_before_flush);
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
        let events_endpoint = events
            .then(|| Self::signal_endpoint(&endpoint, "logs"))
            .flatten();
        let metrics_endpoint = Self::signal_endpoint(&endpoint, "metrics");
        let endpoint = endpoint.to_string();

        Self::initialize(move || {
            Self::build(
                &endpoint,
                events_endpoint.as_ref().map(Url::as_str),
                metrics_endpoint.as_ref().map(Url::as_str),
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

    /// Maps a standard traces path to its `signal` sibling, such as `logs` or
    /// `metrics`, keeping any prefix and query; other paths have no
    /// unambiguous sibling URL.
    fn signal_endpoint(traces: &Url, signal: &str) -> Option<Url> {
        let prefix = traces.path().strip_suffix("/v1/traces")?;
        let mut sibling = traces.clone();
        sibling.set_path(&format!("{prefix}/v1/{signal}"));

        Some(sibling)
    }

    fn build(
        endpoint: &str,
        events_endpoint: Option<&str>,
        metrics_endpoint: Option<&str>,
        service: Service,
    ) -> Result<Self, OtlpError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let diagnostics = Arc::new(AtomicU64::new(0));
        let metric_rejections = Arc::new(AtomicU64::new(0));
        let exporter = SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(endpoint)
            .with_timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| OtlpError::Configuration)?;
        let exporter = CountedExporter {
            exporter,
            failures: Arc::clone(&diagnostics),
        };
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
            .with_attributes(
                service
                    .user_id
                    .map(|user_id| KeyValue::new("user.id", user_id)),
            )
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
                    .with_batch_exporter(CountedExporter {
                        exporter,
                        failures: Arc::clone(&diagnostics),
                    })
                    .build()
            });
        let metrics = metrics_endpoint
            .map(|metrics_endpoint| {
                MetricExporter::builder()
                    .with_http()
                    .with_protocol(Protocol::HttpBinary)
                    .with_endpoint(metrics_endpoint)
                    .with_timeout(Duration::from_secs(2))
                    .build()
            })
            .transpose()
            .map_err(|_| OtlpError::Configuration)?
            .map(|exporter| {
                SdkMeterProvider::builder()
                    .with_resource(resource.clone())
                    .with_periodic_exporter(CountedExporter {
                        exporter,
                        failures: Arc::clone(&metric_rejections),
                    })
                    .build()
            });

        Ok(Self {
            diagnostics,
            events,
            metric_rejections,
            metrics,
            provider: SdkTracerProvider::builder()
                .with_resource(resource)
                .with_sampler(Sampler::AlwaysOn)
                .with_span_processor(processor)
                .build(),
        })
    }
}

struct Diagnostics(Arc<AtomicU64>);

impl Diagnostics {
    /// Matches SDK events that report a failed export result, such as
    /// `Export.Failed.NonRetryable` and `BatchSpanProcessor.ExportError`.
    /// [`CountedExporter`] already counts those failures once per batch.
    fn reports_export_result(name: &str) -> bool {
        name.starts_with("Export.Failed.") || name.ends_with(".ExportError")
    }
}

impl<S: Subscriber> Layer<S> for Diagnostics {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let metadata = event.metadata();
        if metadata.target().starts_with("opentelemetry")
            && matches!(*metadata.level(), Level::WARN | Level::ERROR)
            && !Self::reports_export_result(metadata.name())
        {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Counts rejected exports from the exporter's own result. The batch
/// processors discard export results during flush and shutdown, and the
/// diagnostic subscriber only sees them when it is the process-wide subscriber
/// bound to this export, so this keeps failure reporting deterministic. Every
/// other exporter method is forwarded unchanged.
#[derive(Debug)]
struct CountedExporter<Exporter> {
    exporter: Exporter,
    failures: Arc<AtomicU64>,
}

impl<Exporter> CountedExporter<Exporter> {
    fn count(&self, result: OTelSdkResult) -> OTelSdkResult {
        if result.is_err() {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }

        result
    }
}

impl<Exporter: SdkSpanExporter> SdkSpanExporter for CountedExporter<Exporter> {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        self.count(self.exporter.export(batch).await)
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.exporter.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.exporter.shutdown()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.exporter.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.exporter.set_resource(resource);
    }
}

impl<Exporter: SdkLogExporter> SdkLogExporter for CountedExporter<Exporter> {
    async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
        self.count(self.exporter.export(batch).await)
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.exporter.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.exporter.shutdown()
    }

    fn event_enabled(&self, level: Severity, target: &str, name: Option<&str>) -> bool {
        self.exporter.event_enabled(level, target, name)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.exporter.set_resource(resource);
    }
}

impl<Exporter: PushMetricExporter> PushMetricExporter for CountedExporter<Exporter> {
    async fn export(&self, metrics: &ResourceMetrics) -> OTelSdkResult {
        self.count(self.exporter.export(metrics).await)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.exporter.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.exporter.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.exporter.shutdown()
    }

    fn temporality(&self) -> Temporality {
        self.exporter.temporality()
    }
}

#[cfg(test)]
#[path = "otlp_test.rs"]
mod tests;
