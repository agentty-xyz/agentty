use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use opentelemetry::trace::{SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Array, Context, KeyValue, StringValue, Value, global};

use crate::lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, LifecycleOperationGuard,
};
use crate::model::{CompletionMetadata, ModelMetadata};
use crate::telemetry;

const TOOL_CALL_ID_ATTRIBUTE_LIMIT_BYTES: usize = 128;

/// Projects one ordered harness lifecycle stream to OpenTelemetry `GenAI`
/// spans.
///
/// Install an OpenTelemetry tracer provider before operations start, then
/// attach one observer to a [`crate::Harness`] or [`crate::ModelClient`].
/// Applications retain ownership of exporter configuration, flushing, and
/// shutdown.
pub struct LifecycleTraceObserver {
    state: Mutex<TraceState>,
}

impl LifecycleTraceObserver {
    /// Creates an empty lifecycle trace projection.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TraceState::default()),
        }
    }
}

impl Default for LifecycleTraceObserver {
    fn default() -> Self {
        Self::new()
    }
}

impl LifecycleObserver for LifecycleTraceObserver {
    fn observe(&self, event: LifecycleEvent) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(event.kind());
    }

    fn enter_operation(
        &self,
        operation_id: LifecycleId,
    ) -> Option<Box<dyn LifecycleOperationGuard>> {
        let context = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .operation_context(operation_id)?;

        Some(Box::new(TraceOperationGuard {
            _guard: context.attach(),
        }))
    }
}

struct TraceOperationGuard {
    _guard: opentelemetry::ContextGuard,
}

impl LifecycleOperationGuard for TraceOperationGuard {}

#[derive(Default)]
struct TraceState {
    model_spans: HashMap<LifecycleId, Context>,
    pending_tools: HashMap<LifecycleId, PendingTool>,
    tool_spans: HashMap<LifecycleId, Context>,
    turn_spans: HashMap<LifecycleId, Context>,
}

impl TraceState {
    fn operation_context(&self, operation_id: LifecycleId) -> Option<Context> {
        self.model_spans
            .get(&operation_id)
            .or_else(|| self.tool_spans.get(&operation_id))
            .cloned()
    }

    fn observe(&mut self, event: &LifecycleEventKind) {
        match event {
            LifecycleEventKind::TurnStarted { turn_id } => self.start_turn(*turn_id),
            LifecycleEventKind::TurnCompleted { turn_id, .. } => {
                finish_span(self.turn_spans.remove(turn_id), None, Vec::new());
            }
            LifecycleEventKind::TurnFailed {
                error_type,
                turn_id,
                ..
            } => {
                finish_span(
                    self.turn_spans.remove(turn_id),
                    Some(error_type.as_str().to_string()),
                    Vec::new(),
                );
            }
            LifecycleEventKind::ModelRequestStarted {
                model,
                model_call_id,
                turn_id,
                ..
            } => self.start_model(*model_call_id, model.as_ref(), *turn_id),
            LifecycleEventKind::ModelRequestCompleted {
                completion,
                model_call_id,
                ..
            } => finish_span(
                self.model_spans.remove(model_call_id),
                None,
                completion_attributes(completion.as_ref()),
            ),
            LifecycleEventKind::ModelRequestFailed {
                error_type,
                http_status,
                model_call_id,
                ..
            } => finish_span(
                self.model_spans.remove(model_call_id),
                Some(http_status.map_or_else(
                    || error_type.as_str().to_string(),
                    |status| status.to_string(),
                )),
                Vec::new(),
            ),
            LifecycleEventKind::ModelRequestCancelled { model_call_id, .. } => finish_span(
                self.model_spans.remove(model_call_id),
                Some(telemetry::ERROR_CANCELLED.to_string()),
                Vec::new(),
            ),
            LifecycleEventKind::ToolRequested {
                provider_call_id,
                tool_call_id,
                tool_name,
                turn_id,
            } => {
                self.pending_tools.insert(
                    *tool_call_id,
                    PendingTool {
                        name: tool_name.clone(),
                        provider_call_id: provider_call_id.clone(),
                        turn_id: *turn_id,
                    },
                );
            }
            LifecycleEventKind::ToolStarted {
                tool_call_id,
                turn_id,
            } => self.start_tool(*tool_call_id, *turn_id),
            LifecycleEventKind::ToolCompleted { tool_call_id, .. } => {
                finish_span(self.tool_spans.remove(tool_call_id), None, Vec::new());
            }
            LifecycleEventKind::ToolDenied { tool_call_id, .. } => {
                self.pending_tools.remove(tool_call_id);
            }
            LifecycleEventKind::ToolFailed {
                error_type,
                tool_call_id,
                ..
            } => {
                self.pending_tools.remove(tool_call_id);
                finish_span(
                    self.tool_spans.remove(tool_call_id),
                    Some(error_type.as_str().to_string()),
                    Vec::new(),
                );
            }
        }
    }

    fn start_turn(&mut self, turn_id: LifecycleId) {
        let context = start_span(
            telemetry::OPERATION_INVOKE_AGENT,
            SpanKind::Internal,
            vec![
                KeyValue::new(
                    telemetry::ATTRIBUTE_OPERATION_NAME,
                    telemetry::OPERATION_INVOKE_AGENT,
                ),
                KeyValue::new(telemetry::ATTRIBUTE_OUTPUT_TYPE, telemetry::OUTPUT_JSON),
            ],
            None,
        );
        self.turn_spans.insert(turn_id, context);
    }

    fn start_model(
        &mut self,
        model_call_id: LifecycleId,
        model: Option<&ModelMetadata>,
        turn_id: Option<LifecycleId>,
    ) {
        let Some(model) = model else {
            return;
        };
        let name = format!("{} {}", telemetry::OPERATION_CHAT, model.model());
        let attributes = vec![
            KeyValue::new(
                telemetry::ATTRIBUTE_OPERATION_NAME,
                telemetry::OPERATION_CHAT,
            ),
            KeyValue::new(telemetry::ATTRIBUTE_PROVIDER_NAME, model.provider()),
            KeyValue::new(
                telemetry::ATTRIBUTE_REQUEST_MODEL,
                model.model().to_string(),
            ),
            KeyValue::new(telemetry::ATTRIBUTE_OUTPUT_TYPE, telemetry::OUTPUT_JSON),
        ];
        let context = match turn_id {
            Some(turn_id) => {
                let Some(parent) = self.turn_spans.get(&turn_id) else {
                    return;
                };
                start_span(name, SpanKind::Client, attributes, Some(parent))
            }
            None => start_span(name, SpanKind::Client, attributes, None),
        };
        self.model_spans.insert(model_call_id, context);
    }

    fn start_tool(&mut self, tool_call_id: LifecycleId, turn_id: LifecycleId) {
        let Some(tool) = self.pending_tools.remove(&tool_call_id) else {
            return;
        };
        debug_assert_eq!(tool.turn_id, turn_id);
        let name = format!("{} {}", telemetry::OPERATION_EXECUTE_TOOL, tool.name);
        let mut attributes = vec![
            KeyValue::new(
                telemetry::ATTRIBUTE_OPERATION_NAME,
                telemetry::OPERATION_EXECUTE_TOOL,
            ),
            KeyValue::new(telemetry::ATTRIBUTE_TOOL_NAME, tool.name),
            KeyValue::new(
                telemetry::ATTRIBUTE_TOOL_TYPE,
                telemetry::TOOL_TYPE_FUNCTION,
            ),
        ];
        if tool.provider_call_id.len() <= TOOL_CALL_ID_ATTRIBUTE_LIMIT_BYTES {
            attributes.push(KeyValue::new(
                telemetry::ATTRIBUTE_TOOL_CALL_ID,
                tool.provider_call_id,
            ));
        }
        let Some(parent) = self.turn_spans.get(&turn_id) else {
            return;
        };
        let context = start_span(name, SpanKind::Internal, attributes, Some(parent));
        self.tool_spans.insert(tool_call_id, context);
    }
}

struct PendingTool {
    name: String,
    provider_call_id: String,
    turn_id: LifecycleId,
}

fn start_span(
    name: impl Into<std::borrow::Cow<'static, str>>,
    kind: SpanKind,
    attributes: Vec<KeyValue>,
    parent: Option<&Context>,
) -> Context {
    let tracer = global::tracer(telemetry::INSTRUMENTATION_SCOPE);
    let builder = tracer
        .span_builder(name)
        .with_kind(kind)
        .with_attributes(attributes);
    let parent = parent.cloned().unwrap_or_else(Context::current);
    let span = builder.start_with_context(&tracer, &parent);

    parent.with_span(span)
}

fn finish_span(context: Option<Context>, error_type: Option<String>, attributes: Vec<KeyValue>) {
    let Some(context) = context else {
        return;
    };
    let span = context.span();
    span.set_attributes(attributes);
    if let Some(error_type) = error_type {
        span.set_attribute(KeyValue::new(telemetry::ATTRIBUTE_ERROR_TYPE, error_type));
        span.set_status(Status::error(""));
    }
    span.end();
}

fn completion_attributes(completion: Option<&CompletionMetadata>) -> Vec<KeyValue> {
    let Some(completion) = completion else {
        return Vec::new();
    };
    let mut attributes = vec![KeyValue::new(
        telemetry::ATTRIBUTE_RESPONSE_FINISH_REASONS,
        Value::Array(Array::String(vec![StringValue::from(
            completion.finish_reason().to_string(),
        )])),
    )];
    if let Some(response_id) = completion.response_id() {
        attributes.push(KeyValue::new(
            telemetry::ATTRIBUTE_RESPONSE_ID,
            response_id.to_string(),
        ));
    }
    if let Some(response_model) = completion.response_model() {
        attributes.push(KeyValue::new(
            telemetry::ATTRIBUTE_RESPONSE_MODEL,
            response_model.to_string(),
        ));
    }
    if let Some(usage) = completion.usage() {
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_CACHE_READ_INPUT_TOKENS,
            usage.cache_hit_tokens(),
        );
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_INPUT_TOKENS,
            usage.input_tokens(),
        );
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_OUTPUT_TOKENS,
            usage.output_tokens(),
        );
        push_token_attribute(
            &mut attributes,
            telemetry::ATTRIBUTE_USAGE_REASONING_OUTPUT_TOKENS,
            usage.reasoning_tokens(),
        );
    }

    attributes
}

fn push_token_attribute(attributes: &mut Vec<KeyValue>, key: &'static str, value: Option<u64>) {
    let Some(value) = value.and_then(|value| i64::try_from(value).ok()) else {
        return;
    };
    attributes.push(KeyValue::new(key, value));
}

#[cfg(test)]
#[path = "trace_test.rs"]
mod tests;
