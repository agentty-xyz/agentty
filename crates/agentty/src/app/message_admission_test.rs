use ag_session::SessionStatus;

use crate::app::message_admission::{MessageIntent, MessagePlan};

/// Explicit expectations include statuses whose eligibility belongs to the
/// reply workflow, rather than introducing an admission transition graph.
const EXISTING_CASES: [(Option<SessionStatus>, MessagePlan); 12] = [
    (None, MessagePlan::Resume),
    (Some(SessionStatus::Draft), MessagePlan::Resume),
    (Some(SessionStatus::InProgress), MessagePlan::Queue),
    (Some(SessionStatus::Review), MessagePlan::Resume),
    (Some(SessionStatus::AgentReview), MessagePlan::Resume),
    (Some(SessionStatus::Question), MessagePlan::Resume),
    (Some(SessionStatus::Queued), MessagePlan::Resume),
    (Some(SessionStatus::Rebasing), MessagePlan::Queue),
    (Some(SessionStatus::Merging), MessagePlan::Resume),
    (Some(SessionStatus::Merged), MessagePlan::Resume),
    (Some(SessionStatus::Done), MessagePlan::Resume),
    (Some(SessionStatus::Canceled), MessagePlan::Resume),
];

#[test]
fn explicit_message_intent_takes_precedence_over_current_status() {
    // Arrange
    let modes = [
        (MessageIntent::Stage, MessagePlan::Stage),
        (MessageIntent::Start, MessagePlan::Start),
    ];

    for (mode, expected) in modes {
        for (status, _) in EXISTING_CASES {
            // Act
            let plan = MessagePlan::for_prompt(mode, status);

            // Assert
            assert_eq!(plan, expected, "{mode:?}, {status:?}");
        }
    }
}

#[test]
fn existing_terminal_prompts_queue_only_during_turns_and_rebases() {
    // Arrange
    for (status, expected) in EXISTING_CASES {
        // Act
        let plan = MessagePlan::for_prompt(MessageIntent::FollowUp, status);

        // Assert
        assert_eq!(plan, expected, "{status:?}");
    }
}

#[test]
fn api_messages_start_drafts_and_otherwise_follow_current_status() {
    // Arrange
    let cases = [
        (
            SessionStatus::Draft,
            MessagePlan::Start,
            MessagePlan::StageAndStart,
        ),
        (
            SessionStatus::InProgress,
            MessagePlan::Queue,
            MessagePlan::Queue,
        ),
        (
            SessionStatus::Review,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::AgentReview,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::Question,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::Queued,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::Rebasing,
            MessagePlan::Queue,
            MessagePlan::Queue,
        ),
        (
            SessionStatus::Merging,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::Merged,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::Done,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
        (
            SessionStatus::Canceled,
            MessagePlan::Resume,
            MessagePlan::Resume,
        ),
    ];

    for (status, regular, draft) in cases {
        for (is_draft, expected) in [(false, regular), (true, draft)] {
            // Act
            let plan = MessagePlan::for_api(status, is_draft);

            // Assert
            assert_eq!(plan, expected, "{status:?}, draft={is_draft}");
        }
    }
}
