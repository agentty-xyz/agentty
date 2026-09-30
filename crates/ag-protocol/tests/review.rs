//! Public focused-review evidence and saved-review compatibility contract.

use ag_protocol::{
    FocusedReview, ProtocolRequestProfile, SchemaRequiredPolicy, parse_protocol_response_strict,
    protocol_output_schema,
};

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
