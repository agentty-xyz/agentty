//! Request dispatch: the policy each request declares, the capabilities that
//! offer the optional ones, and the session service calls that answer them.

use std::str::FromStr;

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, GenerateGraphProposalRequest, QuestionAnswer,
    SessionError, SessionGraphsRequest, SessionOperation, SessionService, SessionSettingsChange,
};

use crate::entropy::{Entropy, random_hex};
use crate::protocol::{
    RemoteGraphActionRequest, RemoteProject, RemoteQuestionAnswer, RemoteRequest, RemoteResponse,
    RemoteResult, RemoteReviewRequest, RemoteSession, RemoteSessionMode, RemoteSessionSummary,
    RequestedSessionId,
};

/// Error returned for a request method this server does not know, and for
/// an optional request whose operation the host does not serve.
const UNSUPPORTED_REQUEST: &str = "Unsupported request";
/// Optional requests, each named by the capability that offers it and served
/// only when the host supports its operation.
const CAPABILITIES: &[(SessionOperation, &str)] = &[
    (SessionOperation::HostContext, "host"),
    (SessionOperation::CreateSession, "create_session"),
    (SessionOperation::MergeSession, "merge"),
    (SessionOperation::SessionDiff, "diff"),
    (SessionOperation::SessionFiles, "files"),
    (SessionOperation::ProjectFiles, "project_files"),
    (SessionOperation::StopTurn, "stop_turn"),
    (SessionOperation::RebaseSession, "sync"),
    (SessionOperation::SessionSettings, "settings"),
    (SessionOperation::PublishSession, "publish"),
    (SessionOperation::SyncProject, "sync_project"),
    (SessionOperation::Graphs, "graphs"),
    (SessionOperation::GraphActions, "graph_actions"),
    (SessionOperation::GenerateGraphProposal, "graph_proposal"),
];
/// Capabilities that extend the request another one offers, each listed
/// with it: `settings_agent` says `set_session_settings` takes an `agent`.
const EXTENSIONS: &[(&str, &str)] = &[("settings", "settings_agent")];
/// Random bytes in the delivery identity of a graph proposal a device asks
/// for.
const PROPOSAL_ID_BYTES: usize = 16;
/// Error returned when secure randomness for a proposal identity fails.
const PROPOSAL_ID_UNAVAILABLE: &str = "Cannot generate a graph proposal identity";
/// Error returned when a device asks for a creation mode the host does not
/// serve now.
const UNAVAILABLE_SESSION_MODE: &str = "Session mode is not available";
/// Error returned when a device asks for the patch of a file the session's
/// diff does not change.
const FILE_NOT_IN_DIFF: &str = "File is not in the session diff";

/// Returns the optional requests this server supports, sent in `Welcome`
/// and `Paired`.
///
/// Requests added after the first protocol version are offered only through
/// this list, so a client never sends one to a server that lacks it. Each
/// is listed only when the host serves its operation.
pub(crate) fn capabilities(sessions: &SessionService) -> Vec<String> {
    CAPABILITIES
        .iter()
        .filter(|(operation, _)| sessions.supports(*operation))
        .flat_map(|(_, capability)| {
            let extensions = EXTENSIONS
                .iter()
                .filter(move |(base, _)| base == capability)
                .map(|(_, extension)| *extension);

            std::iter::once(*capability).chain(extensions)
        })
        .map(str::to_string)
        .collect()
}

/// Runs one request against the session service, drawing the identities it
/// mints from `entropy`.
pub(crate) async fn execute(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    request: RemoteRequest,
) -> RemoteResult {
    match respond(sessions, entropy, request).await {
        Ok(response) => RemoteResult::Ok(response),
        Err(error) => RemoteResult::Error(error.to_string()),
    }
}

/// What the server checks before it dispatches a request, and how the
/// request runs relative to others.
///
/// Every request declares its policy in [`Self::of`]. The session a request
/// names is checked by its type instead: a request reaches the host only
/// with the `SessionId` that [`RequestedSessionId::listed`] returns.
pub(crate) struct RequestPolicy<'request> {
    /// How the request runs relative to others for the same session.
    pub(crate) order: RequestOrder<'request>,
    /// Operation the host must serve, which a capability in
    /// [`CAPABILITIES`] offers. `None` for the requests every host serves:
    /// those of the first protocol version, and `Unsupported`, which answers
    /// an error itself.
    operation: Option<SessionOperation>,
}

impl<'request> RequestPolicy<'request> {
    /// Returns the policy of `request`.
    pub(crate) fn of(request: &'request RemoteRequest) -> Self {
        let (operation, order) = match request {
            RemoteRequest::ActOnSessionGraph(request) => (
                Some(SessionOperation::GraphActions),
                RequestOrder::Queued(&request.session_id),
            ),
            RemoteRequest::AnswerQuestions { session_id, .. }
            | RemoteRequest::CancelSession { session_id }
            | RemoteRequest::SendMessage { session_id, .. } => {
                (None, RequestOrder::Queued(session_id))
            }
            RemoteRequest::CreateSession { .. } => (
                Some(SessionOperation::CreateSession),
                RequestOrder::Unqueued,
            ),
            RemoteRequest::GenerateGraphProposal { session_id, .. } => (
                Some(SessionOperation::GenerateGraphProposal),
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::GetHost => (Some(SessionOperation::HostContext), RequestOrder::Read),
            RemoteRequest::GetSession { .. } | RemoteRequest::ListSessions => {
                (None, RequestOrder::Read)
            }
            RemoteRequest::GetSessionDiff { .. } => {
                (Some(SessionOperation::SessionDiff), RequestOrder::Read)
            }
            RemoteRequest::GetSessionGraphs { .. } => {
                (Some(SessionOperation::Graphs), RequestOrder::Read)
            }
            RemoteRequest::ListProjectFiles => {
                (Some(SessionOperation::ProjectFiles), RequestOrder::Read)
            }
            RemoteRequest::ListSessionFiles { .. } => {
                (Some(SessionOperation::SessionFiles), RequestOrder::Read)
            }
            RemoteRequest::MergeSession { session_id } => (
                Some(SessionOperation::MergeSession),
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::PublishSession { .. } => (
                Some(SessionOperation::PublishSession),
                RequestOrder::Unqueued,
            ),
            RemoteRequest::SetSessionSettings { session_id, .. } => (
                Some(SessionOperation::SessionSettings),
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::StopTurn { session_id } => (
                Some(SessionOperation::StopTurn),
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::SyncProject => {
                (Some(SessionOperation::SyncProject), RequestOrder::Unqueued)
            }
            RemoteRequest::SyncSession { session_id } => (
                Some(SessionOperation::RebaseSession),
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::Unsupported => (None, RequestOrder::Read),
        };

        Self { order, operation }
    }

    /// Refuses a request whose operation the host does not serve, so a
    /// client that skips the capability list still reaches nothing more.
    fn require(&self, sessions: &SessionService) -> Result<(), SessionError> {
        if let Some(operation) = self.operation
            && !sessions.supports(operation)
        {
            return Err(SessionError::Operation(UNSUPPORTED_REQUEST.to_string()));
        }

        Ok(())
    }
}

/// How a request runs relative to others for the same session.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RequestOrder<'request> {
    /// Changes nothing, so it runs at once.
    Read,
    /// Changes the session it names, so it waits for the earlier requests
    /// that change the same session and reaches the host after them.
    Queued(&'request RequestedSessionId),
    /// Changes the host but runs at once: creation and a project sync
    /// change no existing session, and a publish can wait behind a running
    /// turn, so queueing it would hold a later stop, cancel, answer, or
    /// message until the turn ends; the host serializes the push itself.
    Unqueued,
}

/// Answers one request once its policy allows it.
///
/// The host must serve the request's operation, then the session it names
/// must be listed, and only then are its fields validated.
async fn respond(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    request: RemoteRequest,
) -> Result<RemoteResponse, SessionError> {
    RequestPolicy::of(&request).require(sessions)?;

    match request {
        RemoteRequest::ActOnSessionGraph(request) => {
            act_on_listed_session_graph(sessions, request).await
        }
        RemoteRequest::AnswerQuestions {
            answers,
            graph_question_target,
            session_id,
        } => answer_listed_questions(sessions, session_id, answers, graph_question_target).await,
        RemoteRequest::CancelSession { session_id } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.cancel_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::CreateSession { mode } => create_session(sessions, mode).await,
        RemoteRequest::GenerateGraphProposal {
            message,
            session_id,
        } => generate_listed_graph_proposal(sessions, entropy, session_id, message).await,
        RemoteRequest::GetHost => Ok(RemoteResponse::from(sessions.host_context().await?)),
        RemoteRequest::GetSession { session_id } => get_listed_session(sessions, session_id).await,
        RemoteRequest::GetSessionDiff { path, session_id } => {
            get_listed_session_diff(sessions, session_id, path.as_deref()).await
        }
        RemoteRequest::GetSessionGraphs {
            graph_id,
            known,
            node_id,
            session_id,
        } => {
            get_listed_session_graphs(sessions, session_id, graph_id, node_id, known.as_deref())
                .await
        }
        RemoteRequest::ListProjectFiles => Ok(RemoteResponse::from_session_files(
            &sessions.list_project_files().await?,
        )),
        RemoteRequest::ListSessionFiles { session_id } => {
            list_listed_session_files(sessions, session_id).await
        }
        RemoteRequest::ListSessions => list_sessions(sessions).await,
        RemoteRequest::MergeSession { session_id } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.merge_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::PublishSession { session_id } => {
            publish_listed_session(sessions, session_id).await
        }
        RemoteRequest::SendMessage {
            message,
            session_id,
        } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.send_message(&session_id, message).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::StopTurn { session_id } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.stop_turn(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SyncProject => {
            sessions.sync_project().await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SyncSession { session_id } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.rebase_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SetSessionSettings {
            agent,
            model,
            reasoning,
            response_style,
            session_id,
            speed_mode,
        } => {
            let session_id = session_id.listed(sessions).await?;
            let change = SessionSettingsChange {
                agent: parse_setting(agent)?,
                model: parse_setting(model)?,
                reasoning_level: parse_setting(reasoning)?,
                response_style: parse_setting(response_style)?,
                speed_mode: parse_setting(speed_mode)?,
            };
            sessions
                .update_session_settings(&session_id, change)
                .await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::Unsupported => Err(SessionError::Operation(UNSUPPORTED_REQUEST.to_string())),
    }
}

/// Applies one graph inspector action only when the host lists the session
/// that owns the graph; the host checks the graph's ownership, snapshot, and
/// the action's availability.
async fn act_on_listed_session_graph(
    sessions: &SessionService,
    request: RemoteGraphActionRequest,
) -> Result<RemoteResponse, SessionError> {
    let session_id = request.session_id.clone().listed(sessions).await?;
    sessions
        .act_on_session_graph(request.into_session_request(session_id)?)
        .await?;

    Ok(RemoteResponse::Accepted)
}

/// Answers the current question set of one session only when the host lists
/// that session: its turn's, or with `graph_question_target` the graph
/// questions of the graph work that key names.
async fn answer_listed_questions(
    sessions: &SessionService,
    session_id: RequestedSessionId,
    answers: Vec<RemoteQuestionAnswer>,
    graph_question_target: Option<String>,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    let request = AnswerQuestionsRequest {
        answers: answers
            .into_iter()
            .map(|answer| QuestionAnswer {
                answer: answer.answer,
                question: answer.question,
            })
            .collect(),
        attachments: Vec::new(),
        graph_question_target,
    };
    sessions.answer_questions(&session_id, request).await?;

    Ok(RemoteResponse::Accepted)
}

/// Creates a session in the host's active project, the only project the
/// host accepts, and only in a mode the host serves now.
///
/// The mode rule belongs to the remote surface and is stricter than the
/// host, which also creates drafts. It reads the served modes from
/// `host_context`, which the `create_session` capability does not require
/// the host to serve: a host that creates sessions without describing its
/// project answers its own `host_context` error here.
async fn create_session(
    sessions: &SessionService,
    mode: RemoteSessionMode,
) -> Result<RemoteResponse, SessionError> {
    let host = sessions.host_context().await?;
    let mode = mode.session_mode();
    if !host
        .session_modes
        .iter()
        .any(|session_mode| session_mode.mode == mode)
    {
        return Err(SessionError::Operation(
            UNAVAILABLE_SESSION_MODE.to_string(),
        ));
    }
    let session_id = sessions
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode,
            project_id: host.project_id,
        })
        .await?;

    Ok(RemoteResponse::Created {
        session_id: session_id.to_string(),
    })
}

/// Asks for a graph proposal for the task `message` only when the host
/// lists the session, under a fresh delivery identity; the host checks the
/// session's readiness and the task's size.
async fn generate_listed_graph_proposal(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    session_id: RequestedSessionId,
    message: String,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    let id = random_hex(entropy, PROPOSAL_ID_BYTES)
        .ok_or_else(|| SessionError::Operation(PROPOSAL_ID_UNAVAILABLE.to_string()))?;
    sessions
        .generate_graph_proposal(GenerateGraphProposalRequest::from_text(
            format!("remote-{id}"),
            session_id,
            message,
        ))
        .await?;

    Ok(RemoteResponse::Accepted)
}

/// Loads one session only when the host lists it, so remote clients read
/// nothing beyond the sessions they can see. An unlisted session reads as
/// absent rather than as an error.
async fn get_listed_session(
    sessions: &SessionService,
    session_id: RequestedSessionId,
) -> Result<RemoteResponse, SessionError> {
    let session_id = match session_id.listed(sessions).await {
        Ok(session_id) => session_id,
        Err(SessionError::NotFound) => return Ok(RemoteResponse::Session { session: None }),
        Err(error) => return Err(error),
    };
    let session = sessions.get_session(&session_id).await?;

    Ok(RemoteResponse::Session {
        session: session.map(|session| Box::new(RemoteSession::from(session))),
    })
}

/// Reads the diff of one session only when the host lists it: the file
/// list, or with `path` that file's patch.
async fn get_listed_session_diff(
    sessions: &SessionService,
    session_id: RequestedSessionId,
    path: Option<&str>,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    let files = sessions
        .get_session_diff(&session_id)
        .await?
        .ok_or(SessionError::NotFound)?;

    RemoteResponse::from_diff(&files, path)
        .ok_or_else(|| SessionError::Operation(FILE_NOT_IN_DIFF.to_string()))
}

/// Reads the graphs of one session only when the host lists it, selecting
/// `graph_id` and `node_id` when given, and answers `unchanged` when the
/// device already shows that response as version `known`.
async fn get_listed_session_graphs(
    sessions: &SessionService,
    session_id: RequestedSessionId,
    graph_id: Option<String>,
    node_id: Option<String>,
    known: Option<&str>,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    let graphs = sessions
        .session_graphs(SessionGraphsRequest {
            graph_id,
            node_id,
            session_id,
        })
        .await?;

    Ok(RemoteResponse::from_graphs(graphs).unchanged_since(known))
}

/// Lists the files and directories in one session's worktree only when the
/// host lists that session.
async fn list_listed_session_files(
    sessions: &SessionService,
    session_id: RequestedSessionId,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    let entries = sessions
        .list_session_files(&session_id)
        .await?
        .ok_or(SessionError::NotFound)?;

    Ok(RemoteResponse::from_session_files(&entries))
}

/// Lists the host's sessions and, when the host reports it, the active
/// project they belong to.
async fn list_sessions(sessions: &SessionService) -> Result<RemoteResponse, SessionError> {
    let summaries = sessions.list_session_entries().await?;
    let project = if sessions.supports(SessionOperation::ProjectStatus) {
        Some(RemoteProject::from(sessions.project_status().await?))
    } else {
        None
    };

    Ok(RemoteResponse::Sessions {
        project,
        sessions: summaries
            .into_iter()
            .map(RemoteSessionSummary::from)
            .collect(),
    })
}

/// Publishes one session and opens or refreshes its review request only
/// when the host lists that session.
async fn publish_listed_session(
    sessions: &SessionService,
    session_id: RequestedSessionId,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    let review_request = sessions.create_review_request(&session_id).await?;

    Ok(RemoteResponse::ReviewRequest {
        review_request: RemoteReviewRequest::from(review_request.summary),
    })
}

/// Parses one optional setting sent as its persisted name, such as `high`.
fn parse_setting<Setting: FromStr<Err = String>>(
    value: Option<String>,
) -> Result<Option<Setting>, SessionError> {
    value
        .map(|value| value.parse().map_err(SessionError::Operation))
        .transpose()
}

#[cfg(test)]
#[path = "request_test.rs"]
mod tests;
