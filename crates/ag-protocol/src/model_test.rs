use crate::model::{
    AgentResponse, MAX_QUESTIONS, ReviewCommentOutcome, ReviewCommentResolution,
    questions_field_description, render_field_description_template,
};
use crate::question::QuestionItem;

#[test]
/// Ensures the dynamic `questions` field description renders from the
/// checked-in prompt-schema template.
fn test_questions_field_description_renders_template_limit() {
    // Arrange
    let expected_limit = format!("Emit at most {MAX_QUESTIONS} items");

    // Act
    let description = questions_field_description();
    let normalized_description = description.split_whitespace().collect::<Vec<_>>().join(" ");

    // Assert
    assert!(normalized_description.contains(&expected_limit));
    assert!(normalized_description.contains("Emit an empty array when no input is required"));
    assert!(normalized_description.contains("field defaults to an empty array when omitted"));
    assert!(normalized_description.contains("genuinely ambiguous requirement"));
    assert!(normalized_description.contains("Never request permission for agreed work"));
    assert!(normalized_description.contains("ask for satisfaction or sign-off"));
    assert!(normalized_description.contains("Execute agreed work"));
    assert!(!description.contains("{{ max_questions }}"));
}

#[test]
/// Ensures display text includes the answer and clarification questions in
/// order.
fn test_agent_response_to_display_text_joins_answer_and_questions() {
    // Arrange
    let response = AgentResponse {
        answer: "Primary answer".to_string(),
        questions: vec![QuestionItem::new("Need one clarification.")],
        review_comment_outcomes: Vec::new(),
    };

    // Act
    let display_text = response.to_display_text();

    // Assert
    assert_eq!(display_text, "Primary answer\n\nNeed one clarification.");
}

#[test]
/// Preserves review-comment outcomes through the wire JSON contract.
fn test_agent_response_review_comment_outcomes_round_trip() {
    // Arrange
    let response = AgentResponse {
        answer: "Addressed the comment.".to_string(),
        questions: Vec::new(),
        review_comment_outcomes: vec![ReviewCommentOutcome {
            reply: "Added the missing validation.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-42".to_string(),
        }],
    };

    // Act
    let serialized = serde_json::to_string(&response).expect("response should serialize");
    let deserialized =
        serde_json::from_str::<AgentResponse>(&serialized).expect("response should deserialize");

    // Assert
    assert_eq!(deserialized, response);
    assert!(serialized.contains(r#""resolution":"fixed""#));
}

#[test]
/// Ensures question extraction respects the protocol question cap.
fn test_agent_response_question_items_applies_question_cap() {
    // Arrange
    let response = AgentResponse {
        answer: String::new(),
        questions: (0..=MAX_QUESTIONS)
            .map(|index| QuestionItem::new(format!("Question {index}")))
            .collect(),
        review_comment_outcomes: Vec::new(),
    };

    // Act
    let questions = response.question_items();

    // Assert
    assert_eq!(questions.len(), MAX_QUESTIONS);
}

#[test]
/// Substitutes a cap placeholder that markdown reflowing wrapped across a
/// line break, so reformatting a template cannot leak raw `{{ ... }}` text
/// into the schema description models read.
fn test_field_description_template_survives_a_wrapped_placeholder() {
    // Arrange
    let template = "Emit at most {{\nmax_items }} items, and no more.\n";

    // Act
    let rendered = render_field_description_template(template, "{{ max_items }}", 4);

    // Assert
    assert_eq!(rendered, "Emit at most 4 items, and no more.");
}

#[test]
/// Leaves an unterminated placeholder untouched instead of truncating the
/// remaining guidance text.
fn test_field_description_template_keeps_unterminated_placeholder_text() {
    // Arrange
    let template = "Emit at most {{ max_items items.";

    // Act
    let rendered = render_field_description_template(template, "{{ max_items }}", 4);

    // Assert
    assert_eq!(rendered, "Emit at most {{ max_items items.");
}
