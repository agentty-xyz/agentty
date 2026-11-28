//! The protocol's JSON Schema and TypeScript declarations: their assembly
//! from the wire types, the copies committed under
//! `crates/ag-remote/schema/`, and the conformance of real frames to the
//! schema.
//!
//! `ClientFrame` is described as a server deserializes it, so a field with
//! a default is optional, and `ServerFrame` as a server serializes it, so a
//! field the server skips is optional and never null.

use std::collections::BTreeMap;
use std::sync::Arc;

use ag_session::{
    ForgeKind, HostProject, HostProjectSyncPhase, ReviewRequest, ReviewRequestMerge,
    ReviewRequestMergeOutcome, ReviewRequestState, ReviewRequestSummary, SessionDiffFileStatus,
    SessionFocusedReview, SessionGraphs, SessionId, SessionListEntry, SessionService,
    SessionStatus, SessionSummary,
};
use jsonschema::Validator;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use super::typescript::render_module;
use super::{diff_file, entry_with_stats, file_entry, project_with_sync};
use crate::PROTOCOL_VERSION;
use crate::protocol::{
    ClientFrame, RemoteGraphAction, RemoteGraphActionRequest, RemoteGraphApproval,
    RemoteGraphProposalPhase, RemoteGraphStatus, RemoteImage, RemoteProject, RemoteProjectEntry,
    RemoteQuestion, RemoteQuestionAnswer, RemoteRequest, RemoteResponse, RemoteResult,
    RemoteReviewRequest, RemoteSession, RemoteSessionGraph, RemoteSessionMode,
    RemoteSessionSummary, ServerFrame,
};
use crate::request::capabilities;
use crate::source_support::{
    RecordingBackend, host_fixture, project_status_fixture, session_fixture,
};

/// The committed JSON Schema, relative to the repository root.
const SCHEMA_PATH: &str = "crates/ag-remote/schema/remote_protocol.json";
/// The committed TypeScript declarations, relative to the repository root.
const TYPESCRIPT_PATH: &str = "crates/ag-remote/schema/remote_protocol.ts";
/// Environment variable that makes the drift check rewrite both files.
const UPDATE_VARIABLE: &str = "AGENTTY_REMOTE_SCHEMA_UPDATE";
/// Command that regenerates both files.
const REGENERATE: &str =
    "AGENTTY_REMOTE_SCHEMA_UPDATE=1 cargo test --locked -p ag-remote --lib protocol::tests::schema";
/// What the schema says about the frames as a whole, line by line.
const DESCRIPTION: [&str; 17] = [
    "One JSON text frame of the Agentty remote-control protocol, which a",
    "device and a host exchange inside their encrypted stream: a",
    "`ClientFrame` the device sends or a `ServerFrame` the host sends.",
    "`ClientFrame` describes what a host accepts, so a field with a default",
    "is optional; `ServerFrame` describes what a host sends, so a field it",
    "may leave out is optional and is never null.",
    "",
    "A host speaks exactly `x-protocol-version` and rejects a `hello` or",
    "`pair` frame that names another. A newer host of the same version may",
    "add fields, frame types, response kinds, and enum values: a client",
    "ignores fields it does not know, skips a frame of an unknown `type`,",
    "treats a response of an unknown `kind` as an answer it cannot use, and",
    "shows an unknown enum value as unknown instead of failing. A client",
    "sends a request that names a capability only to a host whose",
    "`capabilities` lists it. The schema lists only what this version",
    "sends, so a client that rejects incoming frames failing it breaks",
    "with a newer host.",
];
/// What the capability names are, line by line.
const CAPABILITY_DESCRIPTION: [&str; 4] = [
    "An optional request a host offers by listing it in the `capabilities`",
    "of `welcome` and `paired`; the requests of the first protocol version",
    "need none. A newer host may list capabilities this version does not",
    "name, so `capabilities` stays a list of strings.",
];
/// Name of the capability definition.
const CAPABILITY_DEFINITION: &str = "RemoteCapability";

/// Assembles the protocol schema: every frame is a `ClientFrame` or a
/// `ServerFrame`, `x-protocol-version` is [`PROTOCOL_VERSION`], and the
/// definitions are sorted by name.
///
/// # Errors
/// Returns the name of a definition that the two directions describe
/// differently, which one shared name cannot hold.
fn protocol_schema() -> Result<Value, String> {
    let mut client = SchemaSettings::draft2020_12()
        .for_deserialize()
        .into_generator();
    let client_frame = client.subschema_for::<ClientFrame>();
    let mut server = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    let server_frame = server.subschema_for::<ServerFrame>();
    let mut definitions: BTreeMap<String, Value> =
        client.take_definitions(true).into_iter().collect();
    for (name, mut definition) in server.take_definitions(true) {
        omit_null_from_skipped_fields(&mut definition);
        if definitions
            .get(&name)
            .is_some_and(|client_definition| client_definition != &definition)
        {
            return Err(format!("`{name}` differs between client and server frames"));
        }
        definitions.insert(name, definition);
    }
    definitions.insert(CAPABILITY_DEFINITION.to_string(), capability_definition());
    let mut definitions: Map<String, Value> = definitions.into_iter().collect();
    for definition in definitions.values_mut() {
        unlink_descriptions(definition);
    }

    Ok(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "RemoteFrame",
        "description": DESCRIPTION.join("\n"),
        "x-protocol-version": PROTOCOL_VERSION,
        "oneOf": [client_frame, server_frame],
        "$defs": definitions
    }))
}

/// Removes `null` from every property a server may skip.
///
/// A serialized `Option` field outside `required` carries
/// `skip_serializing_if = "Option::is_none"`, so the server leaves it out
/// instead of sending `null`; an `Option` field it always sends stays
/// required and nullable. The conformance tests below serialize both kinds.
fn omit_null_from_skipped_fields(schema: &mut Value) {
    if let Some(object) = schema.as_object_mut() {
        let required: Vec<String> = object
            .get("required")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |required| {
                required
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            });
        if let Some(Value::Object(properties)) = object.get_mut("properties") {
            for (name, property) in properties.iter_mut() {
                if !required.contains(name) {
                    drop_null(property);
                }
            }
        }
        for nested in object.values_mut() {
            omit_null_from_skipped_fields(nested);
        }
    } else if let Some(items) = schema.as_array_mut() {
        for item in items {
            omit_null_from_skipped_fields(item);
        }
    }
}

/// Removes `null` from a nullable schema: `"type": ["string", "null"]`
/// becomes `"type": "string"`, and an `anyOf` of one schema and `null`
/// becomes that schema.
fn drop_null(property: &mut Value) {
    let Some(object) = property.as_object_mut() else {
        return;
    };
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|name| name != "null");
        if let [name] = types.as_slice() {
            let name = name.clone();
            object.insert("type".to_string(), name);
        }
    }
    let null = json!({"type": "null"});
    let Some(Value::Array(variants)) = object.get_mut("anyOf") else {
        return;
    };
    variants.retain(|variant| variant != &null);
    if let [Value::Object(variant)] = variants.as_slice() {
        let variant = variant.clone();
        object.remove("anyOf");
        object.extend(variant);
    }
}

/// Describes every capability a host may list, in the order a host lists
/// them, from a host that serves every operation.
fn capability_definition() -> Value {
    let sessions = SessionService::new(Arc::new(RecordingBackend::serving_everything()));
    let names: Vec<Value> = capabilities(&sessions)
        .into_iter()
        .map(|name| json!({"type": "string", "const": name}))
        .collect();

    json!({"description": CAPABILITY_DESCRIPTION.join("\n"), "oneOf": names})
}

/// Turns rustdoc links such as ``[`ServerFrame::Welcome`]`` in every
/// description into code spans, since the schema has no rustdoc to link.
fn unlink_descriptions(schema: &mut Value) {
    match schema {
        Value::Object(object) => {
            for (key, nested) in object.iter_mut() {
                if key == "description"
                    && let Value::String(description) = nested
                {
                    *description = description.replace("[`", "`").replace("`]", "`");
                } else {
                    unlink_descriptions(nested);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                unlink_descriptions(item);
            }
        }
        _ => {}
    }
}

/// Returns the assembled schema, failing the test when it cannot be built.
fn schema() -> Value {
    protocol_schema().expect("the protocol schema should assemble")
}

/// Returns a validator for frames of `definition`.
fn validator(definition: &str) -> Validator {
    let mut schema = schema();
    if let Some(object) = schema.as_object_mut() {
        object.remove("oneOf");
        object.insert("$ref".to_string(), json!(format!("#/$defs/{definition}")));
    }

    Validator::new(&schema).expect("the protocol schema should compile")
}

/// Returns the schema errors of `frame` against `validator`, joined.
fn violations(validator: &Validator, frame: &Value) -> String {
    validator
        .iter_errors(frame)
        .map(|error| format!("{error} at `{}`", error.instance_path()))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Returns the tag values `definition` declares under `tag`, one per
/// variant.
fn declared_tags(schema: &Value, definition: &str, tag: &str) -> Vec<String> {
    schema["$defs"][definition]["oneOf"]
        .as_array()
        .map_or_else(Vec::new, |variants| {
            variants
                .iter()
                .filter_map(|variant| variant["properties"][tag]["const"].as_str())
                .map(str::to_string)
                .collect()
        })
}

/// Returns the sorted, deduplicated `tag` values of `values`.
fn sent_tags<'value>(values: impl IntoIterator<Item = &'value Value>, tag: &str) -> Vec<String> {
    let mut tags: Vec<String> = values
        .into_iter()
        .filter_map(|value| value[tag].as_str())
        .map(str::to_string)
        .collect();
    tags.sort();
    tags.dedup();

    tags
}

/// Returns `tags` sorted.
fn sorted(mut tags: Vec<String>) -> Vec<String> {
    tags.sort();

    tags
}

/// One request of every method, each optional field set.
fn requests_with_every_field() -> Vec<RemoteRequest> {
    let session_id = || "session-1".into();
    let project_id = || Some("1".into());

    vec![
        RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
            action: RemoteGraphAction::ExtendBudget,
            additional_calls: Some(4),
            budget_id: Some("graph".to_string()),
            graph_id: "graph".to_string(),
            message: Some("tighten the tests".to_string()),
            node_id: Some("node".to_string()),
            output_id: Some("output".to_string()),
            session_id: session_id(),
            snapshot: "snapshot".to_string(),
        }),
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "main".to_string(),
                question: "Which branch?".to_string(),
            }],
            graph_question_target: Some("target".to_string()),
            session_id: session_id(),
        },
        RemoteRequest::CancelSession {
            session_id: session_id(),
        },
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Harness,
            project_id: project_id(),
        },
        RemoteRequest::GenerateGraphProposal {
            message: "Split the work".to_string(),
            session_id: session_id(),
        },
        RemoteRequest::GetHost {
            project_id: project_id(),
        },
        RemoteRequest::GetSessionDiff {
            path: Some("src/lib.rs".to_string()),
            session_id: session_id(),
        },
        RemoteRequest::GetSession {
            session_id: session_id(),
        },
        RemoteRequest::GetSessionGraphs {
            graph_id: Some("graph".to_string()),
            known: Some("version".to_string()),
            node_id: Some("node".to_string()),
            session_id: session_id(),
        },
        RemoteRequest::ListProjectFiles {
            project_id: project_id(),
        },
        RemoteRequest::ListProjects,
        RemoteRequest::ListSessionFiles {
            session_id: session_id(),
        },
        RemoteRequest::ListSessions {
            project_id: project_id(),
        },
        RemoteRequest::MergeReviewRequest {
            bypass: true,
            session_id: session_id(),
        },
        RemoteRequest::MergeSession {
            session_id: session_id(),
        },
        RemoteRequest::PublishSession {
            session_id: session_id(),
        },
        RemoteRequest::SendMessage {
            images: vec![RemoteImage {
                data: "iVBORw0KGgo=".to_string(),
            }],
            message: "Look at [Image #1]".to_string(),
            session_id: session_id(),
        },
        RemoteRequest::StopTurn {
            session_id: session_id(),
        },
        RemoteRequest::SyncProject {
            project_id: project_id(),
        },
        RemoteRequest::SyncSession {
            session_id: session_id(),
        },
        RemoteRequest::SetSessionSettings {
            agent: Some("codex".to_string()),
            model: Some("gpt-6".to_string()),
            reasoning: Some("high".to_string()),
            response_style: Some("concise".to_string()),
            session_id: session_id(),
            speed_mode: Some("fast".to_string()),
        },
    ]
}

/// One request of every method that has optional fields, each left out.
fn requests_without_optional_fields() -> Vec<RemoteRequest> {
    let session_id = || "session-1".into();

    vec![
        RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
            action: RemoteGraphAction::Cancel,
            additional_calls: None,
            budget_id: None,
            graph_id: "graph".to_string(),
            message: None,
            node_id: None,
            output_id: None,
            session_id: session_id(),
            snapshot: "snapshot".to_string(),
        }),
        RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            graph_question_target: None,
            session_id: session_id(),
        },
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
            project_id: None,
        },
        RemoteRequest::GetHost { project_id: None },
        RemoteRequest::GetSessionDiff {
            path: None,
            session_id: session_id(),
        },
        RemoteRequest::GetSessionGraphs {
            graph_id: None,
            known: None,
            node_id: None,
            session_id: session_id(),
        },
        RemoteRequest::ListProjectFiles { project_id: None },
        RemoteRequest::ListSessions { project_id: None },
        RemoteRequest::MergeReviewRequest {
            bypass: false,
            session_id: session_id(),
        },
        RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "hi".to_string(),
            session_id: session_id(),
        },
        RemoteRequest::SyncProject { project_id: None },
        RemoteRequest::SetSessionSettings {
            agent: None,
            model: None,
            reasoning: None,
            response_style: None,
            session_id: session_id(),
            speed_mode: None,
        },
    ]
}

/// Every frame a client may send, each request inside a `request` frame.
fn client_frames() -> Vec<ClientFrame> {
    let mut frames = vec![
        ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
        ClientFrame::Pair {
            code: "ABCD-EFGH".to_string(),
            device_name: "Phone".to_string(),
            protocol_version: PROTOCOL_VERSION,
        },
    ];
    let requests = requests_with_every_field()
        .into_iter()
        .chain(requests_without_optional_fields());
    frames.extend(
        (1..)
            .zip(requests)
            .map(|(id, request)| ClientFrame::Request { id, request }),
    );

    frames
}

/// A session snapshot with every optional field the server sends, built
/// from the session fixture.
fn full_session() -> RemoteSession {
    let mut session = session_fixture(&SessionId::from("session-1"));
    session.draft_prompt = Some("staged prompt".to_string());
    session.focused_review = Some(SessionFocusedReview::Ready {
        text: "### Suggestions".to_string(),
    });
    session.graph_question_target = Some("target".to_string());
    session.progress = Some("Working... Reading files".to_string());
    session.published_upstream_ref = Some("origin/wt/session-1".to_string());
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 30,
        summary: review_request_summary(),
    });
    session.settings.is_draft = true;
    let mut remote = RemoteSession::from(session);
    remote.graph = Some(RemoteSessionGraph {
        awaiting: vec![RemoteGraphApproval {
            node_id: "node".to_string(),
            revisable: true,
            text: "Accept the plan".to_string(),
        }],
        done: 1,
        label: "Running 1/2".to_string(),
        phase: RemoteGraphProposalPhase::Approved,
        status: Some(RemoteGraphStatus::Running),
        total: 2,
        version: 1,
    });

    remote
}

/// A session snapshot with only the fields every server sends, as an
/// older server sends it.
fn bare_session() -> RemoteSession {
    RemoteSession {
        agent: None,
        base_branch: None,
        branch: None,
        created_at: None,
        draft_prompt: None,
        focused_review: None,
        graph: None,
        graph_question_target: None,
        graph_questions: Vec::new(),
        id: "session-1".to_string(),
        is_draft: false,
        messages: Vec::new(),
        model: None,
        model_id: None,
        progress: None,
        published_upstream_ref: None,
        questions: vec![RemoteQuestion {
            options: Vec::new(),
            text: "Which branch?".to_string(),
        }],
        queued_actions: Vec::new(),
        queued_messages: Vec::new(),
        reasoning: None,
        response_style: None,
        review_request: None,
        speed_mode: None,
        status: SessionStatus::Review.to_string(),
        title: None,
        updated_at: 1,
    }
}

/// An open GitHub pull request with a forge status.
fn review_request_summary() -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: "#12".to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session-1".to_string(),
        state: ReviewRequestState::Open,
        status_summary: Some("PR ready".to_string()),
        target_branch: "main".to_string(),
        title: "Build it".to_string(),
        web_url: "https://example.com/pull/12".to_string(),
    }
}

/// The responses about one session, each optional field present in one and
/// absent in another, converted from the host fixtures.
fn session_responses() -> Vec<RemoteResponse> {
    let files = [
        diff_file(
            "src/lib.rs",
            SessionDiffFileStatus::Modified,
            "@@ -1 +1 @@\n",
        ),
        diff_file(
            "src/new.rs",
            SessionDiffFileStatus::Renamed,
            "@@ -0,0 +1 @@\n",
        ),
    ];
    let graphs = RemoteResponse::from_graphs(ag_session::test_support::session_graphs("session-1"));
    let version = match &graphs {
        RemoteResponse::Graphs { version, .. } => version.clone(),
        _ => String::new(),
    };
    let mut no_review_request = review_request_summary();
    no_review_request.status_summary = None;

    vec![
        RemoteResponse::Accepted,
        RemoteResponse::Created {
            session_id: "session-1".to_string(),
        },
        RemoteResponse::from_diff(&files, None).expect("the file list should build"),
        RemoteResponse::from_diff(&files, Some("src/new.rs")).expect("the patch should build"),
        graphs.clone().unchanged_since(Some(&version)),
        graphs,
        RemoteResponse::from_graphs(SessionGraphs {
            graph: None,
            graphs: Vec::new(),
            proposal: None,
        }),
        RemoteResponse::Session {
            session: Some(Box::new(full_session())),
        },
        RemoteResponse::Session {
            session: Some(Box::new(bare_session())),
        },
        RemoteResponse::Session { session: None },
        RemoteResponse::from_session_files(&[file_entry(true, "src"), file_entry(false, "a.rs")]),
        RemoteResponse::ReviewRequest {
            review_request: RemoteReviewRequest::from(no_review_request),
        },
        RemoteResponse::from(ReviewRequestMerge {
            outcome: ReviewRequestMergeOutcome::Merged {
                merge_commit: Some("7a1c0de".to_string()),
            },
            review_request: Some(review_request_summary()),
        }),
        RemoteResponse::from(ReviewRequestMerge {
            outcome: ReviewRequestMergeOutcome::Refused {
                reason: "Pull request #12 is already merged".to_string(),
            },
            review_request: None,
        }),
    ]
}

/// The responses about a project and its sessions, each optional field
/// present in one and absent in another, converted from the host fixtures.
fn project_responses() -> Vec<RemoteResponse> {
    let unlisted = SessionListEntry {
        stats: None,
        summary: SessionSummary {
            id: SessionId::from("session-2"),
            status: SessionStatus::InProgress,
            title: None,
            updated_at: 6,
        },
    };

    vec![
        RemoteResponse::from(host_fixture()),
        RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "GPT".to_string(),
            mode_models: Vec::new(),
            models: Vec::new(),
            project: "agentty".to_string(),
            project_id: None,
            session_modes: Vec::new(),
        },
        RemoteResponse::Projects {
            projects: vec![
                RemoteProjectEntry::from(HostProject {
                    active_session_count: 2,
                    id: 1,
                    is_active: true,
                    name: "agentty".to_string(),
                }),
                RemoteProjectEntry::from(HostProject {
                    active_session_count: 0,
                    id: 2,
                    is_active: false,
                    name: "other".to_string(),
                }),
            ],
        },
        RemoteResponse::Sessions {
            project: Some(RemoteProject::listed(project_status_fixture(), Some(1))),
            sessions: vec![
                RemoteSessionSummary::from(entry_with_stats(Some("Mergeable, PR ready"))),
                RemoteSessionSummary::from(entry_with_stats(None)),
                RemoteSessionSummary::from(unlisted),
            ],
        },
        RemoteResponse::Sessions {
            project: Some(RemoteProject::from(project_with_sync(
                HostProjectSyncPhase::Complete {
                    pulled_commits: Some(2),
                    pushed_commits: Some(1),
                    resolved_conflict_count: 1,
                },
            ))),
            sessions: Vec::new(),
        },
        RemoteResponse::Sessions {
            project: Some(RemoteProject::from(project_with_sync(
                HostProjectSyncPhase::Failed {
                    message: "Authentication failed".to_string(),
                },
            ))),
            sessions: Vec::new(),
        },
        RemoteResponse::Sessions {
            project: None,
            sessions: Vec::new(),
        },
    ]
}

/// Every frame a server sends, each response inside a `response` frame.
fn server_frames() -> Vec<ServerFrame> {
    let sessions = SessionService::new(Arc::new(RecordingBackend::serving_everything()));
    let mut frames = vec![
        ServerFrame::AwaitingApproval {
            confirmation: "K7QX".to_string(),
        },
        ServerFrame::Changed { revision: 8 },
        ServerFrame::Paired {
            capabilities: capabilities(&sessions),
            device_id: "device".to_string(),
            protocol_version: PROTOCOL_VERSION,
            revision: 7,
        },
        ServerFrame::Rejected {
            reason: "Pairing was denied".to_string(),
        },
        ServerFrame::Welcome {
            capabilities: Vec::new(),
            device_id: "device".to_string(),
            protocol_version: PROTOCOL_VERSION,
            revision: 7,
        },
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Error("Session not found".to_string()),
        },
    ];
    let responses = session_responses().into_iter().chain(project_responses());
    frames.extend(
        (2..)
            .zip(responses)
            .map(|(id, response)| ServerFrame::Response {
                id,
                result: RemoteResult::Ok(response),
            }),
    );

    frames
}

#[test]
fn committed_schema_and_typescript_match_the_wire_types() {
    // Arrange
    let schema = schema();

    // Act
    let schema_text = format!(
        "{}\n",
        serde_json::to_string_pretty(&schema).expect("the schema should encode")
    );
    let typescript = render_module(&schema, REGENERATE).expect("the schema should render");

    // Assert
    if std::env::var(UPDATE_VARIABLE).is_ok_and(|update| update == "1") {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        std::fs::write(
            format!("{crate_root}/schema/remote_protocol.json"),
            schema_text,
        )
        .expect("the schema should be written");
        std::fs::write(
            format!("{crate_root}/schema/remote_protocol.ts"),
            typescript,
        )
        .expect("the declarations should be written");

        return;
    }
    assert!(
        schema_text == include_str!("../../schema/remote_protocol.json"),
        "{SCHEMA_PATH} is stale: the protocol types changed. Regenerate it with `{REGENERATE}` \
         and review the diff."
    );
    assert!(
        typescript == include_str!("../../schema/remote_protocol.ts"),
        "{TYPESCRIPT_PATH} is stale: the protocol types changed. Regenerate it with \
         `{REGENERATE}` and review the diff."
    );
}

#[test]
fn schema_carries_the_protocol_version_and_every_capability() {
    // Arrange
    let sessions = SessionService::new(Arc::new(RecordingBackend::serving_everything()));

    // Act
    let schema = schema();

    // Assert
    assert_eq!(schema["x-protocol-version"], json!(PROTOCOL_VERSION));
    let named: Vec<&str> = schema["$defs"][CAPABILITY_DEFINITION]["oneOf"]
        .as_array()
        .expect("capabilities should be listed")
        .iter()
        .filter_map(|capability| capability["const"].as_str())
        .collect();
    assert_eq!(named, capabilities(&sessions));
    assert!(named.contains(&"settings_agent"));
}

#[test]
fn every_client_frame_matches_the_schema_and_parses_back() {
    // Arrange
    let validator = validator("ClientFrame");

    // Act
    let encoded: Vec<Value> = client_frames()
        .iter()
        .map(|frame| serde_json::to_value(frame).expect("the frame should encode"))
        .collect();

    // Assert
    for (frame, value) in client_frames().into_iter().zip(&encoded) {
        assert_eq!(violations(&validator, value), "", "{value}");
        let parsed: ClientFrame =
            serde_json::from_value(value.clone()).expect("the frame should parse");
        assert_eq!(parsed, frame);
    }
    let schema = schema();
    let requests = encoded.iter().map(|frame| &frame["request"]);
    assert_eq!(
        sent_tags(requests, "method"),
        sorted(declared_tags(&schema, "RemoteRequest", "method"))
    );
    assert_eq!(
        sent_tags(&encoded, "type"),
        sorted(declared_tags(&schema, "ClientFrame", "type"))
    );
}

#[test]
fn every_server_frame_matches_the_schema() {
    // Arrange
    let validator = validator("ServerFrame");

    // Act
    let encoded: Vec<Value> = server_frames()
        .iter()
        .map(|frame| serde_json::to_value(frame).expect("the frame should encode"))
        .collect();

    // Assert
    for value in &encoded {
        assert_eq!(violations(&validator, value), "", "{value}");
    }
    let schema = schema();
    let responses = encoded.iter().map(|frame| &frame["result"]["ok"]);
    assert_eq!(
        sent_tags(responses, "kind"),
        sorted(declared_tags(&schema, "RemoteResponse", "kind"))
    );
    assert_eq!(
        sent_tags(&encoded, "type"),
        sorted(declared_tags(&schema, "ServerFrame", "type"))
    );
}

#[test]
fn server_frames_leave_out_what_they_skip_and_send_null_only_where_required() {
    // Arrange
    let validator = validator("ServerFrame");
    let snapshot = |session: Option<RemoteSession>| {
        serde_json::to_value(ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(RemoteResponse::Session {
                session: session.map(Box::new),
            }),
        })
        .expect("the frame should encode")
    };
    let mut skipped_as_null = snapshot(Some(bare_session()));
    skipped_as_null["result"]["ok"]["session"]["agent"] = Value::Null;
    let mut required_left_out = snapshot(Some(bare_session()));
    if let Some(session) = required_left_out["result"]["ok"]["session"].as_object_mut() {
        session.remove("title");
    }

    // Act
    let missing = snapshot(None);
    let untitled = snapshot(Some(bare_session()));

    // Assert
    assert_eq!(missing["result"]["ok"]["session"], Value::Null);
    assert_eq!(untitled["result"]["ok"]["session"]["title"], Value::Null);
    assert!(untitled["result"]["ok"]["session"].get("agent").is_none());
    assert!(validator.is_valid(&missing));
    assert!(validator.is_valid(&untitled));
    assert!(!validator.is_valid(&skipped_as_null));
    assert!(!validator.is_valid(&required_left_out));
}

#[test]
fn client_frames_may_leave_out_defaulted_fields_but_not_required_ones() {
    // Arrange
    let validator = validator("ClientFrame");
    let defaulted = json!({"type": "request", "id": 1, "request": {"method": "create_session"}});
    let nulled = json!({"type": "request", "id": 2, "request": {
        "method": "list_sessions", "project_id": null
    }});
    let missing = json!({"type": "request", "id": 3, "request": {"method": "get_session"}});

    // Act
    let parsed_defaulted = serde_json::from_value::<ClientFrame>(defaulted.clone());
    let parsed_nulled = serde_json::from_value::<ClientFrame>(nulled.clone());
    let parsed_missing = serde_json::from_value::<ClientFrame>(missing.clone());

    // Assert
    assert!(validator.is_valid(&defaulted));
    assert_eq!(
        parsed_defaulted.expect("a request without a mode should parse"),
        ClientFrame::Request {
            id: 1,
            request: RemoteRequest::CreateSession {
                mode: RemoteSessionMode::Regular,
                project_id: None,
            },
        }
    );
    assert!(validator.is_valid(&nulled));
    assert!(parsed_nulled.is_ok());
    assert!(!validator.is_valid(&missing));
    assert!(parsed_missing.is_err());
}

#[test]
fn unknown_requests_and_actions_parse_but_stay_out_of_the_schema() {
    // Arrange
    let schema = schema();
    let validator = validator("ClientFrame");
    let unknown = json!({"type": "request", "id": 1, "request": {"method": "rename_session"}});
    let actions: Vec<&str> = schema["$defs"]["RemoteGraphAction"]["oneOf"]
        .as_array()
        .expect("graph actions should be listed")
        .iter()
        .filter_map(|action| action["const"].as_str())
        .collect();

    // Act
    let parsed = serde_json::from_value::<ClientFrame>(unknown.clone());
    let parsed_action = serde_json::from_value::<RemoteGraphAction>(json!("rewind"));

    // Assert: a server reads an unknown method or action as `Unsupported`,
    // which no client sends, so the schema lists only the ones it serves.
    assert_eq!(
        parsed.expect("an unknown method should parse"),
        ClientFrame::Request {
            id: 1,
            request: RemoteRequest::Unsupported,
        }
    );
    assert!(!validator.is_valid(&unknown));
    assert!(
        !declared_tags(&schema, "RemoteRequest", "method")
            .iter()
            .any(|method| method == "unsupported")
    );
    assert_eq!(
        parsed_action.expect("an unknown action should parse"),
        RemoteGraphAction::Unsupported
    );
    assert_eq!(
        actions,
        [
            "accept",
            "cancel",
            "extend_budget",
            "reply",
            "resume",
            "retry"
        ]
    );
}
