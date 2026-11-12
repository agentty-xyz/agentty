use ag_protocol::{AgentResponse, QuestionItem};

use crate::question::{default_option_index, node_question_notice};

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

#[test]
fn node_question_notice_lists_questions_after_the_answer_they_refer_to() {
    // Arrange
    let mut response = AgentResponse::plain("  Plan: move snippets into **Usage**.  ");
    response.questions = vec![
        QuestionItem::with_options("Which update?", vec!["Cleanup".into(), "Fixes".into()]),
        QuestionItem::new("Anything else?"),
    ];
    let mut question_only = AgentResponse::plain("");
    question_only.questions = vec![QuestionItem::new("Which target?")];
    let mut blank_questions = AgentResponse::plain("Done");
    blank_questions.questions = vec![QuestionItem::new("  ")];

    // Act
    let notice = node_question_notice("plan_readme", &response);
    let bare_notice = node_question_notice("plan_readme", &question_only);
    let no_notice = node_question_notice("plan_readme", &blank_questions);

    // Assert
    assert_eq!(
        notice.as_deref(),
        Some(
            "Plan: move snippets into **Usage**.\n\n`plan_readme` needs your input:\n1. Which \
             update? (Cleanup / Fixes)\n2. Anything else?"
        )
    );
    assert_eq!(
        bare_notice.as_deref(),
        Some("`plan_readme` needs your input:\n1. Which target?")
    );
    assert_eq!(no_notice, None);
}
