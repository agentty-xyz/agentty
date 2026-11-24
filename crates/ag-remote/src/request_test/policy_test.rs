//! The policy of every request variant: the operation the host must serve,
//! the listed-session check, the capability that offers it, and how it
//! queues behind other changes of its session.

use std::sync::Arc;

use ag_session::SessionOperation;

use crate::protocol::{
    RemoteGraphAction, RemoteGraphActionRequest, RemoteRequest, RemoteResponse, RemoteResult,
    RemoteSessionMode, RequestedSessionId,
};
use crate::request::{CAPABILITIES, RequestOrder, RequestPolicy, execute};
use crate::test_support::{FailingEntropy, MemoryDeviceStore, RecordingBackend, StateFixture};

/// Session every sample names; the test backend never lists it.
const UNLISTED: &str = "unlisted";

/// How a request must run relative to others for the same session.
enum Order {
    /// Runs at once and changes nothing.
    Read,
    /// Waits behind the earlier changes of [`UNLISTED`].
    Queued,
    /// Changes the host but runs at once.
    Unqueued,
}

/// One sample of a request variant and the policy it must have.
struct Case {
    /// Operation the host must serve, `None` for requests every host serves.
    operation: Option<SessionOperation>,
    /// How the request queues; before policies, `mutated_session` returned
    /// the session for exactly the `Queued` requests.
    order: Order,
    /// A request of the variant, naming [`UNLISTED`] and carrying invalid
    /// fields wherever it can.
    sample: RemoteRequest,
    /// What the request answers for an unlisted session, `None` exactly
    /// when its frame carries no `session_id`.
    unlisted: Option<RemoteResult>,
}

/// Declares one [`Case`] per request variant behind the variant's pattern.
///
/// The patterns also form one irrefutable pattern in [`assert_covered`], so
/// a new variant stops this file compiling until it gets a case, and each
/// sample is checked against its own pattern.
macro_rules! cases {
    ($($pattern:pat => $case:expr,)+) => {
        /// Returns one case per request variant.
        fn cases() -> Vec<Case> {
            vec![$({
                let case: Case = $case;
                assert!(
                    matches!(case.sample, $pattern),
                    "the sample of `{}` is another variant",
                    stringify!($pattern),
                );

                case
            }),+]
        }

        /// Compiles only while the case patterns cover every variant.
        fn assert_covered(request: &RemoteRequest) {
            let ($($pattern)|+) = request;
        }
    };
}

cases! {
    RemoteRequest::ActOnSessionGraph(_) => Case {
        operation: Some(SessionOperation::GraphActions),
        order: Order::Queued,
        sample: RemoteRequest::ActOnSessionGraph(RemoteGraphActionRequest {
            action: RemoteGraphAction::Unsupported,
            additional_calls: None,
            budget_id: None,
            graph_id: "graph".to_string(),
            message: None,
            node_id: None,
            output_id: None,
            session_id: UNLISTED.into(),
            snapshot: "5eed".to_string(),
        }),
        unlisted: Some(not_found()),
    },
    RemoteRequest::AnswerQuestions { .. } => Case {
        operation: None,
        order: Order::Queued,
        sample: RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            graph_question_target: None,
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::CancelSession { .. } => Case {
        operation: None,
        order: Order::Queued,
        sample: RemoteRequest::CancelSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::CreateSession { .. } => Case {
        operation: Some(SessionOperation::CreateSession),
        order: Order::Unqueued,
        sample: RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Harness,
        },
        unlisted: None,
    },
    RemoteRequest::GenerateGraphProposal { .. } => Case {
        operation: Some(SessionOperation::GenerateGraphProposal),
        order: Order::Queued,
        sample: RemoteRequest::GenerateGraphProposal {
            message: String::new(),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::GetHost => Case {
        operation: Some(SessionOperation::HostContext),
        order: Order::Read,
        sample: RemoteRequest::GetHost,
        unlisted: None,
    },
    RemoteRequest::GetSession { .. } => Case {
        operation: None,
        order: Order::Read,
        sample: RemoteRequest::GetSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(RemoteResult::Ok(RemoteResponse::Session { session: None })),
    },
    RemoteRequest::GetSessionDiff { .. } => Case {
        operation: Some(SessionOperation::SessionDiff),
        order: Order::Read,
        sample: RemoteRequest::GetSessionDiff {
            path: Some("absent.rs".to_string()),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::GetSessionGraphs { .. } => Case {
        operation: Some(SessionOperation::Graphs),
        order: Order::Read,
        sample: RemoteRequest::GetSessionGraphs {
            graph_id: None,
            known: None,
            node_id: None,
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::ListProjectFiles => Case {
        operation: Some(SessionOperation::ProjectFiles),
        order: Order::Read,
        sample: RemoteRequest::ListProjectFiles,
        unlisted: None,
    },
    RemoteRequest::ListSessionFiles { .. } => Case {
        operation: Some(SessionOperation::SessionFiles),
        order: Order::Read,
        sample: RemoteRequest::ListSessionFiles {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::ListSessions => Case {
        operation: None,
        order: Order::Read,
        sample: RemoteRequest::ListSessions,
        unlisted: None,
    },
    RemoteRequest::MergeSession { .. } => Case {
        operation: Some(SessionOperation::MergeSession),
        order: Order::Queued,
        sample: RemoteRequest::MergeSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::PublishSession { .. } => Case {
        operation: Some(SessionOperation::PublishSession),
        order: Order::Unqueued,
        sample: RemoteRequest::PublishSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::SendMessage { .. } => Case {
        operation: None,
        order: Order::Queued,
        sample: RemoteRequest::SendMessage {
            message: "hi".to_string(),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::SetSessionSettings { .. } => Case {
        operation: Some(SessionOperation::SessionSettings),
        order: Order::Queued,
        sample: RemoteRequest::SetSessionSettings {
            agent: Some("vi".to_string()),
            model: Some("vim".to_string()),
            reasoning: Some("huge".to_string()),
            response_style: None,
            session_id: UNLISTED.into(),
            speed_mode: None,
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::StopTurn { .. } => Case {
        operation: Some(SessionOperation::StopTurn),
        order: Order::Queued,
        sample: RemoteRequest::StopTurn {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::SyncProject => Case {
        operation: Some(SessionOperation::SyncProject),
        order: Order::Unqueued,
        sample: RemoteRequest::SyncProject,
        unlisted: None,
    },
    RemoteRequest::SyncSession { .. } => Case {
        operation: Some(SessionOperation::RebaseSession),
        order: Order::Queued,
        sample: RemoteRequest::SyncSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::Unsupported => Case {
        operation: None,
        order: Order::Read,
        sample: RemoteRequest::Unsupported,
        unlisted: None,
    },
}

/// The answer to a request for a session the host does not list.
fn not_found() -> RemoteResult {
    RemoteResult::Error("Session not found".to_string())
}

/// Every operation a capability offers, plus the project status that
/// `list_sessions` reads when the host reports it.
fn every_operation() -> Vec<SessionOperation> {
    CAPABILITIES
        .iter()
        .map(|(operation, _)| *operation)
        .chain([SessionOperation::ProjectStatus])
        .collect()
}

/// Runs `request` on a host that serves `operations` and whose entropy
/// fails, returning the answer and the host calls it made.
async fn run(
    request: RemoteRequest,
    operations: &[SessionOperation],
) -> (RemoteResult, Vec<String>) {
    let fixture = StateFixture::new(
        MemoryDeviceStore::default(),
        RecordingBackend::supporting(operations),
        Arc::new(FailingEntropy),
    );
    let result = execute(
        fixture.state.sessions(),
        fixture.state.entropy.as_ref(),
        request,
    )
    .await;

    (result, fixture.backend.calls())
}

#[test]
fn every_request_has_the_policy_of_its_table_entry() {
    // Arrange
    let cases = cases();
    let unlisted = RequestedSessionId::from(UNLISTED);

    // Act
    let policies: Vec<(Option<SessionOperation>, RequestOrder<'_>)> = cases
        .iter()
        .map(|case| {
            assert_covered(&case.sample);
            let RequestPolicy { order, operation } = RequestPolicy::of(&case.sample);

            (operation, order)
        })
        .collect();

    // Assert
    let expected: Vec<(Option<SessionOperation>, RequestOrder<'_>)> = cases
        .iter()
        .map(|case| {
            let order = match case.order {
                Order::Read => RequestOrder::Read,
                Order::Queued => RequestOrder::Queued(&unlisted),
                Order::Unqueued => RequestOrder::Unqueued,
            };

            (case.operation, order)
        })
        .collect();
    assert_eq!(policies, expected);
}

#[test]
fn a_sample_has_an_unlisted_answer_exactly_when_it_names_a_session() {
    for case in cases() {
        // Arrange
        let has_unlisted_answer = case.unlisted.is_some();

        // Act
        let frame = serde_json::to_value(&case.sample).expect("request should encode");

        // Assert
        assert_eq!(
            frame.get("session_id").is_some(),
            has_unlisted_answer,
            "{:?}",
            case.sample
        );
    }
}

#[test]
fn capabilities_offer_exactly_the_operations_policies_require() {
    // Arrange
    let required: Vec<SessionOperation> = cases()
        .iter()
        .filter_map(|case| RequestPolicy::of(&case.sample).operation)
        .collect();

    // Act
    let offered: Vec<SessionOperation> = CAPABILITIES
        .iter()
        .map(|(operation, _)| *operation)
        .collect();

    // Assert
    for operation in &required {
        assert!(
            offered.contains(operation),
            "{operation:?} has no capability"
        );
    }
    for operation in &offered {
        assert!(
            required.contains(operation),
            "{operation:?} is offered but no request requires it"
        );
    }
    assert_eq!(offered.len(), required.len());
}

#[tokio::test]
async fn a_request_whose_operation_the_host_does_not_serve_reaches_nothing() {
    for case in cases() {
        // Arrange
        let Some(operation) = case.operation else {
            continue;
        };
        let served: Vec<SessionOperation> = every_operation()
            .into_iter()
            .filter(|served| *served != operation)
            .collect();

        // Act
        let (result, calls) = run(case.sample.clone(), &served).await;

        // Assert
        assert_eq!(
            result,
            RemoteResult::Error("Unsupported request".to_string()),
            "{:?}",
            case.sample
        );
        assert_eq!(calls, Vec::<String>::new(), "{:?}", case.sample);
    }
}

#[tokio::test]
async fn a_request_for_an_unlisted_session_reaches_only_the_listing() {
    for case in cases() {
        // Arrange
        let Some(expected) = case.unlisted else {
            continue;
        };

        // Act
        let (result, calls) = run(case.sample.clone(), &every_operation()).await;

        // Assert
        assert_eq!(result, expected, "{:?}", case.sample);
        assert_eq!(calls, ["list"], "{:?}", case.sample);
    }
}
