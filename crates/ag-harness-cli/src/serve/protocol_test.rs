//! Wire-type unit tests: envelope and parameter decoding, conversions to
//! harness options, message encoding, and the committed protocol schema.

use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use ag_harness::event::RunEvent;
use ag_harness::sandbox::{SandboxUnavailableReason, SandboxWarning};
use ag_harness::{
    ApprovalRequest, Decision, ErrorKind, GenerationParams, LimitKind, OutputMode, RunLimits,
    TurnRange,
};
use jsonschema::Validator;
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};

use super::{
    Answer, ApprovalDecision, Call, Empty, Envelope, ErrorData, ErrorObject, ErrorResponse,
    GenerationChoice, INVALID_PARAMS, INVALID_REQUEST, JsonRpcVersion, LimitsChoice,
    METHOD_NOT_FOUND, MethodResult, ModelChoice, Notification, OutputChoice, PROTOCOL_SCHEMA,
    RequestId, Response, SERVER_ERROR, ServerRequest, SessionOpened, StoreChoice, line,
};

/// Result definition of each method, in [`Call::METHODS`] order.
const METHOD_RESULTS: [(&str, &str); 7] = [
    ("session/create", "SessionOpened"),
    ("session/resume", "SessionOpened"),
    ("run/start", "RunStarted"),
    ("run/cancel", "Empty"),
    ("session/transcript", "Transcript"),
    ("session/usage", "SessionUsage"),
    ("shutdown", "Empty"),
];

/// Assembles the protocol schema: every message is a client request, a
/// response, an error response, a notification, a server request, or the
/// client's answer to one; `x-method-results` names each method's result
/// and `x-server-request-results` each server request's answer.
fn protocol_schema() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let call = generator.subschema_for::<Call>();
    let id = generator.subschema_for::<RequestId>();
    let version = generator.subschema_for::<JsonRpcVersion>();
    let response = generator.subschema_for::<Response>();
    let error = generator.subschema_for::<ErrorResponse>();
    let notification = generator.subschema_for::<Notification>();
    let server_request = generator.subschema_for::<ServerRequest>();
    let decision = generator.subschema_for::<ApprovalDecision>();
    let mut definitions = generator.take_definitions(true);
    make_default_params_optional(&mut definitions);
    definitions.insert(
        "ApprovalAnswer".to_string(),
        json!({
            "description": "Client answer to an `approval/decide` server request: the request's \
                            `id` and the decision as `result`. An error response with the \
                            request's `id` denies the call with the error's message.",
            "type": "object",
            "properties": {"id": id, "jsonrpc": version, "result": decision},
            "required": ["id", "jsonrpc", "result"],
            "additionalProperties": false
        }),
    );
    definitions.insert(
        "Request".to_string(),
        json!({
            "description": "Client request: `jsonrpc`, `id`, `method`, and the method's \
                            `params`, which may be omitted when every parameter has a default. \
                            A message without `id` is a client notification, which the server \
                            executes but never answers.",
            "type": "object",
            "properties": {"id": id, "jsonrpc": version},
            "required": ["jsonrpc"],
            "allOf": [call]
        }),
    );
    let results: serde_json::Map<String, Value> = METHOD_RESULTS
        .iter()
        .map(|(method, result)| {
            (
                (*method).to_string(),
                json!({"$ref": format!("#/$defs/{result}")}),
            )
        })
        .collect();

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "ServeMessage",
        "description": "One line of the UNSTABLE `ag-harness serve` JSON-RPC 2.0 protocol: a \
                        client request or answer, or a server response, error response, \
                        `run/event` notification, or `approval/decide` request. The protocol \
                        may change in any release.",
        "anyOf": [
            {"$ref": "#/$defs/Request"},
            response,
            error,
            notification,
            server_request,
            {"$ref": "#/$defs/ApprovalAnswer"}
        ],
        "x-method-results": results,
        "x-server-request-results": {"approval/decide": decision},
        "$defs": definitions
    })
}

/// Lets requests omit `params` for methods whose parameters all have
/// defaults, as the server accepts.
fn make_default_params_optional(definitions: &mut serde_json::Map<String, Value>) {
    let defaulted: Vec<String> = definitions
        .iter()
        .filter(|(_, definition)| definition.get("required").is_none())
        .map(|(name, _)| format!("#/$defs/{name}"))
        .collect();
    let Some(Value::Array(arms)) = definitions
        .get_mut("Call")
        .and_then(|call| call.get_mut("oneOf"))
    else {
        return;
    };
    for arm in arms {
        let params = arm["properties"]["params"]["$ref"].as_str().unwrap_or("");
        if defaulted.iter().any(|name| name == params)
            && let Some(Value::Array(required)) = arm.get_mut("required")
        {
            required.retain(|field| field != "params");
        }
    }
}

fn validator() -> Validator {
    Validator::new(&protocol_schema()).expect("protocol schema should compile")
}

fn completed_event() -> RunEvent {
    serde_json::from_value(json!({
        "at": 1_700_000_000_000_u64,
        "kind": {
            "type": "completed",
            "cost": null,
            "error": {"kind": "budget_exhausted", "limit": "tokens", "message": "limit", "retryable": false},
            "usage": {
                "input": 3, "output": 2, "cache_read": 0, "cache_write": null,
                "reasoning": 0, "unreported_requests": 0
            }
        },
        "run": "run-1",
        "seq": 0
    }))
    .expect("completed event should decode")
}

#[test]
fn protocol_schema_matches_the_committed_snapshot() -> Result<(), serde_json::Error> {
    // Arrange
    let schema = protocol_schema();

    // Act
    let generated = format!("{}\n", serde_json::to_string_pretty(&schema)?);

    // Assert
    assert!(
        generated == PROTOCOL_SCHEMA,
        "crates/ag-harness-cli/schema/serve_protocol.json is stale; replace it with:\n{generated}"
    );

    Ok(())
}

#[test]
fn envelope_reads_identifiers_methods_and_default_params() {
    // Arrange
    let numbered = json!({"jsonrpc": "2.0", "id": 7, "method": "shutdown"});
    let named =
        json!({"jsonrpc": "2.0", "id": "a", "method": "run/cancel", "params": {"run": "r"}});
    let notification = json!({"jsonrpc": "2.0", "method": "shutdown"});

    // Act
    let numbered = Envelope::parse(numbered);
    let named = Envelope::parse(named);
    let notification = Envelope::parse(notification);

    // Assert
    assert_eq!(
        numbered,
        Ok(Envelope {
            id: Some(RequestId(json!(7))),
            method: "shutdown".to_string(),
            params: json!({}),
        })
    );
    assert_eq!(
        named,
        Ok(Envelope {
            id: Some(RequestId(json!("a"))),
            method: "run/cancel".to_string(),
            params: json!({"run": "r"}),
        })
    );
    assert_eq!(notification.map(|envelope| envelope.id), Ok(None));
}

#[test]
fn envelope_rejects_invalid_requests_with_their_readable_identifier() {
    // Arrange
    let messages = [
        json!([1, 2]),
        json!({"jsonrpc": "2.0", "id": 1.5, "method": "shutdown"}),
        json!({"jsonrpc": "1.0", "id": 1, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": 3}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": [1]}),
    ];

    // Act
    let errors: Vec<ErrorResponse> = messages
        .into_iter()
        .filter_map(|message| Envelope::parse(message).err())
        .collect();

    // Assert
    let ids: Vec<Option<RequestId>> = errors.iter().map(|error| error.id.clone()).collect();
    assert_eq!(
        ids,
        [
            None,
            None,
            Some(RequestId(json!(1))),
            Some(RequestId(json!(2))),
            Some(RequestId(json!(3))),
        ]
    );
    assert!(
        errors
            .iter()
            .all(|error| error.error.code == INVALID_REQUEST)
    );
    assert!(errors[1].error.message.contains("`id`"));
    assert!(errors[4].error.message.contains("`params`"));
}

#[test]
fn every_listed_method_decodes_and_unknown_methods_and_params_are_rejected() {
    // Arrange
    let params = [
        json!({"model": {"role": "smart"}, "output": {"type": "text"}}),
        json!({"model": {"id": "openai/gpt-6-luna"}, "session": "s"}),
        json!({"session": "s", "input": "hi"}),
        json!({"run": "r"}),
        json!({"session": "s"}),
        json!({"session": "s"}),
        json!({}),
    ];

    // Act
    let decoded: Vec<Result<Call, ErrorObject>> = Call::METHODS
        .iter()
        .zip(params)
        .map(|(method, params)| Call::parse(method, params))
        .collect();
    let unknown = Call::parse("session/delete", json!({}));
    let invalid = Call::parse(
        "run/start",
        json!({"session": "s", "input": "hi", "extra": 1}),
    );

    // Assert
    assert!(decoded.iter().all(Result::is_ok), "{decoded:?}");
    assert!(matches!(
        decoded[4],
        Ok(Call::SessionTranscript(ref params)) if params.range == TurnRange::All
    ));
    assert_eq!(
        unknown.map_err(|error| error.code).err(),
        Some(METHOD_NOT_FOUND)
    );
    let invalid = invalid.expect_err("unknown fields should be rejected");
    assert_eq!(invalid.code, INVALID_PARAMS);
    assert!(invalid.message.contains("invalid `run/start` params"));
}

#[test]
fn session_params_decode_models_outputs_and_stores() {
    // Arrange
    let params = json!({
        "model": {"id": "openai-compatible/local"},
        "output": {"type": "json", "schema": {"type": "object"}},
        "store": {"type": "sqlite", "path": "/tmp/harness.db"},
        "system_prompt": "Be brief."
    });

    // Act
    let call = Call::parse("session/create", params);

    // Assert
    let params = match call {
        Ok(Call::SessionCreate(params)) => Some(params),
        _ => None,
    }
    .expect("session/create should decode");
    assert!(matches!(params.model, ModelChoice::Id(ref id) if id == "openai-compatible/local"));
    assert!(matches!(params.output, OutputChoice::Json { .. }));
    assert!(
        matches!(params.store, StoreChoice::Sqlite { ref path } if path.ends_with("harness.db"))
    );
    assert_eq!(params.system_prompt.as_deref(), Some("Be brief."));
    assert_eq!(params.session, None);
}

#[test]
fn output_choices_convert_to_output_modes_or_report_invalid_schemas() {
    // Arrange
    let json_output = OutputChoice::Json {
        schema: json!({"type": "object"}),
    };
    let invalid = OutputChoice::Json {
        schema: json!({"type": 5}),
    };

    // Act
    let json_mode = json_output.into_mode();
    let text_mode = OutputChoice::Text.into_mode();
    let invalid = invalid.into_mode();

    // Assert
    assert!(matches!(json_mode, Ok(OutputMode::Json(_))));
    assert!(matches!(text_mode, Ok(OutputMode::Text)));
    assert!(invalid.is_err());
}

#[test]
fn generation_and_limit_choices_convert_every_set_option() {
    // Arrange
    let generation = GenerationChoice {
        max_output_tokens: NonZeroU32::new(64),
    };
    let limits = LimitsChoice {
        model_requests: NonZeroU32::new(2),
        tool_calls: NonZeroU32::new(3),
        total_tokens: NonZeroU64::new(400),
        wall_time_ms: NonZeroU64::new(1_500),
    };

    // Act
    let generation = GenerationParams::from(generation);
    let unset = GenerationParams::from(GenerationChoice::default());
    let limits = RunLimits::from(limits);
    let unlimited = RunLimits::from(LimitsChoice::default());

    // Assert
    assert_eq!(generation.max_output_tokens, NonZeroU32::new(64));
    assert_eq!(unset, GenerationParams::default());
    assert_eq!(limits.max_model_requests, NonZeroU32::new(2));
    assert_eq!(limits.max_tool_calls, NonZeroU32::new(3));
    assert_eq!(limits.max_total_tokens, NonZeroU64::new(400));
    assert_eq!(limits.max_wall_time, Some(Duration::from_millis(1_500)));
    assert_eq!(unlimited, RunLimits::default());
}

#[test]
fn error_data_names_the_limit_of_a_budget_failure_only() {
    // Arrange and Act
    let budget = ErrorData::new(ErrorKind::BudgetExhausted(LimitKind::ToolCalls), false);
    let busy = ErrorData::new(ErrorKind::Busy, true);

    // Assert
    assert_eq!(
        serde_json::to_value(&budget).ok(),
        Some(json!({"kind": "budget_exhausted", "limit": "tool_calls", "retryable": false}))
    );
    assert_eq!(
        serde_json::to_value(&busy).ok(),
        Some(json!({"kind": "busy", "retryable": true}))
    );
}

#[test]
fn server_messages_encode_as_schema_valid_lines() -> Result<(), serde_json::Error> {
    // Arrange
    let validator = validator();
    let error = ErrorObject {
        code: SERVER_ERROR,
        data: Some(ErrorData::new(ErrorKind::NotFound, false)),
        message: "missing".to_string(),
    };
    let messages = [
        line(&Response::new(
            RequestId(json!(1)),
            MethodResult::SessionOpened(SessionOpened {
                session: "s".to_string(),
            }),
        ))?,
        line(&Response::new(
            RequestId(json!("x")),
            MethodResult::Empty(Empty {}),
        ))?,
        line(&ErrorResponse::new(Some(RequestId(json!(2))), error))?,
        line(&ErrorResponse::new(
            None,
            ErrorObject::new(INVALID_REQUEST, "bad".to_string()),
        ))?,
        line(&Notification::run_event("s".to_string(), completed_event()))?,
    ];

    // Act
    let decoded = messages
        .iter()
        .map(|message| serde_json::from_str::<Value>(message))
        .collect::<Result<Vec<_>, _>>()?;

    // Assert
    assert!(messages.iter().all(|message| message.ends_with('\n')));
    for message in &decoded {
        assert!(validator.is_valid(message), "invalid message: {message}");
    }
    assert_eq!(decoded[1]["result"], json!({}));
    assert_eq!(decoded[3]["id"], Value::Null);
    assert_eq!(decoded[4]["method"], json!("run/event"));
    assert_eq!(
        decoded[4]["params"]["event"]["kind"]["type"],
        json!("completed")
    );

    Ok(())
}

#[test]
fn sandbox_warnings_and_refusals_reach_clients_in_the_schema() -> Result<(), serde_json::Error> {
    // Arrange
    let validator = validator();
    let warning = SandboxWarning::new(SandboxUnavailableReason::BubblewrapMissing);
    let event: RunEvent = serde_json::from_value(json!({
        "at": 1_700_000_000_000_u64,
        "kind": {"type": "sandbox_unavailable", "message": warning.message,
                 "reason": "bubblewrap_missing", "remedy": warning.remedy},
        "run": "run-1",
        "seq": 1
    }))?;
    let refusal = ErrorObject {
        code: SERVER_ERROR,
        data: Some(ErrorData::new(ErrorKind::SandboxUnavailable, false)),
        message: "native sandbox unavailable".to_string(),
    };

    // Act
    let notification: Value =
        serde_json::from_str(&line(&Notification::run_event("s".to_string(), event))?)?;
    let error: Value = serde_json::from_str(&line(&ErrorResponse::new(
        Some(RequestId(json!(3))),
        refusal,
    ))?)?;

    // Assert
    assert!(validator.is_valid(&notification), "{notification}");
    assert!(validator.is_valid(&error), "{error}");
    assert_eq!(
        notification["params"]["event"]["kind"]["remedy"],
        json!(SandboxUnavailableReason::BubblewrapMissing.remedy())
    );
    assert_eq!(error["error"]["data"]["kind"], json!("sandbox_unavailable"));

    Ok(())
}

#[test]
fn schema_accepts_requests_and_rejects_unknown_methods() {
    // Arrange
    let validator = validator();
    let accepted = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "run/start",
            "params": {"session": "s", "input": "hi", "limits": {"max_wall_time_ms": 10}}
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "run/cancel", "params": {"run": "r"}}),
    ];
    let rejected = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "run/pause", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "session/usage"}),
    ];

    // Act
    let accepted: Vec<bool> = accepted
        .iter()
        .map(|message| validator.is_valid(message))
        .collect();
    let rejected: Vec<bool> = rejected
        .iter()
        .map(|message| validator.is_valid(message))
        .collect();

    // Assert
    assert_eq!(accepted, [true, true, true]);
    assert_eq!(rejected, [false, false]);
}

#[test]
fn schema_rejects_result_shapes_no_method_returns() {
    // Arrange
    let validator = validator();
    let response = |result: Value| json!({"jsonrpc": "2.0", "id": 1, "result": result});
    let results = [
        json!({"bogus": 1}),
        json!({"session": "s", "run": "r"}),
        json!({"run": 5}),
        json!({}),
    ];

    // Act
    let valid: Vec<bool> = results
        .into_iter()
        .map(|result| validator.is_valid(&response(result)))
        .collect();

    // Assert
    assert_eq!(valid, [false, false, false, true]);
}

#[test]
fn answers_decode_into_decisions_and_other_messages_stay_requests() {
    // Arrange
    let answer = |result: Value| json!({"jsonrpc": "2.0", "id": "approval-0", "result": result});
    let messages = [
        answer(json!({"decision": "allow"})),
        answer(json!({"decision": "allow_with", "arguments": {"path": "b.txt"}})),
        answer(json!({"decision": "deny", "feedback": "no"})),
        json!({"jsonrpc": "2.0", "id": "approval-1", "error": {"code": 1, "message": "busy"}}),
        json!({"jsonrpc": "2.0", "id": "approval-2", "error": {"code": 1}}),
    ];
    let not_answers = [
        json!("text"),
        json!({"jsonrpc": "2.0", "id": 1, "method": "shutdown", "result": {}}),
        json!({"jsonrpc": "2.0", "result": {}}),
        json!({"jsonrpc": "2.0", "id": 1}),
        json!({"jsonrpc": "2.0", "id": 1, "result": {}, "error": {}}),
        json!({"id": "approval-0", "result": {"decision": "allow"}}),
        json!({"jsonrpc": "1.0", "id": "approval-0", "result": {"decision": "allow"}}),
    ];

    // Act
    let decisions: Vec<_> = messages
        .iter()
        .map(|message| Answer::parse(message).map(|answer| answer.decision))
        .collect();
    let invalid = Answer::parse(&answer(json!({"decision": "allow", "extra": 1})));
    let requests: Vec<_> = not_answers.iter().map(Answer::parse).collect();

    // Assert
    assert_eq!(
        decisions,
        [
            Some(Ok(Decision::Allow)),
            Some(Ok(Decision::AllowWith(json!({"path": "b.txt"})))),
            Some(Ok(Decision::Deny {
                feedback: "no".to_string()
            })),
            Some(Ok(Decision::Deny {
                feedback: "busy".to_string()
            })),
            Some(Ok(Decision::Deny {
                feedback: "the client did not decide".to_string()
            })),
        ]
    );
    let invalid = invalid.expect("an answer");
    assert_eq!(invalid.id, json!("approval-0"));
    assert!(invalid.decision.is_err());
    assert!(requests.iter().all(Option::is_none));
}

#[test]
fn server_requests_and_answers_are_schema_valid() -> Result<(), serde_json::Error> {
    // Arrange
    let validator = validator();
    let request: ApprovalRequest = serde_json::from_value(json!({
        "arguments": {"path": "a.txt"},
        "call_id": "call-1",
        "effects": "writes",
        "run": "run-1",
        "session": "s",
        "tool": "write"
    }))?;
    let asked: Value = serde_json::from_str(&line(&ServerRequest::approval(
        "approval-0".to_string(),
        "s".to_string(),
        request,
    ))?)?;
    let answer = |result: Value| json!({"jsonrpc": "2.0", "id": "approval-0", "result": result});

    // Act
    let valid = [
        validator.is_valid(&asked),
        validator.is_valid(&answer(json!({"decision": "allow"}))),
        validator.is_valid(&answer(json!({"decision": "deny", "feedback": "no"}))),
    ];
    let invalid = validator.is_valid(&answer(json!({"decision": "maybe"})));

    // Assert
    assert_eq!(valid, [true, true, true]);
    assert!(!invalid);
    assert_eq!(asked["method"], json!("approval/decide"));
    assert_eq!(asked["params"]["request"]["effects"], json!("writes"));
    let decision: ApprovalDecision = serde_json::from_value(json!({"decision": "allow"}))?;
    assert_eq!(decision, ApprovalDecision::Allow {});

    Ok(())
}

#[test]
fn schema_names_a_result_definition_for_every_method() -> Result<(), serde_json::Error> {
    // Arrange
    let schema: Value = serde_json::from_str(PROTOCOL_SCHEMA)?;

    // Act
    let results = schema["x-method-results"]
        .as_object()
        .cloned()
        .unwrap_or_default();

    // Assert
    let mut methods: Vec<&str> = results.keys().map(String::as_str).collect();
    methods.sort_unstable();
    let mut expected = Call::METHODS.to_vec();
    expected.sort_unstable();
    assert_eq!(methods, expected);
    for reference in results.values() {
        let name = reference["$ref"]
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .unwrap_or_default();
        assert_eq!(
            schema["$defs"][name]["additionalProperties"],
            json!(false),
            "{name} should be a closed object"
        );
    }

    Ok(())
}
