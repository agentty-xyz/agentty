use crate::review::{
    FocusedReview, FocusedReviewDecision, FocusedReviewEvidence, FocusedReviewSeverity,
    FocusedReviewSide, FocusedReviewSuggestion,
};

#[test]
fn decision_deserialization_preserves_explicit_rejection_and_retention() {
    // Arrange
    for suggestion_index in [None, Some(0), Some(7), Some(usize::MAX)] {
        let expected = FocusedReviewDecision {
            candidate_index: 3,
            reason: "Source verifies the disposition".into(),
            suggestion_index,
        };
        let raw = serde_json::json!({
            "candidate_index": 3,
            "reason": "Source verifies the disposition",
            "suggestion_index": suggestion_index,
        });

        // Act
        let decision: FocusedReviewDecision =
            serde_json::from_value(raw.clone()).expect("explicit disposition");
        let serialized = serde_json::to_value(&decision).expect("serialized decision");

        // Assert
        assert_eq!(decision, expected);
        assert_eq!(serialized, raw);
    }
}

#[test]
fn focused_review_formats_structured_fields_as_markdown() {
    // Arrange
    let review = FocusedReview {
        candidate_decisions: Vec::new(),
        project_impact: vec!["Improves review reliability.".to_string()],
        suggestions: vec![
            FocusedReviewSuggestion {
                evidence: None,
                details: "Fix the stale cache check.".to_string(),
                severity: FocusedReviewSeverity::High,
            },
            FocusedReviewSuggestion {
                evidence: None,
                details: "Deduplicate the parsing path.".to_string(),
                severity: FocusedReviewSeverity::Medium,
            },
        ],
    };

    // Act
    let markdown = review.to_markdown();

    // Assert
    assert_eq!(
        markdown,
        "## Review\n\n### Project Impact\n\n- Improves review reliability.\n\n### \
         Suggestions\n\n- [High] (unanchored): Fix the stale cache check.\n- [Medium] \
         (unanchored): Deduplicate the parsing path."
    );
}

#[test]
fn focused_review_formats_empty_arrays_with_none_sentinels() {
    // Arrange
    let review = FocusedReview {
        candidate_decisions: Vec::new(),
        project_impact: Vec::new(),
        suggestions: Vec::new(),
    };

    // Act
    let markdown = review.to_markdown();

    // Assert
    assert_eq!(
        markdown,
        "## Review\n\n### Project Impact\n\n- None\n\n### Suggestions\n\n- None"
    );
}

#[test]
fn focused_review_severity_serializes_as_lowercase() {
    // Arrange
    let severity = FocusedReviewSeverity::High;

    // Act
    let serialized = serde_json::to_string(&severity).expect("severity should serialize");
    let deserialized = serde_json::from_str::<FocusedReviewSeverity>(&serialized)
        .expect("severity should deserialize");

    // Assert
    assert_eq!(serialized, "\"high\"");
    assert_eq!(deserialized, severity);
}

#[test]
fn typed_evidence_round_trips_and_renders_resolved_or_unanchored_locations() {
    // Arrange
    for (side, line, expected) in [
        (FocusedReviewSide::New, 7, "`src/main.rs:7`"),
        (FocusedReviewSide::Old, 7, "`src/main.rs:7` (before change)"),
        (FocusedReviewSide::New, 0, "`src/main.rs` (unanchored)"),
    ] {
        let mut review = FocusedReview {
            candidate_decisions: Vec::new(),
            project_impact: Vec::new(),
            suggestions: vec![FocusedReviewSuggestion {
                details: "src/main.rs:999:12: Validation is bypassed.".into(),
                evidence: Some(FocusedReviewEvidence {
                    correction: "Restore the check.".into(),
                    end_line: 999,
                    existing_code: "run();".into(),
                    impact: "Invalid requests execute.".into(),
                    path: "src/main.rs".into(),
                    side,
                    start_line: 999,
                    trigger: "An invalid request.".into(),
                }),
                severity: FocusedReviewSeverity::High,
            }],
        };

        // Act
        review.suggestions[0].resolve_evidence_range(line, line);
        let encoded = serde_json::to_string(&review).expect("serialize evidence");
        let decoded: FocusedReview = serde_json::from_str(&encoded).expect("parse evidence");

        // Assert
        assert_eq!(decoded, review);
        let markdown = decoded.to_markdown();
        assert!(markdown.contains(expected));
        assert!(!markdown.contains("999"));
        assert!(markdown.contains("Trigger: An invalid request."));
        assert!(markdown.contains("Impact: Invalid requests execute."));
        assert!(markdown.contains("Correction: Restore the check."));
        assert!(markdown.contains("Supporting references are unverified."));
    }
}

#[test]
fn primary_prose_citations_use_evidence_without_changing_supporting_references() {
    // Arrange
    let cases = [
        (
            "src/main.rs",
            "src/main.rs:999: risk",
            7,
            "src/main.rs:7: risk",
        ),
        (
            "src/main.rs",
            "`src/main.rs:999-1001` and src/main.rs:888:2",
            7,
            "`src/main.rs:7` and src/main.rs:888:2",
        ),
        (
            "src/main.rs",
            "(src/main.rs:999:2-1000:3), caller.rs:12",
            7,
            "(src/main.rs:7), caller.rs:12",
        ),
        ("src/main.rs", "src/main.rs:999-1001", 0, "src/main.rs"),
        (
            "src/main.rs",
            "other/src/main.rs:999 mysrc/main.rs:999 _src/main.rs:999",
            7,
            "other/src/main.rs:999 mysrc/main.rs:999 _src/main.rs:999",
        ),
        (
            "src/main.rs",
            "src/main.rs:abc src/main.rs:999: risk src/main.rs:999- risk",
            7,
            "src/main.rs:abc src/main.rs:7: risk src/main.rs:7- risk",
        ),
        (
            "é space.rs",
            "See `é space.rs:999` and other.rs:999",
            7,
            "See `é space.rs:7` and other.rs:999",
        ),
        ("1", "1:999-1:2", 7, "1:7"),
        ("", "other.rs:999", 0, "other.rs:999"),
    ];

    // Act / Assert
    for (path, details, line, expected) in cases {
        let mut suggestion = suggestion_with_evidence(path, details, 999);
        suggestion.resolve_evidence_range(line, line);
        assert_eq!(suggestion.details, expected);
    }
}

#[test]
fn primary_repair_preserves_same_file_callers_before_and_after_the_primary_reference() {
    // Arrange
    let mut suggestion = suggestion_with_evidence(
        "src/main.rs",
        "src/main.rs:80 calls src/main.rs:20; see src/main.rs:80:4 and src/main.rs:20-21",
        20,
    );

    // Act
    suggestion.resolve_evidence_range(25, 26);
    let markdown = suggestion.to_markdown();

    // Assert
    assert_eq!(
        suggestion.details,
        "src/main.rs:80 calls src/main.rs:25; see src/main.rs:80:4 and src/main.rs:25"
    );
    assert!(markdown.contains("`src/main.rs:25`"));
    assert!(markdown.contains("src/main.rs:80:4"));
    assert!(markdown.contains("Supporting references are unverified."));
    assert_eq!(suggestion.evidence.expect("evidence").end_line, 26);
}

#[test]
fn rendering_keeps_supporting_locations_and_resolution_preserves_missing_primary_claims() {
    // Arrange
    let mut suggestion = suggestion_with_evidence("src/main.rs", "src/main.rs:80 calls run", 20);
    let mut unknown = suggestion_with_evidence("src/main.rs", "src/main.rs:80", 0);
    let mut legacy = FocusedReviewSuggestion {
        details: "Saved finding at src/main.rs:80".into(),
        evidence: None,
        severity: FocusedReviewSeverity::High,
    };

    // Act
    let rendered = suggestion.to_markdown();
    suggestion.resolve_evidence_range(25, 25);
    unknown.resolve_evidence_range(25, 25);
    legacy.resolve_evidence_range(25, 25);

    // Assert
    assert!(rendered.contains("`src/main.rs:20`: src/main.rs:80 calls run"));
    assert_eq!(suggestion.details, "src/main.rs:80 calls run");
    assert_eq!(unknown.details, "src/main.rs:80");
    assert_eq!(legacy.details, "Saved finding at src/main.rs:80");
    assert_eq!(legacy.details_with_location(80), legacy.details);
}

fn suggestion_with_evidence(
    path: &str,
    details: &str,
    claimed_line: u32,
) -> FocusedReviewSuggestion {
    FocusedReviewSuggestion {
        details: details.into(),
        evidence: Some(FocusedReviewEvidence {
            correction: "Restore validation".into(),
            end_line: claimed_line,
            existing_code: "run();".into(),
            impact: "Invalid input executes".into(),
            path: path.into(),
            side: FocusedReviewSide::New,
            start_line: claimed_line,
            trigger: "An invalid request".into(),
        }),
        severity: FocusedReviewSeverity::High,
    }
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
fn consolidation_accounts_for_retained_and_merged_candidates() {
    // Arrange
    let review: FocusedReview = serde_json::from_value(serde_json::json!({
        "project_impact":[], "suggestions":[{"details":"Supported risk", "severity":"high"}],
        "candidate_decisions":[
            {"candidate_index":0,"suggestion_index":0,"reason":"Verified source"},
            {"candidate_index":1,"suggestion_index":0,"reason":"Duplicate trigger"},
            {"candidate_index":2,"suggestion_index":null,"reason":"Existing validation"}
        ]
    }))
    .expect("review");

    // Act / Assert
    review
        .validate_candidate_decisions(3)
        .expect("complete decisions");
}
