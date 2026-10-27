//! Opt-in OTLP traces of context runs: one trace per run, whose root span
//! carries the run record and whose harness model-request spans each carry
//! that request's record.

use std::time::SystemTime;

use ag_harness::lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, LifecycleOperationGuard,
    LifecycleTraceObserver,
};
use ag_harness::model::CompletionMetadata;
use ag_telemetry::{Context, KeyValue, Outcome, Span, TraceContextExt as _};
use opentelemetry::{Array, StringValue, Value as AttributeValue};
use serde_json::Value;

/// Prefix of the attributes projected from run and request records.
const ATTRIBUTE_PREFIX: &str = "benchmark";

/// One run's trace. The harness's lifecycle spans nest beneath its root
/// while the run executes in [`RunTrace::context`].
pub(super) struct RunTrace {
    root: Span,
}

impl RunTrace {
    /// Starts a new trace whose root span is named after the scenario.
    pub(super) fn start(scenario: &'static str) -> Self {
        Self {
            root: Span::root(scenario, Vec::new()),
        }
    }

    /// Projects the harness lifecycle to spans, but leaves each completed
    /// model request's span open so [`RunTrace::finish`] can add its record
    /// before ending it at its observed completion time.
    pub(super) fn observer() -> RequestTraceObserver {
        RequestTraceObserver(LifecycleTraceObserver::new())
    }

    /// Returns the context the run executes in.
    pub(super) fn context(&self) -> Context {
        self.root.context()
    }

    /// Returns the hexadecimal trace identifier, as Tempo looks it up.
    pub(super) fn trace_id(&self) -> String {
        self.context().span().span_context().trace_id().to_string()
    }

    /// Ends every captured request span, pairing it in order with the run
    /// record's `requests`, then ends the root with the whole record as
    /// `benchmark.*` attributes and the run's success as its outcome.
    pub(super) fn finish(self, record: &Value, spans: Vec<RequestSpan>) {
        let context = self.context();
        let root = context.span();
        let root_span_id = root.span_context().span_id();
        let requests = record["requests"].as_array().map_or(&[][..], Vec::as_slice);
        for (index, span) in spans.into_iter().enumerate() {
            // A request without its own span ran directly in the root.
            if span.context.span().span_context().span_id() != root_span_id {
                span.end(index, requests.get(index));
            }
        }
        let mut attributes = Vec::new();
        flatten(ATTRIBUTE_PREFIX.to_string(), record, &mut attributes);
        root.set_attributes(attributes);
        let outcome = if record["success"] == true {
            Outcome::Completed
        } else {
            Outcome::Failed
        };

        self.root.finish(outcome);
    }
}

/// A [`LifecycleTraceObserver`] that never ends a completed model request's
/// span itself; failed and cancelled requests end as the harness ends them.
pub(super) struct RequestTraceObserver(LifecycleTraceObserver);

impl LifecycleObserver for RequestTraceObserver {
    fn observe(&self, event: LifecycleEvent) {
        if !matches!(
            event.kind(),
            LifecycleEventKind::ModelRequestCompleted { .. }
        ) {
            self.0.observe(event);
        }
    }

    fn enter_operation(
        &self,
        operation_id: LifecycleId,
    ) -> Option<Box<dyn LifecycleOperationGuard>> {
        self.0.enter_operation(operation_id)
    }
}

/// The span one reported model request ran in, with the completion it
/// returned and when it returned it.
pub(super) struct RequestSpan {
    completion: Option<CompletionMetadata>,
    context: Context,
    ended_at: SystemTime,
}

impl RequestSpan {
    /// Captures a request that just returned in `context`.
    pub(super) fn returned(context: Context, completion: Option<CompletionMetadata>) -> Self {
        Self {
            completion,
            context,
            ended_at: SystemTime::now(),
        }
    }

    /// Adds the completion attributes the harness would have added, the
    /// request's position in the run, and its record, then ends the span
    /// when the request returned. A record whose reported input tokens differ
    /// from this request's is left out rather than attributed to the wrong
    /// request. A span the harness already ended (a rejected output) is left
    /// unchanged.
    fn end(self, index: usize, record: Option<&Value>) {
        let span = self.context.span();
        let mut attributes = vec![KeyValue::new(
            "benchmark.request",
            i64::try_from(index).unwrap_or(i64::MAX),
        )];
        if let Some(completion) = &self.completion {
            completion_attributes(completion, &mut attributes);
        }
        let input_tokens = self
            .completion
            .as_ref()
            .and_then(CompletionMetadata::usage)
            .and_then(|usage| usage.input_tokens());
        if let Some(record) =
            record.filter(|record| record["input_tokens"].as_u64() == input_tokens)
        {
            flatten(ATTRIBUTE_PREFIX.to_string(), record, &mut attributes);
        }
        span.set_attributes(attributes);
        span.end_with_timestamp(self.ended_at);
    }
}

/// Mirrors the `GenAI` response attributes `LifecycleTraceObserver` sets
/// when it ends a completed request.
fn completion_attributes(completion: &CompletionMetadata, attributes: &mut Vec<KeyValue>) {
    attributes.push(KeyValue::new(
        "gen_ai.response.finish_reasons",
        AttributeValue::Array(Array::String(vec![StringValue::from(
            completion.finish_reason().to_string(),
        )])),
    ));
    if let Some(response_id) = completion.response_id() {
        attributes.push(KeyValue::new("gen_ai.response.id", response_id.to_string()));
    }
    if let Some(response_model) = completion.response_model() {
        attributes.push(KeyValue::new(
            "gen_ai.response.model",
            response_model.to_string(),
        ));
    }
    let Some(usage) = completion.usage() else {
        return;
    };
    for (key, tokens) in [
        (
            "gen_ai.usage.cache_read.input_tokens",
            usage.cache_hit_tokens(),
        ),
        ("gen_ai.usage.input_tokens", usage.input_tokens()),
        ("gen_ai.usage.output_tokens", usage.output_tokens()),
        (
            "gen_ai.usage.reasoning.output_tokens",
            usage.reasoning_tokens(),
        ),
    ] {
        if let Some(tokens) = tokens.and_then(|tokens| i64::try_from(tokens).ok()) {
            attributes.push(KeyValue::new(key, tokens));
        }
    }
}

/// Projects a JSON record to attributes named by their dotted path under
/// `key`. Nulls, empty arrays, and arrays of objects (the per-request and
/// per-turn lists) are left out.
pub(super) fn flatten(key: String, value: &Value, attributes: &mut Vec<KeyValue>) {
    match value {
        Value::Null => {}
        Value::Bool(flag) => attributes.push(KeyValue::new(key, *flag)),
        Value::Number(number) => attributes.push(match number.as_i64() {
            Some(integer) => KeyValue::new(key, integer),
            None => KeyValue::new(key, number.as_f64().unwrap_or_default()),
        }),
        Value::String(text) => attributes.push(KeyValue::new(key, text.clone())),
        Value::Array(items) => {
            if let Some(array) = scalar_array(items) {
                attributes.push(KeyValue::new(key, AttributeValue::Array(array)));
            }
        }
        Value::Object(fields) => {
            for (name, field) in fields {
                flatten(format!("{key}.{name}"), field, attributes);
            }
        }
    }
}

/// Converts a nonempty array of only strings or only integers.
fn scalar_array(items: &[Value]) -> Option<Array> {
    if items.is_empty() {
        return None;
    }
    if let Some(texts) = items
        .iter()
        .map(|item| {
            item.as_str()
                .map(|text| StringValue::from(text.to_string()))
        })
        .collect::<Option<Vec<_>>>()
    {
        return Some(Array::String(texts));
    }

    items
        .iter()
        .map(Value::as_i64)
        .collect::<Option<Vec<_>>>()
        .map(Array::I64)
}
