//! The policy of every request variant: the operations the host must serve,
//! the listed-project and listed-session checks, the capability that offers
//! each operation, and how the request queues behind other changes of its
//! session.

use std::sync::Arc;

use ag_session::SessionOperation;

use crate::protocol::{
    RemoteGraphAction, RemoteGraphActionRequest, RemoteImage, RemoteRequest, RemoteResponse,
    RemoteResult, RemoteSessionMode, RequestedProjectId, RequestedSessionId,
};
use crate::request::{CAPABILITIES, RequestOrder, RequestPolicy, execute};
use crate::source_support::{FailingEntropy, MemoryDeviceStore, RecordingBackend, StateFixture};

/// Session every sample names; the test backend never lists it.
const UNLISTED: &str = "unlisted";
/// Project every sample that names one names; the test backend lists only
/// projects `1` and `2`.
const UNLISTED_PROJECT: &str = "9";

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
    /// How the request queues; before policies, `mutated_session` returned
    /// the session for exactly the `Queued` requests.
    order: Order,
    /// Operations the host must serve, empty for requests every host
    /// serves.
    requires: &'static [SessionOperation],
    /// A request of the variant, naming [`UNLISTED`] and carrying invalid
    /// fields wherever it can.
    sample: RemoteRequest,
    /// What the request answers for an unlisted session or, when it names
    /// one, [`UNLISTED_PROJECT`]; `None` exactly when its frame carries no
    /// `session_id` or `project_id`.
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
        fn variant_cases() -> Vec<Case> {
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
        order: Order::Queued,
        requires: &[SessionOperation::GraphActions],
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
        order: Order::Queued,
        requires: &[],
        sample: RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            graph_question_target: None,
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::CancelSession { .. } => Case {
        order: Order::Queued,
        requires: &[],
        sample: RemoteRequest::CancelSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::CreateSession { .. } => Case {
        order: Order::Unqueued,
        requires: &[SessionOperation::CreateSession],
        sample: RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Harness,
            project_id: None,
        },
        unlisted: None,
    },
    RemoteRequest::GenerateGraphProposal { .. } => Case {
        order: Order::Queued,
        requires: &[SessionOperation::GenerateGraphProposal],
        sample: RemoteRequest::GenerateGraphProposal {
            message: String::new(),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::GetHost { .. } => Case {
        order: Order::Read,
        requires: &[SessionOperation::HostContext],
        sample: RemoteRequest::GetHost { project_id: None },
        unlisted: None,
    },
    RemoteRequest::GetSession { .. } => Case {
        order: Order::Read,
        requires: &[],
        sample: RemoteRequest::GetSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(RemoteResult::Ok(RemoteResponse::Session { session: None })),
    },
    RemoteRequest::GetSessionDiff { .. } => Case {
        order: Order::Read,
        requires: &[SessionOperation::SessionDiff],
        sample: RemoteRequest::GetSessionDiff {
            path: Some("absent.rs".to_string()),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::GetSessionGraphs { .. } => Case {
        order: Order::Read,
        requires: &[SessionOperation::Graphs],
        sample: RemoteRequest::GetSessionGraphs {
            graph_id: None,
            known: None,
            node_id: None,
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::ListProjectFiles { .. } => Case {
        order: Order::Read,
        requires: &[SessionOperation::ProjectFiles],
        sample: RemoteRequest::ListProjectFiles { project_id: None },
        unlisted: None,
    },
    RemoteRequest::ListProjects => Case {
        order: Order::Read,
        requires: &[SessionOperation::Projects],
        sample: RemoteRequest::ListProjects,
        unlisted: None,
    },
    RemoteRequest::ListSessionFiles { .. } => Case {
        order: Order::Read,
        requires: &[SessionOperation::SessionFiles],
        sample: RemoteRequest::ListSessionFiles {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::ListSessions { .. } => Case {
        order: Order::Read,
        requires: &[],
        sample: RemoteRequest::ListSessions { project_id: None },
        unlisted: None,
    },
    RemoteRequest::MergeSession { .. } => Case {
        order: Order::Queued,
        requires: &[SessionOperation::MergeSession],
        sample: RemoteRequest::MergeSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::PublishSession { .. } => Case {
        order: Order::Unqueued,
        requires: &[SessionOperation::PublishSession],
        sample: RemoteRequest::PublishSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::SendMessage { .. } => Case {
        order: Order::Queued,
        requires: &[],
        sample: RemoteRequest::SendMessage {
            images: Vec::new(),
            message: "hi".to_string(),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::SetSessionSettings { .. } => Case {
        order: Order::Queued,
        requires: &[SessionOperation::SessionSettings],
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
        order: Order::Queued,
        requires: &[SessionOperation::StopTurn],
        sample: RemoteRequest::StopTurn {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::SyncProject { .. } => Case {
        order: Order::Unqueued,
        requires: &[SessionOperation::SyncProject],
        sample: RemoteRequest::SyncProject { project_id: None },
        unlisted: None,
    },
    RemoteRequest::SyncSession { .. } => Case {
        order: Order::Queued,
        requires: &[SessionOperation::RebaseSession],
        sample: RemoteRequest::SyncSession {
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    },
    RemoteRequest::Unsupported => Case {
        order: Order::Read,
        requires: &[],
        sample: RemoteRequest::Unsupported,
        unlisted: None,
    },
}

/// The answer to a request for a session the host does not list.
fn not_found() -> RemoteResult {
    RemoteResult::Error("Session not found".to_string())
}

/// The answer to a request for a project the host does not list.
fn project_not_found() -> RemoteResult {
    RemoteResult::Error("Project not found".to_string())
}

/// Returns one case per request variant, and one per payload that changes
/// a variant's policy: a message with images needs the `images`
/// capability, and each request about a project that names
/// [`UNLISTED_PROJECT`] needs the `projects` capability too.
fn cases() -> Vec<Case> {
    let mut cases = variant_cases();
    cases.push(Case {
        order: Order::Queued,
        requires: &[SessionOperation::MessageImages],
        sample: RemoteRequest::SendMessage {
            images: vec![RemoteImage {
                data: "not base64".to_string(),
            }],
            message: "hi".to_string(),
            session_id: UNLISTED.into(),
        },
        unlisted: Some(not_found()),
    });
    let named = || Some(RequestedProjectId::from(UNLISTED_PROJECT));
    let named_cases: [(Order, &'static [SessionOperation], RemoteRequest); 5] = [
        (
            Order::Unqueued,
            &[SessionOperation::CreateSession, SessionOperation::Projects],
            RemoteRequest::CreateSession {
                mode: RemoteSessionMode::Harness,
                project_id: named(),
            },
        ),
        (
            Order::Read,
            &[SessionOperation::HostContext, SessionOperation::Projects],
            RemoteRequest::GetHost {
                project_id: named(),
            },
        ),
        (
            Order::Read,
            &[SessionOperation::ProjectFiles, SessionOperation::Projects],
            RemoteRequest::ListProjectFiles {
                project_id: named(),
            },
        ),
        (
            Order::Read,
            &[SessionOperation::Projects],
            RemoteRequest::ListSessions {
                project_id: named(),
            },
        ),
        (
            Order::Unqueued,
            &[SessionOperation::SyncProject, SessionOperation::Projects],
            RemoteRequest::SyncProject {
                project_id: named(),
            },
        ),
    ];
    cases.extend(
        named_cases
            .into_iter()
            .map(|(order, requires, sample)| Case {
                order,
                requires,
                sample,
                unlisted: Some(project_not_found()),
            }),
    );

    cases
}

/// Whether `request`'s frame names a project.
fn names_project(request: &RemoteRequest) -> bool {
    serde_json::to_value(request)
        .expect("request should encode")
        .get("project_id")
        .is_some()
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

/// A policy as the tests compare it: what it requires, how it queues, and
/// the project it names.
type Policy<'request> = (
    &'static [SessionOperation],
    RequestOrder<'request>,
    Option<&'request RequestedProjectId>,
);

#[test]
fn every_request_has_the_policy_of_its_table_entry() {
    // Arrange
    let cases = cases();
    let unlisted = RequestedSessionId::from(UNLISTED);
    let unlisted_project = RequestedProjectId::from(UNLISTED_PROJECT);

    // Act
    let policies: Vec<Policy<'_>> = cases
        .iter()
        .map(|case| {
            assert_covered(&case.sample);
            let RequestPolicy {
                order,
                project,
                requires,
            } = RequestPolicy::of(&case.sample);

            (requires, order, project)
        })
        .collect();

    // Assert
    let expected: Vec<Policy<'_>> = cases
        .iter()
        .map(|case| {
            let order = match case.order {
                Order::Read => RequestOrder::Read,
                Order::Queued => RequestOrder::Queued(&unlisted),
                Order::Unqueued => RequestOrder::Unqueued,
            };
            let project = names_project(&case.sample).then_some(&unlisted_project);

            (case.requires, order, project)
        })
        .collect();
    assert_eq!(policies, expected);
}

#[test]
fn a_sample_has_an_unlisted_answer_exactly_when_it_names_a_session_or_a_project() {
    for case in cases() {
        // Arrange
        let has_unlisted_answer = case.unlisted.is_some();

        // Act
        let frame = serde_json::to_value(&case.sample).expect("request should encode");

        // Assert
        assert_eq!(
            frame.get("session_id").is_some() || frame.get("project_id").is_some(),
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
        .flat_map(|case| RequestPolicy::of(&case.sample).requires)
        .copied()
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
}

#[tokio::test]
async fn a_request_whose_operations_the_host_does_not_all_serve_reaches_nothing() {
    for case in cases() {
        for operation in case.requires {
            // Arrange
            let served: Vec<SessionOperation> = every_operation()
                .into_iter()
                .filter(|served| served != operation)
                .collect();

            // Act
            let (result, calls) = run(case.sample.clone(), &served).await;

            // Assert
            assert_eq!(
                result,
                RemoteResult::Error("Unsupported request".to_string()),
                "{:?} without {operation:?}",
                case.sample
            );
            assert_eq!(
                calls,
                Vec::<String>::new(),
                "{:?} without {operation:?}",
                case.sample
            );
        }
    }
}

#[tokio::test]
async fn a_request_about_projects_reaches_nothing_on_a_host_without_project_status() {
    // Arrange: `projects` is served only with project status, which tells
    // a page which project a listing is.
    let served: Vec<SessionOperation> = every_operation()
        .into_iter()
        .filter(|served| *served != SessionOperation::ProjectStatus)
        .collect();
    let about_projects = cases()
        .into_iter()
        .filter(|case| case.requires.contains(&SessionOperation::Projects));

    for case in about_projects {
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
async fn a_request_for_an_unlisted_session_or_project_reaches_only_the_listing() {
    for case in cases() {
        // Arrange
        let Some(expected) = case.unlisted else {
            continue;
        };
        let listing = if names_project(&case.sample) {
            "projects"
        } else {
            "list"
        };

        // Act
        let (result, calls) = run(case.sample.clone(), &every_operation()).await;

        // Assert
        assert_eq!(result, expected, "{:?}", case.sample);
        assert_eq!(calls, [listing], "{:?}", case.sample);
    }
}
