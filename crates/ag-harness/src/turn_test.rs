use std::io;
use std::time::Duration;

use serde_json::json;

use super::{
    ModelRequestActivity, ResumeFailure, ToolActivity, TurnError, TurnOutcome, TurnReport,
};
use crate::lifecycle::{ModelResponseType, TurnErrorType};
use crate::model::{CompletionMetadata, CompletionUsage, ModelError, ModelErrorType};
use crate::tool::ReadAction;

#[test]
fn outcome_exposes_output_and_report() {
    // Arrange
    let completion = CompletionMetadata::new(
        "stop".to_string(),
        None,
        None,
        None,
        Some(CompletionUsage::new(None, None, None, None, None, Some(1))),
    );
    let model_request = ModelRequestActivity::new(
        Some(completion.clone()),
        Duration::from_millis(2),
        ModelResponseType::Output,
    );
    let tool_call = ToolActivity::Write {
        bytes_written: 2,
        duration: Duration::from_millis(1),
        path: "output.txt".to_string(),
    };
    let output = json!({"summary": "done"});
    let outcome = TurnOutcome::new(
        output.clone(),
        TurnReport::new(
            Duration::from_millis(3),
            vec![model_request],
            vec![tool_call],
        ),
    );

    // Act and Assert
    assert_eq!(outcome.output(), &output);
    assert_eq!(outcome.report().duration(), Duration::from_millis(3));
    assert_eq!(outcome.report().model_requests().len(), 1);
    assert_eq!(outcome.report().tool_calls().len(), 1);
    let activity = &outcome.report().model_requests()[0];
    assert_eq!(activity.completion(), Some(&completion));
    assert_eq!(activity.duration(), Duration::from_millis(2));
    assert_eq!(activity.response_type(), ModelResponseType::Output);
    assert_eq!(outcome.into_output(), output);
}

#[test]
fn tool_activity_display_formats_every_outcome_safely() {
    // Arrange
    let read = ToolActivity::Read {
        duration: Duration::ZERO,
        end_line: None,
        path: "empty\n\u{1b}]52;c;Y2xpcGJvYXJk\u{7}.txt".to_string(),
        start_line: 1,
        truncated: false,
    };
    let inspection = ToolActivity::ReadInspection {
        action: ReadAction::List,
        duration: Duration::from_millis(1),
        summary: ".".to_string(),
    };
    let rejected_inspection = ToolActivity::ReadInspectionRejected {
        action: ReadAction::Search,
        duration: Duration::from_millis(1),
        summary: "needle".to_string(),
    };
    let rejected_read = ToolActivity::ReadRejected {
        duration: Duration::from_millis(2),
        path: "missing.rs".to_string(),
    };
    let write = ToolActivity::Write {
        bytes_written: 4,
        duration: Duration::from_millis(3),
        path: "src/lib.rs".to_string(),
    };
    let rejected_write = ToolActivity::WriteRejected {
        duration: Duration::from_millis(4),
        path: "src/main.rs".to_string(),
    };

    // Act
    let displays = [
        read.to_string(),
        inspection.to_string(),
        rejected_inspection.to_string(),
        rejected_read.to_string(),
        write.to_string(),
        rejected_write.to_string(),
    ];

    // Assert
    assert_eq!(
        displays,
        [
            "read empty\u{fffd}\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}.txt (line 1; <1 ms)",
            "read list . (completed; 1 ms)",
            "read search needle (rejected; 1 ms)",
            "read missing.rs (rejected; 2 ms)",
            "write src/lib.rs (4 bytes; 3 ms)",
            "write src/main.rs (rejected; 4 ms)",
        ]
    );
    assert_eq!(inspection.duration(), Duration::from_millis(1));
    assert_eq!(inspection.path(), ".");
    assert_eq!(rejected_inspection.duration(), Duration::from_millis(1));
    assert_eq!(rejected_inspection.path(), "needle");
    assert_eq!(rejected_read.duration(), Duration::from_millis(2));
    assert_eq!(rejected_read.name(), "read");
    assert_eq!(rejected_read.path(), "missing.rs");
    assert_eq!(write.duration(), Duration::from_millis(3));
    assert_eq!(write.name(), "write");
    assert_eq!(write.path(), "src/lib.rs");
    assert_eq!(rejected_write.duration(), Duration::from_millis(4));
    assert_eq!(rejected_write.name(), "write");
    assert_eq!(rejected_write.path(), "src/main.rs");
}

#[test]
fn resume_failure_preserves_request_context_and_http_status() {
    // Arrange
    let request_error = |status| {
        ModelError::classified_request(
            ModelErrorType::Provider,
            Some(status),
            io::Error::other("provider request failed").into(),
        )
    };
    let failures = [
        ResumeFailure::Native {
            source: request_error(429),
        },
        ResumeFailure::Replay {
            source: request_error(503),
        },
    ];

    // Act
    let errors = failures.map(ResumeFailure::into_model_error);

    // Assert
    assert_eq!(errors[0].http_status(), Some(429));
    assert_eq!(errors[1].http_status(), Some(503));
    assert!(
        errors[0]
            .to_string()
            .starts_with("model request failed: native provider continuation failed:")
    );
    assert!(errors[1].to_string().starts_with(
        "model request failed: native provider continuation was unavailable and history replay \
         failed:"
    ));
}

#[test]
fn repository_required_error_has_stable_classification() {
    // Arrange
    let error = TurnError::RepositoryRequired;

    // Act
    let error_type = error.error_type();

    // Assert
    assert_eq!(error_type, TurnErrorType::RepositoryRequired);
}
