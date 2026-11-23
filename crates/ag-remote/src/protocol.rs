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
    AgentSelection, AgentSelectionMetadata, CreateSessionMode, ExecutionGraphStatus,
    ExtendGraphBudgetRequest, GraphNodeProgress, GraphNodeStatus, GraphResultState, HostContext,
    HostProjectStatus, HostProjectSync, HostProjectSyncPhase, QuestionItem, ReviewRequestSummary,
    Session, SessionDiffFile, SessionDiffFileStatus, SessionError, SessionFileEntry,
    SessionFocusedReview, SessionGraph, SessionGraphAction, SessionGraphActionKind,
    SessionGraphActionRequest, SessionGraphNode, SessionGraphProposal, SessionGraphProposalPhase,
    SessionGraphSummary, SessionGraphs, SessionId, SessionListEntry, SessionMessage,
    SessionMessageKind, session_branch,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// Acts on a session's execution graph as the terminal's inspector
    /// does: accepts an output, retries or replies to a node, resumes or
    /// cancels the graph, or adds provider calls to its spent budget.
    /// Cancelling is permanent. Agentty refuses an action the graph's
    /// `snapshot` no longer matches or does not offer. Offered through the
    /// `graph_actions` capability.
    ActOnSessionGraph(RemoteGraphActionRequest),
    /// Answers the session's complete current question set: its turn's
    /// `questions`, or, with `graph_question_target`, its `graph_questions`,
    /// which go to the graph work that asked them.
    ///
    /// Agentty refuses answers that name graph work other than the one whose
    /// questions are pending now, even when it asks the same questions, and
    /// answers naming graph work while the turn asks its own. Answers that
    /// name no graph work answer only the turn's questions, so a request
    /// from a page that predates graph questions never reaches graph work.
    AnswerQuestions {
        /// Answers paired with the exact question text.
        answers: Vec<RemoteQuestionAnswer>,
        /// The snapshot's `graph_question_target` of the `graph_questions`
        /// answered; absent when answering the turn's `questions`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph_question_target: Option<String>,
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
    /// Starts model work that proposes an execution graph for the task in
    /// `message`, as the terminal's `/graph` does. The proposal appears in
    /// the session's `graph`; nothing launches until a chat reply approves
    /// it. Agentty refuses it unless the session is prepared and idle, so
    /// also for a draft that has not started, and refuses an empty or
    /// oversized task. Offered through the `graph_proposal` capability.
    GenerateGraphProposal {
        /// Task the graph should carry out.
        message: String,
        /// Session that owns the proposal.
        session_id: String,
    },
    /// Describes the host's active project. Offered through the `host`
    /// capability.
    GetHost,
    /// Reads what a session changes against its base branch. Without `path`
    /// the response lists the changed files; with `path` it also carries
    /// that file's patch. Offered through the `diff` capability.
    GetSessionDiff {
        /// Changed file whose patch to return, as the file list names it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Session to read.
        session_id: String,
    },
    /// Loads one session snapshot.
    GetSession {
        /// Session to load.
        session_id: String,
    },
    /// Reads a session's graph proposal and execution graphs as the
    /// terminal's inspector shows them, without acting on any. Offered
    /// through the `graphs` capability.
    GetSessionGraphs {
        /// Graph to select, one the response lists in `graphs`. Without it
        /// the latest proposal's approved graph is selected, or else the
        /// first graph.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph_id: Option<String>,
        /// `version` of the `graphs` response the device shows for this same
        /// selection. When the response would be that version again, it is
        /// `unchanged` instead and carries nothing else.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        known: Option<String>,
        /// Node of the selected graph whose details and outputs to return.
        /// Without it, or when the graph has no such node, the first node.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<String>,
        /// Session to read.
        session_id: String,
    },
    /// Lists the names of the files and directories in the host's active
    /// project checkout, directories first, without their contents, for
    /// mentioning files in a new session's first message. Offered through
    /// the `project_files` capability.
    ListProjectFiles,
    /// Lists the names of the files and directories in a session's worktree,
    /// directories first, without their contents. A session without a
    /// worktree lists nothing. Offered through the `files` capability.
    ListSessionFiles {
        /// Session whose worktree to list.
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
    /// Pushes a session to the branch it was published to before, or under
    /// its default name the first time, and opens or refreshes its review
    /// request with the user's forge credentials. Offered through the
    /// `publish` capability.
    PublishSession {
        /// Session to publish.
        session_id: String,
    },
    /// Sends one chat message, starting, resuming, or queueing a turn.
    SendMessage {
        /// Message text.
        message: String,
        /// Session that receives the message.
        session_id: String,
    },
    /// Stops the session's running turn and returns the session to review,
    /// keeping its worktree and changes. It also discards the session's
    /// queued messages. Offered through the `stop_turn` capability.
    StopTurn {
        /// Session whose turn stops.
        session_id: String,
    },
    /// Syncs the branch of the host's active project with its upstream: a
    /// pull with rebase, then a push, in the host's checkout with the
    /// user's Git credentials. A conflict starts an agent-assisted
    /// resolution. The `project` of a `list_sessions` response reports the
    /// progress and the outcome. Offered through the `sync_project`
    /// capability.
    SyncProject,
    /// Rebases the session branch onto its base branch, or queues the rebase
    /// behind a running turn. A conflict starts an agent-assisted resolution
    /// turn. Offered through the `sync` capability.
    SyncSession {
        /// Session whose branch is synced.
        session_id: String,
    },
    /// Changes the session's settings for later turns; an omitted setting
    /// stays as it is. The fields carry the values a session snapshot
    /// reports, except that `model` is the snapshot's `model_id`. Reasoning
    /// and style apply at once; a model or speed change waits while the
    /// session still has accepted work, as in the terminal. Offered through
    /// the `settings` capability.
    SetSessionSettings {
        /// Model identifier to switch to, one `get_host` lists for the
        /// session's agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Reasoning level, such as `high`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        /// Style of the agent's answers, such as `concise`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_style: Option<String>,
        /// Session whose settings change.
        session_id: String,
        /// Speed preference, `normal` or `fast`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speed_mode: Option<String>,
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

/// The agent, model and settings a new session of one mode starts with, as
/// `get_host` lists it. A device picks other settings for the new session,
/// and another model among the `models` of the same `agent`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteModeModel {
    /// Agent that runs the session, as `models` names it.
    pub agent: String,
    /// Creation mode, one of `session_modes`.
    pub mode: RemoteSessionMode,
    /// Model identifier, as `models` lists it.
    pub model: String,
    /// Display name of the model.
    pub name: String,
    /// Reasoning level the new session starts with, such as `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Style of the new session's answers, such as `concise`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_style: Option<String>,
    /// Speed the new session starts with, `normal` or `fast`. Absent when
    /// the agent has no speed control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_mode: Option<String>,
}

/// One model a session can switch to, as `get_host` lists it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteModelOption {
    /// Agent that runs the model; a session switches only among the models
    /// of its own `agent`, as the terminal's `/model` offers them.
    pub agent: String,
    /// Short description of the model.
    pub description: String,
    /// Model identifier, as a session snapshot reports `model_id` and
    /// `set_session_settings` accepts it as `model`.
    pub model: String,
    /// Display name of the model, as a session snapshot reports `model`.
    pub name: String,
    /// Whether the model can run in fast mode.
    pub supports_fast: bool,
}

impl From<AgentSelection> for RemoteModelOption {
    fn from(selection: AgentSelection) -> Self {
        let model = selection.model();

        Self {
            agent: selection.kind().name().to_string(),
            description: model.description().to_string(),
            model: model.as_str().to_string(),
            name: model.name().to_string(),
            supports_fast: selection.supports_fast_mode(),
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

/// One execution graph action, sent as an `act_on_session_graph` request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphActionRequest {
    /// What to do, one of the actions the `graphs` response lists on the
    /// graph, the node, or the output it applies to.
    pub action: RemoteGraphAction,
    /// Provider calls `extend_budget` adds, from one to the graph's
    /// `budget.max_additional_calls`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_calls: Option<usize>,
    /// The spent budget `extend_budget` adds provider calls to: the graph's
    /// `budget.id` in the `graphs` response the action was chosen from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_id: Option<String>,
    /// Graph to act on, as the `graphs` response names it.
    pub graph_id: String,
    /// Guidance `reply` sends; it must not be blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Node `accept`, `reply`, and `retry` apply to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Output of `node_id` that `accept` accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    /// Session that owns the graph.
    pub session_id: String,
    /// The graph's `snapshot` in the `graphs` response the action was
    /// chosen from.
    pub snapshot: String,
}

impl RemoteGraphActionRequest {
    /// Returns the session API request for this action on `session_id`,
    /// the request's session once the host listed it.
    ///
    /// # Errors
    /// Rejects an action this server does not know and one without a field
    /// it needs.
    pub(crate) fn into_session_request(
        self,
        session_id: SessionId,
    ) -> Result<SessionGraphActionRequest, SessionError> {
        let Self {
            action,
            additional_calls,
            budget_id,
            graph_id,
            message,
            node_id,
            output_id,
            session_id: _,
            snapshot,
        } = self;
        let action = match action {
            RemoteGraphAction::Accept => SessionGraphAction::Accept {
                node_id: required_field(node_id, "node_id")?,
                output_id: required_field(output_id, "output_id")?,
            },
            RemoteGraphAction::Cancel => SessionGraphAction::Cancel,
            RemoteGraphAction::ExtendBudget => SessionGraphAction::ExtendBudget {
                additional_calls: required_field(additional_calls, "additional_calls")?,
                budget_id: required_field(budget_id, "budget_id")?,
            },
            RemoteGraphAction::Reply => SessionGraphAction::Reply {
                message: required_field(message, "message")?,
                node_id: required_field(node_id, "node_id")?,
            },
            RemoteGraphAction::Resume => SessionGraphAction::Resume,
            RemoteGraphAction::Retry => SessionGraphAction::Retry {
                node_id: required_field(node_id, "node_id")?,
            },
            RemoteGraphAction::Unsupported => {
                return Err(SessionError::Operation(
                    UNSUPPORTED_GRAPH_ACTION.to_string(),
                ));
            }
        };

        Ok(SessionGraphActionRequest {
            action,
            graph_id,
            session_id,
            snapshot,
        })
    }
}

/// Error returned for a graph action this server does not know.
const UNSUPPORTED_GRAPH_ACTION: &str = "Unsupported graph action";

/// Returns `value`, or an error naming the missing `field`.
fn required_field<Value>(value: Option<Value>, field: &str) -> Result<Value, SessionError> {
    value.ok_or_else(|| SessionError::Operation(format!("Graph action needs `{field}`")))
}

/// An action of the terminal's graph inspector.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteGraphAction {
    /// Accept a validated output, releasing the nodes that wait for it.
    Accept,
    /// Cancel the graph for good.
    Cancel,
    /// Add provider calls to the graph's spent budget.
    ExtendBudget,
    /// Send guidance that revises a settled node's output.
    Reply,
    /// Restart a paused graph.
    Resume,
    /// Run a failed or cancelled node again.
    Retry,
    /// Any action this server does not know, such as one a newer client
    /// sends. The server answers it with an error.
    #[serde(other)]
    Unsupported,
}

impl From<SessionGraphActionKind> for RemoteGraphAction {
    fn from(action: SessionGraphActionKind) -> Self {
        match action {
            SessionGraphActionKind::Accept => Self::Accept,
            SessionGraphActionKind::Cancel => Self::Cancel,
            SessionGraphActionKind::ExtendBudget => Self::ExtendBudget,
            SessionGraphActionKind::Reply => Self::Reply,
            SessionGraphActionKind::Resume => Self::Resume,
            SessionGraphActionKind::Retry => Self::Retry,
        }
    }
}

/// The wire form of `actions`.
fn remote_actions(actions: Vec<SessionGraphActionKind>) -> Vec<RemoteGraphAction> {
    actions.into_iter().map(RemoteGraphAction::from).collect()
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
    /// What a session changes, answering `get_session_diff`.
    Diff {
        /// Changed files in diff order; only the requested file when the
        /// request names one.
        files: Vec<RemoteDiffFile>,
        /// Unified patch of the requested file, cut at a line boundary when
        /// it is too large.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        patch: Option<String>,
        /// Whether the server left out the end of `files` or `patch` to
        /// keep the response small.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// A session's graphs, answering `get_session_graphs`.
    Graphs {
        /// The selected graph, absent when the session has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graph: Option<Box<RemoteGraph>>,
        /// Every graph of the session in creation order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        graphs: Vec<RemoteGraphEntry>,
        /// The session's latest graph proposal, absent when it has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposal: Option<Box<RemoteGraphProposal>>,
        /// Whether the response is the request's `known` version again; it
        /// then carries only `version`, and the device keeps what it shows.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unchanged: bool,
        /// Opaque token for the content of this response: equal content
        /// gives an equal token within one Agentty process. Absent from
        /// older servers.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        version: String,
    },
    /// The host's active project, where new sessions are created.
    Host {
        /// Branch new sessions start from.
        base_branch: String,
        /// Model a new session starts with.
        default_model: String,
        /// Agent and model a new session of each of `session_modes` starts
        /// with, in the same order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mode_models: Vec<RemoteModeModel>,
        /// Models a session can switch to now, in display order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        models: Vec<RemoteModelOption>,
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
    /// The files and directories in a session's worktree or the active
    /// project's checkout, answering `list_session_files` or
    /// `list_project_files`.
    SessionFiles {
        /// Listed entries, directories first, then by path.
        files: Vec<RemoteFileEntry>,
        /// Whether the server left out the end of `files` to keep the
        /// response small.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// The review request a publish created or refreshed.
    ReviewRequest {
        /// Review request as the forge reported it.
        review_request: RemoteReviewRequest,
    },
    /// The host's loaded sessions in display order.
    Sessions {
        /// The active project the sessions belong to. Absent from hosts
        /// that do not report it and from older servers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<RemoteProject>,
        /// Session listing entries.
        sessions: Vec<RemoteSessionSummary>,
    },
}

impl RemoteResponse {
    /// Builds the `diff` response for a session's changed `files`: the file
    /// list, or with `path` that file and its patch. Returns `None` when
    /// `path` names no changed file.
    ///
    /// The file list and the patch each stay within
    /// [`MAX_FILE_RESPONSE_BYTES`] once encoded, so a response stays well
    /// under the frame limit; whatever is left out sets `truncated`.
    pub(crate) fn from_diff(files: &[SessionDiffFile], path: Option<&str>) -> Option<Self> {
        let Some(path) = path else {
            let mut remaining = MAX_FILE_RESPONSE_BYTES;
            let mut listed = Vec::new();
            for file in files {
                let size = DIFF_FILE_ENTRY_BYTES
                    + encoded_len(&file.path)
                    + file.old_path.as_deref().map_or(0, encoded_len);
                if size > remaining {
                    return Some(Self::Diff {
                        files: listed,
                        patch: None,
                        truncated: true,
                    });
                }
                remaining -= size;
                listed.push(RemoteDiffFile::from(file));
            }

            return Some(Self::Diff {
                files: listed,
                patch: None,
                truncated: false,
            });
        };
        let file = files.iter().find(|file| file.path == path)?;
        let (kept_patch, truncated) = capped_text(&file.patch, MAX_FILE_RESPONSE_BYTES);

        Some(Self::Diff {
            files: vec![RemoteDiffFile::from(file)],
            patch: Some(kept_patch.to_string()),
            truncated,
        })
    }

    /// Builds the `session_files` response for the `entries` of a session's
    /// worktree or the project checkout, in the host's order.
    ///
    /// The entries stay within [`MAX_FILE_RESPONSE_BYTES`] once encoded, so
    /// a response stays well under the frame limit; leaving out the rest
    /// sets `truncated`.
    pub(crate) fn from_session_files(entries: &[SessionFileEntry]) -> Self {
        let mut remaining = MAX_FILE_RESPONSE_BYTES;
        let mut files = Vec::new();
        for entry in entries {
            let size = FILE_ENTRY_BYTES + encoded_len(&entry.path);
            if size > remaining {
                return Self::SessionFiles {
                    files,
                    truncated: true,
                };
            }
            remaining -= size;
            files.push(RemoteFileEntry {
                is_dir: entry.is_dir,
                path: entry.path.clone(),
            });
        }

        Self::SessionFiles {
            files,
            truncated: false,
        }
    }

    /// Builds the `graphs` response for a session's `graphs`.
    ///
    /// The proposal preview stays within [`MAX_FILE_RESPONSE_BYTES`] encoded
    /// bytes, and so do the selected node's output texts together, so a
    /// response stays well under the frame limit; whatever is cut sets
    /// `truncated`. Its `version` hashes everything else it carries, so it
    /// changes whenever any part of the response does.
    pub(crate) fn from_graphs(graphs: SessionGraphs) -> Self {
        let graph = graphs.graph.map(|graph| Box::new(RemoteGraph::from(graph)));
        let entries = graphs
            .graphs
            .into_iter()
            .map(|entry| RemoteGraphEntry {
                id: entry.id,
                is_focused_review: entry.is_focused_review,
                revision: entry.revision.get(),
            })
            .collect::<Vec<_>>();
        let proposal = graphs
            .proposal
            .map(|proposal| Box::new(RemoteGraphProposal::from(proposal)));
        let digest = Sha256::digest(format!("{:?}", (&graph, &entries, &proposal)));

        Self::Graphs {
            graph,
            graphs: entries,
            proposal,
            unchanged: false,
            version: hex::encode(&digest[..16]),
        }
    }

    /// Returns this response, or only its `version` marked `unchanged` when
    /// it is a `graphs` response of version `known`, the one the device
    /// already shows.
    pub(crate) fn unchanged_since(self, known: Option<&str>) -> Self {
        match self {
            Self::Graphs { version, .. } if known == Some(version.as_str()) => Self::Graphs {
                graph: None,
                graphs: Vec::new(),
                proposal: None,
                unchanged: true,
                version,
            },
            response => response,
        }
    }
}

/// Most encoded bytes of file entries or patch text that one `diff` or
/// `session_files` response carries, and, in a `graphs` response, of the
/// proposal preview, of the output texts together, and of the node texts
/// together: a quarter of the 1 MiB frame limit each.
pub(crate) const MAX_FILE_RESPONSE_BYTES: usize = 256 * 1024;

/// Encoded bytes a group of texts in one response may still take.
struct TextBudget(usize);

impl TextBudget {
    /// Returns `text` cut at a line boundary to what the budget has left,
    /// and whether anything was cut, charging the budget what it keeps.
    fn take(&mut self, text: &str) -> (String, bool) {
        let (kept, is_cut) = capped_text(text, self.0);
        self.0 -= encoded_len(kept);

        (kept.to_string(), is_cut)
    }
}

/// Encoded bytes counted for one file entry besides its paths: field names,
/// line counts, status, and punctuation.
const DIFF_FILE_ENTRY_BYTES: usize = 128;

/// Encoded bytes counted for one worktree entry besides its path: field
/// names, the directory flag, and punctuation.
const FILE_ENTRY_BYTES: usize = 32;

/// Returns the length of `text` once escaped as a JSON string, without its
/// quotes.
fn encoded_len(text: &str) -> usize {
    text.chars().map(encoded_char_len).sum()
}

/// Returns the length of `character` once escaped in a JSON string.
fn encoded_char_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        character if character < ' ' => 6,
        character => character.len_utf8(),
    }
}

/// Returns `text` cut to `max_bytes` encoded bytes at the last whole line,
/// and whether anything was cut.
fn capped_text(text: &str, max_bytes: usize) -> (&str, bool) {
    let mut size = 0;
    for (index, character) in text.char_indices() {
        size += encoded_char_len(character);
        if size > max_bytes {
            let kept = &text[..index];
            let kept = kept.rfind('\n').map_or(kept, |end| &kept[..=end]);

            return (kept, true);
        }
    }

    (text, false)
}

/// One graph of a session in a `graphs` response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphEntry {
    /// Graph identity, as `get_session_graphs` accepts it.
    pub id: String,
    /// Whether the graph is a focused review that Agentty drives.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_focused_review: bool,
    /// Current revision.
    pub revision: u64,
}

/// A session's latest graph proposal.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphProposal {
    /// Graph the approved version launched, absent before approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
    /// How the user acts on the pending proposal, absent once it is
    /// approved or cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Proposal identity.
    pub id: String,
    /// Where the proposal stands.
    pub phase: RemoteGraphProposalPhase,
    /// The proposal's details as Markdown, including the captured request
    /// and each proposed node's captured instructions.
    pub preview: String,
    /// Clarification questions the version asks before it resolves. Agentty
    /// bounds them with the size of the proposal's response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<RemoteQuestion>,
    /// Whether the server cut the end of `preview`, at a line boundary, to
    /// keep the response small.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Version number.
    pub version: u32,
}

impl From<SessionGraphProposal> for RemoteGraphProposal {
    /// Converts `proposal`, cutting its preview to
    /// [`MAX_FILE_RESPONSE_BYTES`] encoded bytes.
    fn from(proposal: SessionGraphProposal) -> Self {
        let version = &proposal.state.version;
        let (preview, truncated) = TextBudget(MAX_FILE_RESPONSE_BYTES).take(&proposal.preview);

        Self {
            graph_id: version
                .approval
                .as_ref()
                .map(|approval| approval.graph_id.clone()),
            hint: proposal.hint,
            id: proposal.state.input.id.clone(),
            phase: RemoteGraphProposalPhase::from(proposal.phase),
            preview,
            questions: proposal
                .questions
                .iter()
                .map(RemoteQuestion::from)
                .collect(),
            truncated,
            version: version.number,
        }
    }
}

/// Where a graph proposal stands.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteGraphProposalPhase {
    /// The user approved the version and its graph launched.
    Approved,
    /// The version resolved to a graph waiting for the user's approval.
    AwaitingApproval,
    /// The proposal was discarded.
    Cancelled,
    /// Generation failed.
    Failed,
    /// The model is generating the version.
    Generating,
    /// The model asked clarification questions.
    NeedsInput,
    /// The model proposed a graph Agentty could not resolve.
    Unresolved,
}

impl From<SessionGraphProposalPhase> for RemoteGraphProposalPhase {
    fn from(phase: SessionGraphProposalPhase) -> Self {
        match phase {
            SessionGraphProposalPhase::Approved => Self::Approved,
            SessionGraphProposalPhase::AwaitingApproval => Self::AwaitingApproval,
            SessionGraphProposalPhase::Cancelled => Self::Cancelled,
            SessionGraphProposalPhase::Failed => Self::Failed,
            SessionGraphProposalPhase::Generating => Self::Generating,
            SessionGraphProposalPhase::NeedsInput => Self::NeedsInput,
            SessionGraphProposalPhase::Unresolved => Self::Unresolved,
        }
    }
}

/// The selected graph of a `graphs` response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraph {
    /// What `act_on_session_graph` can do to the graph as a whole now:
    /// `cancel`, `extend_budget`, or `resume`, as the terminal's inspector
    /// offers them. Absent when it can do none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<RemoteGraphAction>,
    /// The spent budget `extend_budget` adds provider calls to, present
    /// while `actions` offers it: the selected node's, or else the first
    /// one a pending node waits for, as the terminal's inspector shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<RemoteGraphBudget>,
    /// Graph identity.
    pub id: String,
    /// Whether the graph is a focused review that Agentty drives.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_focused_review: bool,
    /// The selected node, absent while the graph has no progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Every node in declaration order.
    pub nodes: Vec<RemoteGraphNode>,
    /// What Agentty says about how the graph is driven, when it says
    /// anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Outputs of the selected node's latest execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<RemoteGraphOutput>,
    /// Current revision.
    pub revision: u64,
    /// Opaque token of the shown graph state; it changes whenever the
    /// graph's revision, status, or node progress does.
    pub snapshot: String,
    /// Live status, absent when Agentty has no progress for the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RemoteGraphStatus>,
    /// `status` as the terminal shows it.
    pub status_label: String,
}

impl From<SessionGraph> for RemoteGraph {
    /// Converts `graph`. Its output texts share
    /// [`MAX_FILE_RESPONSE_BYTES`] encoded bytes, in order, and so do its
    /// nodes' objectives, errors, and questions; each part that is cut says
    /// so.
    fn from(graph: SessionGraph) -> Self {
        let mut output_budget = TextBudget(MAX_FILE_RESPONSE_BYTES);
        let mut node_budget = TextBudget(MAX_FILE_RESPONSE_BYTES);
        let outputs = graph
            .outputs
            .iter()
            .map(|output| {
                let (text, truncated) = output_budget.take(&output.display);
                let reference = &output.result.reference;

                RemoteGraphOutput {
                    actions: remote_actions(output.actions.clone()),
                    id: reference.id.clone(),
                    name: reference.output_name.clone(),
                    state: RemoteGraphOutputState::from(output.result.state),
                    text,
                    truncated,
                    version: reference.version.get(),
                }
            })
            .collect();

        Self {
            actions: remote_actions(graph.actions),
            budget: graph.exhausted_budget.map(|id| RemoteGraphBudget {
                id,
                max_additional_calls: ExtendGraphBudgetRequest::MAX_ADDITIONAL_CALLS,
            }),
            id: graph.id,
            is_focused_review: graph.is_focused_review,
            node: graph.node_id,
            nodes: graph
                .nodes
                .into_iter()
                .map(|node| RemoteGraphNode::new(node, &mut node_budget))
                .collect(),
            note: graph.note,
            outputs,
            revision: graph.revision.get(),
            snapshot: graph.snapshot,
            status: graph.status.map(RemoteGraphStatus::from),
            status_label: graph.status_label,
        }
    }
}

/// A graph's spent provider-call budget.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphBudget {
    /// Budget name.
    pub id: String,
    /// Most provider calls one `extend_budget` action adds.
    pub max_additional_calls: usize,
}

/// Live status of a graph.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteGraphStatus {
    /// Prerequisites, acceptance, clarification, or resources hold it.
    Blocked,
    /// It was cancelled.
    Cancelled,
    /// Every node settled successfully.
    Completed,
    /// A node needs a retry.
    Failed,
    /// No driver runs it; it needs a resume.
    Paused,
    /// Work is queued or running.
    Running,
}

impl From<ExecutionGraphStatus> for RemoteGraphStatus {
    fn from(status: ExecutionGraphStatus) -> Self {
        match status {
            ExecutionGraphStatus::Blocked => Self::Blocked,
            ExecutionGraphStatus::Cancelled => Self::Cancelled,
            ExecutionGraphStatus::Completed => Self::Completed,
            ExecutionGraphStatus::Failed => Self::Failed,
            ExecutionGraphStatus::Paused => Self::Paused,
            ExecutionGraphStatus::Running => Self::Running,
        }
    }
}

/// One node of a graph.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphNode {
    /// What `act_on_session_graph` can do to the node now: `reply` or
    /// `retry`, as the terminal's inspector offers them with the node
    /// selected. Absent when it can do neither.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<RemoteGraphAction>,
    /// Name of the agent definition the node runs.
    pub agent: String,
    /// Agent provider, such as `codex`.
    pub agent_kind: String,
    /// Executions admitted so far, retries included.
    pub attempts: usize,
    /// Whether a dependent waits for the user to accept the node's output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub awaits_approval: bool,
    /// Why the pending node cannot dispatch, each as the terminal says it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,
    /// The latest failure, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How the user acts on the node while it waits for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Node identity, as `get_session_graphs` accepts it.
    pub id: String,
    /// The node's status as the terminal shows it.
    pub label: String,
    /// Model identifier the node runs.
    pub model: String,
    /// What the node should achieve.
    pub objective: String,
    /// Questions from the node's latest settled response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<RemoteQuestion>,
    /// Reasoning effort the node runs with, such as `high`.
    pub reasoning: String,
    /// The node's dependencies in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<RemoteGraphRequirement>,
    /// Live state of the node's latest execution.
    pub status: RemoteGraphNodeStatus,
    /// Whether the server cut `objective`, `error`, or a question, at a
    /// line boundary, to keep the response small.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

impl RemoteGraphNode {
    /// Converts `node`, charging its objective, error, and questions to
    /// `budget`; whatever is cut sets `truncated`.
    fn new(node: SessionGraphNode, budget: &mut TextBudget) -> Self {
        let progress = node.progress.unwrap_or_else(|| GraphNodeProgress {
            blocked: Vec::new(),
            executions: Vec::new(),
            last_error: None,
            node_id: node.node.id.clone(),
            questions: Vec::new(),
            results: Vec::new(),
            status: GraphNodeStatus::Pending,
        });
        let mut truncated = false;
        let mut take = |text: &str| {
            let (kept, is_cut) = budget.take(text);
            truncated |= is_cut;

            kept
        };
        let objective = take(&node.node.objective);
        let error = progress.last_error.as_deref().map(&mut take);
        let questions = progress
            .questions
            .iter()
            .map(|question| RemoteQuestion {
                options: question.options.iter().map(|option| take(option)).collect(),
                text: take(&question.text),
            })
            .collect();

        Self {
            actions: remote_actions(node.actions),
            agent: node.node.definition.name,
            agent_kind: node.node.agent.kind().name().to_string(),
            attempts: progress.executions.len(),
            awaits_approval: node.awaits_approval,
            blocked: node.blocked,
            error,
            hint: node.hint,
            id: node.node.id,
            label: node.status_label,
            model: node.node.agent.model().as_str().to_string(),
            objective,
            questions,
            reasoning: node.reasoning_level.as_str().to_string(),
            requires: node
                .requirements
                .into_iter()
                .map(|requirement| RemoteGraphRequirement {
                    gate: requirement.gate,
                    label: requirement.label,
                    node_id: requirement.node_id,
                    satisfied: requirement.is_satisfied,
                })
                .collect(),
            status: RemoteGraphNodeStatus::from(progress.status),
            truncated,
        }
    }
}

/// Live state of a graph node.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteGraphNodeStatus {
    /// It waits for answers to its questions.
    AwaitingAnswers,
    /// It or its graph was cancelled.
    Cancelled,
    /// It failed and needs a retry.
    Failed,
    /// It has not been admitted.
    Pending,
    /// It waits for a worker.
    Queued,
    /// A worker runs it.
    Running,
    /// It settled without questions.
    Succeeded,
}

impl From<GraphNodeStatus> for RemoteGraphNodeStatus {
    fn from(status: GraphNodeStatus) -> Self {
        match status {
            GraphNodeStatus::AwaitingAnswers => Self::AwaitingAnswers,
            GraphNodeStatus::Cancelled => Self::Cancelled,
            GraphNodeStatus::Failed => Self::Failed,
            GraphNodeStatus::Pending => Self::Pending,
            GraphNodeStatus::Queued => Self::Queued,
            GraphNodeStatus::Running => Self::Running,
            GraphNodeStatus::Succeeded => Self::Succeeded,
        }
    }
}

/// One dependency of a graph node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphRequirement {
    /// What the dependency waits for, as the terminal's inspector says it.
    pub gate: String,
    /// The dependency as the terminal's node timeline shows it.
    pub label: String,
    /// Node the dependency waits for.
    pub node_id: String,
    /// Whether the dependency no longer blocks the node.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub satisfied: bool,
}

/// One output of the selected graph node.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphOutput {
    /// What `act_on_session_graph` can do to the output now: `accept`, as
    /// the terminal's inspector offers it with the output selected. Absent
    /// when it can do nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<RemoteGraphAction>,
    /// Output identity.
    pub id: String,
    /// Output name the node declares.
    pub name: String,
    /// Whether the output was validated or accepted.
    pub state: RemoteGraphOutputState,
    /// The output as Markdown, cut at a line boundary when it is too large;
    /// empty when earlier outputs left it no room.
    pub text: String,
    /// Whether the server cut the end of `text` to keep the response small.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Version within the node's named output.
    pub version: u64,
}

/// Evidence state of a graph output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteGraphOutputState {
    /// The user accepted it.
    Accepted,
    /// It was saved but not validated.
    Produced,
    /// It was validated and can be accepted.
    Validated,
}

impl From<GraphResultState> for RemoteGraphOutputState {
    fn from(state: GraphResultState) -> Self {
        match state {
            GraphResultState::Accepted => Self::Accepted,
            GraphResultState::Produced => Self::Produced,
            GraphResultState::Validated => Self::Validated,
        }
    }
}

/// What a session snapshot says about its latest graph proposal and, once
/// approved, the graph it launched.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSessionGraph {
    /// Outputs that wait for the user's approval, in declaration order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub awaiting: Vec<RemoteGraphApproval>,
    /// Nodes of the launched graph that succeeded.
    pub done: usize,
    /// Line describing `phase`, or `status` once known, as the terminal
    /// shows it.
    pub label: String,
    /// Where the latest proposal stands.
    pub phase: RemoteGraphProposalPhase,
    /// Live status of the approved graph, once Agentty reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RemoteGraphStatus>,
    /// Nodes of the resolved proposal; zero before it resolves.
    pub total: usize,
    /// Number of the latest proposal version.
    pub version: u32,
}

impl From<SessionGraphSummary> for RemoteSessionGraph {
    fn from(summary: SessionGraphSummary) -> Self {
        Self {
            awaiting: summary
                .awaiting_approval
                .into_iter()
                .map(|approval| RemoteGraphApproval {
                    node_id: approval.node_id,
                    revisable: approval.takes_revision,
                    text: approval.text,
                })
                .collect(),
            done: summary.done,
            label: summary.label,
            phase: RemoteGraphProposalPhase::from(summary.phase),
            status: summary.status.map(RemoteGraphStatus::from),
            total: summary.total,
            version: summary.version,
        }
    }
}

/// One output that waits for the user's approval.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteGraphApproval {
    /// Node that produced the output.
    pub node_id: String,
    /// Whether a chat reply may revise the output instead of accepting it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revisable: bool,
    /// Line telling the user how to act on it, as the terminal says it.
    pub text: String,
}

impl From<HostContext> for RemoteResponse {
    fn from(host: HostContext) -> Self {
        let mode_models: Vec<RemoteModeModel> = host
            .session_modes
            .iter()
            .filter_map(|session_mode| {
                let mode = RemoteSessionMode::from_session_mode(&session_mode.mode)?;
                let option = RemoteModelOption::from(session_mode.agent);

                Some(RemoteModeModel {
                    agent: option.agent,
                    mode,
                    model: option.model,
                    name: option.name,
                    reasoning: Some(session_mode.reasoning_level.as_str().to_string()),
                    response_style: Some(session_mode.response_style.as_str().to_string()),
                    speed_mode: session_mode
                        .agent
                        .kind()
                        .supports_speed_mode()
                        .then(|| session_mode.speed_mode.as_str().to_string()),
                })
            })
            .collect();

        Self::Host {
            base_branch: host.base_branch,
            default_model: host.default_model.name().to_string(),
            models: host
                .models
                .into_iter()
                .map(RemoteModelOption::from)
                .collect(),
            project: host.project_name,
            session_modes: mode_models
                .iter()
                .map(|mode_model| mode_model.mode)
                .collect(),
            mode_models,
        }
    }
}

/// The host's active project as the session list shows it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteProject {
    /// Commits the branch is ahead of its upstream, as of the host's latest
    /// fetch. Absent while the host has not compared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u32>,
    /// Commits the branch is behind its upstream, as of the host's latest
    /// fetch. Absent while the host has not compared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u32>,
    /// Branch the project's checkout is on. Absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Display name of the project.
    pub name: String,
    /// Latest sync of this project, started by a device or in the terminal,
    /// while the host still shows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<RemoteProjectSync>,
}

impl From<HostProjectStatus> for RemoteProject {
    fn from(status: HostProjectStatus) -> Self {
        Self {
            ahead: status.ahead_behind.map(|(ahead, _)| ahead),
            behind: status.ahead_behind.map(|(_, behind)| behind),
            branch: status.branch,
            name: status.project_name,
            sync: status.sync.map(RemoteProjectSync::from),
        }
    }
}

/// One sync of the active project's branch with its upstream.
///
/// The count and message fields depend on `phase`; a page ignores the ones
/// it does not expect.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteProjectSync {
    /// Branch the sync updates.
    pub branch: String,
    /// Files in conflict that an agent is resolving, or has resolved once
    /// the sync is complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<usize>,
    /// Why the sync is blocked or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// How far the sync has come.
    pub phase: RemoteProjectSyncPhase,
    /// Commits a complete sync pulled, when the host counted them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pulled_commits: Option<u32>,
    /// Commits a complete sync pushed, when the host counted them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_commits: Option<u32>,
}

impl From<HostProjectSync> for RemoteProjectSync {
    fn from(sync: HostProjectSync) -> Self {
        let mut remote = Self {
            branch: sync.branch,
            conflicts: None,
            message: None,
            phase: RemoteProjectSyncPhase::Running,
            pulled_commits: None,
            pushed_commits: None,
        };
        match sync.phase {
            HostProjectSyncPhase::Running => {}
            HostProjectSyncPhase::ResolvingConflicts {
                conflicted_file_count,
            } => {
                remote.phase = RemoteProjectSyncPhase::ResolvingConflicts;
                remote.conflicts = Some(conflicted_file_count);
            }
            HostProjectSyncPhase::Complete {
                pulled_commits,
                pushed_commits,
                resolved_conflict_count,
            } => {
                remote.phase = RemoteProjectSyncPhase::Complete;
                remote.conflicts = (resolved_conflict_count > 0).then_some(resolved_conflict_count);
                remote.pulled_commits = pulled_commits;
                remote.pushed_commits = pushed_commits;
            }
            HostProjectSyncPhase::Blocked { message } => {
                remote.phase = RemoteProjectSyncPhase::Blocked;
                remote.message = Some(message);
            }
            HostProjectSyncPhase::Failed { message } => {
                remote.phase = RemoteProjectSyncPhase::Failed;
                remote.message = Some(message);
            }
        }

        remote
    }
}

/// How far a project sync has come.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteProjectSyncPhase {
    /// The host did not start the sync, such as while a merge runs.
    Blocked,
    /// The branch matches its upstream.
    Complete,
    /// Git, authentication, or conflict resolution failed.
    Failed,
    /// An agent is resolving conflicts.
    ResolvingConflicts,
    /// The host is pulling, rebasing, or pushing.
    Running,
}

/// One file a session's diff changes.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteDiffFile {
    /// Lines the file's patch adds.
    pub added_lines: u64,
    /// Lines the file's patch removes.
    pub deleted_lines: u64,
    /// Path before a rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// Path after the change; a deleted file keeps its old path.
    pub path: String,
    /// How the change affects the file.
    pub status: RemoteDiffFileStatus,
}

impl From<&SessionDiffFile> for RemoteDiffFile {
    fn from(file: &SessionDiffFile) -> Self {
        Self {
            added_lines: file.added_lines,
            deleted_lines: file.deleted_lines,
            old_path: file.old_path.clone(),
            path: file.path.clone(),
            status: match file.status {
                SessionDiffFileStatus::Added => RemoteDiffFileStatus::Added,
                SessionDiffFileStatus::Deleted => RemoteDiffFileStatus::Deleted,
                SessionDiffFileStatus::Modified => RemoteDiffFileStatus::Modified,
                SessionDiffFileStatus::Renamed => RemoteDiffFileStatus::Renamed,
            },
        }
    }
}

/// How a session's diff changes one file.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteDiffFileStatus {
    /// The file is new.
    Added,
    /// The file is removed.
    Deleted,
    /// The file's contents or mode changed.
    Modified,
    /// The file moved, possibly with changes.
    Renamed,
}

/// One file or directory in a session's worktree.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteFileEntry {
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Path relative to the worktree root.
    pub path: String,
}

/// Listing entry for one session.
///
/// The stats fields are optional: a host that lists only summaries, or an
/// older server, omits them, and a page hides what is missing.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteSessionSummary {
    /// Lines the session's diff adds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_lines: Option<u64>,
    /// Commits the session branch is ahead of its base branch. Absent while
    /// the host has not compared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ahead: Option<u32>,
    /// Commits the session branch is behind its base branch. Absent while
    /// the host has not compared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_behind: Option<u32>,
    /// Lines the session's diff removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_lines: Option<u64>,
    /// Whether merging the session branch into its base would conflict.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_merge_conflict: bool,
    /// Stable session identifier.
    pub id: String,
    /// Start of the current active-work interval in Unix seconds, while the
    /// session is running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress_started_at: Option<i64>,
    /// Active-work seconds completed before the current interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress_total_seconds: Option<i64>,
    /// Whether the session branch was published to an upstream branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_published: bool,
    /// Model name, as `get_host` reports `default_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Compact model name for dense rows, such as `opus-5.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_short_name: Option<String>,
    /// Reasoning level, such as `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Forge display id of the linked review request, such as `#123`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_display_id: Option<String>,
    /// Whether the linked pull request can merge, when the forge says so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_ready: Option<RemoteReviewReadiness>,
    /// State of the linked review request: `Open`, `Merged`, or `Closed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
    /// Host size bucket of the diff, from `XS` to `XXL`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// Lifecycle status, such as `InProgress` or `Review`.
    pub status: String,
    /// Optional user-visible title.
    pub title: Option<String>,
    /// Last update timestamp in Unix seconds.
    pub updated_at: i64,
    /// Commits the session branch is ahead of the upstream branch it
    /// tracks. Absent when it tracks none or the host has not compared
    /// them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_ahead: Option<u32>,
    /// Commits the session branch is behind the upstream branch it tracks.
    /// Absent when it tracks none or the host has not compared them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_behind: Option<u32>,
}

impl From<SessionListEntry> for RemoteSessionSummary {
    fn from(entry: SessionListEntry) -> Self {
        let summary = entry.summary;
        let mut remote = Self {
            id: summary.id.to_string(),
            status: summary.status.to_string(),
            title: summary.title,
            updated_at: summary.updated_at,
            ..Self::default()
        };
        let Some(stats) = entry.stats else {
            return remote;
        };
        remote.added_lines = Some(stats.added_lines);
        remote.base_ahead = stats.base_ahead_behind.map(|(ahead, _)| ahead);
        remote.base_behind = stats.base_ahead_behind.map(|(_, behind)| behind);
        remote.deleted_lines = Some(stats.deleted_lines);
        remote.has_merge_conflict = stats.has_merge_conflict;
        remote.in_progress_started_at = stats.in_progress_started_at;
        remote.in_progress_total_seconds = Some(stats.in_progress_total_seconds);
        remote.is_published = stats.is_published;
        remote.model = Some(stats.model.name().to_string());
        remote.model_short_name = Some(stats.model.short_name().to_string());
        remote.reasoning = Some(stats.reasoning_level.as_str().to_string());
        if let Some(review_request) = stats.review_request {
            remote.review_ready = RemoteReviewReadiness::from_summary(&review_request);
            remote.review_state = Some(review_request.state.as_str().to_string());
            remote.review_display_id = Some(review_request.display_id);
        }
        remote.size = Some(stats.size_label);
        remote.upstream_ahead = stats.upstream_ahead_behind.map(|(ahead, _)| ahead);
        remote.upstream_behind = stats.upstream_ahead_behind.map(|(_, behind)| behind);

        remote
    }
}

/// Whether a linked pull request can merge, as the forge reported it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteReviewReadiness {
    /// Checks pass and the pull request can merge.
    Ready,
    /// The pull request can merge only by bypassing branch protections.
    ReadyWithBypass,
}

impl RemoteReviewReadiness {
    /// Returns the readiness the forge reported for `summary`, if any.
    fn from_summary(summary: &ReviewRequestSummary) -> Option<Self> {
        if summary.is_github_pr_ready() {
            Some(Self::Ready)
        } else if summary.is_github_pr_bypass_ready() {
            Some(Self::ReadyWithBypass)
        } else {
            None
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
    /// Messages a draft session has staged and will send together when it
    /// starts. Absent when it has none and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_prompt: Option<String>,
    /// Latest focused review of the session's changes, while the terminal
    /// shows it. Absent when there is none, while a review runs, and from
    /// older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focused_review: Option<RemoteFocusedReview>,
    /// The session's latest graph proposal and, once approved, its graph's
    /// progress, as the terminal summarizes them beside the chat. Absent
    /// when the session has no proposal and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<RemoteSessionGraph>,
    /// Opaque key of the graph work that asks `graph_questions`, which other
    /// graph work asking the same questions does not share, so a device can
    /// tell a new set from one it closed. It identifies no proposal or node;
    /// `answer_questions` echoes it as `graph_question_target` so the
    /// answers reach only the work they were entered for. Absent without
    /// graph questions and from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_question_target: Option<String>,
    /// Clarification questions of the session's graph work, its pending
    /// graph proposal or one of its graph nodes, while its turn asks none.
    /// `answer_questions` naming their `graph_question_target` answers them,
    /// and so does a chat message, so the status need not be `Question`.
    /// Absent when there are none and from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graph_questions: Vec<RemoteQuestion>,
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
    /// Model identifier, as `get_host` lists it in `models` and
    /// `set_session_settings` accepts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// Live line describing the session's current work, such as
    /// `Working... Reading files`, while it works or waits. Further lines add
    /// detail. Absent from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Upstream branch the session branch was published to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_upstream_ref: Option<String>,
    /// Pending clarification questions of the session's turn.
    pub questions: Vec<RemoteQuestion>,
    /// Labels of the workflow actions waiting behind the active turn, such
    /// as a queued sync or publish, in the order they run. Absent when none
    /// wait and from older servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued_actions: Vec<String>,
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
        let questions: Vec<RemoteQuestion> =
            session.questions.iter().map(RemoteQuestion::from).collect();
        // Older pages answer `questions` only while the status is
        // `Question`, so graph questions travel apart from them.
        let (questions, graph_questions) = if session.graph_question_target.is_some() {
            (Vec::new(), questions)
        } else {
            (questions, Vec::new())
        };

        Self {
            agent: Some(agent.kind().name().to_string()),
            base_branch: Some(settings.base_branch),
            branch: Some(session_branch(&session.id)),
            created_at: Some(session.created_at),
            draft_prompt: session.draft_prompt,
            focused_review: session.focused_review.map(RemoteFocusedReview::from),
            graph: session.graph.map(RemoteSessionGraph::from),
            graph_question_target: session.graph_question_target,
            graph_questions,
            is_draft: settings.is_draft,
            model: Some(agent.model().name().to_string()),
            model_id: Some(agent.model().as_str().to_string()),
            progress: session.progress,
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
            questions,
            queued_actions: session.queued_actions,
            queued_messages: session.queued_messages,
            status: session.status.to_string(),
            title: session.title,
            updated_at: session.updated_at,
        }
    }
}

/// Outcome of a session's latest focused review.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteFocusedReview {
    /// Whether the review finished or failed.
    pub state: RemoteFocusedReviewState,
    /// The review's Markdown when it is `ready`, or the plain-text failure
    /// message when it `failed`.
    pub text: String,
}

impl From<SessionFocusedReview> for RemoteFocusedReview {
    fn from(review: SessionFocusedReview) -> Self {
        match review {
            SessionFocusedReview::Ready { text } => Self {
                state: RemoteFocusedReviewState::Ready,
                text,
            },
            SessionFocusedReview::Failed { message } => Self {
                state: RemoteFocusedReviewState::Failed,
                text: message,
            },
        }
    }
}

/// State of a focused review a remote client can show.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFocusedReviewState {
    /// The review could not be produced.
    Failed,
    /// The review finished.
    Ready,
}

/// Pull or merge request linked to a session, as of the host's latest
/// forge refresh.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteReviewRequest {
    /// Forge display id, such as `#123`.
    pub display_id: String,
    /// Forge name, such as `GitHub` or `GitLab`. Absent from older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<String>,
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
            forge: Some(summary.forge_kind.display_name().to_string()),
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

impl From<&QuestionItem> for RemoteQuestion {
    fn from(question: &QuestionItem) -> Self {
        Self {
            options: question.options.clone(),
            text: question.text.clone(),
        }
    }
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
