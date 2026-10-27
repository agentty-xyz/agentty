//! Context-run trace projection over a scripted provider.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_harness::model::{ContextBudget, ModelClient};
use ag_harness::provider::QwenConfig;
use ag_harness::{Harness, OutputSchema, Tool};
use ag_telemetry::{Context, FutureExt as _, KeyValue};
use opentelemetry::trace::{SpanId, Status};
use opentelemetry::{Array, StringValue, Value as AttributeValue, global};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use crate::probe::ProbedModel;
use crate::repository_fixture::repository_with_host_git;
use crate::trace::{RequestSpan, RunTrace, flatten};

/// Serializes tests that replace the process-wide tracer provider.
static PROVIDER_LOCK: Mutex<()> = Mutex::const_new(());

const MODEL: &str = "trace-model";

/// Answers each provider request with the next scripted response.
struct Script {
    next: AtomicUsize,
    responses: Vec<Value>,
}

impl Respond for Script {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let index = self.next.fetch_add(1, Ordering::SeqCst);

        ResponseTemplate::new(200).set_body_json(&self.responses[index])
    }
}

fn response(id: &str, message: &Value, input: u64, cached: u64) -> Value {
    json!({
        "choices": [{
            "finish_reason": if message["tool_calls"].is_null() { "stop" } else { "tool_calls" },
            "message": message
        }],
        "id": id,
        "model": MODEL,
        "usage": {
            "completion_tokens": 5,
            "prompt_tokens": input,
            "prompt_tokens_details": { "cached_tokens": cached },
            "total_tokens": input + 5
        }
    })
}

/// A rejected output, a `read` call, then the answer.
fn scripted_responses() -> Vec<Value> {
    vec![
        response(
            "rejected",
            &json!({ "content": r#"{"unexpected":true}"# }),
            80,
            0,
        ),
        response(
            "read",
            &json!({
                "content": null,
                "tool_calls": [{
                    "id": "read-call",
                    "type": "function",
                    "function": { "name": "read", "arguments": r#"{"path":"notes.txt"}"# }
                }]
            }),
            100,
            0,
        ),
        response(
            "answer",
            &json!({ "content": r#"{"answer":"done"}"# }),
            150,
            10,
        ),
    ]
}

fn install_provider() -> (SdkTracerProvider, InMemorySpanExporter) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    global::set_tracer_provider(provider.clone());

    (provider, exporter)
}

fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a AttributeValue> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| &attribute.value)
}

fn single<'a>(spans: &'a [SpanData], name: &str) -> &'a SpanData {
    let mut matching = spans.iter().filter(|span| span.name == name);
    let span = matching.next().expect("span should be exported");
    assert!(matching.next().is_none(), "{name} should be exported once");

    span
}

/// What one traced run of the scripted provider exported.
struct TracedRun {
    digests: usize,
    model_requests: usize,
    spans: Vec<SpanData>,
    trace_id: String,
}

impl TracedRun {
    fn root(&self) -> &SpanData {
        single(&self.spans, "trace_scenario")
    }

    fn turn(&self) -> &SpanData {
        single(&self.spans, "invoke_agent")
    }

    /// Model-request spans in the order the requests started.
    fn requests(&self) -> Vec<&SpanData> {
        let mut requests = self
            .spans
            .iter()
            .filter(|span| span.name == format!("chat {MODEL}"))
            .collect::<Vec<_>>();
        requests.sort_by_key(|span| span.start_time);

        requests
    }
}

/// Runs one read turn of the scripted provider in a run trace and finishes
/// the trace with a record of its three requests.
async fn traced_scripted_run() -> TracedRun {
    let (provider, exporter) = install_provider();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(Script {
            next: AtomicUsize::new(0),
            responses: scripted_responses(),
        })
        .mount(&server)
        .await;
    let client = ModelClient::qwen(QwenConfig {
        api_key: "test-key".to_string(),
        base_url: server.uri(),
        model: MODEL.to_string(),
    })
    .expect("fixture configuration should be valid");
    let (model, sent) = ProbedModel::new(client);
    let workspace = tempfile::tempdir().expect("workspace should be created");
    std::fs::write(workspace.path().join("notes.txt"), "code=amber\n")
        .expect("fixture file should be written");
    let schema = OutputSchema::new(json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"],
        "additionalProperties": false
    }))
    .expect("fixture schema should be valid");
    let trace = RunTrace::start("trace_scenario");
    let harness = Harness::new(model, ContextBudget::new(NonZeroU64::MAX))
        .repository(repository_with_host_git(workspace.path()))
        .allow(Tool::Read)
        .with_lifecycle_observer(RunTrace::observer());
    let record = json!({
        "scenario": "trace_scenario",
        "success": true,
        "correctness": 1.0,
        "duplicate_reads": 0,
        "lost_prefixes": { "harness": 0, "provider": 1 },
        "summary_outcomes": [],
        "requests": [
            { "purpose": "turn", "rejected_output": true },
            { "purpose": "turn", "input_tokens": 100, "prefix_held": null },
            { "purpose": "turn", "input_tokens": 150, "prefix_held": false, "lost_to": "provider" },
        ],
    });
    let outcome = harness
        .run_once("Read notes.txt.", schema)
        .with_context(trace.context())
        .await
        .expect("scripted turn should complete");
    let trace_id = trace.trace_id();
    let digests = sent.take().len();
    trace.finish(&record, sent.take_spans());
    provider.force_flush().expect("spans should flush");

    TracedRun {
        digests,
        model_requests: outcome.report().model_requests().len(),
        spans: exporter.get_finished_spans().expect("spans should export"),
        trace_id,
    }
}

#[tokio::test]
async fn run_trace_nests_the_harness_spans_under_one_root() {
    // Arrange
    let _provider_guard = PROVIDER_LOCK.lock().await;

    // Act
    let run = traced_scripted_run().await;

    // Assert
    let root = run.root();
    let turn = run.turn();
    let tool = single(&run.spans, "execute_tool read");
    let requests = run.requests();
    assert_eq!(run.model_requests, 3);
    assert_eq!(run.digests, 3);
    assert_eq!(root.span_context.trace_id().to_string(), run.trace_id);
    assert_eq!(root.parent_span_id, SpanId::INVALID);
    assert_eq!(turn.parent_span_id, root.span_context.span_id());
    assert_eq!(tool.parent_span_id, turn.span_context.span_id());
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(
            request.span_context.trace_id(),
            root.span_context.trace_id()
        );
        assert_eq!(request.parent_span_id, turn.span_context.span_id());
        assert!(request.end_time <= turn.end_time);
    }
}

#[tokio::test]
async fn run_trace_annotates_the_root_and_completed_requests_with_their_records() {
    // Arrange
    let _provider_guard = PROVIDER_LOCK.lock().await;

    // Act
    let run = traced_scripted_run().await;

    // Assert
    let root = run.root();
    let requests = run.requests();
    assert_eq!(attribute(root, "benchmark.success"), Some(&true.into()));
    assert_eq!(attribute(root, "benchmark.correctness"), Some(&1.0.into()));
    assert_eq!(
        attribute(root, "benchmark.lost_prefixes.provider"),
        Some(&1_i64.into())
    );
    assert_eq!(attribute(root, "benchmark.requests"), None);
    assert_eq!(attribute(root, "benchmark.summary_outcomes"), None);
    assert_eq!(
        attribute(root, "agentty.outcome"),
        Some(&"completed".into())
    );
    // The harness ended the rejected request with its error.
    assert!(matches!(requests[0].status, Status::Error { .. }));
    assert_eq!(attribute(requests[0], "benchmark.request"), None);
    assert_eq!(
        attribute(requests[1], "benchmark.request"),
        Some(&1_i64.into())
    );
    assert_eq!(
        attribute(requests[1], "gen_ai.response.finish_reasons"),
        Some(&AttributeValue::Array(Array::String(vec![
            StringValue::from("tool_calls")
        ])))
    );
    assert_eq!(
        attribute(requests[2], "gen_ai.usage.input_tokens"),
        Some(&150_i64.into())
    );
    assert_eq!(
        attribute(requests[2], "gen_ai.usage.cache_read.input_tokens"),
        Some(&10_i64.into())
    );
    assert_eq!(
        attribute(requests[2], "gen_ai.response.id"),
        Some(&"answer".into())
    );
    assert_eq!(
        attribute(requests[2], "benchmark.lost_to"),
        Some(&"provider".into())
    );
    assert_eq!(
        attribute(requests[2], "benchmark.prefix_held"),
        Some(&false.into())
    );
}

#[tokio::test]
async fn run_trace_fails_an_unsuccessful_root_and_never_ends_it_as_a_request() {
    // Arrange
    let _provider_guard = PROVIDER_LOCK.lock().await;
    let (provider, exporter) = install_provider();
    let trace = RunTrace::start("failed_scenario");
    let untraced = RequestSpan::returned(Context::new(), None);
    let in_root = RequestSpan::returned(trace.context(), None);

    // Act
    trace.finish(
        &json!({ "success": false, "detail": "timed out after 1s" }),
        vec![untraced, in_root],
    );
    provider.force_flush().expect("spans should flush");

    // Assert
    let spans = exporter.get_finished_spans().expect("spans should export");
    assert_eq!(spans.len(), 1);
    let root = &spans[0];
    assert!(matches!(root.status, Status::Error { .. }));
    assert_eq!(attribute(root, "agentty.outcome"), Some(&"failed".into()));
    assert_eq!(
        attribute(root, "benchmark.detail"),
        Some(&"timed out after 1s".into())
    );
    assert_eq!(attribute(root, "benchmark.request"), None);
}

#[test]
fn flatten_projects_scalars_nested_fields_and_scalar_arrays() {
    // Arrange
    let record = json!({
        "absent": null,
        "big": u64::MAX,
        "durations": [12, 30],
        "mixed": [1, "two"],
        "nested": { "harness": 2, "share": 0.5 },
        "outcomes": ["completed"],
        "requests": [{ "ms": 1 }],
        "success": true,
    });
    let mut attributes = Vec::new();

    // Act
    flatten("benchmark".to_string(), &record, &mut attributes);

    // Assert
    assert_eq!(
        attributes,
        [
            KeyValue::new("benchmark.big", 1.844_674_407_370_955_2e19),
            KeyValue::new(
                "benchmark.durations",
                AttributeValue::Array(Array::I64(vec![12, 30]))
            ),
            KeyValue::new("benchmark.nested.harness", 2_i64),
            KeyValue::new("benchmark.nested.share", 0.5),
            KeyValue::new(
                "benchmark.outcomes",
                AttributeValue::Array(Array::String(vec![StringValue::from("completed")]))
            ),
            KeyValue::new("benchmark.success", true),
        ]
    );
}
