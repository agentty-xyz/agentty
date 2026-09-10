use super::super::append_line_comments;
use super::support::{RESTORE_DRAFT_TEXT, non_default_prompt_snapshot};
use crate::presentation::app_mode::{
    DiffLineCommentAnchor, DiffLineCommentTarget, DiffLineComments,
};

#[test]
fn test_append_line_comment_preserves_draft_and_attachment() {
    // Arrange
    let mut snapshot = non_default_prompt_snapshot();
    let mut line_comments = DiffLineComments::default();
    let anchor = DiffLineCommentAnchor {
        content: "updated()".to_string(),
        line: 9,
        path: "src/lib.rs".to_string(),
        side: crate::presentation::app_mode::DiffLineSide::Old,
    };
    line_comments.start_editing_target(DiffLineCommentTarget::single(anchor));
    line_comments
        .editing_input_mut()
        .expect("comment should be editable")
        .insert_text("Update this call");
    line_comments.finish_editing();

    // Act
    append_line_comments(&mut snapshot, &line_comments);

    // Assert
    assert!(snapshot.input.text().starts_with(RESTORE_DRAFT_TEXT));
    assert!(
        snapshot.input.text().ends_with(
            "Line comments:\n- src/lib.rs:9 [old, source=\"updated()\"]: Update this call"
        )
    );
    assert_eq!(snapshot.attachment_state.attachments.len(), 1);
    assert_eq!(snapshot.history_state.selected_index, None);
    assert!(snapshot.at_mention_state.is_none());

    // Arrange
    let empty_comments = DiffLineComments::default();
    let unchanged_text = snapshot.input.text().to_string();

    // Act
    append_line_comments(&mut snapshot, &empty_comments);

    // Assert
    assert_eq!(snapshot.input.text(), unchanged_text);
}
