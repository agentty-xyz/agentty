use std::path::PathBuf;

use ag_protocol::render_prompt_text_for_agent;
use ag_session::test_support as model_fixture;

use super::{
    PendingImagePaste, PromptAttachment, PromptAttachmentState, PromptComposerState,
    PromptHistoryState, PromptSlashStage, PromptSlashState, PromptSuggestionItem,
    PromptSuggestionList, PromptSuggestionSelection, build_prompt_slash_suggestion_list,
    current_line_delete_range, drain_prompt_submission, insert_prompt_local_image,
    insert_prompt_local_image_at, prompt_slash_option_count, resolve_prompt_slash_selection,
};
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, HarnessModelOption, HarnessModelStatus, ModelBilling,
    ReasoningLevel, ResponseStyle, ServedModel,
};
use crate::domain::input::{INPUT_HISTORY_LIMIT, InputState};
use crate::domain::permission::PermissionMode;

fn insert_test_attachment(
    attachment_state: &mut PromptAttachmentState,
    input: &mut InputState,
    local_image_path: PathBuf,
) -> String {
    let mut history_state = PromptHistoryState::default();
    let mut slash_state = PromptSlashState::default();
    insert_prompt_local_image(
        attachment_state,
        &mut history_state,
        input,
        &mut slash_state,
        local_image_path,
    );

    attachment_state
        .attachments
        .last()
        .expect("attachment should be registered")
        .placeholder
        .clone()
}

#[test]
fn test_prompt_attachment_state_registers_images_in_placeholder_order() {
    // Arrange
    let mut attachment_state = PromptAttachmentState::default();

    // Act
    let first_placeholder =
        attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"), 0);
    let second_placeholder =
        attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"), 10);

    // Assert
    assert_eq!(first_placeholder, "[Image #1]");
    assert_eq!(second_placeholder, "[Image #2]");
    assert_eq!(attachment_state.attachments.len(), 2);
    let attachment = attachment_state
        .attachment_for_placeholder("[Image #2]")
        .expect("second attachment should exist");
    assert_eq!(attachment.attachment_number, 2);
    assert_eq!(
        attachment.local_image_path,
        PathBuf::from("/tmp/second-image.png")
    );
    assert_eq!(attachment.placeholder, "[Image #2]");
}

#[test]
fn test_prompt_attachment_state_reset_clears_attachments_and_restarts_numbering() {
    // Arrange
    let mut attachment_state = PromptAttachmentState::default();
    let _ = attachment_state.register_local_image(PathBuf::from("/tmp/first-image.png"), 0);
    attachment_state.pending_image_paste = Some(PendingImagePaste {
        anchor: Some(0),
        request_id: 7,
    });
    let was_pasting_image = attachment_state.is_pasting_image();

    // Act
    attachment_state.reset();
    let placeholder =
        attachment_state.register_local_image(PathBuf::from("/tmp/second-image.png"), 0);

    // Assert
    assert!(was_pasting_image);
    assert!(!attachment_state.is_pasting_image());
    assert_eq!(attachment_state.attachments.len(), 1);
    assert_eq!(attachment_state.next_attachment_number, 2);
    assert_eq!(placeholder, "[Image #1]");
}

/// Returns attachment state with one running capture anchored at `anchor`.
fn pasting_attachment_state(anchor: Option<usize>) -> PromptAttachmentState {
    PromptAttachmentState {
        pending_image_paste: Some(PendingImagePaste {
            anchor,
            request_id: 1,
        }),
        ..PromptAttachmentState::default()
    }
}

/// Returns the running capture's anchor.
fn pending_image_paste_anchor(attachment_state: &PromptAttachmentState) -> Option<usize> {
    attachment_state
        .pending_image_paste
        .expect("capture should still be pending")
        .anchor
}

#[test]
fn test_prompt_attachment_state_shifts_pending_image_paste_anchor_across_edits() {
    // Arrange
    let input = InputState::default();
    let edits = [
        // Insertion before the anchor shifts it right.
        (Some(5), (1, 1, 4), Some(8)),
        // Deletion before the anchor shifts it left.
        (Some(5), (1, 3, 1), Some(3)),
        // Typing at the anchor keeps the image before the typed text.
        (Some(5), (5, 5, 6), Some(5)),
        // Edits after the anchor leave it in place.
        (Some(5), (7, 9, 7), Some(5)),
        // Replacing text around the anchor moves it after the replacement.
        (Some(5), (3, 8, 4), Some(4)),
        // A dropped anchor stays dropped.
        (None, (0, 0, 1), None),
    ];

    for (anchor, (old_start, old_end, new_end), expected_anchor) in edits {
        let mut attachment_state = pasting_attachment_state(anchor);

        // Act
        attachment_state.sync_after_edit(&input, old_start, old_end, new_end);

        // Assert
        assert_eq!(
            pending_image_paste_anchor(&attachment_state),
            expected_anchor
        );
    }
}

#[test]
fn test_prompt_attachment_state_drops_pending_image_paste_anchor_on_text_replacement() {
    // Arrange
    let input = InputState::default();
    let mut history_restored = pasting_attachment_state(Some(3));
    let mut archived = pasting_attachment_state(Some(3));
    let mut draft_restored = pasting_attachment_state(Some(3));
    let mut idle = PromptAttachmentState::default();

    // Act
    history_restored.sync_after_history_restore(&input);
    archived.archive_current();
    draft_restored.restore_draft_revision(input.revision(), &input);
    idle.archive_current();

    // Assert
    assert_eq!(pending_image_paste_anchor(&history_restored), None);
    assert_eq!(pending_image_paste_anchor(&archived), None);
    assert_eq!(pending_image_paste_anchor(&draft_restored), None);
    assert!(!idle.is_pasting_image());
}

#[test]
fn test_insert_prompt_local_image_at_keeps_cursor_on_its_text() {
    // Arrange
    let cases = [
        // Cursor after the anchor moves past the placeholder.
        (8, 4, "abc [Image #1]defgh", 18),
        // Cursor at the anchor ends after the placeholder.
        (4, 4, "abc [Image #1]defgh", 14),
        // Cursor before the anchor stays put.
        (1, 4, "abc [Image #1]defgh", 1),
        // Anchors past the text end are clamped.
        (8, 20, "abc defgh[Image #1]", 8),
    ];

    for (cursor, anchor, expected_text, expected_cursor) in cases {
        let mut attachment_state = PromptAttachmentState::default();
        let mut history_state = PromptHistoryState::default();
        let mut slash_state = PromptSlashState::default();
        let mut input = InputState::with_text("abc defgh".to_string());
        input.cursor = cursor;

        // Act
        insert_prompt_local_image_at(
            &mut attachment_state,
            &mut history_state,
            &mut input,
            &mut slash_state,
            PathBuf::from("/tmp/image.png"),
            anchor,
        );

        // Assert
        assert_eq!(input.text(), expected_text);
        assert_eq!(input.cursor, expected_cursor);
        assert_eq!(attachment_state.attachments.len(), 1);
    }
}

#[test]
fn test_prompt_attachment_state_refresh_next_attachment_number_stays_monotonic() {
    // Arrange
    let mut attachment_state = PromptAttachmentState {
        archived_attachments: Vec::new(),
        attachments: vec![
            PromptAttachment::new(1, PathBuf::from("/tmp/first-image.png")),
            PromptAttachment::new(3, PathBuf::from("/tmp/third-image.png")),
        ],
        next_attachment_number: 99,
        pending_image_paste: None,
    };

    // Act
    attachment_state.refresh_next_attachment_number();

    // Assert
    assert_eq!(attachment_state.next_attachment_number, 4);
}

#[test]
fn test_prompt_attachment_state_ignores_revision_before_attachment_is_placed() {
    // Arrange
    let input = InputState::default();
    let mut attachment_state = PromptAttachmentState::default();
    attachment_state.attachments.push(PromptAttachment::new(
        1,
        PathBuf::from("/tmp/first-image.png"),
    ));

    // Act
    attachment_state.remember_current_revision(&input);

    // Assert
    assert_eq!(
        attachment_state.attachments[0].valid_locations,
        [] as [crate::domain::composer::AttachmentRevision; 0]
    );
}

#[test]
fn test_prompt_attachment_ignores_edits_while_not_in_input() {
    // Arrange
    let mut attachment = PromptAttachment::new(1, PathBuf::from("/tmp/first-image.png"));

    // Act
    attachment.apply_edit(0, 0, 1);

    // Assert
    assert_eq!(attachment.current_start, None);
}

#[test]
fn test_prompt_attachment_state_sync_restores_undone_attachment() {
    // Arrange
    let mut input = InputState::default();
    let mut attachment_state = PromptAttachmentState::default();
    let placeholder = insert_test_attachment(
        &mut attachment_state,
        &mut input,
        PathBuf::from("/tmp/first-image.png"),
    );
    attachment_state.remember_current_revision(&input);
    input.replace_range(0, placeholder.chars().count(), "");
    attachment_state.sync_after_edit(&input, 0, placeholder.chars().count(), 0);

    // Act
    input.undo();
    attachment_state.sync_after_history_restore(&input);

    // Assert
    assert_eq!(attachment_state.attachments.len(), 1);
    assert_eq!(
        attachment_state.archived_attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert_eq!(attachment_state.next_attachment_number, 2);
}

#[test]
fn test_prompt_attachment_state_does_not_activate_manually_entered_placeholder() {
    // Arrange
    let mut input = InputState::default();
    let mut attachment_state = PromptAttachmentState::default();
    let placeholder = insert_test_attachment(
        &mut attachment_state,
        &mut input,
        PathBuf::from("/tmp/first-image.png"),
    );
    attachment_state.remember_current_revision(&input);
    input.replace_range(0, placeholder.chars().count(), "");
    attachment_state.sync_after_edit(&input, 0, placeholder.chars().count(), 0);

    // Act
    let insert_start = input.cursor;
    input.insert_text(&placeholder);
    attachment_state.sync_after_edit(&input, insert_start, insert_start, input.cursor);

    // Assert
    assert_eq!(
        attachment_state.attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert_eq!(attachment_state.archived_attachments.len(), 1);
    let submission = drain_prompt_submission(&mut attachment_state, &mut input);
    assert_eq!(
        submission.attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert_eq!(submission.text, placeholder);
}

#[test]
fn test_prompt_attachment_state_prunes_attachment_after_revision_eviction() {
    // Arrange
    let mut input = InputState::default();
    let mut attachment_state = PromptAttachmentState::default();
    let placeholder = insert_test_attachment(
        &mut attachment_state,
        &mut input,
        PathBuf::from("/tmp/first-image.png"),
    );
    attachment_state.remember_current_revision(&input);
    input.replace_range(0, placeholder.chars().count(), "");
    attachment_state.sync_after_edit(&input, 0, placeholder.chars().count(), 0);
    for _ in 0..INPUT_HISTORY_LIMIT {
        let insert_start = input.cursor;
        input.insert_char('x');
        attachment_state.sync_after_edit(&input, insert_start, insert_start, input.cursor);
    }

    // Act
    let unreachable = attachment_state.prune_unreachable(&input);

    // Assert
    assert_eq!(
        attachment_state.archived_attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert_eq!(unreachable.len(), 1);
    assert_eq!(
        unreachable[0].local_image_path,
        PathBuf::from("/tmp/first-image.png")
    );
}

#[test]
fn test_prompt_slash_state_replace_available_agent_kinds_clears_unavailable_selection() {
    // Arrange
    let mut slash_state =
        PromptSlashState::with_available_agent_kinds(vec![AgentKind::Claude, AgentKind::Codex]);
    slash_state.selected_agent = Some(AgentKind::Claude);
    slash_state.selected_index = 2;
    slash_state.stage = PromptSlashStage::Model;

    // Act
    slash_state.replace_available_agent_kinds(vec![AgentKind::Codex]);

    // Assert
    assert_eq!(slash_state.available_agent_kinds, vec![AgentKind::Codex]);
    assert_eq!(slash_state.selected_agent, None);
    assert_eq!(slash_state.selected_index, 0);
    assert_eq!(slash_state.stage, PromptSlashStage::Agent);
}

#[test]
fn test_slash_suggestion_list_for_command_stage_has_description() {
    // Arrange
    let composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/model".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );

    // Act
    let suggestion_list = composer
        .slash_suggestion_list(AgentKind::Codex)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(
        suggestion_list,
        PromptSuggestionList {
            items: vec![PromptSuggestionItem {
                badge: None,
                detail: Some("Choose an agent and model for this session.".to_string()),
                label: "/model".to_string(),
                metadata: None,
            }],
            selected_index: 0,
            title: "Slash Command (j/k move, Enter select)".to_string(),
        }
    );
}

#[test]
fn test_slash_suggestion_list_for_command_stage_contains_matches_non_prefix_input() {
    // Arrange
    let composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/o".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );

    // Act
    let suggestion_list = composer
        .slash_suggestion_list(AgentKind::Codex)
        .expect("expected suggestion list");

    // Assert
    let labels = suggestion_list
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["/mode", "/model", "/reasoning"]);
}

#[test]
fn test_mode_is_selected_for_m_shortcut() {
    // Arrange
    let slash_state = PromptSlashState::default();

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/m", &slash_state, AgentKind::Codex, false)
            .expect("expected suggestion list");
    let selection = resolve_prompt_slash_selection("/m", &slash_state, AgentKind::Codex, false);

    // Assert
    assert_eq!(
        suggestion_list
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        vec!["/mode", "/model"]
    );
    assert_eq!(selection, Some(PromptSuggestionSelection::Command("/mode")));
}

#[test]
fn test_selected_slash_action_uses_fuzzy_matched_command() {
    // Arrange
    let composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/rsn".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );

    // Act
    let selection = composer.selected_slash_action(AgentKind::Codex);

    // Assert
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Command("/reasoning"))
    );
}

#[test]
fn test_slash_suggestion_list_for_agent_stage_uses_available_agent_kinds() {
    // Arrange
    let mut composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/model".to_string()),
        vec![AgentKind::Claude],
        Vec::new(),
    );
    composer.slash_state.stage = PromptSlashStage::Agent;

    // Act
    let suggestion_list = composer
        .slash_suggestion_list(AgentKind::Codex)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(suggestion_list.items.len(), 1);
    assert_eq!(suggestion_list.items[0].label, "claude");
}

#[test]
fn test_selected_slash_action_returns_selected_model() {
    // Arrange
    let mut composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/model".to_string()),
        vec![AgentKind::Claude],
        Vec::new(),
    );
    composer.slash_state.stage = PromptSlashStage::Model;
    composer.slash_state.selected_agent = Some(AgentKind::Claude);

    // Act
    let selection = composer.selected_slash_action(AgentKind::Codex);

    // Assert
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Model(AgentSelection::new(
            AgentKind::Claude,
            AgentModel::ClaudeFable51,
        )))
    );
}

#[test]
fn test_mode_stage_lists_modes_and_returns_selected_mode() {
    // Arrange
    let slash_state = PromptSlashState {
        selected_index: 1,
        stage: PromptSlashStage::Mode,
        ..PromptSlashState::default()
    };

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/mode", &slash_state, AgentKind::Codex, false)
            .expect("mode suggestions should render");
    let selection = resolve_prompt_slash_selection("/mode", &slash_state, AgentKind::Codex, false);

    // Assert
    assert_eq!(suggestion_list.title, "/mode (j/k move, Enter select)");
    assert_eq!(
        suggestion_list
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Auto Edit",
            "Auto Edit + Auto Address Comments",
            "Read Only"
        ]
    );
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Mode(
            PermissionMode::AutoEditAddressComments
        ))
    );
}

#[test]
fn test_selected_slash_action_returns_selected_reasoning_level() {
    // Arrange
    let mut composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/reasoning".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );
    composer.slash_state.stage = PromptSlashStage::Reasoning;
    composer.slash_state.selected_index = 2;

    // Act
    let selection = composer.selected_slash_action(AgentKind::Codex);

    // Assert
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Reasoning(ReasoningLevel::High))
    );
}

#[test]
fn test_fast_command_is_available_only_for_supported_agents() {
    for &agent_kind in AgentKind::ALL {
        // Arrange
        let slash_state = PromptSlashState::default();

        // Act
        let suggestion_list =
            build_prompt_slash_suggestion_list("/fast", &slash_state, agent_kind, false);
        let selection = resolve_prompt_slash_selection("/fast", &slash_state, agent_kind, false);

        // Assert
        if matches!(agent_kind, AgentKind::Claude | AgentKind::Codex) {
            let suggestion_list = suggestion_list.expect("fast command should render");
            assert_eq!(suggestion_list.items.len(), 1);
            assert_eq!(suggestion_list.items[0].label, "/fast");
            assert_eq!(
                suggestion_list.items[0].detail.as_deref(),
                Some("Toggle between normal and fast responses for this session.")
            );
            assert_eq!(selection, Some(PromptSuggestionSelection::Command("/fast")));
        } else {
            assert!(suggestion_list.is_none());
            assert_eq!(selection, None);
        }
    }
}

#[test]
fn test_selected_slash_action_clamps_stale_command_index() {
    // Arrange
    let mut composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/s".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );
    composer.slash_state.selected_index = 9;

    // Act
    let selection = composer.selected_slash_action(AgentKind::Codex);

    // Assert
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Command("/reasoning"))
    );
}

#[test]
fn test_model_stage_suggestion_list_prefers_available_session_agent_when_unset() {
    // Arrange
    let mut composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/model".to_string()),
        vec![AgentKind::Antigravity, AgentKind::Codex],
        Vec::new(),
    );
    composer.slash_state.stage = PromptSlashStage::Model;

    // Act
    let suggestion_list = composer
        .slash_suggestion_list(AgentKind::Codex)
        .expect("expected suggestion list");

    // Assert
    let labels = suggestion_list
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            "gpt-6-astra".to_string(),
            model_fixture::CODEX_MODEL_ID.to_string(),
            "gpt-6-luna".to_string(),
            "gpt-5.6-terra".to_string(),
            "gpt-5.3-codex-spark".to_string(),
        ]
    );
}

#[test]
fn test_reasoning_stage_suggestion_list_omits_default_option() {
    // Arrange
    let mut composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/reasoning".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );
    composer.slash_state.stage = PromptSlashStage::Reasoning;

    // Act
    let suggestion_list = composer
        .slash_suggestion_list(AgentKind::Codex)
        .expect("expected suggestion list");
    let labels = suggestion_list
        .items
        .into_iter()
        .map(|item| item.label)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(labels, vec!["low", "medium", "high", "xhigh", "max"]);
}

#[test]
fn test_prompt_composer_delete_range_removes_whole_image_token() {
    // Arrange
    let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
    composer.insert_text("Review ");
    composer.insert_local_image(PathBuf::from("/tmp/image.png"));
    composer.insert_text(" now");

    // Act
    composer.delete_range(10, 11);

    // Assert
    assert_eq!(composer.input.text(), "Review  now");
    assert_eq!(
        composer.attachment_state.attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
    assert_eq!(composer.attachment_state.archived_attachments.len(), 1);
    assert_eq!(composer.attachment_state.next_attachment_number, 2);
}

#[test]
fn test_prompt_composer_insert_char_keeps_attachment_position_synchronized() {
    // Arrange
    let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
    composer.insert_local_image(PathBuf::from("/tmp/image.png"));
    composer.input.cursor = 0;

    // Act
    composer.insert_char('x');
    let submission = composer.take_submission();

    // Assert
    assert_eq!(submission.text, "x[Image #1]");
    assert_eq!(submission.attachments.len(), 1);
    assert_eq!(submission.attachments[0].current_start, Some(1));
}

#[test]
fn test_take_submission_filters_deleted_attachment_placeholders() {
    // Arrange
    let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
    composer.insert_text("One ");
    composer.insert_local_image(PathBuf::from("/tmp/one.png"));
    composer.insert_text(" two ");
    composer.insert_local_image(PathBuf::from("/tmp/two.png"));
    composer.delete_range(4, 15);

    // Act
    let submission = composer.take_submission();

    // Assert
    assert_eq!(submission.text, "One two [Image #2]");
    assert_eq!(submission.attachments.len(), 1);
    assert_eq!(submission.attachments[0].placeholder, "[Image #2]");
}

#[test]
fn test_drain_prompt_submission_keeps_raw_at_lookup_text() {
    // Arrange
    let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
    composer.input =
        InputState::with_text("Check @src/main.rs and @docs/guide.md before @".to_string());

    // Act
    let submission = composer.take_submission();

    // Assert
    assert_eq!(
        submission.text,
        "Check @src/main.rs and @docs/guide.md before @"
    );
    assert_eq!(submission.attachments.len(), 0);
}

#[test]
fn test_drain_prompt_submission_preserves_email_lookalikes() {
    // Arrange
    let mut composer = PromptComposerState::new(AgentKind::ALL.to_vec());
    composer.input = InputState::with_text("Notify user@example.com and @!".to_string());

    // Act
    let submission = composer.take_submission();

    // Assert
    assert_eq!(submission.text, "Notify user@example.com and @!");
    assert_eq!(
        submission.attachments,
        [] as [crate::domain::composer::PromptAttachment; 0]
    );
}

#[test]
fn test_render_prompt_text_for_agent_quotes_user_at_lookups() {
    // Arrange
    let prompt_text = "Check @src/main.rs and (@docs/guide.md)";

    // Act
    let rendered_text = render_prompt_text_for_agent(prompt_text);

    // Assert
    assert_eq!(
        rendered_text,
        "Check \"src/main.rs\" and (\"docs/guide.md\")"
    );
}

/// Ensures prompt preparation does not special-case literal `looked/up/`
/// text beyond ordinary `@` lookup quoting.
#[test]
fn test_render_prompt_text_for_agent_preserves_literal_looked_up_paths() {
    // Arrange
    let prompt_text = "Check looked/up/README.md, @looked/up/Cargo.toml, \
                       \"looked/up/src/main.rs\", or `looked/up/lib.rs`";

    // Act
    let rendered_text = render_prompt_text_for_agent(prompt_text);

    // Assert
    assert_eq!(
        rendered_text,
        "Check looked/up/README.md, \"looked/up/Cargo.toml\", \"looked/up/src/main.rs\", or \
         `looked/up/lib.rs`"
    );
}

#[test]
fn test_render_prompt_text_for_agent_preserves_non_lookup_at_tokens() {
    // Arrange
    let prompt_text = "Notify user@example.com and leave @ alone";

    // Act
    let rendered_text = render_prompt_text_for_agent(prompt_text);

    // Assert
    assert_eq!(rendered_text, "Notify user@example.com and leave @ alone");
}

#[test]
fn test_current_line_delete_range_returns_first_line_range() {
    // Arrange
    let mut input = InputState::with_text("first line\nsecond line".to_string());
    input.cursor = 0;

    // Act
    let delete_range = current_line_delete_range(&input);

    // Assert
    assert_eq!(delete_range, Some((0, 11)));
}

#[test]
fn test_slash_suggestion_list_includes_apply_command() {
    // Arrange
    let composer = PromptComposerState::with_input_and_history(
        InputState::with_text("/a".to_string()),
        AgentKind::ALL.to_vec(),
        Vec::new(),
    );

    // Act
    let suggestion_list = composer
        .slash_suggestion_list(AgentKind::Codex)
        .expect("expected suggestion list");

    // Assert
    assert_eq!(suggestion_list.items[0].label, "/apply");
    assert_eq!(
        suggestion_list.items[0].detail.as_deref(),
        Some("Verify focused-review suggestions, then apply the correct ones.")
    );
}

#[test]
fn test_prompt_slash_command_list_omits_apply_when_disabled() {
    // Arrange
    let slash_state = PromptSlashState::default();

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/", &slash_state, AgentKind::Codex, false)
            .expect("expected suggestion list");
    let labels = suggestion_list
        .items
        .iter()
        .map(|item| item.label.as_str())
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        labels,
        vec!["/mode", "/model", "/reasoning", "/style", "/fast"]
    );
    assert_eq!(suggestion_list.selected_index, 0);
}

#[test]
fn plan_command_availability_keeps_rendering_selection_and_navigation_aligned() {
    for enabled in [false, true] {
        // Arrange
        let mut slash_state = PromptSlashState {
            allow_plan_command: enabled,
            ..PromptSlashState::default()
        };
        slash_state.reset();

        // Act
        let menu = build_prompt_slash_suggestion_list("/", &slash_state, AgentKind::Codex, false)
            .expect("command menu");
        let selection =
            resolve_prompt_slash_selection("/plan", &slash_state, AgentKind::Codex, false);
        let count = prompt_slash_option_count("/plan", &slash_state, AgentKind::Codex, false);

        // Assert
        assert_eq!(menu.items.iter().any(|item| item.label == "/plan"), enabled);
        assert_eq!(
            selection,
            enabled.then_some(PromptSuggestionSelection::Command("/plan"))
        );
        assert_eq!(count, usize::from(enabled));
    }
}

#[test]
fn test_prompt_slash_command_list_omits_fast_for_unsupported_agent() {
    // Arrange
    let slash_state = PromptSlashState::default();

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/", &slash_state, AgentKind::Gemini, false)
            .expect("expected suggestion list");

    // Assert
    assert_eq!(
        suggestion_list
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>(),
        vec!["/mode", "/model", "/reasoning", "/style"]
    );
}

#[test]
fn test_prompt_slash_selection_uses_filtered_command_indexes() {
    // Arrange
    let slash_state = PromptSlashState {
        selected_index: 0,
        ..PromptSlashState::default()
    };

    // Act
    let selection = resolve_prompt_slash_selection("/", &slash_state, AgentKind::Codex, false);

    // Assert
    assert_eq!(selection, Some(PromptSuggestionSelection::Command("/mode")));
}

#[test]
fn test_style_stage_lists_descriptions_and_returns_selected_style() {
    // Arrange
    let slash_state = PromptSlashState {
        selected_index: 2,
        stage: PromptSlashStage::Style,
        ..PromptSlashState::default()
    };

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/style", &slash_state, AgentKind::Codex, false)
            .expect("expected style suggestions");
    let selection = resolve_prompt_slash_selection("/style", &slash_state, AgentKind::Codex, false);

    // Assert
    assert_eq!(
        suggestion_list.title,
        "/style Response style (j/k move, Enter select)"
    );
    assert_eq!(
        suggestion_list
            .items
            .iter()
            .map(|item| (item.label.as_str(), item.detail.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            (
                "Concise",
                Some("Compact answers with essential results, caveats, and verification.")
            ),
            (
                "Balanced",
                Some("Enough context to understand and verify without exhaustive detail.")
            ),
            (
                "Detailed",
                Some("Thorough decisions, trade-offs, effects, and verification.")
            ),
        ]
    );
    assert_eq!(suggestion_list.selected_index, 2);
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Style(ResponseStyle::Detailed))
    );
}

/// Returns one harness model option.
fn harness_option(
    model: AgentModel,
    billing: ModelBilling,
    provider: &str,
    status: HarnessModelStatus,
) -> HarnessModelOption {
    HarnessModelOption {
        billing,
        context_window: None,
        loaded_context_window: None,
        model,
        provider: provider.to_string(),
        status,
    }
}

/// Returns the served local model `name`.
fn served(name: &str) -> AgentModel {
    AgentModel::OpenAiCompatible(ServedModel::new(name).expect("served name"))
}

/// Returns a Harness `/model` stage over `harness_models`.
fn harness_model_stage(
    harness_models: Vec<HarnessModelOption>,
    selected_index: usize,
) -> PromptSlashState {
    PromptSlashState {
        harness_models,
        selected_agent: Some(AgentKind::Harness),
        selected_index,
        stage: PromptSlashStage::Model,
        ..PromptSlashState::with_available_agent_kinds(vec![AgentKind::Harness])
    }
}

/// Returns local and paid models covering every row label shape.
fn labeled_harness_models() -> Vec<HarnessModelOption> {
    vec![
        HarnessModelOption {
            context_window: Some(40_960),
            loaded_context_window: Some(32_768),
            ..harness_option(
                served("qwen3:latest"),
                ModelBilling::Local,
                "Ollama",
                HarnessModelStatus::Ready,
            )
        },
        HarnessModelOption {
            loaded_context_window: Some(4096),
            ..harness_option(
                served("llama3.2:3b"),
                ModelBilling::Local,
                "Ollama",
                HarnessModelStatus::Ready,
            )
        },
        harness_option(
            served("gemma:latest"),
            ModelBilling::Local,
            "Ollama",
            HarnessModelStatus::NoToolSupport,
        ),
        HarnessModelOption {
            context_window: Some(262_144),
            ..harness_option(
                AgentModel::MuseSpark13,
                ModelBilling::Paid,
                "Muse",
                HarnessModelStatus::Ready,
            )
        },
        harness_option(
            AgentModel::KimiK3,
            ModelBilling::Paid,
            "Kimi",
            HarnessModelStatus::MissingConfiguration("KIMI_API_KEY"),
        ),
        HarnessModelOption {
            context_window: Some(512),
            ..harness_option(
                served("tiny"),
                ModelBilling::Local,
                "OpenAI-compatible server",
                HarnessModelStatus::Ready,
            )
        },
        harness_option(
            served("plain"),
            ModelBilling::Local,
            "OpenAI-compatible server",
            HarnessModelStatus::Ready,
        ),
    ]
}

#[test]
fn harness_model_stage_labels_local_free_and_paid_models() {
    // Arrange
    let slash_state = harness_model_stage(labeled_harness_models(), 0);

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/model", &slash_state, AgentKind::Harness, true)
            .expect("harness models should render");

    // Assert
    assert_eq!(
        suggestion_list.title,
        "/model Harness model (j/k move, Enter select)"
    );
    let rows = suggestion_list
        .items
        .iter()
        .map(|item| {
            (
                item.badge.as_deref().unwrap_or_default(),
                item.label.trim_end(),
                item.metadata.as_deref().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            ("local", "qwen3:latest", "free · Ollama · runs 32k ctx"),
            (
                "local",
                "llama3.2:3b",
                "free · Ollama · runs 4k ctx, needs 16k"
            ),
            ("local", "gemma:latest", "free · Ollama · no tool calls"),
            ("paid", "muse-spark-1.3", "Muse · 256k ctx"),
            ("paid", "kimi-k3", "Kimi · needs KIMI_API_KEY"),
            (
                "local",
                "tiny",
                "free · OpenAI-compatible server · up to 512 ctx"
            ),
            ("local", "plain", "free · OpenAI-compatible server"),
        ]
    );
    // Metadata starts in one column after each `[badge] label`.
    let metadata_columns = suggestion_list
        .items
        .iter()
        .map(|item| item.badge.as_deref().unwrap_or_default().len() + item.label.len())
        .collect::<Vec<_>>();
    assert!(
        metadata_columns
            .iter()
            .all(|column| *column == "paid".len() + "muse-spark-1.3".len())
    );
    assert!(
        suggestion_list
            .items
            .iter()
            .all(|item| item.detail.is_none())
    );
}

#[test]
fn harness_model_stage_selects_only_models_that_can_run() {
    // Arrange
    let harness_models = vec![
        harness_option(
            served("qwen3:latest"),
            ModelBilling::Local,
            "Ollama",
            HarnessModelStatus::Ready,
        ),
        harness_option(
            AgentModel::KimiK3,
            ModelBilling::Paid,
            "Kimi",
            HarnessModelStatus::MissingConfiguration("KIMI_API_KEY"),
        ),
    ];

    // Act
    let local = resolve_prompt_slash_selection(
        "/model",
        &harness_model_stage(harness_models.clone(), 0),
        AgentKind::Harness,
        true,
    );
    let missing_key = resolve_prompt_slash_selection(
        "/model",
        &harness_model_stage(harness_models, 1),
        AgentKind::Harness,
        true,
    );

    // Assert
    assert_eq!(
        local,
        Some(PromptSuggestionSelection::Model(AgentSelection::new(
            AgentKind::Harness,
            served("qwen3:latest"),
        )))
    );
    assert_eq!(missing_key, None);
}

#[test]
fn harness_model_stage_explains_how_to_add_local_models() {
    // Arrange
    let slash_state = harness_model_stage(
        vec![harness_option(
            AgentModel::MuseSpark13,
            ModelBilling::Paid,
            "Muse",
            HarnessModelStatus::Ready,
        )],
        0,
    );

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/model", &slash_state, AgentKind::Harness, true)
            .expect("harness models should render");
    let hint_selection =
        resolve_prompt_slash_selection("/model", &slash_state, AgentKind::Harness, true);

    // Assert
    assert_eq!(suggestion_list.items[0].label.trim_end(), "no local models");
    assert_eq!(
        suggestion_list.items[0].metadata.as_deref(),
        Some("start Ollama or set OPENAI_COMPATIBLE_BASE_URL")
    );
    assert_eq!(suggestion_list.items[1].label.trim_end(), "muse-spark-1.3");
    assert_eq!(hint_selection, None);
}

#[test]
fn harness_model_stage_lists_the_paid_catalog_before_discovery() {
    // Arrange
    let slash_state = harness_model_stage(Vec::new(), 1);

    // Act
    let suggestion_list =
        build_prompt_slash_suggestion_list("/model", &slash_state, AgentKind::Harness, true)
            .expect("harness catalog should render");
    let selection =
        resolve_prompt_slash_selection("/model", &slash_state, AgentKind::Harness, true);

    // Assert
    assert_eq!(
        suggestion_list.items.len(),
        AgentKind::Harness.models().len()
    );
    assert!(
        suggestion_list
            .items
            .iter()
            .all(|item| item.badge.as_deref() == Some("paid"))
    );
    assert_eq!(
        selection,
        Some(PromptSuggestionSelection::Model(AgentSelection::new(
            AgentKind::Harness,
            AgentModel::KimiK3,
        )))
    );
}
