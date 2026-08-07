use std::time::Instant;

use opentelemetry::metrics::Histogram;
use opentelemetry::{KeyValue, global};

use crate::model::ModelMetadata;

const DURATION_BOUNDARIES_SECONDS: [f64; 14] = [
    0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
];

/// Records one model request's duration when it leaves scope.
pub(crate) struct RequestDuration<'a> {
    metric: Histogram<f64>,
    model: &'a str,
    provider: &'static str,
    started_at: Instant,
}

impl<'a> RequestDuration<'a> {
    /// Starts timing one model request.
    pub(crate) fn start(metadata: ModelMetadata<'a>) -> Self {
        let metric = global::meter("ag-harness")
            .f64_histogram("gen_ai.client.operation.duration")
            .with_description("GenAI operation duration.")
            .with_unit("s")
            .with_boundaries(DURATION_BOUNDARIES_SECONDS.to_vec())
            .build();

        Self {
            metric,
            model: metadata.model(),
            provider: metadata.provider(),
            started_at: Instant::now(),
        }
    }
}

impl Drop for RequestDuration<'_> {
    fn drop(&mut self) {
        let attributes = [
            KeyValue::new("gen_ai.provider.name", self.provider),
            KeyValue::new("gen_ai.request.model", self.model.to_string()),
        ];

        self.metric
            .record(self.started_at.elapsed().as_secs_f64(), &attributes);
    }
}
