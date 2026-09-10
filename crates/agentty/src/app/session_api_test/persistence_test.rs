use ag_session::SessionError as ApiSessionError;

use super::super::{api_questions_from_json, build_api_session};
use super::support::session_row;
use crate::infra::db::SessionMessageRow;

#[test]
fn build_api_session_rejects_invalid_persisted_data() {
    // Arrange
    let mut missing_project = session_row();
    missing_project.project_id = None;
    let mut invalid_status = session_row();
    invalid_status.status = "Unknown".to_string();
    let mut invalid_permission_mode = session_row();
    invalid_permission_mode.permission_mode = "invalid".to_string();
    let invalid_message = SessionMessageRow {
        content: "content".to_string(),
        kind: "unknown".to_string(),
        position: 4,
    };
    let mut invalid_review = session_row();
    invalid_review
        .review_request
        .as_mut()
        .expect("fixture should have review metadata")
        .state = "Unknown".to_string();
    let mut invalid_questions = session_row();
    invalid_questions.questions = Some("{invalid".to_string());

    // Act
    let missing_project_error = build_api_session(missing_project, Vec::new(), Vec::new())
        .expect_err("project is required");
    let invalid_status_error = build_api_session(invalid_status, Vec::new(), Vec::new())
        .expect_err("status should be validated");
    let invalid_permission_mode_error =
        build_api_session(invalid_permission_mode, Vec::new(), Vec::new())
            .expect_err("permission mode should be validated");
    let invalid_message_error = build_api_session(session_row(), vec![invalid_message], Vec::new())
        .expect_err("message kind should be validated");
    let invalid_review_error = build_api_session(invalid_review, Vec::new(), Vec::new())
        .expect_err("review should be validated");
    let invalid_questions_error = build_api_session(invalid_questions, Vec::new(), Vec::new())
        .expect_err("questions should be validated");
    let legacy_questions = api_questions_from_json(Some(r#"["Legacy question"]"#), "session-1")
        .expect("legacy questions should convert");
    let empty_questions =
        api_questions_from_json(Some(""), "session-1").expect("empty questions should convert");

    // Assert
    assert!(matches!(
        missing_project_error,
        ApiSessionError::InvalidData(_)
    ));
    assert!(matches!(
        invalid_status_error,
        ApiSessionError::InvalidData(_)
    ));
    assert!(matches!(
        invalid_permission_mode_error,
        ApiSessionError::InvalidData(_)
    ));
    assert!(matches!(
        invalid_message_error,
        ApiSessionError::InvalidData(_)
    ));
    assert!(matches!(
        invalid_review_error,
        ApiSessionError::InvalidData(_)
    ));
    assert!(matches!(
        invalid_questions_error,
        ApiSessionError::InvalidData(_)
    ));
    assert_eq!(legacy_questions[0].text, "Legacy question");
    assert_eq!(empty_questions, Vec::new());
}
