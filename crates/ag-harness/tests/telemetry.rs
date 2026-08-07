//! Integration coverage for `ag-harness` OpenTelemetry signals.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ag_harness::{
        Model, ModelError, ModelRequest, ModelResponse, OutputSchema, Qwen, QwenConfig,
    };
    use opentelemetry::global;
    use opentelemetry::trace::{Status, TraceId, TracerProvider as _};
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLoggerProvider};
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, Histogram, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use serde_json::json;
    use tracing_subscriber::layer::SubscriberExt as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PRIVATE_PROMPT: &str = "private-prompt-sentinel";
    const PRIVATE_RESPONSE: &str = "private-response-sentinel";

    struct TestTelemetry {
        _subscriber_guard: tracing::subscriber::DefaultGuard,
        log_exporter: InMemoryLogExporter,
        logger_provider: SdkLoggerProvider,
        meter_provider: SdkMeterProvider,
        metric_exporter: InMemoryMetricExporter,
        span_exporter: InMemorySpanExporter,
        tracer_provider: SdkTracerProvider,
    }

    impl TestTelemetry {
        /// Installs process-global metrics for this integration binary, which
        /// intentionally contains exactly one test because its metric
        /// assertions depend on exact aggregate counts.
        fn install() -> Self {
            let span_exporter = InMemorySpanExporter::default();
            let tracer_provider = SdkTracerProvider::builder()
                .with_simple_exporter(span_exporter.clone())
                .build();
            let log_exporter = InMemoryLogExporter::default();
            let logger_provider = SdkLoggerProvider::builder()
                .with_simple_exporter(log_exporter.clone())
                .build();
            let metric_exporter = InMemoryMetricExporter::default();
            let meter_provider = SdkMeterProvider::builder()
                .with_periodic_exporter(metric_exporter.clone())
                .build();
            let tracer = tracer_provider.tracer("ag-harness-test");
            let subscriber = tracing_subscriber::registry()
                .with(tracing_opentelemetry::layer().with_tracer(tracer))
                .with(OpenTelemetryTracingBridge::new(&logger_provider));
            let subscriber_guard = tracing::subscriber::set_default(subscriber);
            global::set_meter_provider(meter_provider.clone());

            Self {
                _subscriber_guard: subscriber_guard,
                log_exporter,
                logger_provider,
                meter_provider,
                metric_exporter,
                span_exporter,
                tracer_provider,
            }
        }

        fn flush(&self) {
            self.tracer_provider
                .force_flush()
                .expect("spans should flush");
            self.logger_provider
                .force_flush()
                .expect("logs should flush");
            self.meter_provider
                .force_flush()
                .expect("metrics should flush");
        }
    }

    fn schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn unsupported_schema() -> OutputSchema {
        OutputSchema::new(json!({
            "properties": {
                "name": { "type": "string" }
            }
        }))
        .expect("compile-failure schema should be valid")
    }

    fn model(server: &MockServer) -> Qwen {
        Qwen::new(QwenConfig {
            api_key: "test-key".to_string(),
            base_url: server.uri(),
            model: "qwen-plus".to_string(),
        })
        .expect("test client should initialize")
    }

    async fn mount_invalid_response(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": r#"{"name":42}"#}
                }],
                "id": "response-invalid",
                "model": "qwen-plus-2026-07",
                "usage": {
                    "completion_tokens": 5,
                    "prompt_tokens": 6
                }
            })))
            .mount(server)
            .await;
    }

    fn span_attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a opentelemetry::Value> {
        span.attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == key)
            .map(|attribute| &attribute.value)
    }

    fn call_span_for_session<'a>(spans: &'a [SpanData], session_id: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| {
                span_attribute(span, "gen_ai.conversation.id")
                    .is_some_and(|value| value.to_string() == session_id)
            })
            .expect("call span should be exported")
    }

    fn child_span<'a>(spans: &'a [SpanData], call_span: &SpanData, name: &str) -> &'a SpanData {
        spans
            .iter()
            .find(|span| {
                span.parent_span_id == call_span.span_context.span_id() && span.name == name
            })
            .expect("child span should be exported")
    }

    fn assert_success_trace(spans: &[SpanData]) -> TraceId {
        let call_span = call_span_for_session(spans, "session-success");
        let trace_id = call_span.span_context.trace_id();
        let trace_spans = spans
            .iter()
            .filter(|span| span.span_context.trace_id() == trace_id)
            .collect::<Vec<_>>();
        let mut child_names = trace_spans
            .iter()
            .filter(|span| span.parent_span_id == call_span.span_context.span_id())
            .map(|span| span.name.as_ref())
            .collect::<Vec<_>>();
        child_names.sort_unstable();

        assert_eq!(trace_spans.len(), 4);
        assert_eq!(call_span.name, "chat qwen-plus");
        assert_eq!(
            span_attribute(call_span, "gen_ai.usage.input_tokens").map(ToString::to_string),
            Some("4".to_string())
        );
        assert_eq!(
            span_attribute(call_span, "gen_ai.usage.output_tokens").map(ToString::to_string),
            Some("3".to_string())
        );
        assert_eq!(
            child_names,
            [
                "HTTP POST",
                "gen_ai.response.parse_validate",
                "gen_ai.schema.compile"
            ]
        );

        trace_id
    }

    fn assert_retry_trace(spans: &[SpanData]) -> TraceId {
        let call_span = call_span_for_session(spans, "session-retry");
        let trace_id = call_span.span_context.trace_id();
        let trace_spans = spans
            .iter()
            .filter(|span| span.span_context.trace_id() == trace_id)
            .collect::<Vec<_>>();
        let mut resend_counts = trace_spans
            .iter()
            .filter(|span| span.name == "HTTP POST")
            .filter_map(|span| span_attribute(span, "http.request.resend_count"))
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        resend_counts.sort_unstable();

        assert_eq!(trace_spans.len(), 5);
        assert_eq!(resend_counts, ["0", "1"]);

        trace_id
    }

    fn assert_invalid_trace(spans: &[SpanData]) -> TraceId {
        let call_span = call_span_for_session(spans, "session-invalid");
        let parse_span = child_span(spans, call_span, "gen_ai.response.parse_validate");

        assert_eq!(
            span_attribute(call_span, "gen_ai.usage.input_tokens").map(ToString::to_string),
            Some("6".to_string())
        );
        assert_eq!(
            span_attribute(call_span, "gen_ai.usage.output_tokens").map(ToString::to_string),
            Some("5".to_string())
        );
        assert_eq!(
            span_attribute(call_span, "error.type").map(ToString::to_string),
            Some("schema_violation".to_string())
        );
        assert!(matches!(parse_span.status, Status::Error { .. }));
        assert_eq!(
            span_attribute(parse_span, "error.type").map(ToString::to_string),
            Some("schema_violation".to_string())
        );

        call_span.span_context.trace_id()
    }

    fn assert_compile_failure_trace(spans: &[SpanData]) -> TraceId {
        let call_span = call_span_for_session(spans, "session-compile-failure");
        let compile_span = child_span(spans, call_span, "gen_ai.schema.compile");

        assert!(matches!(compile_span.status, Status::Error { .. }));
        assert_eq!(
            span_attribute(compile_span, "error.type").map(ToString::to_string),
            Some("unsupported_output_schema".to_string())
        );

        call_span.span_context.trace_id()
    }

    fn assert_trace_logs(exporter: &InMemoryLogExporter, trace_ids: &[TraceId]) {
        let logs = exporter
            .get_emitted_logs()
            .expect("logs should be exported");
        for trace_id in trace_ids {
            assert!(
                logs.iter()
                    .filter(|log| {
                        log.record
                            .trace_context()
                            .is_some_and(|context| context.trace_id == *trace_id)
                    })
                    .filter(|log| {
                        log.record
                            .target()
                            .is_some_and(|target| target == "ag_harness::qwen")
                    })
                    .count()
                    >= 2
            );
        }
        let exported_logs = format!("{logs:?}");
        assert!(!exported_logs.contains(PRIVATE_PROMPT));
        assert!(!exported_logs.contains(PRIVATE_RESPONSE));
    }

    fn assert_metric_attributes<T>(histogram: &Histogram<T>) {
        for point in histogram.data_points() {
            let mut keys = point
                .attributes()
                .map(|attribute| attribute.key.as_str())
                .collect::<Vec<_>>();
            keys.sort_unstable();

            assert_eq!(keys, ["gen_ai.provider.name", "gen_ai.request.model"]);
        }
    }

    fn assert_metrics(exporter: &InMemoryMetricExporter) {
        let resource_metrics = exporter
            .get_finished_metrics()
            .expect("metrics should be exported");
        let metrics = resource_metrics
            .iter()
            .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .collect::<Vec<_>>();

        assert_eq!(metrics.len(), 2);
        assert!(metrics.iter().any(|metric| {
            metric.name() == "gen_ai.client.operation.duration"
                && matches!(
                    metric.data(),
                    AggregatedMetrics::F64(MetricData::Histogram(histogram))
                        if {
                            assert_metric_attributes(histogram);
                            histogram.data_points().all(|point| point.count() == 4)
                        }
                )
        }));
        assert!(metrics.iter().any(|metric| {
            metric.name() == "gen_ai.client.token.usage"
                && matches!(
                    metric.data(),
                    AggregatedMetrics::U64(MetricData::Histogram(histogram))
                        if {
                            assert_metric_attributes(histogram);
                            histogram
                                .data_points()
                                .all(|point| point.count() == 2 && point.sum() == 18)
                        }
                )
        }));
    }

    fn assert_expected_failures(
        invalid_result: &Result<ModelResponse, ModelError>,
        compile_failure_result: &Result<ModelResponse, ModelError>,
    ) {
        assert!(matches!(
            invalid_result,
            Err(ModelError::SchemaViolation { .. })
        ));
        assert!(matches!(
            compile_failure_result,
            Err(ModelError::UnsupportedOutputSchema { .. })
        ));
    }

    #[tokio::test]
    async fn exports_content_free_telemetry_and_distinct_retry_spans() {
        // Arrange
        let success_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "finish_reason": "stop",
                    "message": {"content": format!(r#"{{"name":"{PRIVATE_RESPONSE}"}}"#)}
                }],
                "id": "response-42",
                "model": "qwen-plus-2026-07",
                "usage": {
                    "completion_tokens": 3,
                    "prompt_tokens": 4
                }
            })))
            .mount(&success_server)
            .await;
        let retry_server = MockServer::start().await;
        let attempt_count = Arc::new(AtomicUsize::new(0));
        {
            let attempt_count = Arc::clone(&attempt_count);
            Mock::given(method("POST"))
                .and(path("/chat/completions"))
                .respond_with(move |_: &wiremock::Request| {
                    if attempt_count.fetch_add(1, Ordering::SeqCst) == 0 {
                        return ResponseTemplate::new(503);
                    }

                    ResponseTemplate::new(200).set_body_json(json!({
                        "choices": [{
                            "finish_reason": "stop",
                            "message": {"content": r#"{"name":"Ada"}"#}
                        }]
                    }))
                })
                .expect(2)
                .mount(&retry_server)
                .await;
        }
        let invalid_server = MockServer::start().await;
        mount_invalid_response(&invalid_server).await;
        let success_model = model(&success_server);
        let retry_model = model(&retry_server);
        let invalid_model = model(&invalid_server);
        let telemetry = TestTelemetry::install();

        // Act
        let success_response = success_model
            .complete(
                ModelRequest::new(PRIVATE_PROMPT, schema()).with_session_id("session-success"),
            )
            .await
            .expect("instrumented request should succeed");
        let retry_response = retry_model
            .complete(ModelRequest::new("retry request", schema()).with_session_id("session-retry"))
            .await
            .expect("transient failure should be retried");
        let invalid_result = invalid_model
            .complete(
                ModelRequest::new("invalid request", schema()).with_session_id("session-invalid"),
            )
            .await;
        let compile_failure_result = success_model
            .complete(
                ModelRequest::new("compile failure", unsupported_schema())
                    .with_session_id("session-compile-failure"),
            )
            .await;
        telemetry.flush();

        // Assert
        assert_eq!(
            success_response.output(),
            &json!({ "name": PRIVATE_RESPONSE })
        );
        assert_eq!(retry_response.output(), &json!({ "name": "Ada" }));
        assert_expected_failures(&invalid_result, &compile_failure_result);
        assert_eq!(attempt_count.load(Ordering::SeqCst), 2);
        let spans = telemetry
            .span_exporter
            .get_finished_spans()
            .expect("spans should be exported");
        let success_trace_id = assert_success_trace(&spans);
        let retry_trace_id = assert_retry_trace(&spans);
        let invalid_trace_id = assert_invalid_trace(&spans);
        let compile_failure_trace_id = assert_compile_failure_trace(&spans);
        let exported_spans = format!("{spans:?}");
        assert!(!exported_spans.contains(PRIVATE_PROMPT));
        assert!(!exported_spans.contains(PRIVATE_RESPONSE));
        assert_trace_logs(
            &telemetry.log_exporter,
            &[
                success_trace_id,
                retry_trace_id,
                invalid_trace_id,
                compile_failure_trace_id,
            ],
        );
        assert_metrics(&telemetry.metric_exporter);
    }
}
