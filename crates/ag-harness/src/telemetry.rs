use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use opentelemetry::metrics::Histogram;
use opentelemetry::{KeyValue, global};

use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver};
use crate::model::{CompletionMetadata, ModelError, ModelMetadata};

pub(crate) const ATTRIBUTE_ERROR_TYPE: &str = "error.type";
pub(crate) const ATTRIBUTE_OUTPUT_TYPE: &str = "gen_ai.output.type";
pub(crate) const ATTRIBUTE_OPERATION_NAME: &str = "gen_ai.operation.name";
pub(crate) const ATTRIBUTE_PROVIDER_NAME: &str = "gen_ai.provider.name";
pub(crate) const ATTRIBUTE_REQUEST_MODEL: &str = "gen_ai.request.model";
pub(crate) const ATTRIBUTE_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
pub(crate) const ATTRIBUTE_RESPONSE_ID: &str = "gen_ai.response.id";
pub(crate) const ATTRIBUTE_RESPONSE_MODEL: &str = "gen_ai.response.model";
pub(crate) const ATTRIBUTE_TOKEN_TYPE: &str = "gen_ai.token.type";
pub(crate) const ATTRIBUTE_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";
pub(crate) const ATTRIBUTE_TOOL_NAME: &str = "gen_ai.tool.name";
pub(crate) const ATTRIBUTE_TOOL_TYPE: &str = "gen_ai.tool.type";
pub(crate) const ATTRIBUTE_USAGE_CACHE_READ_INPUT_TOKENS: &str =
    "gen_ai.usage.cache_read.input_tokens";
pub(crate) const ATTRIBUTE_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub(crate) const ATTRIBUTE_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
pub(crate) const ATTRIBUTE_USAGE_REASONING_OUTPUT_TOKENS: &str =
    "gen_ai.usage.reasoning.output_tokens";
pub(crate) const DURATION_BOUNDARIES_SECONDS: [f64; 14] = [
    0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
];
pub(crate) const DURATION_DESCRIPTION: &str = "GenAI operation duration.";
pub(crate) const DURATION_METRIC: &str = "gen_ai.client.operation.duration";
pub(crate) const DURATION_UNIT: &str = "s";
pub(crate) const ERROR_CANCELLED: &str = "cancelled";
pub(crate) const ERROR_INVALID_OUTPUT: &str = "invalid_output";
pub(crate) const ERROR_INVALID_PROVIDER_RESPONSE: &str = "invalid_provider_response";
pub(crate) const ERROR_INVALID_RESPONSE: &str = "invalid_response";
pub(crate) const ERROR_INVALID_TOOL_CALL: &str = "invalid_tool_call";
pub(crate) const ERROR_PROVIDER: &str = "provider_error";
pub(crate) const ERROR_REQUEST: &str = "request_error";
pub(crate) const ERROR_RESPONSE_TOO_LARGE: &str = "response_too_large";
pub(crate) const ERROR_TRANSPORT: &str = "transport_error";
pub(crate) const ERROR_TOOL_CALL_LIMIT: &str = "tool_call_limit";
pub(crate) const ERROR_TOOL_DENIED: &str = "tool_denied";
pub(crate) const ERROR_TOOL_EXECUTION: &str = "tool_execution_error";
pub(crate) const ERROR_REPOSITORY_REQUIRED: &str = "repository_required";
pub(crate) const ERROR_SESSION: &str = "session_error";
pub(crate) const ERROR_UNSUPPORTED_OUTPUT: &str = "unsupported_output";
pub(crate) const INSTRUMENTATION_SCOPE: &str = "ag-harness";
pub(crate) const OPERATION_CHAT: &str = "chat";
pub(crate) const OPERATION_EXECUTE_TOOL: &str = "execute_tool";
pub(crate) const OPERATION_INVOKE_AGENT: &str = "invoke_agent";
pub(crate) const OUTPUT_JSON: &str = "json";
pub(crate) const PROVIDER_ALIBABA_CLOUD: &str = "alibaba_cloud";
pub(crate) const PROVIDER_META: &str = "meta";
pub(crate) const PROVIDER_MOONSHOT_AI: &str = "moonshot_ai";
pub(crate) const TOKEN_BOUNDARIES: [f64; 14] = [
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
pub(crate) const TOKEN_DESCRIPTION: &str = "Number of input and output tokens used.";
pub(crate) const TOKEN_METRIC: &str = "gen_ai.client.token.usage";
pub(crate) const TOKEN_TYPE_INPUT: &str = "input";
pub(crate) const TOKEN_TYPE_OUTPUT: &str = "output";
pub(crate) const TOKEN_UNIT: &str = "{token}";
const AGENT_CALL_BOUNDARIES: [f64; 8] = [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 64.0, 128.0];
const AGENT_DURATION_BOUNDARIES_SECONDS: [f64; 13] = [
    0.1, 0.2, 0.4, 0.8, 1.6, 3.2, 6.4, 12.8, 25.6, 51.2, 102.4, 204.8, 409.6,
];
const AGENT_DURATION_DESCRIPTION: &str = "The end-to-end duration of a single in-process agent \
                                          invocation, from the moment the invocation starts until \
                                          the agent emits the last chunk of its final response or \
                                          terminates with an error.";
const AGENT_DURATION_METRIC: &str = "gen_ai.invoke_agent.duration";
const AGENT_INFERENCE_CALLS_DESCRIPTION: &str =
    "The number of inference (model) calls a GenAI agent makes during a single invocation.";
const AGENT_INFERENCE_CALLS_METRIC: &str = "gen_ai.invoke_agent.inference_calls";
const AGENT_INFERENCE_CALLS_UNIT: &str = "{inference_call}";
const AGENT_TOOL_CALLS_DESCRIPTION: &str =
    "The number of tool calls a GenAI agent makes during a single invocation.";
const AGENT_TOOL_CALLS_METRIC: &str = "gen_ai.invoke_agent.tool_calls";
const AGENT_TOOL_CALLS_UNIT: &str = "{tool_call}";
const TOOL_DURATION_DESCRIPTION: &str = "The duration of a single tool execution.";
const TOOL_DURATION_METRIC: &str = "gen_ai.execute_tool.duration";
pub(crate) const TOOL_TYPE_FUNCTION: &str = "function";

/// OpenTelemetry metric projection over ordered harness lifecycle events.
///
/// Applications install an OpenTelemetry meter provider before constructing
/// this observer and retain ownership of export, flushing, and shutdown.
pub struct LifecycleMetrics {
    agent_duration: Histogram<f64>,
    agent_inference_calls: Histogram<u64>,
    agent_tool_calls: Histogram<u64>,
    state: Mutex<LifecycleMetricState>,
    tool_duration: Histogram<f64>,
}

impl LifecycleMetrics {
    /// Creates a lifecycle observer backed by the global meter provider.
    pub fn new() -> Self {
        let meter = global::meter(INSTRUMENTATION_SCOPE);

        Self {
            agent_duration: meter
                .f64_histogram(AGENT_DURATION_METRIC)
                .with_description(AGENT_DURATION_DESCRIPTION)
                .with_unit(DURATION_UNIT)
                .with_boundaries(AGENT_DURATION_BOUNDARIES_SECONDS.to_vec())
                .build(),
            agent_inference_calls: meter
                .u64_histogram(AGENT_INFERENCE_CALLS_METRIC)
                .with_description(AGENT_INFERENCE_CALLS_DESCRIPTION)
                .with_unit(AGENT_INFERENCE_CALLS_UNIT)
                .with_boundaries(AGENT_CALL_BOUNDARIES.to_vec())
                .build(),
            agent_tool_calls: meter
                .u64_histogram(AGENT_TOOL_CALLS_METRIC)
                .with_description(AGENT_TOOL_CALLS_DESCRIPTION)
                .with_unit(AGENT_TOOL_CALLS_UNIT)
                .with_boundaries(AGENT_CALL_BOUNDARIES.to_vec())
                .build(),
            state: Mutex::new(LifecycleMetricState::default()),
            tool_duration: meter
                .f64_histogram(TOOL_DURATION_METRIC)
                .with_description(TOOL_DURATION_DESCRIPTION)
                .with_unit(DURATION_UNIT)
                .with_boundaries(DURATION_BOUNDARIES_SECONDS.to_vec())
                .build(),
        }
    }

    fn record(&self, measurement: MetricMeasurement) {
        match measurement {
            MetricMeasurement::Agent(measurement) => {
                let duration_attributes = measurement
                    .error_type
                    .map(|error_type| vec![KeyValue::new(ATTRIBUTE_ERROR_TYPE, error_type)])
                    .unwrap_or_default();
                self.agent_duration
                    .record(measurement.duration.as_secs_f64(), &duration_attributes);
                self.agent_inference_calls
                    .record(measurement.inference_calls, &[]);
                self.agent_tool_calls.record(measurement.tool_calls, &[]);
            }
            MetricMeasurement::Tool(measurement) => {
                let mut attributes = Vec::with_capacity(3);
                if let Some(error_type) = measurement.error_type {
                    attributes.push(KeyValue::new(ATTRIBUTE_ERROR_TYPE, error_type));
                }
                attributes.push(KeyValue::new(ATTRIBUTE_TOOL_NAME, measurement.name));
                attributes.push(KeyValue::new(ATTRIBUTE_TOOL_TYPE, TOOL_TYPE_FUNCTION));
                self.tool_duration
                    .record(measurement.duration.as_secs_f64(), &attributes);
            }
        }
    }
}

impl Default for LifecycleMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleObserver for LifecycleMetrics {
    fn observe(&self, event: LifecycleEvent) {
        let measurement = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(&event);
        if let Some(measurement) = measurement {
            self.record(measurement);
        }
    }
}

#[derive(Default)]
struct LifecycleMetricState {
    tools: HashMap<LifecycleId, PendingToolMeasurement>,
    turns: HashMap<LifecycleId, PendingAgentMeasurement>,
}

impl LifecycleMetricState {
    fn observe(&mut self, event: &LifecycleEvent) -> Option<MetricMeasurement> {
        match event.kind() {
            LifecycleEventKind::TurnStarted { turn_id } => {
                self.turns
                    .insert(*turn_id, PendingAgentMeasurement::default());

                None
            }
            LifecycleEventKind::TurnCompleted { duration, turn_id } => {
                self.complete_turn(*duration, None, *turn_id)
            }
            LifecycleEventKind::TurnFailed {
                duration,
                error_type,
                turn_id,
            } => self.complete_turn(*duration, Some(error_type.as_str()), *turn_id),
            LifecycleEventKind::ModelRequestStarted {
                turn_id: Some(turn_id),
                ..
            } => {
                if let Some(turn) = self.turns.get_mut(turn_id) {
                    turn.inference_calls += 1;
                }

                None
            }
            LifecycleEventKind::ToolRequested {
                tool_call_id,
                tool_name,
                turn_id,
                ..
            } => {
                if let Some(turn) = self.turns.get_mut(turn_id) {
                    turn.tool_calls += 1;
                    self.tools.insert(
                        *tool_call_id,
                        PendingToolMeasurement {
                            name: tool_name.clone(),
                            started: false,
                            turn_id: *turn_id,
                        },
                    );
                }

                None
            }
            LifecycleEventKind::ToolStarted { tool_call_id, .. } => {
                if let Some(tool) = self.tools.get_mut(tool_call_id) {
                    tool.started = true;
                }

                None
            }
            LifecycleEventKind::ToolCompleted {
                duration,
                tool_call_id,
                ..
            } => self.complete_tool(*duration, None, *tool_call_id),
            LifecycleEventKind::ToolDenied { tool_call_id, .. } => {
                self.tools.remove(tool_call_id);

                None
            }
            LifecycleEventKind::ToolFailed {
                duration,
                error_type,
                tool_call_id,
                ..
            } => self.complete_tool(*duration, Some(error_type.as_str()), *tool_call_id),
            LifecycleEventKind::ModelRequestStarted { turn_id: None, .. }
            | LifecycleEventKind::ModelRequestCompleted { .. }
            | LifecycleEventKind::ModelRequestFailed { .. }
            | LifecycleEventKind::ModelRequestCancelled { .. } => None,
        }
    }

    fn complete_turn(
        &mut self,
        duration: Duration,
        error_type: Option<&'static str>,
        turn_id: LifecycleId,
    ) -> Option<MetricMeasurement> {
        let pending = self.turns.remove(&turn_id)?;
        self.tools.retain(|_, tool| tool.turn_id != turn_id);

        Some(MetricMeasurement::Agent(AgentMeasurement {
            duration,
            error_type,
            inference_calls: pending.inference_calls,
            tool_calls: pending.tool_calls,
        }))
    }

    fn complete_tool(
        &mut self,
        duration: Duration,
        error_type: Option<&'static str>,
        tool_call_id: LifecycleId,
    ) -> Option<MetricMeasurement> {
        let pending = self.tools.remove(&tool_call_id)?;
        pending
            .started
            .then_some(MetricMeasurement::Tool(ToolMeasurement {
                duration,
                error_type,
                name: pending.name,
            }))
    }
}

struct AgentMeasurement {
    duration: Duration,
    error_type: Option<&'static str>,
    inference_calls: u64,
    tool_calls: u64,
}

enum MetricMeasurement {
    Agent(AgentMeasurement),
    Tool(ToolMeasurement),
}

#[derive(Default)]
struct PendingAgentMeasurement {
    inference_calls: u64,
    tool_calls: u64,
}

struct PendingToolMeasurement {
    name: String,
    started: bool,
    turn_id: LifecycleId,
}

struct ToolMeasurement {
    duration: Duration,
    error_type: Option<&'static str>,
    name: String,
}

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

#[cfg(test)]
#[path = "telemetry_test.rs"]
mod tests;
