//! Request dispatch: the policy each request declares, the capabilities that
//! offer the optional ones, and the session service calls that answer them.

use std::str::FromStr;

use ag_session::{
    AnswerQuestionsRequest, CreateSessionRequest, GenerateGraphProposalRequest, MessageImage,
    ProjectScope, QuestionAnswer, SessionError, SessionGraphsRequest, SessionOperation,
    SessionService, SessionSettingsChange,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::entropy::{Entropy, random_hex};
use crate::protocol::{
    RemoteGraphActionRequest, RemoteImage, RemoteProject, RemoteProjectEntry, RemoteQuestionAnswer,
    RemoteRequest, RemoteResponse, RemoteResult, RemoteReviewRequest, RemoteSession,
    RemoteSessionMode, RemoteSessionSummary, RequestedProjectId, RequestedSessionId,
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
    (SessionOperation::MessageImages, "images"),
    (SessionOperation::Projects, "projects"),
];
/// Operations a capability's operation also needs before the capability is
/// offered. A page that picks a project knows a listing is that project's
/// only from the `id` of the project status the listing carries, so
/// `projects` needs the host to report project status.
const PREREQUISITES: &[(SessionOperation, SessionOperation)] =
    &[(SessionOperation::Projects, SessionOperation::ProjectStatus)];
/// Capabilities that extend the request another one offers, each listed
/// with it: `settings_agent` says `set_session_settings` takes an `agent`.
const EXTENSIONS: &[(&str, &str)] = &[("settings", "settings_agent")];
/// Most images one message carries.
const MAX_MESSAGE_IMAGES: usize = 8;
/// Most image bytes one message carries, decoded. Their base64 text must
/// fit in one frame, `MAX_MESSAGE_SIZE` in `crate::server`.
const MAX_MESSAGE_IMAGE_BYTES: usize = 6 << 20;
// Base64 grows the images by a third; the rest of the frame stays for the
// text.
const _: () =
    assert!(MAX_MESSAGE_IMAGE_BYTES.div_ceil(3) * 4 + (1 << 20) <= crate::server::MAX_MESSAGE_SIZE);
/// Error returned for a message with more images than it may carry.
const TOO_MANY_IMAGES: &str = "A message carries at most 8 images";
/// Error returned for a message whose images are larger than allowed.
const IMAGES_TOO_LARGE: &str = "A message's images are at most 6 MB together";
/// Error returned for an image that is not valid base64.
const INVALID_IMAGE_ENCODING: &str = "Image data is not valid base64";
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
/// is listed only when the host serves its operation and the
/// [`PREREQUISITES`] of that operation.
pub(crate) fn capabilities(sessions: &SessionService) -> Vec<String> {
    CAPABILITIES
        .iter()
        .filter(|(operation, _)| {
            sessions.supports(*operation)
                && PREREQUISITES
                    .iter()
                    .filter(|(dependent, _)| dependent == operation)
                    .all(|(_, prerequisite)| sessions.supports(*prerequisite))
        })
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
/// Every request declares its policy in [`Self::of`]. The project and the
/// session a request names are checked by their types instead: a request
/// reaches the host only with the `ProjectScope` that
/// [`RequestedProjectId::listed`] returns and the `SessionId` that
/// [`RequestedSessionId::listed`] returns.
pub(crate) struct RequestPolicy<'request> {
    /// How the request runs relative to others for the same session.
    pub(crate) order: RequestOrder<'request>,
    /// Project the request names, checked before its session and fields;
    /// `None` for the host's active project and for requests about no
    /// project.
    pub(crate) project: Option<&'request RequestedProjectId>,
    /// Operations the host must serve, each of which a capability in
    /// [`CAPABILITIES`] offers: the request's own, and
    /// [`SessionOperation::Projects`] whenever it names a project. Empty for
    /// the requests every host serves: those of the first protocol version,
    /// and `Unsupported`, which answers an error itself.
    pub(crate) requires: &'static [SessionOperation],
}

impl<'request> RequestPolicy<'request> {
    /// Returns the policy of `request`.
    pub(crate) fn of(request: &'request RemoteRequest) -> Self {
        let unscoped = |requires, order| Self {
            order,
            project: None,
            requires,
        };
        // A request about a project needs `Projects` too when it names one.
        let scoped =
            |project_id: &'request Option<RequestedProjectId>, active, named, order| Self {
                order,
                project: project_id.as_ref(),
                requires: if project_id.is_some() { named } else { active },
            };

        match request {
            RemoteRequest::ActOnSessionGraph(request) => unscoped(
                &[SessionOperation::GraphActions],
                RequestOrder::Queued(&request.session_id),
            ),
            RemoteRequest::SendMessage {
                images, session_id, ..
            } if !images.is_empty() => unscoped(
                &[SessionOperation::MessageImages],
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::AnswerQuestions { session_id, .. }
            | RemoteRequest::CancelSession { session_id }
            | RemoteRequest::SendMessage { session_id, .. } => {
                unscoped(&[], RequestOrder::Queued(session_id))
            }
            RemoteRequest::CreateSession { project_id, .. } => scoped(
                project_id,
                &[SessionOperation::CreateSession],
                &[SessionOperation::CreateSession, SessionOperation::Projects],
                RequestOrder::Unqueued,
            ),
            RemoteRequest::GenerateGraphProposal { session_id, .. } => unscoped(
                &[SessionOperation::GenerateGraphProposal],
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::GetHost { project_id } => scoped(
                project_id,
                &[SessionOperation::HostContext],
                &[SessionOperation::HostContext, SessionOperation::Projects],
                RequestOrder::Read,
            ),
            RemoteRequest::GetSession { .. } | RemoteRequest::Unsupported => {
                unscoped(&[], RequestOrder::Read)
            }
            RemoteRequest::GetSessionDiff { .. } => {
                unscoped(&[SessionOperation::SessionDiff], RequestOrder::Read)
            }
            RemoteRequest::GetSessionGraphs { .. } => {
                unscoped(&[SessionOperation::Graphs], RequestOrder::Read)
            }
            RemoteRequest::ListProjectFiles { project_id } => scoped(
                project_id,
                &[SessionOperation::ProjectFiles],
                &[SessionOperation::ProjectFiles, SessionOperation::Projects],
                RequestOrder::Read,
            ),
            RemoteRequest::ListProjects => {
                unscoped(&[SessionOperation::Projects], RequestOrder::Read)
            }
            RemoteRequest::ListSessionFiles { .. } => {
                unscoped(&[SessionOperation::SessionFiles], RequestOrder::Read)
            }
            RemoteRequest::ListSessions { project_id } => scoped(
                project_id,
                &[],
                &[SessionOperation::Projects],
                RequestOrder::Read,
            ),
            RemoteRequest::MergeSession { session_id } => unscoped(
                &[SessionOperation::MergeSession],
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::PublishSession { .. } => {
                unscoped(&[SessionOperation::PublishSession], RequestOrder::Unqueued)
            }
            RemoteRequest::SetSessionSettings { session_id, .. } => unscoped(
                &[SessionOperation::SessionSettings],
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::StopTurn { session_id } => unscoped(
                &[SessionOperation::StopTurn],
                RequestOrder::Queued(session_id),
            ),
            RemoteRequest::SyncProject { project_id } => scoped(
                project_id,
                &[SessionOperation::SyncProject],
                &[SessionOperation::SyncProject, SessionOperation::Projects],
                RequestOrder::Unqueued,
            ),
            RemoteRequest::SyncSession { session_id } => unscoped(
                &[SessionOperation::RebaseSession],
                RequestOrder::Queued(session_id),
            ),
        }
    }

    /// Refuses a request whose operations the host does not serve, so a
    /// client that skips the capability list still reaches nothing more,
    /// and a request that names a project never reaches a host that serves
    /// only its active project, which would answer for that one instead.
    /// Then returns the scope of the project the request names, which
    /// reaches only a project the host lists.
    async fn admit(&self, sessions: &SessionService) -> Result<ProjectScope, SessionError> {
        if !self
            .requires
            .iter()
            .all(|operation| sessions.supports(*operation))
        {
            return Err(SessionError::Operation(UNSUPPORTED_REQUEST.to_string()));
        }

        match self.project {
            Some(project_id) => project_id.listed(sessions).await,
            None => Ok(ProjectScope::Active),
        }
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
/// The host must serve the request's operations, then the project it names
/// must be listed, then the session it names, and only then are its fields
/// validated.
async fn respond(
    sessions: &SessionService,
    entropy: &dyn Entropy,
    request: RemoteRequest,
) -> Result<RemoteResponse, SessionError> {
    let scope = RequestPolicy::of(&request).admit(sessions).await?;

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
        RemoteRequest::CreateSession { mode, .. } => create_session(sessions, scope, mode).await,
        RemoteRequest::GenerateGraphProposal {
            message,
            session_id,
        } => generate_listed_graph_proposal(sessions, entropy, session_id, message).await,
        RemoteRequest::GetHost { .. } => Ok(RemoteResponse::from(
            sessions.host_context_for(scope).await?,
        )),
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
        RemoteRequest::ListProjectFiles { .. } => Ok(RemoteResponse::from_session_files(
            &sessions.list_project_files_for(scope).await?,
        )),
        RemoteRequest::ListProjects => list_projects(sessions).await,
        RemoteRequest::ListSessionFiles { session_id } => {
            list_listed_session_files(sessions, session_id).await
        }
        RemoteRequest::ListSessions { .. } => list_sessions(sessions, scope).await,
        RemoteRequest::MergeSession { session_id } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.merge_session(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::PublishSession { session_id } => {
            publish_listed_session(sessions, session_id).await
        }
        RemoteRequest::SendMessage {
            images,
            message,
            session_id,
        } => send_listed_message(sessions, session_id, message, images).await,
        RemoteRequest::StopTurn { session_id } => {
            let session_id = session_id.listed(sessions).await?;
            sessions.stop_turn(&session_id).await?;

            Ok(RemoteResponse::Accepted)
        }
        RemoteRequest::SyncProject { .. } => {
            sessions.sync_project_for(scope).await?;

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

/// Sends a message, with its images when it has any, only when the host
/// lists the session.
async fn send_listed_message(
    sessions: &SessionService,
    session_id: RequestedSessionId,
    message: String,
    images: Vec<RemoteImage>,
) -> Result<RemoteResponse, SessionError> {
    let session_id = session_id.listed(sessions).await?;
    if images.is_empty() {
        sessions.send_message(&session_id, message).await?;
    } else {
        sessions
            .send_message_with_images(&session_id, message, decode_images(images)?)
            .await?;
    }

    Ok(RemoteResponse::Accepted)
}

/// Decodes the images of one message, refusing more than
/// [`MAX_MESSAGE_IMAGES`], more than [`MAX_MESSAGE_IMAGE_BYTES`] together,
/// and any that is not a PNG or JPEG file.
fn decode_images(images: Vec<RemoteImage>) -> Result<Vec<MessageImage>, SessionError> {
    if images.len() > MAX_MESSAGE_IMAGES {
        return Err(SessionError::Operation(TOO_MANY_IMAGES.to_string()));
    }
    let mut total_bytes = 0;

    images
        .into_iter()
        .map(|image| {
            let data = STANDARD
                .decode(image.data)
                .map_err(|_| SessionError::Operation(INVALID_IMAGE_ENCODING.to_string()))?;
            total_bytes += data.len();
            if total_bytes > MAX_MESSAGE_IMAGE_BYTES {
                return Err(SessionError::Operation(IMAGES_TOO_LARGE.to_string()));
            }

            MessageImage::new(data)
        })
        .collect()
}

/// Creates a session in the project of `scope`, the listed project a
/// request named or the active one as the host describes it, and only in a
/// mode the host serves there now.
///
/// The mode rule belongs to the remote surface and is stricter than the
/// host, which also creates drafts. It reads the served modes and the
/// active project from `host_context_for`, which the `create_session`
/// capability does not require the host to serve: a host that creates sessions
/// without describing its project answers its own `host_context` error here.
async fn create_session(
    sessions: &SessionService,
    scope: ProjectScope,
    mode: RemoteSessionMode,
) -> Result<RemoteResponse, SessionError> {
    let host = sessions.host_context_for(scope).await?;
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
    // A named project is the listed one, whatever the description says.
    let project_id = match scope {
        ProjectScope::Active => host.project_id,
        ProjectScope::Project(id) => id,
    };
    let session_id = sessions
        .create_session(CreateSessionRequest {
            inherit_from_session_id: None,
            mode,
            project_id,
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

/// Lists the projects the host lists, in its order.
async fn list_projects(sessions: &SessionService) -> Result<RemoteResponse, SessionError> {
    let projects = sessions.list_projects().await?;

    Ok(RemoteResponse::Projects {
        projects: projects.into_iter().map(RemoteProjectEntry::from).collect(),
    })
}

/// Lists the sessions of the project of `scope` and, when the host reports
/// it, that project's status, in one host call, named by the project's id
/// when the host read both together.
async fn list_sessions(
    sessions: &SessionService,
    scope: ProjectScope,
) -> Result<RemoteResponse, SessionError> {
    let listed = sessions.project_sessions(scope).await?;

    Ok(RemoteResponse::Sessions {
        project: listed
            .status
            .map(|status| RemoteProject::listed(status, listed.project_id)),
        sessions: listed
            .entries
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
