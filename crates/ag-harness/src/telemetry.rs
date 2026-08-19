use std::time::Instant;

use opentelemetry::metrics::Histogram;
use opentelemetry::{KeyValue, global};

use crate::model::{CompletionMetadata, ModelError, ModelMetadata};

const DURATION_BOUNDARIES_SECONDS: [f64; 14] = [
    0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
];
const TOKEN_BOUNDARIES: [f64; 14] = [
    1.0,
    4.0,
    16.0,
    64.0,
    256.0,
    1_024.0,
    4_096.0,
    16_384.0,
    65_536.0,
    262_144.0,
    1_048_576.0,
    4_194_304.0,
    16_777_216.0,
    67_108_864.0,
];

/// Records one model request's operational metrics.
pub(crate) struct RequestMetrics<'a> {
    duration: Histogram<f64>,
    is_active: bool,
    model: &'a str,
    provider: &'static str,
    started_at: Instant,
}

impl<'a> RequestMetrics<'a> {
    /// Starts recording one model request.
    pub(crate) fn start(metadata: &'a ModelMetadata) -> Self {
        Self {
            duration: Self::duration_histogram(),
            is_active: true,
            model: metadata.model(),
            provider: metadata.provider(),
            started_at: Instant::now(),
        }
    }

    /// Records a successful request and provider-reported token usage.
    pub(crate) fn completed(mut self, metadata: &CompletionMetadata) {
        self.is_active = false;
        self.record_duration(None);
        self.record_token_usage(metadata);
    }

    /// Records a failed request with a bounded error classification.
    pub(crate) fn failed(mut self, error: &ModelError, metadata: Option<&CompletionMetadata>) {
        self.is_active = false;
        let http_status = error.http_status().map(|status| status.to_string());
        let error_type = http_status
            .as_deref()
            .unwrap_or_else(|| error.error_type().as_str());
        self.record_duration(Some(error_type));

        if let Some(metadata) = metadata {
            self.record_token_usage(metadata);
        }
    }

    fn duration_histogram() -> Histogram<f64> {
        global::meter("ag-harness")
            .f64_histogram("gen_ai.client.operation.duration")
            .with_description("GenAI operation duration.")
            .with_unit("s")
            .with_boundaries(DURATION_BOUNDARIES_SECONDS.to_vec())
            .build()
    }

    fn token_histogram() -> Histogram<u64> {
        global::meter("ag-harness")
            .u64_histogram("gen_ai.client.token.usage")
            .with_description("Number of input and output tokens used.")
            .with_unit("{token}")
            .with_boundaries(TOKEN_BOUNDARIES.to_vec())
            .build()
    }

    fn record_duration(&self, error_type: Option<&str>) {
        let attributes = self.attributes(error_type);

        self.duration
            .record(self.started_at.elapsed().as_secs_f64(), &attributes);
    }

    fn record_token_usage(&self, metadata: &CompletionMetadata) {
        let Some(usage) = metadata.usage() else {
            return;
        };
        let metric = Self::token_histogram();

        if let Some(input) = usage.input_tokens() {
            let mut attributes = self.attributes(None);
            attributes.push(KeyValue::new("gen_ai.token.type", "input"));
            metric.record(input, &attributes);
        }
        if let Some(output) = usage.output_tokens() {
            let mut attributes = self.attributes(None);
            attributes.push(KeyValue::new("gen_ai.token.type", "output"));
            metric.record(output, &attributes);
        }
    }

    fn attributes(&self, error_type: Option<&str>) -> Vec<KeyValue> {
        let mut attributes = vec![
            KeyValue::new("gen_ai.operation.name", "chat"),
            KeyValue::new("gen_ai.provider.name", self.provider),
            KeyValue::new("gen_ai.request.model", self.model.to_string()),
        ];

        if let Some(error_type) = error_type {
            attributes.push(KeyValue::new("error.type", error_type.to_string()));
        }

        attributes
    }
}

impl Drop for RequestMetrics<'_> {
    fn drop(&mut self) {
        if self.is_active {
            self.record_duration(Some("cancelled"));
        }
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};

    use super::*;

    #[test]
    fn dropping_active_request_records_cancellation() {
        // Arrange
        let exporter = InMemoryMetricExporter::default();
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(exporter.clone())
            .build();
        global::set_meter_provider(meter_provider.clone());
        let metadata = ModelMetadata::new("test_provider", "cancelled-unit-test")
            .expect("fixture metadata should be valid");
        let metrics = RequestMetrics::start(&metadata);

        // Act
        drop(metrics);
        meter_provider
            .force_flush()
            .expect("cancellation metric should flush");

        // Assert
        let resource_metrics = exporter
            .get_finished_metrics()
            .expect("cancellation metric should be exported");
        let duration = resource_metrics
            .iter()
            .flat_map(opentelemetry_sdk::metrics::data::ResourceMetrics::scope_metrics)
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .find(|metric| metric.name() == "gen_ai.client.operation.duration")
            .expect("duration metric should be exported");
        assert!(matches!(
            duration.data(),
            AggregatedMetrics::F64(MetricData::Histogram(histogram)) if {
                let point = histogram
                    .data_points()
                    .find(|point| {
                        point.attributes().any(|attribute| {
                            attribute.key.as_str() == "gen_ai.request.model"
                                && attribute.value.to_string() == "cancelled-unit-test"
                        })
                    })
                    .expect("cancelled request point should be exported");
                let mut attributes = point
                    .attributes()
                    .map(|attribute| (attribute.key.as_str(), attribute.value.to_string()))
                    .collect::<Vec<_>>();
                attributes.sort_unstable();
                assert_eq!(point.count(), 1);
                assert_eq!(
                    attributes,
                    [
                        ("error.type", "cancelled".to_string()),
                        ("gen_ai.operation.name", "chat".to_string()),
                        ("gen_ai.provider.name", "test_provider".to_string()),
                        (
                            "gen_ai.request.model",
                            "cancelled-unit-test".to_string()
                        ),
                    ]
                );

                true
            }
        ));
    }
}
