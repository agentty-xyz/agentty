//! Public graph-proposal wire, schema, and instruction contracts.

use std::path::Path;

use ag_protocol::{
    GraphProposalOutcome, GraphProposalResponse, ProtocolRequestProfile,
    ProtocolSchemaInstructionMode, QuestionItem, ReviewCommentOutcome, SchemaRequiredPolicy,
    parse_protocol_response_strict, prepend_protocol_instructions,
    prepend_protocol_refresh_reminder, protocol_output_schema,
};
use serde_json::{Value, json};

/// Complete logical graph exercising every gate and execution requirement.
fn proposal_response() -> Value {
    let mut nodes = Vec::new();
    for (id, context, post_processing, profile, access) in [
        (
            "plan",
            "task_conversation",
            "none",
            "SessionTurn",
            "shared_read",
        ),
        (
            "review",
            "fresh",
            "none",
            "FocusedReview",
            "background_read",
        ),
        (
            "apply",
            "task_conversation",
            "session_workflow",
            "SessionTurn",
            "exclusive_write",
        ),
    ] {
        nodes.push(json!({
            "agent_id": id,
            "dependencies": [],
            "execution": {
                "context": context,
                "post_processing": post_processing,
                "response_profile": profile,
                "workspace_access": access
            },
            "id": id,
            "inputs": [],
            "objective": format!("Perform {id}"),
            "outputs": [{"name": "result", "schema": profile}]
        }));
    }
    nodes[1]["dependencies"] = json!([
        {"node_id": "plan", "gate": {"kind": "execution_succeeded"}},
        {"node_id": "plan", "gate": {"kind": "result_validated", "output_name": "result"}}
    ]);
    nodes[1]["inputs"] = json!([
        {"name": "plan", "producer_node_id": "plan", "output_name": "result"}
    ]);
    nodes[2]["dependencies"] = json!([
        {"node_id": "plan", "gate": {"kind": "result_accepted", "output_name": "result"}},
        {"node_id": "review", "gate": {"kind": "result_validated", "output_name": "result"}}
    ]);
    nodes[2]["inputs"] = json!([
        {"name": "accepted_plan", "producer_node_id": "plan", "output_name": "result"},
        {"name": "review", "producer_node_id": "review", "output_name": "result"}
    ]);

    json!({"outcome": {"kind": "proposal", "proposal": {"nodes": nodes}}})
}

#[test]
fn proposal_outcomes_round_trip_without_entering_session_question_state() {
    // Arrange
    let clarification = json!({"outcome": {"kind": "clarification", "questions": [
        {"text": "Which component?", "options": ["Protocol", "Storage"]}
    ]}});

    for raw in [proposal_response(), clarification] {
        // Act
        let response =
            parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::GraphProposal)
                .expect("complete response");
        let typed: GraphProposalResponse =
            serde_json::from_str(&response.answer).expect("typed response preserved");

        // Assert
        assert_eq!(serde_json::to_value(&typed).expect("round trip"), raw);
        assert_eq!(response.questions, Vec::<QuestionItem>::new());
        assert_eq!(
            response.review_comment_outcomes,
            Vec::<ReviewCommentOutcome>::new()
        );
        match typed.outcome {
            GraphProposalOutcome::Proposal { proposal } => {
                assert_eq!(proposal.nodes.len(), 3);
                assert_eq!(proposal.nodes[2].inputs[0].name, "accepted_plan");
            }
            GraphProposalOutcome::Clarification { questions } => {
                assert_eq!(questions[0].text, "Which component?");
            }
        }
    }
}

#[test]
fn proposal_parser_rejects_incomplete_mixed_or_host_authored_fields() {
    // Arrange
    let mut invalid = vec![
        json!({}),
        json!({"answer": "a graph"}),
        json!({"outcome": {"kind": "proposal"}}),
        json!({"outcome": {"kind": "proposal", "proposal": {"nodes": []}}}),
        json!({"outcome": {"kind": "clarification", "questions": []}}),
        json!({"outcome": {"kind": "clarification", "questions": [{"text": " "}]}}),
        json!({"outcome": {"kind": "clarification", "questions": vec![json!({"text": "More?"}); 6]}}),
        json!({"outcome": {"kind": "clarification", "questions": [{"text": "Which?"}], "proposal": {"nodes": []}}}),
    ];
    for (pointer, value) in [
        ("/outcome/proposal/nodes/0/id", json!(null)),
        (
            "/outcome/proposal/nodes/0/execution/context",
            json!("unknown"),
        ),
        (
            "/outcome/proposal/nodes/0/execution/response_profile",
            json!("unknown"),
        ),
        (
            "/outcome/proposal/nodes/1/dependencies/1/gate",
            json!({"kind": "result_validated"}),
        ),
    ] {
        let mut raw = proposal_response();
        *raw.pointer_mut(pointer).expect("fixture path") = value;
        invalid.push(raw);
    }
    for (pointer, field) in [
        ("", "approval_receipt"),
        ("/outcome", "questions"),
        ("/outcome/proposal", "graph_id"),
        ("/outcome/proposal/nodes/0", "definition"),
        ("/outcome/proposal/nodes/0", "env_id"),
        ("/outcome/proposal/nodes/0/execution", "task_id"),
        ("/outcome/proposal/nodes/0/execution", "model"),
        ("/outcome/proposal/nodes/0/execution", "shared_budget_id"),
        ("/outcome/proposal/nodes/0/outputs/0", "state"),
        ("/outcome/proposal/nodes/1/inputs/0", "result_id"),
        ("/outcome/proposal/nodes/1/dependencies/0", "evidence"),
        ("/outcome/proposal/nodes/1/dependencies/0/gate", "accepted"),
    ] {
        let mut raw = proposal_response();
        raw.pointer_mut(pointer)
            .expect("fixture path")
            .as_object_mut()
            .expect("object")
            .insert(field.to_string(), json!("host-owned"));
        invalid.push(raw);
    }

    // Act / Assert
    for raw in invalid {
        assert!(
            parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::GraphProposal)
                .is_err(),
            "must reject {raw}"
        );
    }
    for raw in ["", "not JSON", "```json\n{}\n```"] {
        assert!(
            parse_protocol_response_strict(raw, ProtocolRequestProfile::GraphProposal).is_err()
        );
    }
}

#[test]
fn proposal_schemas_enforce_exclusive_outcomes_for_both_transport_policies() {
    // Arrange
    let proposal = proposal_response();
    let clarification = json!({"outcome": {"kind": "clarification", "questions": [
        {"text": "Which?", "options": []}
    ]}});

    for policy in [
        SchemaRequiredPolicy::AllProperties,
        SchemaRequiredPolicy::MinimumProtocolKeys,
    ] {
        // Act
        let schema = protocol_output_schema(ProtocolRequestProfile::GraphProposal, policy);
        let validator = jsonschema::validator_for(&schema).expect("valid schema");

        // Assert
        assert_eq!(schema["type"], "object");
        assert!(!schema.to_string().contains("\"oneOf\""));
        assert!(validator.is_valid(&proposal));
        assert!(validator.is_valid(&clarification));
        for raw in [
            json!({}),
            json!({"outcome": {"kind": "clarification", "questions": []}}),
            json!({"outcome": {"kind": "proposal", "proposal": {"nodes": []}}}),
            json!({"outcome": {"kind": "clarification", "questions": vec![json!({"text": "Which?", "options": []}); 6]}}),
            json!({"outcome": {"kind": "proposal", "proposal": proposal["outcome"]["proposal"], "questions": []}}),
        ] {
            assert!(!validator.is_valid(&raw), "schema must reject {raw}");
        }
        let mut host_field = proposal.clone();
        host_field["outcome"]["proposal"]["nodes"][0]["execution"]["task_id"] = json!("host");
        assert!(!validator.is_valid(&host_field));
    }
}

#[test]
fn proposal_instructions_preserve_the_contract_during_bootstrap_and_refresh() {
    // Arrange
    let root = Path::new("workspace");
    let profile = ProtocolRequestProfile::GraphProposal;

    for mode in [
        ProtocolSchemaInstructionMode::PromptSchema,
        ProtocolSchemaInstructionMode::TransportSchema,
    ] {
        // Act
        let bootstrap = prepend_protocol_instructions("Generate", profile, mode, root);
        let refresh = prepend_protocol_refresh_reminder("Continue", profile, root);

        // Assert
        for rendered in [&bootstrap, &refresh] {
            let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(normalized.contains("kind: \"clarification\""));
            assert!(normalized.contains("kind: \"proposal\""));
            assert!(normalized.contains("do not emit them"));
            assert!(normalized.contains("does not authorize execution"));
        }
        if mode == ProtocolSchemaInstructionMode::PromptSchema {
            let schema_text = bootstrap
                .split_once("Authoritative JSON Schema: ")
                .expect("prompt schema")
                .1
                .split_once(
                    "______________________________________________________________________",
                )
                .expect("prompt boundary")
                .0
                .trim();
            let schema: Value = serde_json::from_str(schema_text).expect("schema JSON");
            assert!(
                jsonschema::validator_for(&schema)
                    .expect("prompt schema")
                    .is_valid(&proposal_response())
            );
        } else {
            assert!(!bootstrap.contains("Authoritative JSON Schema:"));
        }
    }
}
