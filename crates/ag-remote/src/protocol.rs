//! JSON frames exchanged over the remote control WebSocket.
//!
//! Every frame is one text message. The client's first frame is
//! [`ClientFrame::Hello`] or [`ClientFrame::Pair`]. A pairing client first
//! receives [`ServerFrame::AwaitingApproval`] while the terminal decides.
//! After the server answers with [`ServerFrame::Welcome`] or
//! [`ServerFrame::Paired`], the client sends
//! [`ClientFrame::Request`] frames and the server answers each with a
//! [`ServerFrame::Response`] carrying the same `id`. The server also pushes
//! [`ServerFrame::Changed`] when host session state changes, so the client can
//! fetch fresh snapshots.

use ag_session::{
    AgentSelectionMetadata, CreateSessionMode, HostContext, ReviewRequestSummary, Session,
    SessionMessage, SessionMessageKind, SessionSummary, session_branch,
};
use serde::{Deserialize, Serialize};

/// A frame sent by a remote client.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    /// Authenticates a previously paired device.
    Hello {
        /// Protocol version the client speaks.
        protocol_version: u32,
        /// Device token returned by [`ServerFrame::Paired`]. A device
        /// connected through a relay omits it: its encrypted channel already
        /// proves its key.
        #[serde(default)]
        token: String,
    },
    /// Pairs a new device with a one-time code shown in the terminal.
    Pair {
        /// Pairing code, matched ignoring case, dashes, and whitespace.
        code: String,
        /// Name the terminal shows for this device.
        device_name: String,
        /// Protocol version the client speaks.
        protocol_version: u32,
    },
    /// Invokes one session operation after authentication.
    Request {
        /// Client-chosen identifier echoed in the response.
        id: u64,
        /// Operation to run.
        request: RemoteRequest,
    },
}

/// Session operations a remote client may invoke.
///
/// Each request beyond reading and chatting is a mutation the user approved
/// for remote devices. A server offers an optional request only through its
/// capability, and every request that names a session reaches only sessions
/// the host lists.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum RemoteRequest {
    /// Answers the session's complete current question set.
    AnswerQuestions {
        /// Answers paired with the exact question text.
        answers: Vec<RemoteQuestionAnswer>,
        /// Session that asked the questions.
        session_id: String,
    },
    /// Cancels the whole session, not only its current turn.
    ///
    /// The host stops the session's agent work, marks the session `Canceled`,
    /// and removes its worktree, Git record, and branch, which discards any
    /// unmerged and unpublished changes.
    CancelSession {
        /// Session to cancel.
        session_id: String,
    },
    /// Creates a session in the host's active project, with its worktree
    /// and branch. Offered through the `create_session` capability.
    CreateSession {
        /// How the session runs; one of the modes `get_host` lists.
        #[serde(default)]
        mode: RemoteSessionMode,
    },
    /// Describes the host's active project. Offered through the `host`
    /// capability.
    GetHost,
    /// Loads one session snapshot.
    GetSession {
        /// Session to load.
        session_id: String,
    },
    /// Lists the host's loaded sessions.
    ListSessions,
    /// Merges a review-ready session into its base branch in the host's
    /// checkout. Offered through the `merge` capability.
    MergeSession {
        /// Session to merge.
        session_id: String,
    },
    /// Sends one chat message, starting, resuming, or queueing a turn.
    SendMessage {
        /// Message text.
        message: String,
        /// Session that receives the message.
        session_id: String,
    },
    /// Any method this server does not know, such as one a newer client
    /// sends. The server answers it with an error and keeps the connection.
    #[serde(other)]
    Unsupported,
}

/// How a session created remotely runs.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteSessionMode {
    /// On the in-process harness with its default model.
    Harness,
    /// On the project's default agent and model.
    #[default]
    Regular,
}

impl RemoteSessionMode {
    /// Returns the remote mode for a host creation mode, or `None` for a
    /// mode remote devices cannot request.
    fn from_session_mode(mode: &CreateSessionMode) -> Option<Self> {
        match mode {
            CreateSessionMode::Draft => None,
            CreateSessionMode::Harness => Some(Self::Harness),
            CreateSessionMode::Regular => Some(Self::Regular),
        }
    }

    /// Returns the host creation mode this remote mode asks for.
    #[must_use]
    pub fn session_mode(self) -> CreateSessionMode {
        match self {
            Self::Harness => CreateSessionMode::Harness,
            Self::Regular => CreateSessionMode::Regular,
        }
    }
}

/// One answer to a pending clarification question.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteQuestionAnswer {
    /// Answer text.
    pub answer: String,
    /// Exact question text being answered.
    pub question: String,
}

/// A frame sent by the server.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// A valid pairing code was accepted and waits for approval in the
    /// terminal, which shows the same `confirmation` beside the request.
    AwaitingApproval {
        /// Short code the user compares between the device and terminal.
        confirmation: String,
    },
    /// Host session state changed after `revision` was last reported.
    Changed {
        /// Latest host revision.
        revision: u64,
    },
    /// Pairing succeeded; the client keeps `token` for later connections.
    Paired {
        /// Optional requests the server supports, as in
        /// [`ServerFrame::Welcome`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        /// Identifier of the new device.
        device_id: String,
        /// Protocol version the server speaks.
        protocol_version: u32,
        /// Host revision at authentication time.
        revision: u64,
        /// Secret device token. The server keeps only its hash. Absent for
        /// a device paired through a relay, which authenticates with its
        /// key instead.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        token: String,
    },
    /// The server refused the connection and closes it next.
    Rejected {
        /// Human-readable reason.
        reason: String,
    },
    /// Result of one [`ClientFrame::Request`].
    Response {
        /// Identifier from the request.
        id: u64,
        /// Operation outcome.
        result: RemoteResult,
    },
    /// Authentication with an existing token succeeded.
    Welcome {
        /// Optional requests the server supports. A client offers a feature
        /// only when its capability is listed, so it never sends a request
        /// an older server lacks. Absent when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        capabilities: Vec<String>,
        /// Identifier of the authenticated device.
        device_id: String,
        /// Protocol version the server speaks.
        protocol_version: u32,
        /// Host revision at authentication time.
        revision: u64,
    },
}

/// Outcome of one remote request, serialized as `{"ok": …}` or
/// `{"error": "…"}`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteResult {
    /// The host rejected or failed the operation.
    Error(String),
    /// The operation succeeded.
    Ok(RemoteResponse),
}

/// Successful request payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RemoteResponse {
    /// The host admitted the mutation; its work may still be running.
    Accepted,
    /// The host created a session.
    Created {
        /// Identifier of the new session.
        session_id: String,
    },
    /// The host's active project, where new sessions are created.
    Host {
        /// Branch new sessions start from.
        base_branch: String,
        /// Model a new session starts with.
        default_model: String,
        /// Display name of the active project.
        project: String,
        /// Modes `create_session` accepts now, in display order.
        session_modes: Vec<RemoteSessionMode>,
    },
    /// One session snapshot, or `None` when it does not exist.
    Session {
        /// Loaded session, boxed to keep other responses small.
        session: Option<Box<RemoteSession>>,
    },
    /// The host's loaded sessions in display order.
    Sessions {
        /// Session listing entries.
        sessions: Vec<RemoteSessionSummary>,
    },
}

impl From<HostContext> for RemoteResponse {
    fn from(host: HostContext) -> Self {
        Self::Host {
            base_branch: host.base_branch,
            default_model: host.default_model.name().to_string(),
            project: host.project_name,
            session_modes: host
                .session_modes
                .iter()
                .filter_map(RemoteSessionMode::from_session_mode)
                .collect(),
        }
    }
}

/// Listing entry for one session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSessionSummary {
    /// Stable session identifier.
    pub id: String,
    /// Lifecycle status, such as `InProgress` or `Review`.
    pub status: String,
    /// Optional user-visible title.
    pub title: Option<String>,
    /// Last update timestamp in Unix seconds.
    pub updated_at: i64,
}

impl From<SessionSummary> for RemoteSessionSummary {
    fn from(summary: SessionSummary) -> Self {
        Self {
            id: summary.id.to_string(),
            status: summary.status.to_string(),
            title: summary.title,
            updated_at: summary.updated_at,
        }
    }
}

/// Chat-visible snapshot of one session.
///
/// The settings, branch, and review-request fields are optional so a page
/// and a server of different versions still understand each other: an older
/// server omits them, and a page hides what is missing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSession {
    /// Agent provider, such as `codex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Branch the session started from and merges into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Session worktree branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Creation timestamp in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// Stable session identifier.
    pub id: String,
    /// Whether the session defers its worktree until its first message.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_draft: bool,
    /// Durable transcript without hidden agent prompts.
    pub messages: Vec<RemoteMessage>,
    /// Model name, as `get_host` reports `default_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Upstream branch the session branch was published to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_upstream_ref: Option<String>,
    /// Pending clarification questions.
    pub questions: Vec<RemoteQuestion>,
    /// Chat messages waiting behind the active turn.
    pub queued_messages: Vec<String>,
    /// Reasoning level, such as `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Style of the agent's answers, such as `concise`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_style: Option<String>,
    /// Linked pull or merge request, when the session has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_request: Option<RemoteReviewRequest>,
    /// Speed preference, `normal` or `fast`. Absent when the agent has no
    /// speed control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mode: Option<String>,
    /// Lifecycle status, such as `InProgress` or `Review`.
    pub status: String,
    /// Optional user-visible title.
    pub title: Option<String>,
    /// Last update timestamp in Unix seconds.
    pub updated_at: i64,
}

impl From<Session> for RemoteSession {
    fn from(session: Session) -> Self {
        let settings = session.settings;
        let agent = settings.agent;

        Self {
            agent: Some(agent.kind().name().to_string()),
            base_branch: Some(settings.base_branch),
            branch: Some(session_branch(&session.id)),
            created_at: Some(session.created_at),
            is_draft: settings.is_draft,
            model: Some(agent.model().name().to_string()),
            published_upstream_ref: session.published_upstream_ref,
            reasoning: Some(settings.reasoning_level.as_str().to_string()),
            response_style: Some(settings.response_style.as_str().to_string()),
            review_request: session
                .review_request
                .map(|review_request| RemoteReviewRequest::from(review_request.summary)),
            speed_mode: agent
                .kind()
                .supports_speed_mode()
                .then(|| settings.speed_mode.as_str().to_string()),
            id: session.id.to_string(),
            messages: session
                .messages
                .into_iter()
                .filter_map(RemoteMessage::from_session_message)
                .collect(),
            questions: session
                .questions
                .into_iter()
                .map(|question| RemoteQuestion {
                    options: question.options,
                    text: question.text,
                })
                .collect(),
            queued_messages: session.queued_messages,
            status: session.status.to_string(),
            title: session.title,
            updated_at: session.updated_at,
        }
    }
}

/// Pull or merge request linked to a session, as of the host's latest
/// forge refresh.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteReviewRequest {
    /// Forge display id, such as `#123`.
    pub display_id: String,
    /// Branch published for review.
    pub source_branch: String,
    /// Remote state: `Open`, `Merged`, or `Closed`.
    pub state: String,
    /// Condensed forge status, such as `PR ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_summary: Option<String>,
    /// Branch receiving the review request.
    pub target_branch: String,
    /// Review-request title.
    pub title: String,
    /// Browser URL of the review request. Clients must check its scheme
    /// before they link it.
    pub web_url: String,
}

impl From<ReviewRequestSummary> for RemoteReviewRequest {
    fn from(summary: ReviewRequestSummary) -> Self {
        Self {
            display_id: summary.display_id,
            source_branch: summary.source_branch,
            state: summary.state.as_str().to_string(),
            status_summary: summary.status_summary,
            target_branch: summary.target_branch,
            title: summary.title,
            web_url: summary.web_url,
        }
    }
}

/// One chat-visible transcript message.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteMessage {
    /// Message text.
    pub content: String,
    /// Display category.
    pub kind: RemoteMessageKind,
}

impl RemoteMessage {
    /// Converts a transcript message, omitting prompts hidden from chat.
    fn from_session_message(message: SessionMessage) -> Option<Self> {
        let kind = match message.kind {
            SessionMessageKind::ActivitySummary => RemoteMessageKind::Activity,
            SessionMessageKind::AgentPrompt => return None,
            SessionMessageKind::AssistantAnswer => RemoteMessageKind::Assistant,
            SessionMessageKind::UserPrompt => RemoteMessageKind::User,
            SessionMessageKind::WorkflowNotice => RemoteMessageKind::Notice,
        };

        Some(Self {
            content: message.content,
            kind,
        })
    }
}

/// Display category of one transcript message.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteMessageKind {
    /// Tool and skill usage summary.
    Activity,
    /// Agent answer.
    Assistant,
    /// Workflow notice.
    Notice,
    /// User prompt.
    User,
}

/// One pending clarification question.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteQuestion {
    /// Suggested answers, recommended first.
    pub options: Vec<String>,
    /// Question text.
    pub text: String,
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
