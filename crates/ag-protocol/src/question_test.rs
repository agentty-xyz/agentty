use crate::question::QuestionItem;

#[test]
/// Ensures predefined answer options are kept in order with the question.
fn test_question_item_with_options_keeps_text_and_options() {
    // Arrange
    let options = vec!["Yes".to_string(), "No".to_string()];

    // Act
    let question = QuestionItem::with_options("Ship it?", options.clone());

    // Assert
    assert_eq!(question.text, "Ship it?");
    assert_eq!(question.options, options);
}
