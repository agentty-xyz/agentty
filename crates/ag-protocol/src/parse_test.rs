use std::io::{self, Read};

use crate::model::{
    AgentResponse, AgentResponseParseError, ProtocolRequestProfile, ReviewCommentOutcome,
    ReviewCommentResolution,
};
use crate::parse::{
    describe_json_error_category, format_protocol_parse_debug_details, parse_agent_response_strict,
    parse_protocol_response_strict, push_character_boundary_debug_lines,
};

#[test]
/// Strict parsing accepts a complete schema payload.
fn test_parse_agent_response_strict_structured_json_payload() {
    // Arrange
    let raw = r#"{"answer":"Here is my analysis.","questions":[]}"#;

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(
        response.expect("response should parse").answer,
        "Here is my analysis."
    );
}

#[test]
fn parse_protocol_response_strict_normalizes_direct_focused_review() {
    // Arrange
    let raw = concat!(
        r#"{"project_impact":["Improves reliability."],"suggestions":["#,
        r#"{"details":"Fix this.","severity":"high"},"#,
        r#"{"details":"Improve that.","severity":"medium"}]}"#,
    );

    // Act
    let response = parse_protocol_response_strict(raw, ProtocolRequestProfile::FocusedReview)
        .expect("focused review should parse");

    // Assert
    assert_eq!(response, AgentResponse::plain(raw));
}

#[test]
fn parse_protocol_response_strict_rejects_empty_focused_review() {
    // Arrange
    let raw = "  \n ";

    // Act
    let error = parse_protocol_response_strict(raw, ProtocolRequestProfile::FocusedReview)
        .expect_err("empty focused review should fail");

    // Assert
    assert_eq!(error, AgentResponseParseError::Empty);
}

#[test]
fn parse_protocol_response_strict_rejects_focused_review_trailing_text() {
    // Arrange
    let raw = r#"{"project_impact":[],"suggestions":[]} trailing text"#;

    // Act
    let error = parse_protocol_response_strict(raw, ProtocolRequestProfile::FocusedReview)
        .expect_err("trailing text should require protocol repair");

    // Assert
    assert!(error.to_string().contains("trailing characters"));
}

#[test]
fn parse_protocol_response_strict_rejects_unknown_focused_review_field() {
    // Arrange
    let raw = r#"{"project_impact":[],"suggestions":[],"summary":"extra"}"#;

    // Act
    let error = parse_protocol_response_strict(raw, ProtocolRequestProfile::FocusedReview)
        .expect_err("unknown focused-review field should require protocol repair");

    // Assert
    assert!(error.to_string().contains("unknown field `summary`"));
}

#[test]
fn parse_protocol_response_strict_rejects_unknown_suggestion_field() {
    // Arrange
    let raw = concat!(
        r#"{"project_impact":[],"suggestions":[{"#,
        r#""details":"Fix this.","severity":"medium","path":"src/lib.rs"}]}"#,
    );

    // Act
    let error = parse_protocol_response_strict(raw, ProtocolRequestProfile::FocusedReview)
        .expect_err("unknown suggestion field should require protocol repair");

    // Assert
    assert!(error.to_string().contains("unknown field `path`"));
}

#[test]
/// Strict parsing recovers a trailing protocol payload when a provider
/// prepends extra prose before the final JSON object.
fn test_parse_agent_response_strict_recovers_wrapped_text() {
    // Arrange
    let raw = concat!(
        "Some wrapper text\n",
        r#"{"answer":"Recovered payload","questions":[]}"#
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(
        response.expect("response should parse"),
        AgentResponse::plain("Recovered payload")
    );
}

#[test]
/// Strict parsing rejects plain text that contains no protocol payload.
fn test_parse_agent_response_strict_rejects_plain_text() {
    // Arrange
    let raw = "plain text";

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert!(response.is_err());
}

#[test]
/// Strict parsing rejects JSON objects with only unrecognized fields
/// because at least one protocol key must be present.
fn test_parse_agent_response_strict_rejects_unrecognized_only_fields() {
    // Arrange
    let raw = r#"{"message":"not the expected shape"}"#;

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert!(response.is_err());
}

#[test]
/// Strict parsing rejects an empty JSON object because no recognized
/// protocol key is present.
fn test_parse_agent_response_strict_rejects_empty_json_object() {
    // Arrange
    let raw = "{}";

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert!(response.is_err());
}

#[test]
/// Strict parsing rejects an empty code fence instead of treating it as a
/// structured response.
fn test_parse_agent_response_strict_rejects_empty_code_fence() {
    // Arrange
    let raw = "```json\n\n```";

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert!(response.is_err());
}

#[test]
/// Strict parsing strips code fences and recovers the inner JSON payload.
fn test_parse_agent_response_strict_strips_code_fenced_payload() {
    // Arrange
    let raw = concat!(
        "```json\n",
        r#"{"answer":"Need details.","questions":[]}"#,
        "\n```"
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(
        response.expect("response should parse").answer,
        "Need details."
    );
}

#[test]
/// Strict parsing strips code fences even when leading/trailing whitespace
/// surrounds the fenced block.
fn test_parse_agent_response_strict_strips_code_fenced_payload_with_whitespace() {
    // Arrange
    let raw = concat!(
        "\n\n```json\n",
        r#"{"answer":"Recovered.","questions":[]}"#,
        "\n```\n"
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(
        response.expect("response should parse").answer,
        "Recovered."
    );
}

#[test]
/// Strict parsing strips plain code fences without a language tag.
fn test_parse_agent_response_strict_strips_plain_code_fenced_payload() {
    // Arrange
    let raw = concat!(
        "```\n",
        r#"{"answer":"Plain fence.","questions":[]}"#,
        "\n```"
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(
        response.expect("response should parse").answer,
        "Plain fence."
    );
}

#[test]
/// Strict parsing tolerates extra top-level fields that providers may add
/// beyond the protocol schema.
fn test_parse_agent_response_strict_tolerates_extra_top_level_fields() {
    // Arrange
    let raw = r#"{"answer":"Hello.","questions":[],"reasoning":"internal thought"}"#;

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(response.expect("response should parse").answer, "Hello.");
}

#[test]
/// Strict parsing tolerates extra fields inside nested question objects.
fn test_parse_agent_response_strict_tolerates_extra_question_fields() {
    // Arrange
    let raw = r#"{"answer":"","questions":[{"text":"Which approach?","options":["A","B"],"priority":"high"}]}"#;

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    let questions = response.expect("response should parse").question_items();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].text, "Which approach?");
}

#[test]
/// Parser accepts a payload with `questions` but no `answer` key,
/// exercising the documented asymmetry where the parser is lenient
/// (any recognized key suffices) while the prompt schema requires
/// `answer`.
fn test_parse_agent_response_strict_accepts_questions_without_answer() {
    // Arrange
    let raw = r#"{"questions":[{"text":"Which approach?"}]}"#;

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    let response = response.expect("parser should accept questions-only payload");
    assert_eq!(response.answer, "");
    assert_eq!(response.question_items().len(), 1);
}

#[test]
/// Parser accepts a review-comment outcome without an `answer` key because
/// it is a recognized protocol field.
fn test_parse_agent_response_strict_accepts_review_outcome_without_answer() {
    // Arrange
    let raw = concat!(
        r#"{"review_comment_outcomes":[{"reply":"Fixed it.","resolution":"fixed","#,
        r#""thread_id":"thread-42"}]}"#
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    let response = response.expect("parser should accept review-outcome-only payload");
    assert_eq!(
        response.review_comment_outcomes,
        vec![ReviewCommentOutcome {
            reply: "Fixed it.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-42".to_string(),
        }]
    );
}

#[test]
/// Recovery path skips non-protocol JSON objects embedded in prose when
/// they contain no recognized protocol keys.
fn test_parse_agent_response_strict_rejects_wrapped_non_protocol_json() {
    // Arrange
    let raw = concat!(
        "Some wrapper text\n",
        r#"{"reasoning":"internal thought","confidence":0.9}"#
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert!(response.is_err());
}

#[test]
/// Strict parsing still rejects trailing wrapper text after a recovered
/// schema object.
fn test_parse_agent_response_strict_rejects_trailing_wrapper_after_payload() {
    // Arrange
    let raw = concat!(
        "Some wrapper text\n",
        r#"{"answer":"Recovered payload","questions":[]}"#,
        "\ntrailing wrapper text"
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert!(response.is_err());
}

#[test]
/// Strict parsing recovers protocol JSON from an embedded code fence
/// preceded by prose text.
fn test_parse_agent_response_strict_recovers_embedded_code_fence_in_prose() {
    // Arrange
    let raw = concat!(
        "The commit message looks good. Let me refine it.\n\n",
        "```json\n",
        r#"{"answer":"Refined commit message","questions":[]}"#,
        "\n```"
    );

    // Act
    let response = parse_agent_response_strict(raw);

    // Assert
    assert_eq!(
        response.expect("response should parse").answer,
        "Refined commit message"
    );
}

#[test]
/// Strict parsing reports empty input, schema mismatches, and fenced
/// payloads without a protocol object as distinct failures.
fn test_parse_agent_response_strict_reports_failure_reasons() {
    // Arrange
    let cases = [
        ("  \n ", AgentResponseParseError::Empty),
        (
            r#"{"answer":5}"#,
            AgentResponseParseError::InvalidFormat {
                reason: concat!(
                    "direct parse failed (response is not valid protocol JSON: schema validation ",
                    "failed (invalid type: number, expected a string)); no markdown ",
                    "wrapper/embedded protocol object found",
                )
                .to_string(),
            },
        ),
        (
            "```json\n{\"reasoning\":\"x\"}\n```",
            AgentResponseParseError::InvalidFormat {
                reason: concat!(
                    "markdown code fence extraction failed (response is not valid protocol ",
                    "JSON: json object is missing all protocol keys (answer, questions, ",
                    "review_comment_outcomes))",
                )
                .to_string(),
            },
        ),
    ];

    for (raw, expected_error) in cases {
        // Act
        let response = parse_agent_response_strict(raw);

        // Assert
        assert_eq!(response, Err(expected_error), "{raw:?}");
    }
}

#[test]
/// Strict parsing recovers a trailing protocol object from prose inside a
/// leading or embedded code fence.
fn test_parse_agent_response_strict_recovers_prose_wrapped_payload_inside_fences() {
    // Arrange
    let leading_fence = "```text\nNotes first {\"answer\":\"From fence\"}\n```";
    let embedded_fence = "Intro\n```text\nNotes first {\"answer\":\"From embedded\"}\n```";

    // Act
    let leading_response = parse_agent_response_strict(leading_fence);
    let embedded_response = parse_agent_response_strict(embedded_fence);

    // Assert
    assert_eq!(
        leading_response.expect("leading fence should parse").answer,
        "From fence"
    );
    assert_eq!(
        embedded_response
            .expect("embedded fence should parse")
            .answer,
        "From embedded"
    );
}

#[test]
/// Session-turn profile parsing uses the strict agent response parser.
fn test_parse_protocol_response_strict_session_turn_uses_agent_response_parser() {
    // Arrange
    let raw = r#"{"answer":"Done."}"#;

    // Act
    let response = parse_protocol_response_strict(raw, ProtocolRequestProfile::SessionTurn);

    // Assert
    assert_eq!(response.expect("response should parse").answer, "Done.");
}

#[test]
/// Debug formatting stops after size metadata for blank responses.
fn test_format_protocol_parse_debug_details_reports_blank_response_sizes_only() {
    // Arrange
    let raw = " \n ";

    // Act
    let details = format_protocol_parse_debug_details(raw);

    // Assert
    assert_eq!(
        details,
        "response_len: 3 chars\nresponse_lines: 2\ntrimmed_len: 0 \
         chars\nwrapped_in_markdown_fence: false"
    );
}

#[test]
/// Debug formatting describes fenced JSON, recognized protocol keys,
/// non-object values, and end-of-input JSON errors.
fn test_format_protocol_parse_debug_details_reports_fence_types_and_keys() {
    // Arrange
    let cases = [
        (
            "```json\n{\"answer\":\"x\"}\n```",
            vec![
                "wrapped_in_markdown_fence: true",
                "code_fence_inner_len: 14 chars",
                "code_fence_json_recognized_protocol_keys: answer",
                "embedded_json_candidate: none",
            ],
        ),
        ("null", vec!["direct_json_type: null"]),
        ("true", vec!["direct_json_type: boolean"]),
        ("1", vec!["direct_json_type: number"]),
        ("\"text\"", vec!["direct_json_type: string"]),
        ("[1]", vec!["direct_json_type: array"]),
        (
            "{",
            vec![
                "direct_json_error_category: eof",
                "embedded_json_candidate: none",
            ],
        ),
    ];

    for (raw, expected_lines) in cases {
        // Act
        let details = format_protocol_parse_debug_details(raw);

        // Assert
        for expected_line in expected_lines {
            assert!(
                details.lines().any(|line| line == expected_line),
                "{raw:?} should report {expected_line:?} in:\n{details}"
            );
        }
    }
}

#[test]
/// Character-boundary diagnostics add nothing for empty text.
fn test_push_character_boundary_debug_lines_skips_empty_text() {
    // Arrange
    let mut detail_lines = Vec::new();

    // Act
    push_character_boundary_debug_lines(&mut detail_lines, "");

    // Assert
    assert_eq!(detail_lines, Vec::<String>::new());
}

#[test]
/// JSON error categories map every serde category to a stable label.
fn test_describe_json_error_category_labels_io_and_data_errors() {
    // Arrange
    let io_error = serde_json::from_reader::<_, serde_json::Value>(FailingReader)
        .expect_err("reader failure should surface");
    let data_error =
        serde_json::from_str::<u8>("\"text\"").expect_err("type mismatch should surface");

    // Act
    let io_category = describe_json_error_category(&io_error);
    let data_category = describe_json_error_category(&data_error);

    // Assert
    assert_eq!(io_category, "io");
    assert_eq!(data_category, "data");
}

/// Reader that always fails so serde reports an I/O error category.
struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("read failed"))
    }
}

#[test]
/// The debug report describes the payload without reproducing any of it,
/// so a turn error cannot print raw provider output into the transcript.
fn test_format_protocol_parse_debug_details_never_quotes_payload() {
    // Arrange
    let raw = format!("{} SECRETTAIL", "payload-filler ".repeat(40));

    // Act
    let details = format_protocol_parse_debug_details(&raw);

    // Assert
    assert!(!details.contains("payload-filler"));
    assert!(!details.contains("SECRETTAIL"));
    assert!(details.contains(&format!("response_len: {} chars", raw.chars().count())));
    assert!(details.contains("direct_json_error"));
}

#[test]
/// Debug formatting reports JSON parser location details for plain-text
/// responses that never produced protocol JSON.
fn test_format_protocol_parse_debug_details_reports_plain_text_json_error() {
    // Arrange
    let raw = "plain text";

    // Act
    let details = format_protocol_parse_debug_details(raw);

    // Assert
    assert!(details.contains("response_len: 10 chars"));
    assert!(details.contains("first_non_whitespace_char: 'p'"));
    assert!(details.contains("direct_json_error_category: syntax"));
    assert!(details.contains("direct_json_error_location: line 1, column 1"));
    assert!(details.contains("embedded_json_candidate: none"));
}

#[test]
/// Debug formatting reports visible top-level keys when the response is
/// valid JSON but does not include any protocol fields.
fn test_format_protocol_parse_debug_details_reports_unrecognized_json_keys() {
    // Arrange
    let raw = r#"{"message":"not the expected shape"}"#;

    // Act
    let details = format_protocol_parse_debug_details(raw);

    // Assert
    assert!(details.contains("direct_json_type: object"));
    assert!(details.contains("direct_json_keys: message"));
    assert!(details.contains("direct_json_recognized_protocol_keys: (none)"));
    assert!(
        details.contains(
            "direct_json_missing_protocol_keys: answer, questions, review_comment_outcomes"
        )
    );
}

#[test]
fn utility_profiles_validate_direct_wire_objects_before_normalizing() {
    // Arrange
    let metadata = serde_json::json!({"title":"Keep \"quotes\"", "description":"First line\nSecond line", "is_title_change_significant":false});
    let inputs = [
        (
            crate::ProtocolRequestProfile::UtilityPrompt,
            r#"{"answer":"First line\nSay \"hello\""}"#.to_string(),
        ),
        (
            crate::ProtocolRequestProfile::ReviewMetadata,
            metadata.to_string(),
        ),
    ];

    for (profile, wire) in inputs {
        // Act
        let response =
            crate::parse_protocol_response_strict(&wire, profile).expect("valid direct object");

        // Assert
        assert_eq!(response.questions.len(), 0);
        assert_ne!(response.answer, "");
        assert!(crate::parse_protocol_response_strict("", profile).is_err());
        assert!(crate::parse_protocol_response_strict("{}", profile).is_err());
        assert!(
            crate::parse_protocol_response_strict(r#"{"answer":"x","questions":[]}"#, profile)
                .is_err()
        );
    }
    let response = crate::parse_protocol_response_strict(
        &metadata.to_string(),
        crate::ProtocolRequestProfile::ReviewMetadata,
    )
    .expect("metadata");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&response.answer).expect("json"),
        metadata
    );
}

#[test]
/// Utility answers that serialize another wire object are unwrapped to the
/// innermost result text, while other JSON-looking answers, including
/// one-element arrays, stay verbatim.
fn utility_profile_unwraps_self_wrapped_answers() {
    // Arrange
    let inner = r#"{"answer":"Remove FYI page\n\n- Delete the module"}"#;
    let single_wrapped = serde_json::json!({ "answer": inner }).to_string();
    let double_wrapped = serde_json::json!({ "answer": format!(" {single_wrapped} ") }).to_string();
    let other_json = serde_json::json!({ "answer": r#"{"answer":"x","extra":1}"# }).to_string();
    let array_json = serde_json::json!({ "answer": r#"["hello"]"# }).to_string();

    // Act
    let single =
        parse_protocol_response_strict(&single_wrapped, ProtocolRequestProfile::UtilityPrompt)
            .expect("single-wrapped utility answer");
    let double =
        parse_protocol_response_strict(&double_wrapped, ProtocolRequestProfile::UtilityPrompt)
            .expect("double-wrapped utility answer");
    let other = parse_protocol_response_strict(&other_json, ProtocolRequestProfile::UtilityPrompt)
        .expect("JSON-looking utility answer");
    let array = parse_protocol_response_strict(&array_json, ProtocolRequestProfile::UtilityPrompt)
        .expect("JSON array utility answer");

    // Assert
    assert_eq!(single.answer, "Remove FYI page\n\n- Delete the module");
    assert_eq!(double.answer, "Remove FYI page\n\n- Delete the module");
    assert_eq!(other.answer, r#"{"answer":"x","extra":1}"#);
    assert_eq!(array.answer, r#"["hello"]"#);
}

#[test]
fn utility_wrapper_recovery_still_rejects_unexpected_fields() {
    // Arrange
    let payload = r#"{"answer":"Done"}"#;
    let wrappers = [
        payload.to_string(),
        format!("```json\n{payload}\n```"),
        format!("Commentary\n{payload}"),
        format!("Commentary\n```json\n{payload}\n```"),
    ];

    for raw in wrappers {
        // Act
        let response = crate::parse_protocol_response_strict(
            &raw,
            crate::ProtocolRequestProfile::UtilityPrompt,
        )
        .expect("recover valid utility");

        // Assert
        assert_eq!(response.answer, "Done");
    }
    assert!(
        crate::parse_protocol_response_strict(
            r#"{"answer":"Done","unexpected":[]}"#,
            crate::ProtocolRequestProfile::UtilityPrompt
        )
        .is_err()
    );
}
