use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Histogram, Meter};

const DURATION_BOUNDARIES_SECONDS: [f64; 14] = [
    0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
];

/// OpenTelemetry instruments shared by instrumented model adapters.
///
/// The instruments record `gen_ai.client.operation.duration` in seconds and
/// `gen_ai.client.token.usage` in tokens. Both use only provider and model as
/// attributes, keeping metric cardinality bounded.
pub(crate) struct RequestMetrics {
    duration: Histogram<f64>,
    token_usage: Histogram<u64>,
}

impl RequestMetrics {
    /// Creates the duration and token-usage histograms from the supplied meter.
    pub(crate) fn new(meter: &Meter) -> Self {
        let duration = meter
            .f64_histogram("gen_ai.client.operation.duration")
            .with_description("GenAI operation duration.")
            .with_unit("s")
            .with_boundaries(DURATION_BOUNDARIES_SECONDS.to_vec())
            .build();
        let token_usage = meter
            .u64_histogram("gen_ai.client.token.usage")
            .with_description("Number of tokens used by a GenAI operation.")
            .with_unit("{token}")
            .build();

        Self {
            duration,
            token_usage,
        }
    }

    /// Records one completed operation and its provider-reported token total,
    /// when usage metadata is available.
    pub(crate) fn record(
        &self,
        duration: Duration,
        provider: &'static str,
        model: &str,
        total_tokens: Option<u64>,
    ) {
        let attributes = [
            KeyValue::new("gen_ai.provider.name", provider),
            KeyValue::new("gen_ai.request.model", model.to_string()),
        ];

        self.duration.record(duration.as_secs_f64(), &attributes);
        if let Some(total_tokens) = total_tokens {
            self.token_usage.record(total_tokens, &attributes);
        }
    }
}
