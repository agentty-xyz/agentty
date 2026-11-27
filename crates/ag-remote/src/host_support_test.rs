//! In-memory host doubles for running a real server in tests.
//!
//! A program that embeds the crate, or an application that serves the web
//! client, builds a [`crate::RemoteHost`] from a [`MemoryDeviceStore`] and a
//! [`RecordingBackend`], starts it with [`crate::start`], and pairs devices
//! through the returned [`crate::RemoteServerHandle`]. Both doubles keep
//! everything in memory and answer deterministically, so tests can assert on
//! what a device sees and on what reached the host.

use std::sync::{Mutex, MutexGuard, PoisonError};

use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    ExecutionGraphStatus, ForgeKind, GenerateGraphProposalRequest, HostContext, HostProject,
    HostProjectStatus, HostProjectSync, HostProjectSyncPhase, HostSessionMode, MessageImage,
    ProjectScope, ProjectSessions, QuestionItem, ReasoningLevel, ResponseStyle, ReviewRequest,
    ReviewRequestMerge, ReviewRequestMergeOutcome, ReviewRequestState, ReviewRequestSummary,
    Session, SessionBackend, SessionDiffFile, SessionDiffFileStatus, SessionError,
    SessionFileEntry, SessionFocusedReview, SessionGraphActionRequest, SessionGraphApproval,
    SessionGraphProposalPhase, SessionGraphSummary, SessionGraphs, SessionGraphsRequest, SessionId,
    SessionListEntry, SessionListStats, SessionMessage, SessionMessageKind, SessionOperation,
    SessionSettings, SessionSettingsChange, SessionStatus, SessionSummary, SpeedMode,
};
use async_trait::async_trait;
use tokio::sync::watch;

use crate::device::{DeviceStore, DeviceStoreError, NewRemoteDevice, RemoteDevice};

/// Pairing time, in Unix seconds, of every device the store inserts.
const PAIRED_AT: i64 = 100;

/// Time, in Unix seconds, that the store records for every authentication.
const SEEN_AT: i64 = 200;

/// In-memory [`DeviceStore`].
///
/// Every inserted device is paired at `100` and, once it authenticates, last
/// seen at `200`. [`MemoryDeviceStore::failing_deletes`] creates a store
/// whose deletions fail, as when host persistence is unavailable.
#[derive(Default)]
pub struct MemoryDeviceStore {
    /// Each stored device with its credential digest.
    devices: Mutex<Vec<(RemoteDevice, String)>>,
    is_delete_failing: bool,
}

impl MemoryDeviceStore {
    /// Creates an empty store whose deletions fail.
    #[must_use]
    pub fn failing_deletes() -> Self {
        Self {
            is_delete_failing: true,
            ..Self::default()
        }
    }

    /// Returns the identifiers of the stored devices in pairing order.
    #[must_use]
    pub fn device_ids(&self) -> Vec<String> {
        locked(&self.devices)
            .iter()
            .map(|(device, _)| device.id.clone())
            .collect()
    }
}

#[async_trait]
impl DeviceStore for MemoryDeviceStore {
    async fn list_devices(&self) -> Result<Vec<RemoteDevice>, DeviceStoreError> {
        Ok(locked(&self.devices)
            .iter()
            .map(|(device, _)| device.clone())
            .collect())
    }

    async fn find_device_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<RemoteDevice>, DeviceStoreError> {
        Ok(locked(&self.devices)
            .iter()
            .find(|(_, hash)| hash == token_hash)
            .map(|(device, _)| device.clone()))
    }

    async fn insert_device(&self, device: NewRemoteDevice) -> Result<(), DeviceStoreError> {
        locked(&self.devices).push((
            RemoteDevice {
                created_at: PAIRED_AT,
                id: device.id,
                last_seen_at: None,
                name: device.name,
            },
            device.token_hash,
        ));

        Ok(())
    }

    async fn touch_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        for (device, _) in locked(&self.devices).iter_mut() {
            if device.id == device_id {
                device.last_seen_at = Some(SEEN_AT);
            }
        }

        Ok(())
    }

    async fn delete_device(&self, device_id: &str) -> Result<(), DeviceStoreError> {
        if self.is_delete_failing {
            return Err(DeviceStoreError("delete failed".to_string()));
        }
        locked(&self.devices).retain(|(device, _)| device.id != device_id);

        Ok(())
    }
}

/// Session backend with two seeded projects that records every call.
///
/// The terminal has project `1`, `agentty`, open. It is on branch `main`,
/// two commits behind its upstream, and lists `session-1`, titled
/// `Build it`, and `session-2`, titled `Plan it`. Project `2`, `website`, is
/// on branch `site`, not compared with its upstream yet, and lists
/// `session-3`, titled `Write the docs`. The backend serves every optional
/// operation but [`SessionOperation::Projects`], so by default it lists and
/// serves only the open project, as a host that lists only its active
/// project does; [`RecordingBackend::with_projects`] creates one that also
/// lists and serves `website`.
///
/// Listings show `session-2` in progress, with the figures of a turn two
/// commits ahead of its base, and the other sessions in review. Opened,
/// `session-2` runs a turn beside a graph whose `inspect` output awaits
/// approval. Every other session, except the absent `missing`, asks
/// `Which branch?` after a hidden prompt and the answer `which branch?`, and
/// has a finished focused review. Once a question of its is answered, it
/// waits in review on its graph work's question `Where should the graph
/// run?` instead. Every session's diff adds one line to `a.rs`, and its
/// worktree holds `src/lib.rs`.
///
/// Each method of [`SessionBackend`] records a call that
/// [`RecordingBackend::calls`] returns, such as `send:session-1:hi`. A
/// backend that serves projects appends to each project request the id of
/// the project it reached, as `list:2`. The message `busy` is refused,
/// `panic` panics, and `wait` and every publish are held until
/// [`RecordingBackend::release`]. Cancellations report a missing session,
/// session syncs and merges are refused, and project syncs start.
#[derive(Default)]
pub struct RecordingBackend {
    /// Index in [`PROJECTS`] of the project the terminal has open.
    active: Mutex<usize>,
    calls: Mutex<Vec<String>>,
    /// Ids of the projects the backend no longer lists, as when the
    /// catalog hides them.
    hidden: Mutex<Vec<i64>>,
    /// Whether the next listing leaves out the project it read, as a host
    /// that reads the sessions and the status apart can.
    is_next_listing_unnamed: Mutex<bool>,
    released: watch::Sender<bool>,
    /// Whether the backend serves [`SessionOperation::Projects`].
    serves_projects: bool,
    /// Ids of the projects a sync was requested for.
    syncing: Mutex<Vec<i64>>,
}

impl RecordingBackend {
    /// Creates a backend that also lists both seeded projects and serves
    /// requests for either.
    #[must_use]
    pub fn with_projects() -> Self {
        Self {
            serves_projects: true,
            ..Self::default()
        }
    }

    /// Returns the recorded calls in the order the host received them.
    #[must_use]
    pub fn calls(&self) -> Vec<String> {
        locked(&self.calls).clone()
    }

    /// Lets every held `wait` message and publish, now and later, finish.
    pub fn release(&self) {
        self.released.send_replace(true);
    }

    /// Opens project `id` in the terminal; an unknown id changes nothing.
    pub fn switch_to(&self, id: i64) {
        if let Some(index) = PROJECTS.iter().position(|project| project.id == id) {
            *locked(&self.active) = index;
        }
    }

    /// Stops listing project `id`, and serving it and its sessions.
    pub fn hide(&self, id: i64) {
        locked(&self.hidden).push(id);
    }

    /// Makes the next listing leave out the project it read; the listings
    /// after it name it again.
    pub fn leave_the_project_out_of_the_next_listing(&self) {
        *locked(&self.is_next_listing_unnamed) = true;
    }

    fn record(&self, call: String) {
        locked(&self.calls).push(call);
    }

    /// The projects the backend lists.
    fn listed(&self) -> impl Iterator<Item = &'static Project> {
        let hidden = locked(&self.hidden).clone();

        PROJECTS
            .iter()
            .filter(move |project| !hidden.contains(&project.id))
    }

    /// Returns the project `scope` is for and records `call` for it, or
    /// refuses a scope the backend does not serve, as a host does.
    fn scoped(&self, call: &str, scope: ProjectScope) -> Result<&'static Project, SessionError> {
        let project = match scope {
            ProjectScope::Active => &PROJECTS[*locked(&self.active)],
            ProjectScope::Project(id) if self.serves_projects => self
                .listed()
                .find(|project| project.id == id)
                .ok_or_else(|| SessionError::Operation("Project is not listed".to_string()))?,
            ProjectScope::Project(_) => {
                return Err(SessionError::Operation(
                    "Host serves only its active project".to_string(),
                ));
            }
        };
        if self.serves_projects {
            self.record(format!("{call}:{}", project.id));
        } else {
            self.record(call.to_string());
        }

        Ok(project)
    }

    /// Lists the sessions of `project`; `session-2` counts as updated once
    /// for every message sent and every answer given to any session, so a
    /// device sees another session change.
    fn summaries(&self, project: &Project) -> Vec<SessionSummary> {
        let plan_updated_at = self
            .calls()
            .iter()
            .filter(|call| call.starts_with("send:") || call.starts_with("answer:"))
            .fold(30, |updated_at, _| updated_at + 1);

        project
            .sessions
            .iter()
            .map(|id| match *id {
                "session-1" => SessionSummary {
                    id: SessionId::from("session-1"),
                    status: SessionStatus::Review,
                    title: Some("Build it".to_string()),
                    updated_at: 20,
                },
                "session-2" => SessionSummary {
                    id: SessionId::from("session-2"),
                    status: SessionStatus::InProgress,
                    title: Some("Plan it".to_string()),
                    updated_at: plan_updated_at,
                },
                id => SessionSummary {
                    id: SessionId::from(id),
                    status: SessionStatus::Review,
                    title: Some("Write the docs".to_string()),
                    updated_at: 25,
                },
            })
            .collect()
    }
}

#[async_trait]
impl SessionBackend for RecordingBackend {
    fn supports(&self, operation: SessionOperation) -> bool {
        operation != SessionOperation::Projects || self.serves_projects
    }

    // Lists the listed [`PROJECTS`] in their order, the terminal's marked
    // active.
    async fn list_projects(&self) -> Result<Vec<HostProject>, SessionError> {
        self.record("projects".to_string());
        let active = PROJECTS[*locked(&self.active)].id;

        Ok(self
            .listed()
            .map(|project| HostProject {
                active_session_count: u32::try_from(project.sessions.len()).unwrap_or(u32::MAX),
                id: project.id,
                is_active: project.id == active,
                name: project.name.to_string(),
            })
            .collect())
    }

    // A project's new session is the last one it lists: `session-2` in
    // `agentty` and `session-3` in `website`.
    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.record(format!("create:{:?}:{}", request.mode, request.project_id));
        let project = PROJECTS
            .iter()
            .find(|project| project.id == request.project_id)
            .ok_or_else(|| SessionError::Operation("Project is not listed".to_string()))?;
        let created = project.sessions.last().copied().unwrap_or("session-2");

        Ok(SessionId::from(created))
    }

    // The host serves only regular sessions, in either project.
    async fn host_context_for(&self, scope: ProjectScope) -> Result<HostContext, SessionError> {
        let project = self.scoped("host", scope)?;

        Ok(HostContext {
            base_branch: project.branch.to_string(),
            default_model: ag_session::test_support::CODEX_MODEL,
            models: vec![ag_session::test_support::codex_selection()],
            project_id: project.id,
            project_name: project.name.to_string(),
            session_modes: vec![HostSessionMode {
                agent: ag_session::test_support::codex_selection(),
                mode: CreateSessionMode::Regular,
                reasoning_level: ReasoningLevel::High,
                response_style: ResponseStyle::Balanced,
                speed_mode: SpeedMode::Normal,
            }],
        })
    }

    // Lists the project's sessions with its status: `agentty` is two
    // commits behind its upstream, `website` not compared yet, and each is
    // syncing once a sync was requested for it. `session-2` comes with the
    // figures of a running turn two commits ahead of its base, every other
    // session without figures, as an older host lists them. A backend that
    // serves projects names the project it listed, unless
    // [`RecordingBackend::leave_the_project_out_of_the_next_listing`] says
    // to leave it out of this one.
    async fn project_sessions(&self, scope: ProjectScope) -> Result<ProjectSessions, SessionError> {
        let project = self.scoped("list", scope)?;
        let is_unnamed = std::mem::take(&mut *locked(&self.is_next_listing_unnamed));
        let is_syncing = locked(&self.syncing).contains(&project.id);
        let entries = self
            .summaries(project)
            .into_iter()
            .map(|summary| {
                let stats = (summary.id.as_str() == "session-2").then(|| SessionListStats {
                    added_lines: 64,
                    base_ahead_behind: Some((2, 0)),
                    deleted_lines: 9,
                    has_merge_conflict: false,
                    in_progress_started_at: Some(100),
                    in_progress_total_seconds: 12,
                    is_published: false,
                    model: ag_session::test_support::codex_selection().model(),
                    reasoning_level: ReasoningLevel::High,
                    review_request: None,
                    size_label: "M".to_string(),
                    upstream_ahead_behind: None,
                });

                SessionListEntry { stats, summary }
            })
            .collect();

        Ok(ProjectSessions {
            entries,
            project_id: (self.serves_projects && !is_unnamed).then_some(project.id),
            status: Some(HostProjectStatus {
                ahead_behind: project.has_counts.then_some((0, 2)),
                branch: Some(project.branch.to_string()),
                project_name: project.name.to_string(),
                sync: is_syncing.then(|| HostProjectSync {
                    branch: project.branch.to_string(),
                    phase: HostProjectSyncPhase::Running,
                }),
            }),
        })
    }

    // Every project sync starts.
    async fn sync_project_for(&self, scope: ProjectScope) -> Result<(), SessionError> {
        let project = self.scoped("sync_project", scope)?;
        locked(&self.syncing).push(project.id);

        Ok(())
    }

    // A backend that serves projects lists a session of either; one that
    // does not answers as hosts that list only their active project do.
    async fn is_session_listed(&self, session_id: &SessionId) -> Result<bool, SessionError> {
        if !self.serves_projects {
            let summaries = self.list_sessions().await?;

            return Ok(summaries.iter().any(|summary| &summary.id == session_id));
        }
        self.record(format!("listed:{session_id}"));

        Ok(self
            .listed()
            .any(|project| project.sessions.contains(&session_id.as_str())))
    }

    // A snapshot queues the messages sent to its session; `missing` is
    // absent.
    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        self.record(format!("get:{session_id}"));
        if session_id.as_str() == "missing" {
            return Ok(None);
        }

        let prefix = format!("send:{session_id}:");
        let answered = format!("answer:{session_id}:");
        let calls = self.calls();
        let sent = calls
            .iter()
            .filter_map(|call| call.strip_prefix(&prefix).map(str::to_string))
            .collect();
        let is_answered = calls.iter().any(|call| call.starts_with(&answered));

        Ok(Some(session(session_id, sent, is_answered)))
    }

    // Every diff adds one line to `a.rs`.
    async fn get_session_diff(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionDiffFile>>, SessionError> {
        self.record(format!("diff:{session_id}"));

        Ok(Some(vec![SessionDiffFile {
            added_lines: 1,
            deleted_lines: 0,
            old_path: None,
            patch: "diff --git a/a.rs b/a.rs\n+new\n".to_string(),
            path: "a.rs".to_string(),
            status: SessionDiffFileStatus::Added,
        }]))
    }

    // Every session has the graphs of
    // `ag_session::test_support::session_graphs`; a graph other than `graph`
    // belongs to another session.
    async fn session_graphs(
        &self,
        request: SessionGraphsRequest,
    ) -> Result<SessionGraphs, SessionError> {
        let graph_id = request.graph_id.as_deref().unwrap_or("-");
        let node_id = request.node_id.as_deref().unwrap_or("-");
        self.record(format!(
            "graphs:{}:{graph_id}:{node_id}",
            request.session_id
        ));
        if !matches!(graph_id, "-" | "graph") {
            return Err(SessionError::Operation(
                "Graph does not belong to this session".to_string(),
            ));
        }

        Ok(ag_session::test_support::session_graphs(
            request.session_id.as_str(),
        ))
    }

    // Every graph action on a snapshot other than `5eed`, the snapshot of
    // `session_graphs`, finds the graph changed.
    async fn act_on_session_graph(
        &self,
        request: SessionGraphActionRequest,
    ) -> Result<(), SessionError> {
        self.record(format!(
            "graph_action:{}:{}:{}:{:?}",
            request.session_id, request.graph_id, request.snapshot, request.action
        ));
        if request.snapshot != "5eed" {
            return Err(SessionError::Operation(
                "Graph changed; refresh the view and inspect it before acting again.".to_string(),
            ));
        }

        Ok(())
    }

    // Every worktree holds the directory `src` and the file `src/lib.rs`.
    async fn list_session_files(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Vec<SessionFileEntry>>, SessionError> {
        self.record(format!("files:{session_id}"));

        Ok(Some(vec![
            SessionFileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            SessionFileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
        ]))
    }

    // A project's checkout holds one file, `README.md` in `agentty`.
    async fn list_project_files_for(
        &self,
        scope: ProjectScope,
    ) -> Result<Vec<SessionFileEntry>, SessionError> {
        let project = self.scoped("project_files", scope)?;

        Ok(vec![SessionFileEntry {
            is_dir: false,
            path: project.readme.to_string(),
        }])
    }

    // Lists the active project's sessions, as `project_sessions` does.
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let summaries = self.summaries(&PROJECTS[*locked(&self.active)]);
        self.record("list".to_string());

        Ok(summaries)
    }

    // The task `busy` is refused as the host refuses a session that is not
    // ready. A delivery identity other than a fresh one the server minted,
    // `remote-` and 32 hex digits, is refused too.
    async fn generate_graph_proposal(
        &self,
        request: GenerateGraphProposalRequest,
    ) -> Result<(), SessionError> {
        let message = request.request.text;
        self.record(format!("graph_proposal:{}:{message}", request.session_id));
        let is_minted = request.id.strip_prefix("remote-").is_some_and(|digits| {
            digits.len() == 32 && digits.chars().all(|digit| digit.is_ascii_hexdigit())
        });
        if !is_minted {
            return Err(SessionError::Operation(format!(
                "Unexpected proposal identity `{}`",
                request.id
            )));
        }
        if message == "busy" {
            return Err(SessionError::Operation(
                "Graph generation requires a prepared idle session".to_string(),
            ));
        }

        Ok(())
    }

    // The message `busy` is rejected, `panic` panics, and `wait` is held,
    // and recorded only once [`RecordingBackend::release`] is called.
    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        if message == "wait" {
            let mut released = self.released.subscribe();
            let _ = released.wait_for(|is_released| *is_released).await;
        }
        self.record(format!("send:{session_id}:{message}"));
        if message == "busy" {
            return Err(SessionError::Operation("Session is busy".to_string()));
        }
        if message == "panic" {
            std::panic::resume_unwind(Box::new("the test backend fails this message"));
        }

        Ok(())
    }

    // Records the message with each image's extension and size.
    async fn send_message_with_images(
        &self,
        session_id: &SessionId,
        message: String,
        images: Vec<MessageImage>,
    ) -> Result<(), SessionError> {
        let images: Vec<String> = images
            .iter()
            .map(|image| format!("{}={}", image.format().extension(), image.data().len()))
            .collect();
        self.record(format!("send:{session_id}:{message}:{}", images.join(",")));

        Ok(())
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        _request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.record(format!("durable:{session_id}"));

        Ok(())
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        let answers: Vec<String> = request
            .answers
            .iter()
            .map(|answer| format!("{}={}", answer.question, answer.answer))
            .collect();
        // Answers for graph work end with `@` and the work's key.
        let target = request
            .graph_question_target
            .map_or_default(|target| format!("@{target}"));
        self.record(format!("answer:{session_id}:{}{target}", answers.join(",")));

        Ok(())
    }

    // Every cancellation reports a missing session.
    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("cancel:{session_id}"));

        Err(SessionError::NotFound)
    }

    // Every turn stop succeeds.
    async fn stop_turn(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("stop:{session_id}"));

        Ok(())
    }

    // Every settings change succeeds and records its reasoning level.
    async fn update_session_settings(
        &self,
        session_id: &SessionId,
        change: SessionSettingsChange,
    ) -> Result<(), SessionError> {
        let reasoning = change.reasoning_level.map_or("-", ReasoningLevel::as_str);
        self.record(format!("settings:{session_id}:{reasoning}"));

        Ok(())
    }

    // Every sync is refused.
    async fn rebase_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("sync:{session_id}"));

        Err(SessionError::Operation(
            "Session cannot sync in status `Done`".to_string(),
        ))
    }

    // Every merge is refused.
    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.record(format!("merge:{session_id}"));

        Err(SessionError::Operation(
            "Session is not ready to merge".to_string(),
        ))
    }

    // A publish is held like the message `wait`, and then opens pull
    // request `#7`.
    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        let mut released = self.released.subscribe();
        let _ = released.wait_for(|is_released| *is_released).await;
        self.record(format!("review:{session_id}"));

        Ok(ReviewRequest {
            last_refreshed_at: 30,
            summary: ReviewRequestSummary {
                display_id: "#7".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: format!("agentty/{session_id}"),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Build it".to_string(),
                web_url: "https://github.com/example/project/pull/7".to_string(),
            },
        })
    }

    // A merge of pull request `#7` without bypass joins its merge queue; a
    // bypass merges it as commit `7a1c0de`.
    async fn merge_review_request(
        &self,
        session_id: &SessionId,
        bypass: bool,
    ) -> Result<ReviewRequestMerge, SessionError> {
        self.record(format!("merge_review_request:{session_id}:{bypass}"));
        let (outcome, state, status_summary) = if bypass {
            (
                ReviewRequestMergeOutcome::Merged {
                    merge_commit: Some("7a1c0de".to_string()),
                },
                ReviewRequestState::Merged,
                None,
            )
        } else {
            (
                ReviewRequestMergeOutcome::Queued,
                ReviewRequestState::Open,
                Some(ReviewRequestSummary::GITHUB_MERGE_QUEUE_STATUS_COMPONENT.to_string()),
            )
        };

        Ok(ReviewRequestMerge {
            outcome,
            review_request: Some(ReviewRequestSummary {
                display_id: "#7".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: format!("agentty/{session_id}"),
                state,
                status_summary,
                target_branch: "main".to_string(),
                title: "Build it".to_string(),
                web_url: "https://github.com/example/project/pull/7".to_string(),
            }),
        })
    }
}

/// One of the backend's seeded projects.
struct Project {
    /// Branch its checkout is on.
    branch: &'static str,
    /// Whether the host has compared the branch with its upstream; the
    /// terminal's project has, as the host's first stage compares only
    /// that one.
    has_counts: bool,
    id: i64,
    name: &'static str,
    /// The one file its checkout holds.
    readme: &'static str,
    /// The session a new session in it is, and the sessions it lists.
    sessions: &'static [&'static str],
}

/// The backend's projects: `agentty`, active at first, with `session-1`
/// and `session-2`, and `website` with `session-3`. Only a backend that
/// serves [`SessionOperation::Projects`] lists them and serves `website`.
const PROJECTS: [Project; 2] = [
    Project {
        branch: "main",
        has_counts: true,
        id: 1,
        name: "agentty",
        readme: "README.md",
        sessions: &["session-1", "session-2"],
    },
    Project {
        branch: "site",
        has_counts: false,
        id: 2,
        name: "website",
        readme: "index.html",
        sessions: &["session-3"],
    },
];

/// Locks `mutex`, recovering its data if another test thread panicked.
fn locked<Value>(mutex: &Mutex<Value>) -> MutexGuard<'_, Value> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Builds a session with `queued_messages` waiting behind its turn.
/// `session-2` runs a turn after an activity summary and a notice, beside
/// a graph whose `inspect` output waits for approval; any other session has
/// a hidden prompt, an answer, one question, and a finished focused review.
/// Once `is_answered`, that question is gone, and the session waits in
/// review on its graph work's question instead.
fn session(session_id: &SessionId, queued_messages: Vec<String>, is_answered: bool) -> Session {
    let is_running = session_id.as_str() == "session-2";
    let (messages, questions, status, title) = if is_running {
        (
            vec![
                SessionMessage::new(0, SessionMessageKind::UserPrompt, "plan it"),
                SessionMessage::new(
                    1,
                    SessionMessageKind::ActivitySummary,
                    "✓ Read plan.md\n✗ Run tests\n… Edit plan.md",
                ),
                SessionMessage::new(
                    2,
                    SessionMessageKind::WorkflowNotice,
                    "[Commit Error] hook failed",
                ),
            ],
            Vec::new(),
            SessionStatus::InProgress,
            "Plan it",
        )
    } else {
        (
            vec![
                SessionMessage::new(0, SessionMessageKind::UserPrompt, "build it"),
                SessionMessage::new(1, SessionMessageKind::AgentPrompt, "hidden"),
                SessionMessage::new(2, SessionMessageKind::AssistantAnswer, "which branch?"),
            ],
            vec![if is_answered {
                QuestionItem {
                    options: vec!["Locally".to_string()],
                    text: "Where should the graph run?".to_string(),
                }
            } else {
                QuestionItem {
                    options: vec!["main".to_string()],
                    text: "Which branch?".to_string(),
                }
            }],
            if is_answered {
                SessionStatus::Review
            } else {
                SessionStatus::Question
            },
            "Build it",
        )
    };

    Session {
        created_at: 10,
        draft_prompt: None,
        focused_review: (!is_running).then(|| SessionFocusedReview::Ready {
            text: "### Suggestions\n- Add a test".to_string(),
        }),
        graph: is_running.then(|| SessionGraphSummary {
            awaiting_approval: vec![SessionGraphApproval {
                node_id: "inspect".to_string(),
                takes_revision: false,
                text: "inspect awaits your approval; reply in chat to accept it".to_string(),
            }],
            done: 1,
            label: "Blocked".to_string(),
            phase: SessionGraphProposalPhase::Approved,
            status: Some(ExecutionGraphStatus::Blocked),
            total: 2,
            version: 1,
        }),
        graph_question_target: (is_answered && !is_running).then(|| "c0ffee".to_string()),
        id: session_id.clone(),
        messages,
        progress: None,
        published_upstream_ref: None,
        questions,
        queued_actions: Vec::new(),
        queued_messages,
        review_request: None,
        settings: SessionSettings {
            agent: ag_session::test_support::codex_selection(),
            base_branch: "main".to_string(),
            is_draft: false,
            project_id: 1,
            reasoning_level: ReasoningLevel::High,
            response_style: ResponseStyle::Balanced,
            speed_mode: SpeedMode::Normal,
        },
        status,
        title: Some(title.to_string()),
        updated_at: 20,
    }
}
