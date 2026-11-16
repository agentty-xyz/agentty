//! Staged draft attachment storage tests covering round trips, metadata
//! directory handling, and rejection of non-UTF-8 paths.

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

use super::{parse_staged_draft_attachments, serialize_staged_draft_attachments};
use crate::domain::turn_prompt::TurnPromptAttachment;

#[test]
fn test_serialize_and_parse_staged_draft_attachments_round_trip() {
    // Arrange
    let attachments = vec![TurnPromptAttachment {
        placeholder: "[Image #1]".to_string(),
        local_image_path: PathBuf::from("/tmp/image-001.png"),
    }];

    // Act
    let serialized_attachments =
        serialize_staged_draft_attachments(&attachments).expect("failed to serialize");
    let parsed_attachments = parse_staged_draft_attachments(serialized_attachments.as_deref());

    // Assert
    assert!(serialized_attachments.is_some());
    assert_eq!(parsed_attachments, attachments);
}

#[test]
fn test_serialize_staged_draft_attachments_maps_empty_slice_to_none() {
    // Arrange
    let attachments: Vec<TurnPromptAttachment> = Vec::new();

    // Act
    let serialized_attachments =
        serialize_staged_draft_attachments(&attachments).expect("failed to serialize");

    // Assert
    assert_eq!(serialized_attachments, None);
}

#[test]
/// Ensures missing or malformed persisted metadata loads as no attachments.
fn test_parse_staged_draft_attachments_treats_missing_and_invalid_values_as_empty() {
    // Arrange
    let invalid_value = Some("not json");

    // Act
    let missing_attachments = parse_staged_draft_attachments(None);
    let invalid_attachments = parse_staged_draft_attachments(invalid_value);

    // Assert
    assert_eq!(missing_attachments, Vec::<TurnPromptAttachment>::new());
    assert_eq!(invalid_attachments, Vec::<TurnPromptAttachment>::new());
}

#[cfg(unix)]
#[test]
/// Ensures attachment paths that JSON cannot represent fail before any
/// metadata is persisted.
fn test_serialize_staged_draft_attachments_rejects_non_utf8_attachment_paths() {
    // Arrange
    let attachments = vec![TurnPromptAttachment {
        placeholder: "[Image #1]".to_string(),
        local_image_path: PathBuf::from(OsStr::from_bytes(b"/tmp/image-\xff.png")),
    }];

    // Act
    let result = serialize_staged_draft_attachments(&attachments);

    // Assert
    assert!(
        result
            .expect_err("non-UTF-8 paths cannot be serialized")
            .to_string()
            .contains("UTF-8")
    );
}
