//! Requests structured output from a configured Qwen model and prints the
//! validated JSON, with optional traces, metrics, and logs over OTLP/HTTP.

use std::error::Error;
use std::future::Future;
use std::io::{self, Write};

use ag_harness::{Model, ModelRequest, OutputSchema, Qwen, QwenConfig};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use serde_json::json;
use thiserror::Error;
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

type DynError = Box<dyn Error + Send + Sync>;

const EXTERNAL_OTEL_ENV: &str = "AG_HARNESS_EXTERNAL_OTEL";
const OTLP_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";

/// Owns the example's OpenTelemetry providers and flushes them exactly once.
struct Telemetry {
    is_shutdown: bool,
    logger_provider: SdkLoggerProvider,
    meter_provider: SdkMeterProvider,
    tracer_provider: SdkTracerProvider,
}

impl Telemetry {
    /// Installs local logging or the operator-configured OTLP providers and
    /// subscriber layers.
    fn init() -> Result<Self, DynError> {
        let external_otel = std::env::var(EXTERNAL_OTEL_ENV).ok();
        let otlp_endpoint = std::env::var(OTLP_ENDPOINT_ENV).ok();
        if !Self::external_otel_enabled(external_otel.as_deref(), otlp_endpoint.as_deref())? {
            tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_filter(Self::filter()))
                .try_init()?;

            return Ok(Self {
                is_shutdown: false,
                logger_provider: SdkLoggerProvider::builder().build(),
                meter_provider: SdkMeterProvider::builder().build(),
                tracer_provider: SdkTracerProvider::builder().build(),
            });
        }

        let resource = Resource::builder()
            .with_service_name("ag-harness-qwen")
            .build();
        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .build()?;
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .build()?;
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource.clone())
            .build();
        let log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .build()?;
        let logger_provider = SdkLoggerProvider::builder()
            .with_batch_exporter(log_exporter)
            .with_resource(resource)
            .build();

        let tracer = tracer_provider.tracer("ag-harness");
        let trace_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_filter(Self::filter());
        let log_layer =
            OpenTelemetryTracingBridge::new(&logger_provider).with_filter(Self::filter());
        tracing_subscriber::registry()
            .with(trace_layer)
            .with(log_layer)
            .with(tracing_subscriber::fmt::layer().with_filter(Self::filter()))
            .try_init()?;
        global::set_meter_provider(meter_provider.clone());

        Ok(Self {
            is_shutdown: false,
            logger_provider,
            meter_provider,
            tracer_provider,
        })
    }

    /// Validates the master telemetry opt-in and requires an explicit
    /// operator-owned endpoint when export is enabled.
    fn external_otel_enabled(
        external_otel: Option<&str>,
        otlp_endpoint: Option<&str>,
    ) -> Result<bool, TelemetryConfigError> {
        let configured_value = external_otel.unwrap_or_default().trim();
        match configured_value.to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "no" | "off" | "disabled" => Ok(false),
            "1" | "true" | "yes" | "on" | "enabled" => {
                if otlp_endpoint.is_none_or(|endpoint| endpoint.trim().is_empty()) {
                    return Err(TelemetryConfigError::MissingEndpoint);
                }

                Ok(true)
            }
            _ => Err(TelemetryConfigError::InvalidExternalOtel {
                value: configured_value.to_string(),
            }),
        }
    }

    /// Restricts exported and formatted events to harness and Qwen targets.
    fn filter() -> Targets {
        Targets::new()
            .with_target("ag_harness", LevelFilter::TRACE)
            .with_target("qwen", LevelFilter::INFO)
    }

    /// Shuts every provider down once without blocking a Tokio worker thread.
    async fn shutdown(&mut self) -> Result<(), DynError> {
        if self.is_shutdown {
            return Ok(());
        }

        self.is_shutdown = true;
        let logger_provider = self.logger_provider.clone();
        let meter_provider = self.meter_provider.clone();
        let tracer_provider = self.tracer_provider.clone();

        tokio::task::spawn_blocking(move || {
            Self::shutdown_providers(&logger_provider, &meter_provider, &tracer_provider)
        })
        .await?
    }

    /// Attempts every provider shutdown even if an earlier provider reports an
    /// error, then returns the first error in meter, log, and trace order.
    fn shutdown_providers(
        logger_provider: &SdkLoggerProvider,
        meter_provider: &SdkMeterProvider,
        tracer_provider: &SdkTracerProvider,
    ) -> Result<(), DynError> {
        let meter_result = meter_provider.shutdown();
        let logger_result = logger_provider.shutdown();
        let tracer_result = tracer_provider.shutdown();

        meter_result?;
        logger_result?;
        tracer_result?;

        Ok(())
    }
}

/// Invalid external-telemetry environment configuration.
#[derive(Debug, Eq, Error, PartialEq)]
enum TelemetryConfigError {
    #[error(
        "AG_HARNESS_EXTERNAL_OTEL requires an operator-owned OTLP endpoint in \
         OTEL_EXPORTER_OTLP_ENDPOINT"
    )]
    MissingEndpoint,
    #[error(
        "invalid AG_HARNESS_EXTERNAL_OTEL value `{value}`; expected a boolean enable or disable \
         value"
    )]
    InvalidExternalOtel { value: String },
}

/// Provides a synchronous last-resort flush when async shutdown is skipped.
impl Drop for Telemetry {
    fn drop(&mut self) {
        if self.is_shutdown {
            return;
        }

        self.is_shutdown = true;
        let _ = Self::shutdown_providers(
            &self.logger_provider,
            &self.meter_provider,
            &self.tracer_provider,
        );
    }
}

/// Initializes telemetry before any model construction and flushes it before
/// the example exits.
#[tokio::main]
async fn main() -> Result<(), DynError> {
    let telemetry = Telemetry::init()?;

    run_with_telemetry(telemetry, run()).await
}

/// Runs one operation under a process-wide Ctrl-C listener.
///
/// The operation future is dropped before telemetry shutdown so cancellation
/// spans, logs, and duration metrics are recorded before exporters close.
async fn run_with_telemetry<F>(mut telemetry: Telemetry, operation: F) -> Result<(), DynError>
where
    F: Future<Output = Result<(), DynError>>,
{
    let mut operation = Box::pin(operation);
    let mut interrupt_task = spawn_ctrl_c_listener();
    let (operation_result, interrupt_result) = tokio::select! {
        result = &mut operation => (Some(result), None),
        signal = &mut interrupt_task => {
            (None, Some(signal))
        }
    };
    drop(operation);

    let shutdown_result = telemetry.shutdown().await;
    let interrupt_result = if let Some(interrupt_result) = interrupt_result {
        Some(interrupt_result)
    } else if interrupt_task.is_finished() {
        Some(interrupt_task.await)
    } else {
        interrupt_task.abort();
        None
    };

    if let Some(interrupt_result) = interrupt_result {
        interrupt_result??;
    }
    if let Some(operation_result) = operation_result {
        operation_result?;
    }
    shutdown_result
}

/// Spawns the example's sole Ctrl-C listener for the post-initialization run.
fn spawn_ctrl_c_listener() -> tokio::task::JoinHandle<io::Result<()>> {
    tokio::spawn(async {
        tokio::signal::ctrl_c().await?;
        tracing::info!("Ctrl-C received; flushing telemetry");

        Ok(())
    })
}

/// Builds and executes the configured demonstration request.
async fn run() -> Result<(), DynError> {
    let model = Qwen::new(QwenConfig {
        api_key: std::env::var("DASHSCOPE_API_KEY")?,
        base_url: std::env::var("DASHSCOPE_BASE_URL")?,
        model: "qwen-plus".to_string(),
    })?;
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": {
            "message": {
                "type": "string",
                "const": "hello"
            }
        },
        "required": ["message"],
        "additionalProperties": false
    }))?;
    let request = greeting_request(schema, std::env::var("AG_HARNESS_SESSION_ID").ok());

    let response = model.complete(request).await?;

    writeln!(io::stdout().lock(), "{}", response.output())?;

    Ok(())
}

/// Creates the demonstration request with optional trace correlation.
fn greeting_request(schema: OutputSchema, session_id: Option<String>) -> ModelRequest {
    let mut request = ModelRequest::new(
        "Return a JSON greeting with the message set to hello.",
        schema,
    );
    if let Some(session_id) = session_id {
        request = request.with_session_id(session_id);
    }

    request
}

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[cfg(unix)]
    use {
        opentelemetry::trace::Status,
        opentelemetry_sdk::error::OTelSdkResult,
        opentelemetry_sdk::logs::{InMemoryLogExporter, LogBatch, LogExporter, SdkLoggerProvider},
        opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData},
        opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider},
        opentelemetry_sdk::trace::{
            InMemorySpanExporter, SdkTracerProvider, SpanData, SpanExporter,
        },
        rustix::process::{self, Pid, Signal},
        std::process::Stdio,
        std::time::Duration,
        tokio::net::TcpListener,
    };

    use super::*;

    const PRIVATE_CANCELLATION_PROMPT: &str = "private cancellation prompt";

    #[test]
    fn builds_request_without_session_id() {
        // Arrange
        let schema =
            OutputSchema::new(json!({"type": "object"})).expect("fixture schema should be valid");

        // Act
        let request = greeting_request(schema, None);

        // Assert
        assert_eq!(request.session_id(), None);
    }

    #[test]
    fn builds_request_with_session_id() {
        // Arrange
        let schema =
            OutputSchema::new(json!({"type": "object"})).expect("fixture schema should be valid");

        // Act
        let request = greeting_request(schema, Some("session-example".to_string()));

        // Assert
        assert_eq!(request.session_id(), Some("session-example"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exports_all_signals_over_real_otlp_http() {
        // Arrange
        let receiver = MockServer::start().await;
        for signal in ["traces", "metrics", "logs"] {
            Mock::given(method("POST"))
                .and(path(format!("/v1/{signal}")))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&receiver)
                .await;
        }
        let resource = Resource::builder()
            .with_service_name("ag-harness-otlp-transport-test")
            .build();
        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{}/v1/traces", receiver.uri()))
            .build()
            .expect("span exporter should initialize");
        let tracer_provider = SdkTracerProvider::builder()
            .with_batch_exporter(span_exporter)
            .with_resource(resource.clone())
            .build();
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{}/v1/metrics", receiver.uri()))
            .build()
            .expect("metric exporter should initialize");
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource.clone())
            .build();
        let log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_protocol(Protocol::HttpBinary)
            .with_endpoint(format!("{}/v1/logs", receiver.uri()))
            .build()
            .expect("log exporter should initialize");
        let logger_provider = SdkLoggerProvider::builder()
            .with_batch_exporter(log_exporter)
            .with_resource(resource)
            .build();
        let tracer = tracer_provider.tracer("ag-harness-otlp-transport-test");
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(OpenTelemetryTracingBridge::new(&logger_provider));
        let subscriber_guard = tracing::subscriber::set_default(subscriber);
        let meter = meter_provider.meter("ag-harness-otlp-transport-test");
        let counter = meter.u64_counter("ag_harness.otlp.transport.test").build();
        let mut telemetry = Telemetry {
            is_shutdown: false,
            logger_provider,
            meter_provider,
            tracer_provider,
        };

        // Act
        counter.add(1, &[]);
        tracing::info_span!(target: "ag_harness::qwen", "otlp.transport.test").in_scope(|| {
            tracing::info!(target: "ag_harness::qwen", "OTLP transport test event");
        });
        drop(subscriber_guard);
        telemetry
            .shutdown()
            .await
            .expect("all OTLP signals should export");

        // Assert
        receiver.verify().await;
    }

    #[cfg(unix)]
    mod fixture_environment {
        pub(super) const ACTIVE_CALL: &str = "AG_HARNESS_ACTIVE_CALL_FIXTURE";
        pub(super) const SIGNAL: &str = "AG_HARNESS_SIGNAL_FIXTURE";
    }

    #[cfg(unix)]
    use fixture_environment::{
        ACTIVE_CALL as ACTIVE_CALL_FIXTURE_ENV, SIGNAL as SIGNAL_FIXTURE_ENV,
    };

    #[cfg(unix)]
    #[derive(Clone, Debug)]
    struct RetainedLogExporter(InMemoryLogExporter);

    #[cfg(unix)]
    impl LogExporter for RetainedLogExporter {
        async fn export(&self, batch: LogBatch<'_>) -> OTelSdkResult {
            self.0.export(batch).await
        }

        fn set_resource(&mut self, resource: &Resource) {
            self.0.set_resource(resource);
        }
    }

    #[cfg(unix)]
    #[derive(Clone, Debug)]
    struct RetainedSpanExporter(InMemorySpanExporter);

    #[cfg(unix)]
    impl SpanExporter for RetainedSpanExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            self.0.export(batch).await
        }

        fn set_resource(&mut self, resource: &Resource) {
            self.0.set_resource(resource);
        }
    }

    #[cfg(unix)]
    struct TestTelemetry {
        _subscriber_guard: tracing::subscriber::DefaultGuard,
        log_exporter: InMemoryLogExporter,
        metric_exporter: InMemoryMetricExporter,
        span_exporter: InMemorySpanExporter,
        telemetry: Option<Telemetry>,
    }

    #[cfg(unix)]
    impl TestTelemetry {
        fn install() -> Self {
            let span_exporter = InMemorySpanExporter::default();
            let tracer_provider = SdkTracerProvider::builder()
                .with_simple_exporter(RetainedSpanExporter(span_exporter.clone()))
                .build();
            let log_exporter = InMemoryLogExporter::default();
            let logger_provider = SdkLoggerProvider::builder()
                .with_simple_exporter(RetainedLogExporter(log_exporter.clone()))
                .build();
            let metric_exporter = InMemoryMetricExporter::default();
            let meter_provider = SdkMeterProvider::builder()
                .with_periodic_exporter(metric_exporter.clone())
                .build();
            let tracer = tracer_provider.tracer("ag-harness-cancellation-test");
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(OpenTelemetryTracingBridge::new(&logger_provider));
            let subscriber_guard = tracing::subscriber::set_default(subscriber);
            global::set_meter_provider(meter_provider.clone());
            let telemetry = Telemetry {
                is_shutdown: false,
                logger_provider,
                meter_provider,
                tracer_provider,
            };

            Self {
                _subscriber_guard: subscriber_guard,
                log_exporter,
                metric_exporter,
                span_exporter,
                telemetry: Some(telemetry),
            }
        }

        fn take_telemetry(&mut self) -> Telemetry {
            self.telemetry
                .take()
                .expect("test telemetry should only be consumed once")
        }

        fn assert_cancelled_call_exported(&self) {
            let spans = self
                .span_exporter
                .get_finished_spans()
                .expect("cancelled span should be exported");
            assert!(!format!("{spans:?}").contains(PRIVATE_CANCELLATION_PROMPT));
            let call_span = spans.iter().find(|span| {
                span.attributes.iter().any(|attribute| {
                    attribute.key.as_str() == "gen_ai.conversation.id"
                        && attribute.value.to_string() == "session-cancelled"
                })
            });
            assert!(
                call_span.is_some(),
                "cancelled call span should be exported: {spans:#?}"
            );
            let call_span = call_span.expect("cancelled call span should exist");
            assert!(matches!(call_span.status, Status::Error { .. }));
            assert!(call_span.attributes.iter().any(|attribute| {
                attribute.key.as_str() == "error.type" && attribute.value.to_string() == "cancelled"
            }));

            let metrics = self
                .metric_exporter
                .get_finished_metrics()
                .expect("cancelled duration metric should be exported");
            let duration_count = metrics
                .iter()
                .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
                .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
                .find_map(|metric| {
                    if metric.name() != "gen_ai.client.operation.duration" {
                        return None;
                    }

                    match metric.data() {
                        AggregatedMetrics::F64(MetricData::Histogram(histogram)) => Some(
                            histogram
                                .data_points()
                                .map(|point| point.count())
                                .sum::<u64>(),
                        ),
                        _ => None,
                    }
                })
                .expect("duration histogram should be exported");
            assert_eq!(duration_count, 1);

            let logs = self
                .log_exporter
                .get_emitted_logs()
                .expect("cancelled failure log should be exported");
            assert!(!format!("{logs:?}").contains(PRIVATE_CANCELLATION_PROMPT));
            let trace_id = call_span.span_context.trace_id();
            let trace_logs = logs
                .iter()
                .filter(|log| {
                    log.record
                        .trace_context()
                        .is_some_and(|context| context.trace_id == trace_id)
                })
                .collect::<Vec<_>>();
            assert!(format!("{trace_logs:?}").contains("cancelled"));
        }
    }

    fn telemetry() -> Telemetry {
        Telemetry {
            is_shutdown: false,
            logger_provider: SdkLoggerProvider::builder().build(),
            meter_provider: SdkMeterProvider::builder().build(),
            tracer_provider: SdkTracerProvider::builder().build(),
        }
    }

    #[test]
    fn external_otel_requires_master_opt_in_and_endpoint() {
        // Arrange
        let configurations = [
            (None, None, Ok(false)),
            (None, Some("http://localhost:4318"), Ok(false)),
            (Some("off"), Some("http://localhost:4318"), Ok(false)),
            (Some("true"), Some("http://localhost:4318"), Ok(true)),
            (
                Some("enabled"),
                None,
                Err(TelemetryConfigError::MissingEndpoint),
            ),
        ];

        // Act
        let results = configurations
            .iter()
            .map(|(external_otel, endpoint, _)| {
                Telemetry::external_otel_enabled(*external_otel, *endpoint)
            })
            .collect::<Vec<_>>();

        // Assert
        for ((_, _, expected), result) in configurations.into_iter().zip(results) {
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn external_otel_rejects_unknown_master_value() {
        // Arrange
        let external_otel = Some("sometimes");
        let endpoint = Some("http://localhost:4318");

        // Act
        let result = Telemetry::external_otel_enabled(external_otel, endpoint);

        // Assert
        assert_eq!(
            result,
            Err(TelemetryConfigError::InvalidExternalOtel {
                value: "sometimes".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn shutdown_closes_every_provider_and_is_idempotent() {
        // Arrange
        let mut telemetry = telemetry();

        // Act
        let first_result = telemetry.shutdown().await;
        let second_result = telemetry.shutdown().await;

        // Assert
        first_result.expect("first shutdown should succeed");
        second_result.expect("repeated shutdown should succeed");
        assert!(telemetry.logger_provider.shutdown().is_err());
        assert!(telemetry.meter_provider.shutdown().is_err());
        assert!(telemetry.tracer_provider.shutdown().is_err());
    }

    #[tokio::test]
    async fn shutdown_attempts_every_provider_after_an_error() {
        // Arrange
        let mut telemetry = telemetry();
        telemetry
            .meter_provider
            .shutdown()
            .expect("meter provider should shut down");

        // Act
        let result = telemetry.shutdown().await;

        // Assert
        assert!(result.is_err());
        assert!(telemetry.logger_provider.shutdown().is_err());
        assert!(telemetry.tracer_provider.shutdown().is_err());
    }

    #[test]
    fn drop_shuts_down_providers_when_async_shutdown_is_skipped() {
        // Arrange
        let telemetry = telemetry();
        let logger_provider = telemetry.logger_provider.clone();
        let meter_provider = telemetry.meter_provider.clone();
        let tracer_provider = telemetry.tracer_provider.clone();

        // Act
        drop(telemetry);

        // Assert
        assert!(logger_provider.shutdown().is_err());
        assert!(meter_provider.shutdown().is_err());
        assert!(tracer_provider.shutdown().is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ctrl_c_listener_handles_early_and_late_signals() {
        // Arrange
        let executable = std::env::current_exe().expect("test executable should be available");

        // Act and assert
        for phase in ["early", "late"] {
            assert_signal_fixture(&executable, phase).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ctrl_c_cancels_active_call_and_flushes_telemetry() {
        // Arrange
        let executable = std::env::current_exe().expect("test executable should be available");
        let child = tokio::process::Command::new(executable)
            .args([
                "--exact",
                "tests::ctrl_c_active_call_process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env(ACTIVE_CALL_FIXTURE_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("active-call fixture should start");

        // Act
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .expect("active-call fixture should exit promptly")
            .expect("active-call fixture should be waitable");

        // Assert
        assert!(
            output.status.success(),
            "active-call fixture failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    async fn assert_signal_fixture(executable: &std::path::Path, phase: &str) {
        // Arrange
        let child = tokio::process::Command::new(executable)
            .args([
                "--exact",
                "tests::ctrl_c_process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env(SIGNAL_FIXTURE_ENV, phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("signal fixture should start");
        let raw_pid = child.id().expect("signal fixture should have a process id");
        let raw_pid = i32::try_from(raw_pid).expect("process id should fit in an i32");
        let pid = Pid::from_raw(raw_pid).expect("signal fixture should have a non-zero process id");
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Act
        process::kill_process(pid, Signal::INT).expect("SIGINT should be delivered");
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .expect("signal fixture should exit promptly")
            .expect("signal fixture should be waitable");

        // Assert
        assert!(
            output.status.success(),
            "{phase} signal fixture failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "spawned by ctrl_c_listener_handles_early_and_late_signals"]
    async fn ctrl_c_process_fixture() {
        // Arrange
        let phase = std::env::var(SIGNAL_FIXTURE_ENV).expect("fixture phase should be configured");
        let interrupt_task = spawn_ctrl_c_listener();
        if phase == "late" {
            tokio::time::sleep(Duration::from_millis(50)).await;
        } else {
            assert_eq!(phase, "early");
        }

        // Act
        let signal_result = tokio::time::timeout(Duration::from_secs(2), interrupt_task)
            .await
            .expect("fixture should receive SIGINT")
            .expect("signal listener task should complete");

        // Assert
        signal_result.expect("signal listener should handle SIGINT");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "spawned by ctrl_c_cancels_active_call_and_flushes_telemetry"]
    async fn ctrl_c_active_call_process_fixture() {
        // Arrange
        assert_eq!(std::env::var(ACTIVE_CALL_FIXTURE_ENV).as_deref(), Ok("1"));
        let mut test_telemetry = TestTelemetry::install();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("pending-call server should bind");
        let address = listener
            .local_addr()
            .expect("pending-call server address should be available");
        let server_task = tokio::spawn(async move {
            let (_stream, _) = listener
                .accept()
                .await
                .expect("pending-call server should accept a request");
            tokio::time::sleep(Duration::from_millis(50)).await;
            process::kill_process(process::getpid(), Signal::INT)
                .expect("fixture should deliver SIGINT to itself");
            std::future::pending::<()>().await;
        });
        let model = Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: format!("http://{address}"),
            model: "qwen-plus".to_string(),
        })
        .expect("fixture client should initialize");
        let schema = OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" }
            },
            "required": ["message"],
            "additionalProperties": false
        }))
        .expect("fixture schema should be valid");
        let request = ModelRequest::new(PRIVATE_CANCELLATION_PROMPT, schema)
            .with_session_id("session-cancelled");
        let operation = async move {
            model.complete(request).await?;

            Ok(())
        };

        // Act
        let result = run_with_telemetry(test_telemetry.take_telemetry(), operation).await;
        server_task.abort();

        // Assert
        result.expect("SIGINT should cancel the operation and flush telemetry");
        test_telemetry.assert_cancelled_call_exported();
    }
}
