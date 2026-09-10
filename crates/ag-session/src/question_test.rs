use ag_protocol::QuestionItem;

use crate::question::default_option_index;

#[test]
fn default_option_index_selects_only_predefined_options() {
    // Arrange
    let questions = [
        QuestionItem::with_options("Continue?", vec!["Yes".to_string()]),
        QuestionItem::new("Why?"),
    ];

    // Act
    let predefined = default_option_index(&questions, 0);
    let free_text = default_option_index(&questions, 1);
    let missing = default_option_index(&questions, 2);

    // Assert
    assert_eq!(predefined, Some(0));
    assert_eq!(free_text, None);
    assert_eq!(missing, None);
}
