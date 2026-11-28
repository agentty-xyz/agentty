//! Session message kind, transcript formatting, replay, and stored-content
//! normalization tests.

use crate::message::{
    SessionMessage, SessionMessageKind, SessionTranscript, normalized_message_content,
    stored_message_content,
};

#[test]
fn ephemeral_transcripts_keep_projection_behavior_without_session_persistence() {
    // Arrange
    let mut transcript = SessionTranscript::ephemeral(Vec::new());

    // Act
    transcript.append_message(SessionMessageKind::UserPrompt, "Task input");
    transcript.append_message(SessionMessageKind::WorkflowNotice, "Task diagnostic");

    // Assert
    assert!(!transcript.should_persist());
    assert!(SessionTranscript::default().should_persist());
    assert!(SessionTranscript::new(Vec::new()).should_persist());
    assert_eq!(transcript.messages().len(), 2);
    assert!(
        transcript
            .replay_text()
            .expect("replay")
            .contains("Task diagnostic")
    );
}

#[test]
fn test_session_message_kind_round_trips_database_value() {
    // Arrange
    let kinds = [
        SessionMessageKind::UserPrompt,
        SessionMessageKind::AgentPrompt,
        SessionMessageKind::AssistantAnswer,
        SessionMessageKind::WorkflowNotice,
    ];

    // Act
    let parsed = kinds.map(|kind| {
        kind.as_str()
            .parse::<SessionMessageKind>()
            .expect("kind should parse")
    });

    // Assert
    assert_eq!(parsed, kinds);
    assert_eq!(SessionMessageKind::AgentPrompt.to_string(), "agent_prompt");
    assert!(SessionMessageKind::AgentPrompt.is_conversation_message());
    assert!(SessionMessageKind::AgentPrompt.is_prompt());
    assert!(!SessionMessageKind::AssistantAnswer.is_prompt());
}

#[test]
fn test_session_message_kind_rejects_unknown_database_value() {
    // Arrange / Act
    let error = "unknown"
        .parse::<SessionMessageKind>()
        .expect_err("unknown kind should fail");

    // Assert
    assert_eq!(error.to_string(), "unknown session message kind `unknown`");
}

#[test]
fn test_session_transcript_formats_messages_by_position() {
    // Arrange
    let messages = vec![
        SessionMessage::conversation(2, SessionMessageKind::AssistantAnswer, " answer\n"),
        SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "\nprompt "),
    ];

    // Act
    let transcript = SessionTranscript::new(messages);

    // Assert
    assert_eq!(
        transcript.replay_text().expect("expected replay text"),
        " › prompt\n\nanswer\n\n"
    );
}

#[test]
fn test_session_transcript_content_hash_tracks_exact_message_content() {
    // Arrange
    let original = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        "alpha",
    )]);
    let replacement = SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        "bravo",
    )]);

    // Act
    let original_hash = original.content_hash();
    let replacement_hash = replacement.content_hash();

    // Assert
    assert_eq!(
        original.total_content_len(),
        replacement.total_content_len()
    );
    assert_ne!(original_hash, replacement_hash);
}

#[test]
fn test_session_transcript_formats_multiline_user_prompt() {
    // Arrange
    let messages = vec![SessionMessage::conversation(
        1,
        SessionMessageKind::UserPrompt,
        "first\nsecond",
    )];

    // Act
    let transcript = SessionTranscript::new(messages);

    // Assert
    assert_eq!(
        transcript.replay_text().expect("expected replay text"),
        " › first\n   second\n\n"
    );
}

#[test]
fn test_session_transcript_replays_generated_agent_prompt() {
    // Arrange
    let messages = vec![SessionMessage::conversation(
        1,
        SessionMessageKind::AgentPrompt,
        "resolve review comments",
    )];

    // Act
    let transcript = SessionTranscript::new(messages);

    // Assert
    assert_eq!(
        transcript.replay_text().expect("expected replay text"),
        " › resolve review comments\n\n"
    );
    assert_eq!(
        stored_message_content(SessionMessageKind::AgentPrompt, "\n  generated  \n"),
        "  generated"
    );
}

#[test]
fn test_session_transcript_formats_clarification_prompt_with_question_spacing() {
    // Arrange
    let messages = vec![SessionMessage::conversation(
        1,
        SessionMessageKind::UserPrompt,
        "Clarifications:\n1. Q: Need target branch?\n   A: main\n2. Q: Need tests?\n   A: yes",
    )];

    // Act
    let transcript = SessionTranscript::new(messages);

    // Assert
    assert_eq!(
        transcript.replay_text().expect("expected replay text"),
        " › Clarifications:\n   \n   1. Q: Need target branch?\n      A: main\n   \n   2. Q: Need \
         tests?\n      A: yes\n\n"
    );
}

#[test]
fn test_session_transcript_formats_prompt_spacing_after_assistant_answer() {
    // Arrange
    let messages = vec![
        SessionMessage::conversation(0, SessionMessageKind::AssistantAnswer, "answer"),
        SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "next prompt"),
    ];

    // Act
    let transcript = SessionTranscript::new(messages);

    // Assert
    assert_eq!(
        transcript.replay_text().expect("expected replay text"),
        "answer\n\n\n › next prompt\n\n"
    );
}

#[test]
fn test_session_transcript_conversation_replay_text_excludes_workflow_notices() {
    // Arrange
    let transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "review changes"),
        SessionMessage::new(
            1,
            SessionMessageKind::WorkflowNotice,
            "[Commit] No changes to commit.\n",
        ),
        SessionMessage::conversation(2, SessionMessageKind::AssistantAnswer, "done"),
    ]);

    // Act
    let conversation_text = transcript
        .conversation_replay_text()
        .expect("conversation text should render");

    // Assert
    assert_eq!(conversation_text, " › review changes\n\ndone\n\n");
    assert!(!conversation_text.contains("[Commit]"));
}

#[test]
fn test_session_transcript_conversation_replay_text_ignores_notice_only_transcript() {
    // Arrange
    let transcript = SessionTranscript::new(vec![SessionMessage::new(
        0,
        SessionMessageKind::WorkflowNotice,
        "[Sync] Complete.\n",
    )]);

    // Act
    let conversation_text = transcript.conversation_replay_text();

    // Assert
    assert_eq!(conversation_text, None);
}

#[test]
fn provider_replay_keeps_workflow_notices_but_not_context_sandbox_or_notes_notices() {
    // Arrange
    let transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "keep going"),
        SessionMessage::new(
            1,
            SessionMessageKind::WorkflowNotice,
            "\n[Context] Summarized earlier turns to fit the context window.\n",
        ),
        SessionMessage::new(
            1,
            SessionMessageKind::WorkflowNotice,
            "\n[Sandbox Warning] Commands run without the sandbox. Install bubblewrap.\n",
        ),
        SessionMessage::new(
            1,
            SessionMessageKind::WorkflowNotice,
            "\n[Sandbox] A command may have been blocked by the sandbox.\n",
        ),
        SessionMessage::new(
            2,
            SessionMessageKind::WorkflowNotice,
            "\n[Context Warning] Could not summarize earlier turns (transport).\n",
        ),
        SessionMessage::new(
            2,
            SessionMessageKind::WorkflowNotice,
            "\n[Session Notes Warning] The session notes did not add 1 note from node `plan`.\n",
        ),
        SessionMessage::new(
            3,
            SessionMessageKind::WorkflowNotice,
            "[Commit] No changes to commit.\n",
        ),
        SessionMessage::conversation(4, SessionMessageKind::AssistantAnswer, "done"),
    ]);
    let notices_only = SessionTranscript::new(vec![SessionMessage::new(
        0,
        SessionMessageKind::WorkflowNotice,
        "[Context] Summarized earlier turns to fit the context window.",
    )]);

    // Act
    let provider_text = transcript
        .provider_replay_text()
        .expect("provider text should render");
    let displayed = transcript
        .replay_text()
        .expect("display text should render");

    // Assert
    assert_eq!(
        provider_text,
        " › keep going\n\n[Commit] No changes to commit.\ndone\n\n"
    );
    assert!(displayed.contains("[Context] Summarized"));
    assert!(displayed.contains("[Context Warning]"));
    assert!(displayed.contains("[Sandbox Warning]"));
    assert!(displayed.contains("[Sandbox] A command"));
    assert!(displayed.contains("[Session Notes Warning]"));
    assert_eq!(notices_only.provider_replay_text(), None);
}

#[test]
fn test_session_transcript_replay_text_is_absent_without_content() {
    // Arrange
    let transcript = SessionTranscript::new(Vec::new());

    // Act
    let replay_text = transcript.replay_text();

    // Assert
    assert_eq!(replay_text, None);
}

#[test]
fn test_session_transcript_append_message_preserves_constructor_ordering() {
    // Arrange
    let mut transcript = SessionTranscript::new(vec![
        SessionMessage::conversation(4, SessionMessageKind::AssistantAnswer, "first answer"),
        SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "prompt"),
    ]);

    // Act
    transcript.append_message(SessionMessageKind::WorkflowNotice, "[Sync] Complete.\n");
    let reconstructed = SessionTranscript::new(transcript.messages().to_vec());

    // Assert
    assert_eq!(
        transcript.messages(),
        &[
            SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "prompt"),
            SessionMessage::conversation(4, SessionMessageKind::AssistantAnswer, "first answer",),
            SessionMessage::new(5, SessionMessageKind::WorkflowNotice, "[Sync] Complete.\n",),
        ]
    );
    assert_eq!(transcript.content_hash(), reconstructed.content_hash());
}

#[test]
fn test_session_transcript_incremental_hash_matches_reconstructed_hash() {
    // Arrange
    let mut appended = SessionTranscript::default();
    let mut ephemeral = SessionTranscript::ephemeral(Vec::new());
    let appends = [
        (SessionMessageKind::UserPrompt, "\nprompt "),
        (SessionMessageKind::AssistantAnswer, "  \n"),
        (SessionMessageKind::AssistantAnswer, " answer\n"),
        (SessionMessageKind::WorkflowNotice, "   "),
        (SessionMessageKind::WorkflowNotice, "[Sync] Complete.\n"),
        (SessionMessageKind::UserPrompt, "\n"),
    ];

    // Act
    let mut hashes_after_each_append = Vec::new();
    for (kind, content) in appends {
        appended.append_message(kind, content);
        ephemeral.append_message(kind, content);
        hashes_after_each_append.push((
            appended.content_hash(),
            SessionTranscript::content_hash_for_messages(appended.messages()),
        ));
    }
    let reconstructed = SessionTranscript::new(appended.messages().to_vec());

    // Assert
    assert_eq!(
        SessionTranscript::default().content_hash(),
        SessionTranscript::content_hash_for_messages(&[])
    );
    assert_eq!(appended.messages().len(), 3);
    for (incremental_hash, full_hash) in hashes_after_each_append {
        assert_eq!(incremental_hash, full_hash);
    }
    assert_eq!(appended.content_hash(), reconstructed.content_hash());
    assert_eq!(appended.content_hash(), ephemeral.content_hash());
    assert_eq!(appended, reconstructed);
}

#[test]
fn test_session_transcript_incremental_hash_extends_constructed_and_cloned_transcripts() {
    // Arrange
    let constructed = SessionTranscript::new(vec![
        SessionMessage::conversation(4, SessionMessageKind::AssistantAnswer, "first answer"),
        SessionMessage::conversation(1, SessionMessageKind::UserPrompt, "prompt"),
    ]);
    let mut cloned = constructed.clone();

    // Act
    cloned.append_message(SessionMessageKind::UserPrompt, "follow-up");
    let reconstructed = SessionTranscript::new(cloned.messages().to_vec());

    // Assert
    assert_eq!(
        constructed.content_hash(),
        SessionTranscript::content_hash_for_messages(constructed.messages())
    );
    assert_ne!(cloned.content_hash(), constructed.content_hash());
    assert_eq!(cloned.content_hash(), reconstructed.content_hash());
    assert_eq!(cloned, reconstructed);
}

#[test]
fn test_session_transcript_ignores_empty_messages() {
    // Arrange
    let mut transcript = SessionTranscript::default();
    let empty_messages = [
        SessionMessage::new(0, SessionMessageKind::UserPrompt, "\n"),
        SessionMessage::new(1, SessionMessageKind::AssistantAnswer, "  "),
    ];

    // Act
    transcript.append_message(SessionMessageKind::UserPrompt, "\n");
    let replay_text = SessionTranscript::display_text_for_messages(&empty_messages);

    // Assert
    assert!(transcript.is_empty());
    assert_eq!(replay_text, "");
}

#[test]
fn test_session_transcript_total_content_len_updates_on_append() {
    // Arrange
    let mut transcript = SessionTranscript::new(vec![SessionMessage::conversation(
        4,
        SessionMessageKind::UserPrompt,
        "prompt",
    )]);

    // Act
    transcript.append_message(SessionMessageKind::AssistantAnswer, " answer\n");

    // Assert
    assert_eq!(
        transcript.total_content_len(),
        "prompt".len() + "answer".len()
    );
}

#[test]
fn test_normalized_message_content_removes_outer_whitespace_only() {
    // Arrange, Act, Assert
    assert_eq!(
        normalized_message_content("\n  keep\ninner spacing  \n"),
        "keep\ninner spacing"
    );
}

#[test]
fn test_stored_message_content_preserves_compatibility_spacing() {
    // Arrange
    let workflow_notice = "\n[Sync Error] failed\n";

    // Act
    let stored = stored_message_content(SessionMessageKind::WorkflowNotice, workflow_notice);

    // Assert
    assert_eq!(stored, workflow_notice);
}

#[test]
fn test_stored_message_content_preserves_user_prompt_indentation() {
    // Arrange, Act, Assert
    assert_eq!(
        stored_message_content(
            SessionMessageKind::UserPrompt,
            "\n    first\n        second  \n"
        ),
        "    first\n        second"
    );
}

#[test]
fn test_stored_message_content_normalizes_assistant_spacing() {
    // Arrange, Act, Assert
    assert_eq!(
        stored_message_content(SessionMessageKind::AssistantAnswer, "\n  hello  \n"),
        "hello"
    );
}

#[test]
fn activity_summary_is_durable_but_excluded_from_replay() {
    // Arrange
    let mut transcript = SessionTranscript::default();
    let kind: SessionMessageKind = "activity_summary".parse().expect("activity kind");

    // Act
    transcript.append_message(SessionMessageKind::AssistantAnswer, "Answer");
    transcript.append_message(kind, " Tools: Read ×1\n");

    // Assert
    assert_eq!(kind.as_str(), "activity_summary");
    assert!(!kind.is_conversation_message());
    assert!(!kind.is_prompt());
    assert_eq!(transcript.messages()[1].content, "Tools: Read ×1");
    assert_eq!(transcript.replay_text().as_deref(), Some("Answer\n\n"));
    assert_eq!(
        transcript.conversation_replay_text(),
        transcript.replay_text()
    );
}
