use super::*;

#[test]
/// Ensures CLI prompt rendering replaces image placeholders with local
/// file paths in placeholder order.
fn test_render_prompt_with_local_images_replaces_placeholders_in_order() {
    // Arrange
    let attachments = vec![
        TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/first-image.png"),
        },
        TurnPromptAttachment {
            placeholder: "[Image #2]".to_string(),
            local_image_path: PathBuf::from("/tmp/second-image.png"),
        },
    ];

    // Act
    let rendered_prompt = render_prompt_with_local_images(
        "Compare [Image #2] with [Image #1]",
        &attachments,
        "TestBackend",
    )
    .expect("prompt rendering should succeed");

    // Assert
    assert_eq!(
        rendered_prompt,
        "Compare /tmp/second-image.png with /tmp/first-image.png"
    );
}

#[test]
/// Ensures CLI prompt rendering appends local image paths when attachment
/// metadata survives without a placeholder match.
fn test_render_prompt_with_local_images_appends_missing_paths() {
    // Arrange
    let attachments = vec![TurnPromptAttachment {
        placeholder: "[Image #1]".to_string(),
        local_image_path: PathBuf::from("/tmp/first-image.png"),
    }];

    // Act
    let rendered_prompt =
        render_prompt_with_local_images("Review this change", &attachments, "TestBackend")
            .expect("prompt rendering should succeed");

    // Assert
    assert_eq!(
        rendered_prompt,
        "Review this change\n/tmp/first-image.png\n"
    );
}

#[cfg(unix)]
#[test]
/// Ensures CLI prompt rendering fails fast with the provider label when an
/// attachment path is not valid UTF-8.
fn test_render_prompt_with_local_images_rejects_non_utf8_paths() {
    // Arrange
    let attachments = vec![TurnPromptAttachment {
        placeholder: "[Image #1]".to_string(),
        local_image_path: PathBuf::from(OsString::from_vec(vec![0x66, 0x80, 0x6f])),
    }];

    // Act
    let error = render_prompt_with_local_images("Review [Image #1]", &attachments, "Claude")
        .expect_err("prompt rendering should fail");

    // Assert
    assert_eq!(
        error,
        AgentBackendError::CommandBuild("Claude prompt image path is not valid UTF-8".to_string())
    );
}

#[test]
/// Ensures CLI prompt access roots deduplicate sorted attachment
/// directories when the provider only needs attachment parents.
fn test_cli_prompt_access_directories_deduplicates_attachment_directories() {
    // Arrange
    let workspace_folder = PathBuf::from("/tmp/session");
    let attachments = vec![
        TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/images-b/two.png"),
        },
        TurnPromptAttachment {
            placeholder: "[Image #2]".to_string(),
            local_image_path: PathBuf::from("/tmp/images-a/one.png"),
        },
        TurnPromptAttachment {
            placeholder: "[Image #3]".to_string(),
            local_image_path: PathBuf::from("/tmp/images-a/three.png"),
        },
    ];

    // Act
    let directories = cli_prompt_access_directories(
        &workspace_folder,
        &attachments,
        CliPromptAccessRootMode::AttachmentsOnly,
    );

    // Assert
    assert_eq!(
        directories,
        vec![
            PathBuf::from("/tmp/images-a"),
            PathBuf::from("/tmp/images-b")
        ]
    );
}

#[test]
/// Ensures Antigravity-style access roots keep the workspace first and do
/// not duplicate it when an attachment also lives under that directory.
fn test_cli_prompt_access_directories_keeps_workspace_first() {
    // Arrange
    let workspace_folder = PathBuf::from("/tmp/z-session");
    let attachments = vec![
        TurnPromptAttachment {
            placeholder: "[Image #1]".to_string(),
            local_image_path: PathBuf::from("/tmp/z-session/one.png"),
        },
        TurnPromptAttachment {
            placeholder: "[Image #2]".to_string(),
            local_image_path: PathBuf::from("/tmp/a-images/two.png"),
        },
    ];

    // Act
    let directories = cli_prompt_access_directories(
        &workspace_folder,
        &attachments,
        CliPromptAccessRootMode::WorkspaceThenAttachments,
    );

    // Assert
    assert_eq!(
        directories,
        vec![workspace_folder, PathBuf::from("/tmp/a-images")]
    );
}
