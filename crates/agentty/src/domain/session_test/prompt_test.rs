use ag_session::SessionStatus as Status;

use super::super::super::session_message::SessionTranscript;
use crate::test_support::SessionFixtureBuilder;

#[test]
fn test_latest_user_prompt_position_tracks_generated_turn() {
    // Arrange
    let mut session = SessionFixtureBuilder::new().status(Status::Review).build();
    session.transcript = Some(SessionTranscript::new(vec![
        crate::domain::session_message::SessionMessage::conversation(
            7,
            crate::domain::session_message::SessionMessageKind::AgentPrompt,
            "resolve comments",
        ),
    ]));

    // Act
    let latest_prompt_position = session.latest_user_prompt_position();

    // Assert
    assert_eq!(latest_prompt_position, Some(7));
}
