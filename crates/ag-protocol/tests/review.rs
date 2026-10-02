//! Public focused-review evidence and saved-review compatibility contract.

use ag_protocol::{
    FocusedReview, FocusedReviewDecision, ProtocolRequestProfile, SchemaRequiredPolicy,
    parse_protocol_response_strict, protocol_output_schema,
};

#[test]
fn consolidation_decisions_round_trip_and_account_for_retention_merges_and_rejection() {
    // Arrange
    let raw = serde_json::json!({
        "project_impact": [],
        "suggestions": [{"details":"Validated risk", "severity":"high"}],
        "candidate_decisions": [
            {"candidate_index":0, "suggestion_index":0, "reason":"Verified immediate path"},
            {"candidate_index":1, "suggestion_index":0, "reason":"Same trigger and correction"},
            {"candidate_index":2, "suggestion_index":null, "reason":"Existing guard rejects input"}
        ]
    });

    // Act
    let response =
        parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::FocusedReview)
            .expect("consolidation response");
    let review: FocusedReview = serde_json::from_str(&response.answer).expect("review");
    let schema = protocol_output_schema(
        ProtocolRequestProfile::FocusedReview,
        SchemaRequiredPolicy::AllProperties,
    );

    // Assert
    review
        .validate_candidate_decisions(3)
        .expect("complete accounting");
    assert_eq!(serde_json::to_value(&review).expect("round trip"), raw);
    assert!(
        schema["required"]
            .as_array()
            .expect("required")
            .contains(&serde_json::json!("candidate_decisions"))
    );
    assert_eq!(
        schema["$defs"]["FocusedReviewDecision"]["additionalProperties"],
        false
    );
    assert!(
        schema["$defs"]["FocusedReviewDecision"]["required"]
            .as_array()
            .expect("decision fields")
            .contains(&serde_json::json!("suggestion_index"))
    );
}

#[test]
fn consolidation_requires_every_output_to_reference_an_input_candidate() {
    // Arrange
    let mut review: FocusedReview = serde_json::from_value(serde_json::json!({
        "project_impact": [],
        "suggestions": [
            {"details": "Retained risk", "severity": "high"},
            {"details": "Unlinked risk", "severity": "medium"}
        ],
        "candidate_decisions": [
            {"candidate_index":0, "suggestion_index":null, "reason":"Rejected input"}
        ]
    }))
    .expect("review");

    // Act / Assert: rejecting inputs or retaining only one output is
    // incomplete.
    assert!(review.validate_candidate_decisions(1).is_err());
    review.candidate_decisions[0].suggestion_index = Some(0);
    assert!(review.validate_candidate_decisions(1).is_err());
    review.candidate_decisions.push(FocusedReviewDecision {
        candidate_index: 1,
        reason: "Second candidate supports the second output".into(),
        suggestion_index: Some(1),
    });
    review
        .validate_candidate_decisions(2)
        .expect("linked outputs");

    // Act / Assert: no inputs cannot account for a new output.
    review.candidate_decisions.clear();
    assert!(review.validate_candidate_decisions(0).is_err());
}

#[test]
fn consolidation_rejects_incomplete_or_invalid_candidate_accounting() {
    // Arrange
    let decisions = [
        serde_json::json!([]),
        serde_json::json!([{"candidate_index":1,"suggestion_index":null,"reason":"Unknown input"}]),
        serde_json::json!([{"candidate_index":0,"suggestion_index":null,"reason":" "}]),
        serde_json::json!([{"candidate_index":0,"suggestion_index":1,"reason":"Unknown output"}]),
        serde_json::json!([
            {"candidate_index":0,"suggestion_index":null,"reason":"One rejection"},
            {"candidate_index":0,"suggestion_index":null,"reason":"Repeated decision"}
        ]),
    ];

    // Act / Assert
    for candidate_decisions in decisions {
        let review: FocusedReview = serde_json::from_value(serde_json::json!({"project_impact":[],"suggestions":[],"candidate_decisions":candidate_decisions})).expect("typed response");
        assert!(review.validate_candidate_decisions(1).is_err());
    }
    let legacy: FocusedReview =
        serde_json::from_str(r#"{"project_impact":[],"suggestions":[]}"#).expect("legacy review");
    assert_eq!(
        legacy.candidate_decisions,
        Vec::<FocusedReviewDecision>::new()
    );
    legacy
        .validate_candidate_decisions(0)
        .expect("no input candidates");
}

#[test]
fn consolidation_rejects_malformed_dispositions_instead_of_defaulting_to_rejection() {
    // Arrange
    let mut decisions = vec![
        r#"{"candidate_index":0,"reason":"Verified source"}"#.to_string(),
        r#"{"candidate_index":0,"suggestion_index":null}"#.to_string(),
        r#"{"reason":"Verified source","suggestion_index":null}"#.to_string(),
        r#"{"candidate_index":0,"reason":"Verified source","suggestion_index":null,"extra":0}"#.to_string(),
        r#"{"candidate_index":0,"reason":"Verified source","suggestion_index":null,"suggestion_index":0}"#.to_string(),
    ];
    for value in ["-1", "1.5", "false", r#""0""#, "[]", "{}"] {
        decisions.push(format!(
            r#"{{"candidate_index":0,"reason":"Verified source","suggestion_index":{value}}}"#
        ));
    }
    decisions.push(format!(
        r#"{{"candidate_index":0,"reason":"Verified source","suggestion_index":{}}}"#,
        usize::MAX as u128 + 1,
    ));

    // Act / Assert
    for decision in decisions {
        let raw = format!(
            r#"{{"project_impact":[],"suggestions":[],"candidate_decisions":[{decision}]}}"#
        );
        assert!(
            parse_protocol_response_strict(&raw, ProtocolRequestProfile::FocusedReview).is_err(),
            "Invalid decision must not become a rejection: {decision}"
        );
    }
}

#[test]
fn consolidation_requires_an_explicit_nullable_disposition_in_every_schema_and_parser() {
    // Arrange
    let mut raw = serde_json::json!({
        "project_impact": [], "suggestions": [],
        "candidate_decisions": [{"candidate_index":0, "reason":"Verified source"}]
    });

    // Act / Assert
    assert!(serde_json::from_value::<FocusedReview>(raw.clone()).is_err());
    assert!(
        parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::FocusedReview)
            .is_err()
    );
    for policy in [
        SchemaRequiredPolicy::AllProperties,
        SchemaRequiredPolicy::MinimumProtocolKeys,
    ] {
        let schema = protocol_output_schema(ProtocolRequestProfile::FocusedReview, policy);
        let decision = &schema["$defs"]["FocusedReviewDecision"];
        assert!(
            decision["required"]
                .as_array()
                .expect("required fields")
                .contains(&serde_json::json!("suggestion_index"))
        );
        assert_eq!(
            decision["properties"]["suggestion_index"]["type"],
            serde_json::json!(["integer", "null"])
        );
    }
    let prompt_schema: serde_json::Value =
        serde_json::from_str(&ag_protocol::focused_review_json_schema_json())
            .expect("prompt schema");
    assert!(
        prompt_schema["$defs"]["FocusedReviewDecision"]["required"]
            .as_array()
            .expect("prompt required fields")
            .contains(&serde_json::json!("suggestion_index"))
    );
    raw["candidate_decisions"][0]["suggestion_index"] = serde_json::Value::Null;
    let response =
        parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::FocusedReview)
            .expect("explicit rejection");
    let review: FocusedReview = serde_json::from_str(&response.answer).expect("review");
    review
        .validate_candidate_decisions(1)
        .expect("explicit accounting");
    assert_eq!(review.candidate_decisions[0].suggestion_index, None);
}

#[test]
fn public_review_contract_accepts_typed_evidence_and_preserves_saved_location_free_findings() {
    // Arrange
    let raw = serde_json::json!({
        "project_impact": [],
        "suggestions": [{
            "details": "src/main.rs:999: A concrete risk; src/main.rs:80 and caller.rs:12 explain the trigger", "severity": "high",
            "evidence": {
                "correction": "Restore validation", "end_line": 999,
                "existing_code": "run();", "impact": "Invalid requests execute",
                "path": "src/main.rs", "side": "new", "start_line": 999,
                "trigger": "An invalid request"
            }
        }]
    });

    // Act
    let response =
        parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::FocusedReview)
            .expect("typed review");
    let mut review: FocusedReview =
        serde_json::from_str(&response.answer).expect("review contract");
    review.suggestions[0].resolve_evidence_range(8, 8);
    let schema = protocol_output_schema(
        ProtocolRequestProfile::FocusedReview,
        SchemaRequiredPolicy::AllProperties,
    );

    // Assert
    assert_eq!(
        review.suggestions[0]
            .evidence
            .as_ref()
            .expect("source evidence")
            .path,
        "src/main.rs"
    );
    assert!(
        schema["$defs"]["FocusedReviewSuggestion"]["required"]
            .as_array()
            .expect("required fields")
            .contains(&serde_json::json!("evidence"))
    );
    let legacy: FocusedReview = serde_json::from_str(
        r#"{"project_impact":[],"suggestions":[{"details":"Saved finding","severity":"high"}]}"#,
    )
    .expect("saved review");
    assert!(legacy.suggestions[0].evidence.is_none());
    let markdown = review.to_markdown();
    assert!(markdown.contains("src/main.rs:8: A concrete risk"));
    assert!(markdown.contains("caller.rs:12"));
    assert!(markdown.contains("src/main.rs:80"));
    assert!(markdown.contains("Supporting references are unverified."));
    assert!(!markdown.contains("999"));
}

#[test]
fn missing_and_null_evidence_render_individual_unanchored_labels() {
    // Arrange
    let raw = serde_json::json!({"project_impact": [], "suggestions": [
        {"details": "src/file.rs:999: Unverified source claim", "severity": "high", "evidence": null},
        {"details": "caller.rs:80: Saved source claim", "severity": "medium"}
    ]});

    // Act
    let response =
        parse_protocol_response_strict(&raw.to_string(), ProtocolRequestProfile::FocusedReview)
            .expect("location-free review");
    let review: FocusedReview = serde_json::from_str(&response.answer).expect("review contract");
    let markdown = review.to_markdown();

    // Assert
    assert!(markdown.contains("- [High] (unanchored): src/file.rs:999: Unverified source claim"));
    assert!(markdown.contains("- [Medium] (unanchored): caller.rs:80: Saved source claim"));
}
