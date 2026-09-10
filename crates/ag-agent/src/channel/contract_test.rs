use ag_protocol::ProtocolRequestProfile;

use crate::channel::contract::{
    AgentRequestKind, PersonalityPrompt, PersonalityPromptUpdate, TurnContinuation,
};

#[test]
fn test_personality_prompt_tracks_active_change_and_clear_state() {
    // Arrange / Act
    let changed = PersonalityPrompt::active("Review carefully.".to_string(), true);
    let unchanged = PersonalityPrompt::active("Review carefully.".to_string(), false);
    let cleared = PersonalityPrompt::cleared(true);
    let empty = PersonalityPrompt::cleared(false);

    // Assert
    assert_eq!(changed.current(), Some("Review carefully."));
    assert_eq!(
        changed.update(),
        &PersonalityPromptUpdate::Set("Review carefully.".to_string())
    );
    assert_eq!(unchanged.update(), &PersonalityPromptUpdate::Unchanged);
    assert_eq!(cleared.current(), None);
    assert_eq!(cleared.update(), &PersonalityPromptUpdate::Clear);
    assert_eq!(empty.update(), &PersonalityPromptUpdate::Unchanged);
}

#[test]
fn test_turn_continuation_fresh_has_no_context() {
    // Arrange / Act
    let continuation = TurnContinuation::fresh();

    // Assert
    assert_eq!(continuation.replay_transcript(), None);
    assert_eq!(continuation.provider_conversation_id(), None);
    assert_eq!(continuation.persisted_instruction_conversation_id(), None);
}

#[test]
fn test_turn_continuation_replaying_exposes_transcript_only() {
    // Arrange
    let continuation = TurnContinuation::replaying("prior turn".to_string());

    // Act
    let parts = continuation.clone().into_parts();

    // Assert
    assert_eq!(continuation.replay_transcript(), Some("prior turn"));
    assert_eq!(continuation.provider_conversation_id(), None);
    assert!(parts.live_transcript.is_none());
    assert_eq!(parts.persisted_instruction_conversation_id, None);
    assert_eq!(parts.provider_conversation_id, None);
    assert_eq!(parts.replay_transcript.as_deref(), Some("prior turn"));
}

#[test]
fn test_turn_continuation_provider_exposes_persisted_context() {
    // Arrange / Act
    let continuation = TurnContinuation::provider(
        None,
        Some("instruction-1".to_string()),
        Some("thread-1".to_string()),
        Some("prior turn".to_string()),
    );

    // Assert
    assert_eq!(continuation.replay_transcript(), Some("prior turn"));
    assert_eq!(continuation.provider_conversation_id(), Some("thread-1"));
    assert_eq!(
        continuation.persisted_instruction_conversation_id(),
        Some("instruction-1")
    );
}

#[test]
/// Ensures session request kinds derive the session-turn protocol
/// profile.
fn test_agent_request_kind_session_variants_use_session_protocol_profile() {
    // Arrange
    let start = AgentRequestKind::SessionStart;
    let resume = AgentRequestKind::SessionResume;

    // Act
    let start_profile = start.protocol_profile();
    let resume_profile = resume.protocol_profile();

    // Assert
    assert_eq!(start_profile, ProtocolRequestProfile::SessionTurn);
    assert_eq!(resume_profile, ProtocolRequestProfile::SessionTurn);
}

#[test]
/// Ensures utility prompts derive the utility protocol profile.
fn test_agent_request_kind_utility_prompt_uses_utility_protocol_profile() {
    // Arrange
    let request_kind = AgentRequestKind::UtilityPrompt;

    // Act
    let protocol_profile = request_kind.protocol_profile();

    // Assert
    assert_eq!(protocol_profile, ProtocolRequestProfile::UtilityPrompt);
}

#[test]
fn focused_review_request_uses_focused_review_protocol_profile() {
    // Arrange
    let request_kind = AgentRequestKind::FocusedReview;

    // Act
    let protocol_profile = request_kind.protocol_profile();

    // Assert
    assert_eq!(protocol_profile, ProtocolRequestProfile::FocusedReview);
}

#[test]
/// Ensures account-read requests are non-session utility requests.
fn test_agent_request_kind_account_read_uses_utility_protocol_profile() {
    // Arrange
    let request_kind = AgentRequestKind::AccountRead;

    // Act
    let protocol_profile = request_kind.protocol_profile();

    // Assert
    assert_eq!(protocol_profile, ProtocolRequestProfile::UtilityPrompt);
}
