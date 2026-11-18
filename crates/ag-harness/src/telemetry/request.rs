//! OpenTelemetry `GenAI` duration and token metrics for one model request.

use std::time::Instant;

use opentelemetry::metrics::Histogram;
use opentelemetry::{KeyValue, global};

use super::label::ERROR_CANCELLED;
use super::metric::{
    ATTRIBUTE_ERROR_TYPE, ATTRIBUTE_OPERATION_NAME, ATTRIBUTE_PROVIDER_NAME,
    ATTRIBUTE_REQUEST_MODEL, DURATION_BOUNDARIES_SECONDS, DURATION_UNIT, INSTRUMENTATION_SCOPE,
    OPERATION_CHAT,
};
use crate::model::{CompletionMetadata, ModelError, ModelMetadata};

pub(crate) const DURATION_METRIC: &str = "gen_ai.client.operation.duration";
const ATTRIBUTE_TOKEN_TYPE: &str = "gen_ai.token.type";
const DURATION_DESCRIPTION: &str = "GenAI operation duration.";
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
const TOKEN_DESCRIPTION: &str = "Number of input and output tokens used.";
const TOKEN_METRIC: &str = "gen_ai.client.token.usage";
const TOKEN_TYPE_INPUT: &str = "input";
const TOKEN_TYPE_OUTPUT: &str = "output";
const TOKEN_UNIT: &str = "{token}";

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

    /// Records a successful request and any provider-reported token usage.
    pub(crate) fn completed(mut self, metadata: Option<&CompletionMetadata>) {
        self.is_active = false;
        self.record_duration(None);

        if let Some(metadata) = metadata {
            self.record_token_usage(metadata);
        }
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
        global::meter(INSTRUMENTATION_SCOPE)
            .f64_histogram(DURATION_METRIC)
            .with_description(DURATION_DESCRIPTION)
            .with_unit(DURATION_UNIT)
            .with_boundaries(DURATION_BOUNDARIES_SECONDS.to_vec())
            .build()
    }

    fn token_histogram() -> Histogram<u64> {
        global::meter(INSTRUMENTATION_SCOPE)
            .u64_histogram(TOKEN_METRIC)
            .with_description(TOKEN_DESCRIPTION)
            .with_unit(TOKEN_UNIT)
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
            attributes.push(KeyValue::new(ATTRIBUTE_TOKEN_TYPE, TOKEN_TYPE_INPUT));
            metric.record(input, &attributes);
        }
        if let Some(output) = usage.output_tokens() {
            let mut attributes = self.attributes(None);
            attributes.push(KeyValue::new(ATTRIBUTE_TOKEN_TYPE, TOKEN_TYPE_OUTPUT));
            metric.record(output, &attributes);
        }
    }

    fn attributes(&self, error_type: Option<&str>) -> Vec<KeyValue> {
        let mut attributes = vec![
            KeyValue::new(ATTRIBUTE_OPERATION_NAME, OPERATION_CHAT),
            KeyValue::new(ATTRIBUTE_PROVIDER_NAME, self.provider),
            KeyValue::new(ATTRIBUTE_REQUEST_MODEL, self.model.to_string()),
        ];

        if let Some(error_type) = error_type {
            attributes.push(KeyValue::new(ATTRIBUTE_ERROR_TYPE, error_type.to_string()));
        }

        attributes
    }
}

impl Drop for RequestMetrics<'_> {
    fn drop(&mut self) {
        if self.is_active {
            self.record_duration(Some(ERROR_CANCELLED));
        }
    }
}
