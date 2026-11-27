//! Sessions' tasks on a platform's Sync, the account's shared record of the
//! work its sessions do on every computer it is signed in on.
//!
//! This is unrelated to syncing a project with its default branch. It runs
//! only while the latest account answer enables Sync. While the first
//! message of a session is processed, before the session starts, a
//! background request asks the platform whether that work exists: when it
//! does not, the session takes a new or matching open task and starts;
//! when another session holds it, the session stays unstarted, with its
//! prompt retained, until the user takes the task over or starts separate
//! work; and any other answer starts the session without a task. A held
//! task then hears of activity when each turn starts and ends, is finished
//! when the session is merged or done, and is released when the session is
//! deleted or canceled. None of these requests is repeated after a
//! failure, and none delays or fails the user's action.
//!
//! Every request runs in the background and reports through
//! [`AppEvent::PlatformTaskUpdated`]. What a request sends is limited to
//! what `domain::platform_sync` builds, and no answer text reaches an
//! agent's prompt: it is shown only in the session's transient output.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use ag_git::GitClient;

use crate::app::{App, AppError, AppEvent, spawn};
use crate::domain::platform_sync::{
    PlatformTaskChoice, finish_summary, last_active_text, session_label, sync_repository,
    work_brief, work_title,
};
use crate::domain::session::{Session, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{
    NewPlatformTask, PlatformTaskClaim, PlatformTaskName, PlatformTaskRow, PlatformTaskState,
    SessionPreparationState,
};
use crate::infra::platform::{
    AccessToken, ClaimId, PlatformClient, SyncError, SyncStart, SyncTaskRef, SyncWork,
};

/// Shown while the start request runs.
const CHECKING: &str =
    "Asking the platform's Sync whether another session already works on this...";
/// Shown when Agentty quit before the start request was answered.
const INTERRUPTED: &str = "Agentty quit before the platform's Sync answered whether another \
                           session already works on this, so this session has not started.\nPress \
                           s to ask again. Your saved prompt is retained.";
/// Shown when the repository names no host and path the platform accepts.
const NO_REPOSITORY: &str = "This session started without a task on the platform's Sync: its \
                             repository has no `origin` remote that names a host and a path.";
/// Shown when the platform no longer offers Sync to the account.
const SYNC_DISABLED: &str =
    "This session started without a task: the platform no longer offers Sync to this account.";
/// Names a holding session whose client gave it no label.
const UNLABELLED_HOLDER: &str = "a session without a label";
/// Shown when a choice is made after Sync stopped being offered.
const SYNC_OFF: &str =
    "The platform's Sync is off for this account now, so this session started without a task.";

/// What a Sync request needs while the latest account answer enables
/// Sync.
pub(crate) struct SyncAccess {
    /// Client for the platform.
    pub(crate) client: Arc<dyn PlatformClient>,
    /// Address of the platform, the only one a claim goes to.
    pub(crate) platform_url: String,
    /// Access token of the sign-in.
    pub(crate) token: AccessToken,
}

/// What the user chose for a session its platform task holds back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlatformTaskAnswer {
    /// Ask again after an interrupted start.
    AskAgain,
    /// Start the work as separate from the task another session holds.
    SeparateWork,
    /// Take the task over from the session holding it.
    TakeOver,
}

/// Answer of a background Sync request delivered to the reducer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PlatformTaskUpdate {
    /// A start request was answered, or failed before or without one.
    Started {
        /// The task the session now holds, or why it holds none.
        result: Result<SyncStart, StartFailure>,
        /// Session that asked.
        session_id: String,
    },
    /// A request about a held task found that the session no longer holds
    /// it.
    Lost {
        /// [`SyncError::ClaimLost`] or [`SyncError::TaskClosed`].
        error: SyncError,
        /// Session that held the task.
        session_id: String,
    },
    /// The platform no longer offers Sync to the account.
    Disabled,
}

/// Why a start request gave the session no task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StartFailure {
    /// The repository names no host and path the platform accepts, so no
    /// request was sent.
    NoRepository,
    /// The platform refused, failed, or did not answer.
    Sync(SyncError),
}

/// One start request: the work to describe and the session label.
struct StartRequest {
    brief: String,
    not_duplicates: Vec<u64>,
    repository_dir: PathBuf,
    session: String,
    takeover: Option<u64>,
    title: String,
}

impl App {
    /// Returns whether the first turn of `session_id`, about to start with
    /// `prompt`, waits for the platform's Sync: while the start request is
    /// answered, after a conflict, or after an interrupted request. The
    /// first call for a session sends the start request, when the account
    /// answer enables Sync and the message has text. A session waiting
    /// while Sync is off starts without a task instead.
    pub(crate) async fn platform_task_holds_start(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> bool {
        let tasks = self.services.db().platform_tasks();
        let task = tasks.load_platform_task(session_id).await.ok().flatten();
        match task {
            None => self.begin_platform_task(session_id, prompt).await,
            Some(task) if task.state == PlatformTaskState::Resolved => false,
            Some(_) if self.platform.sync_access().is_some() => true,
            Some(_) => {
                let _ = tasks.resolve_platform_task(session_id).await;
                self.clear_platform_task_notice(session_id);

                false
            }
        }
    }

    /// Sends the start request for the first message `prompt` of the
    /// `Draft` session `session_id` when the account answer enables Sync,
    /// the session has no platform task yet, and the message has text.
    /// Returns whether a request is now on its way, so the session's start
    /// waits for it.
    pub(crate) async fn begin_platform_task(
        &mut self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> bool {
        let is_first_message = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.status == Status::Draft);
        let Some(access) = self.platform.sync_access().filter(|_| is_first_message) else {
            return false;
        };
        let Some(title) = work_title(&prompt.text) else {
            return false;
        };
        let brief = work_brief(&prompt.text);
        let begun = self
            .services
            .db()
            .platform_tasks()
            .begin_platform_task(NewPlatformTask {
                brief: &brief,
                platform_url: &access.platform_url,
                session_id,
                title: &title,
            })
            .await
            .unwrap_or(false);
        if !begun {
            return false;
        }
        let request = self
            .start_request(session_id, title, brief, None, Vec::new())
            .await;
        self.send_platform_task_start(session_id, access, request);

        true
    }

    /// Keeps `prompt` with the session's platform task until the start is
    /// answered, for a start no other store holds.
    ///
    /// # Errors
    /// Returns an error when a first prompt is already waiting.
    pub(crate) async fn save_platform_task_prompt(
        &self,
        session_id: &str,
        prompt: &TurnPrompt,
    ) -> Result<(), AppError> {
        let prompt =
            serde_json::to_string(prompt).map_err(|error| AppError::Workflow(error.to_string()))?;
        let saved = self
            .services
            .db()
            .platform_tasks()
            .save_platform_task_prompt(session_id, &prompt)
            .await?;
        if !saved {
            return Err(AppError::Workflow(
                "A first prompt is already waiting for the platform's Sync.".to_string(),
            ));
        }

        Ok(())
    }

    /// Takes the task over from the session holding it, after a conflict.
    pub(crate) async fn take_over_platform_task(&mut self, session_id: &str) {
        self.answer_platform_task(session_id, PlatformTaskAnswer::TakeOver)
            .await;
    }

    /// Starts the work as separate from the task another session holds,
    /// after a conflict.
    pub(crate) async fn start_separate_platform_work(&mut self, session_id: &str) {
        self.answer_platform_task(session_id, PlatformTaskAnswer::SeparateWork)
            .await;
    }

    /// Asks the platform's Sync again after Agentty quit before it
    /// answered.
    pub(crate) async fn ask_platform_task_again(&mut self, session_id: &str) {
        self.answer_platform_task(session_id, PlatformTaskAnswer::AskAgain)
            .await;
    }

    /// Applies one answer of a background Sync request.
    pub(crate) async fn apply_platform_task_update(&mut self, update: PlatformTaskUpdate) {
        match update {
            PlatformTaskUpdate::Started { result, session_id } => {
                self.apply_platform_task_start(&session_id, result).await;
            }
            PlatformTaskUpdate::Lost { error, session_id } => {
                let claim = self
                    .services
                    .db()
                    .platform_tasks()
                    .take_platform_task_claim(&session_id)
                    .await
                    .ok()
                    .flatten();
                let number =
                    claim.map_or_else(String::new, |claim| format!(" #{}", claim.task_number));
                let notice = if error == SyncError::ClaimLost {
                    format!(
                        "Another session took over task{number} on the platform's Sync, so this \
                         session no longer reports its work there."
                    )
                } else {
                    format!(
                        "Task{number} was finished or canceled elsewhere on the platform's Sync, \
                         so this session no longer reports its work there."
                    )
                };
                self.post_platform_task_notice(&session_id, notice);
            }
            PlatformTaskUpdate::Disabled => self.platform.disable_sync(),
        }
    }

    /// Reports activity of the task `session_id` holds, as each turn
    /// start and end does.
    pub(crate) async fn report_platform_task_activity(&mut self, session_id: &str) {
        let Some(access) = self.platform.sync_access() else {
            return;
        };
        let task = self
            .services
            .db()
            .platform_tasks()
            .load_platform_task(session_id)
            .await
            .ok()
            .flatten();
        let Some((task_ref, claim_id)) =
            task.and_then(|task| held_claim(&task, &access.platform_url))
        else {
            return;
        };
        let event_tx = self.services.event_sender();
        let session_id = session_id.to_string();
        spawn::spawn_detached(async move {
            let result = access
                .client
                .report_sync_activity(&access.token, &task_ref, &claim_id)
                .await;
            if let Some(update) = failure_update(&session_id, result) {
                let _ = event_tx.send(AppEvent::PlatformTaskUpdated { update });
            }
        });
    }

    /// Finishes the held tasks of sessions that were merged or are done,
    /// and releases those of canceled sessions, each claim at most once.
    pub(crate) async fn settle_platform_tasks(&mut self) {
        let Some(access) = self.platform.sync_access() else {
            return;
        };
        let tasks = self.services.db().platform_tasks();
        let settled = tasks
            .load_settled_platform_tasks()
            .await
            .unwrap_or_default();
        for task in settled {
            let claim = tasks
                .take_platform_task_claim(&task.session_id)
                .await
                .ok()
                .flatten();
            let Some((task_ref, claim_id)) =
                claim.and_then(|claim| issued_claim(claim, &access.platform_url))
            else {
                continue;
            };
            let summary = (task.status != Status::Canceled.to_string())
                .then(|| finish_summary(task.title.as_deref()));
            send_settlement(
                &access,
                self.services.event_sender(),
                task.session_id,
                task_ref,
                claim_id,
                summary.map(|summary| (summary, task.review_request_url)),
            );
        }
    }

    /// Releases the task `session_id` holds before the session is deleted,
    /// reading the claim before its row disappears.
    pub(crate) async fn release_platform_task_before_delete(&mut self, session_id: &str) {
        self.platform.set_task_choice(session_id, None);
        // Without Sync, the claim goes with the row and nothing is sent.
        let Some(access) = self.platform.sync_access() else {
            return;
        };
        let claim = self
            .services
            .db()
            .platform_tasks()
            .take_platform_task_claim(session_id)
            .await
            .ok()
            .flatten();
        let Some((task_ref, claim_id)) =
            claim.and_then(|claim| issued_claim(claim, &access.platform_url))
        else {
            return;
        };
        send_settlement(
            &access,
            self.services.event_sender(),
            session_id.to_string(),
            task_ref,
            claim_id,
            None,
        );
    }

    /// Shows the platform tasks that hold `project_id`'s loaded sessions
    /// back, as a restart or a page load finds them.
    pub(crate) async fn show_waiting_platform_tasks(&mut self, project_id: i64) {
        let tasks = self
            .services
            .db()
            .platform_tasks()
            .load_waiting_platform_tasks(project_id)
            .await
            .unwrap_or_default();
        let now_ms = self.platform_sync_now_ms();
        for task in tasks {
            let (body, choice) = match task.state {
                PlatformTaskState::Conflict => (
                    TransientMessageBody::Plain(conflict_notice(
                        &ConflictView::from_task(&task),
                        now_ms,
                    )),
                    Some(PlatformTaskChoice::Conflict),
                ),
                PlatformTaskState::Interrupted => (
                    TransientMessageBody::Plain(INTERRUPTED.to_string()),
                    Some(PlatformTaskChoice::Interrupted),
                ),
                PlatformTaskState::Checking | PlatformTaskState::Resolved => {
                    (TransientMessageBody::Loading(CHECKING.to_string()), None)
                }
            };
            self.show_platform_task_notice(&task.session_id, body, choice);
        }
    }

    /// Lets the notice of an answered start leave with the next turn, once
    /// the turn it was shown for ended.
    pub(crate) fn settle_platform_task_notice(&mut self, session_id: &str) {
        let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) else {
            return;
        };
        let Some(message) = session
            .transient_messages
            .get(TransientMessageSlot::PlatformSync)
            .filter(|message| message.lifecycle == TransientMessageLifecycle::UntilResolved)
            .cloned()
        else {
            return;
        };
        session.transient_messages.upsert(TransientMessage {
            anchor: TransientMessageAnchor::AfterCompletedTurn,
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            turn_position: session.latest_user_prompt_position(),
            ..message
        });
        self.mark_dirty();
    }

    /// Applies the answer to the start request of `session_id`, unless the
    /// session was deleted or no longer waits, in which case a task it was
    /// given is released at once.
    async fn apply_platform_task_start(
        &mut self,
        session_id: &str,
        result: Result<SyncStart, StartFailure>,
    ) {
        let tasks = self.services.db().platform_tasks();
        let is_waiting = tasks
            .load_platform_task(session_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|task| task.state == PlatformTaskState::Checking);
        if !is_waiting {
            if let (Ok(start), Some(access)) = (&result, self.platform.sync_access()) {
                send_settlement(
                    &access,
                    self.services.event_sender(),
                    session_id.to_string(),
                    start.task.reference(),
                    start.claim_id.clone(),
                    None,
                );
            }

            return;
        }
        let notice = match result {
            Ok(start) => {
                let task = &start.task;
                let _ = tasks
                    .hold_platform_task(
                        session_id,
                        PlatformTaskName {
                            number: task.number,
                            project: &task.project,
                            title: &task.title,
                            url: task.url.as_deref(),
                        },
                        start.claim_id.expose(),
                    )
                    .await;

                held_notice(&start, self.platform_sync_now_ms())
            }
            Err(StartFailure::Sync(SyncError::TaskInProgress { holder, task })) => {
                let _ = tasks
                    .record_platform_task_conflict(
                        session_id,
                        PlatformTaskName {
                            number: task.number,
                            project: &task.project,
                            title: &task.title,
                            url: task.url.as_deref(),
                        },
                        holder.session.as_deref(),
                        i64::try_from(holder.last_activity_at_ms / 1000).unwrap_or(i64::MAX),
                    )
                    .await;
                let notice = conflict_notice(
                    &ConflictView {
                        holder: holder.session.as_deref(),
                        last_active_ms: Some(holder.last_activity_at_ms),
                        number: task.number,
                        title: &task.title,
                        url: task.url.as_deref(),
                    },
                    self.platform_sync_now_ms(),
                );
                self.show_platform_task_notice(
                    session_id,
                    TransientMessageBody::Plain(notice),
                    Some(PlatformTaskChoice::Conflict),
                );

                return;
            }
            Err(failure) => {
                let _ = tasks.resolve_platform_task(session_id).await;
                match failure {
                    StartFailure::NoRepository => NO_REPOSITORY.to_string(),
                    StartFailure::Sync(SyncError::SyncDisabled) => {
                        self.platform.disable_sync();

                        SYNC_DISABLED.to_string()
                    }
                    StartFailure::Sync(error) => format!(
                        "This session started without a task on the platform's Sync: {error}"
                    ),
                }
            }
        };
        self.resume_platform_task_start(session_id).await;
        self.post_platform_task_notice(session_id, notice);
        self.settle_platform_tasks().await;
    }

    /// Starts the session whose start waited for the platform's Sync: with
    /// the prompt the platform task kept, or with the prompt workspace
    /// preparation keeps, once the workspace is ready.
    async fn resume_platform_task_start(&mut self, session_id: &str) {
        let prompt = self
            .services
            .db()
            .platform_tasks()
            .take_platform_task_prompt(session_id)
            .await
            .ok()
            .flatten()
            .and_then(|prompt| serde_json::from_str::<TurnPrompt>(&prompt).ok());
        let result = match prompt {
            Some(prompt) => self.start_session(session_id, prompt).await,
            None => self.resume_prepared_platform_task_start(session_id).await,
        };
        if let Err(error) = result {
            self.append_output_for_session(session_id, &TranscriptNotice::StartError.format(error))
                .await;
        }
    }

    /// Hands the prompt workspace preparation keeps to the session, and
    /// retries preparation a restart left failed with that prompt.
    async fn resume_prepared_platform_task_start(
        &mut self,
        session_id: &str,
    ) -> Result<(), AppError> {
        let saved_prompt_state = self
            .services
            .db()
            .sessions()
            .load_session_preparation(session_id)
            .await?
            .and_then(|preparation| preparation.prompt.map(|_| preparation.state));
        match saved_prompt_state {
            Some(SessionPreparationState::Failed) => {
                self.retry_workspace_preparation(session_id, "session.create")
                    .await
            }
            Some(_) => {
                self.dispatch_prepared_prompt(session_id).await;

                Ok(())
            }
            // A staged draft keeps its drafts in the session itself.
            None if self
                .sessions
                .session_for_id(session_id)
                .is_some_and(Session::is_draft_session) =>
            {
                self.start_staged_session(session_id).await
            }
            None => Ok(()),
        }
    }

    /// Sends the start request again as `answer` asks, or starts the
    /// session without a task when Sync is off now.
    async fn answer_platform_task(&mut self, session_id: &str, answer: PlatformTaskAnswer) {
        let tasks = self.services.db().platform_tasks();
        let Ok(Some(task)) = tasks.load_platform_task(session_id).await else {
            return;
        };
        let mut not_duplicates = task.not_duplicates.clone();
        let takeover = match (answer, task.state, task.task_number) {
            (PlatformTaskAnswer::TakeOver, PlatformTaskState::Conflict, Some(number)) => {
                Some(number)
            }
            (PlatformTaskAnswer::SeparateWork, PlatformTaskState::Conflict, Some(number)) => {
                not_duplicates.push(number);

                None
            }
            (PlatformTaskAnswer::AskAgain, PlatformTaskState::Interrupted, _) => None,
            _ => return,
        };
        let Some(access) = self.platform.sync_access() else {
            let _ = tasks.resolve_platform_task(session_id).await;
            self.resume_platform_task_start(session_id).await;
            self.post_platform_task_notice(session_id, SYNC_OFF.to_string());

            return;
        };
        // The state was read in this same reducer turn, so the row still
        // waits on it.
        let _ = tasks
            .check_platform_task_again(session_id, &access.platform_url, &not_duplicates)
            .await;
        let request = self
            .start_request(session_id, task.title, task.brief, takeover, not_duplicates)
            .await;
        self.send_platform_task_start(session_id, access, request);
    }

    /// Builds the start request of `session_id`, labelled with this
    /// Agentty's name, for the repository of the session's project.
    async fn start_request(
        &self,
        session_id: &str,
        title: String,
        brief: String,
        takeover: Option<u64>,
        not_duplicates: Vec<u64>,
    ) -> StartRequest {
        StartRequest {
            brief,
            not_duplicates,
            // An unknown checkout reads as no `origin` remote, so no
            // request is sent.
            repository_dir: self
                .session_working_dir(session_id)
                .await
                .unwrap_or_default(),
            session: session_label(self.platform.instance_name(), session_id),
            takeover,
            title,
        }
    }

    /// Returns the checkout of `session_id`'s project, whose `origin`
    /// remote names the repository.
    ///
    /// # Errors
    /// Returns an error when the session's project cannot be loaded.
    async fn session_working_dir(&self, session_id: &str) -> Result<PathBuf, AppError> {
        let project_id = self.session_project_id(session_id).await?;

        self.project_working_dir(project_id).await
    }

    /// Shows that the start request of `session_id` runs and sends it in
    /// the background.
    fn send_platform_task_start(
        &mut self,
        session_id: &str,
        access: SyncAccess,
        request: StartRequest,
    ) {
        self.show_platform_task_notice(
            session_id,
            TransientMessageBody::Loading(CHECKING.to_string()),
            None,
        );
        let git_client = self.services.git_client();
        let event_tx = self.services.event_sender();
        let session_id = session_id.to_string();
        spawn::spawn_detached(async move {
            let result = start_platform_task(&access, git_client.as_ref(), request).await;
            let _ = event_tx.send(AppEvent::PlatformTaskUpdated {
                update: PlatformTaskUpdate::Started { result, session_id },
            });
        });
    }

    /// Shows a notice that stays while the platform task holds `session_id`
    /// back, and the choice it offers.
    fn show_platform_task_notice(
        &mut self,
        session_id: &str,
        body: TransientMessageBody,
        choice: Option<PlatformTaskChoice>,
    ) {
        self.platform.set_task_choice(session_id, choice);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session.transient_messages.upsert(TransientMessage {
                anchor: TransientMessageAnchor::Tail,
                body,
                lifecycle: TransientMessageLifecycle::UntilResolved,
                slot: TransientMessageSlot::PlatformSync,
                turn_position: None,
            });
        }
        self.mark_dirty();
    }

    /// Shows `notice` about the answered platform task of `session_id`: in
    /// a running session it leaves with the next turn, and before the
    /// first turn it stays until that turn ends.
    fn post_platform_task_notice(&mut self, session_id: &str, notice: String) {
        self.platform.set_task_choice(session_id, None);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            let turn_position = session.latest_user_prompt_position();
            let (anchor, lifecycle) = match (session.status, turn_position) {
                (Status::Draft, _) | (_, None) => (
                    TransientMessageAnchor::Tail,
                    TransientMessageLifecycle::UntilResolved,
                ),
                (Status::InProgress | Status::Queued, Some(_)) => (
                    TransientMessageAnchor::AfterActiveTurn,
                    TransientMessageLifecycle::ClearOnNewTurn,
                ),
                (_, Some(_)) => (
                    TransientMessageAnchor::AfterCompletedTurn,
                    TransientMessageLifecycle::ClearOnNewTurn,
                ),
            };
            session.transient_messages.upsert(TransientMessage {
                anchor,
                body: TransientMessageBody::Plain(notice),
                lifecycle,
                slot: TransientMessageSlot::PlatformSync,
                turn_position,
            });
        }
        self.mark_dirty();
    }

    /// Removes the platform task notice of `session_id`.
    fn clear_platform_task_notice(&mut self, session_id: &str) {
        self.platform.set_task_choice(session_id, None);
        if let Some(session) = self.sessions.state_mut().session_mut_for_id(session_id) {
            session
                .transient_messages
                .retract(TransientMessageSlot::PlatformSync);
        }
        self.mark_dirty();
    }

    /// Returns the current time in milliseconds since the Unix epoch.
    fn platform_sync_now_ms(&self) -> u64 {
        self.services
            .clock()
            .now_system_time()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

/// Reads the repository of `request` and sends the start request; a
/// repository without an `origin` remote the platform accepts sends none.
async fn start_platform_task(
    access: &SyncAccess,
    git_client: &dyn GitClient,
    request: StartRequest,
) -> Result<SyncStart, StartFailure> {
    let remote = git_client.repo_url(request.repository_dir).await.ok();
    let repository = remote
        .as_deref()
        .and_then(sync_repository)
        .ok_or(StartFailure::NoRepository)?;
    let work = SyncWork {
        brief: request.brief,
        not_duplicates: request.not_duplicates,
        repository,
        session: request.session,
        takeover: request.takeover,
        title: request.title,
    };

    access
        .client
        .start_sync_work(&access.token, &work)
        .await
        .map_err(StartFailure::Sync)
}

/// Sends the finish of a held task with `summary` and its review request,
/// or its release without one, in the background.
fn send_settlement(
    access: &SyncAccess,
    event_tx: tokio::sync::mpsc::UnboundedSender<AppEvent>,
    session_id: String,
    task_ref: SyncTaskRef,
    claim_id: ClaimId,
    summary: Option<(String, Option<String>)>,
) {
    let client = Arc::clone(&access.client);
    let token = access.token.clone();
    spawn::spawn_detached(async move {
        let result = match summary {
            Some((summary, result_url)) => {
                client
                    .finish_sync_task(&token, &task_ref, &claim_id, &summary, result_url)
                    .await
            }
            None => client.release_sync_task(&token, &task_ref, &claim_id).await,
        };
        if let Some(update) = failure_update(&session_id, result) {
            let _ = event_tx.send(AppEvent::PlatformTaskUpdated { update });
        }
    });
}

/// Returns the update a failed request about a held task reports: a lost
/// claim or closed task, or Sync turned off; other failures are dropped,
/// since no such request is repeated.
fn failure_update(session_id: &str, result: Result<(), SyncError>) -> Option<PlatformTaskUpdate> {
    match result {
        Err(error @ (SyncError::ClaimLost | SyncError::TaskClosed)) => {
            Some(PlatformTaskUpdate::Lost {
                error,
                session_id: session_id.to_string(),
            })
        }
        Err(SyncError::SyncDisabled) => Some(PlatformTaskUpdate::Disabled),
        Ok(()) | Err(_) => None,
    }
}

/// Returns the task and claim `task` holds, when the platform at
/// `platform_url` issued it.
fn held_claim(task: &PlatformTaskRow, platform_url: &str) -> Option<(SyncTaskRef, ClaimId)> {
    let (Some(claim_id), Some(project), Some(number), true) = (
        &task.claim_id,
        &task.task_project,
        task.task_number,
        task.platform_url == platform_url,
    ) else {
        return None;
    };

    Some((
        SyncTaskRef {
            number,
            project: project.clone(),
        },
        ClaimId::new(claim_id.clone()),
    ))
}

/// Returns the task and claim `claim` names, when the platform at
/// `platform_url` issued it.
fn issued_claim(claim: PlatformTaskClaim, platform_url: &str) -> Option<(SyncTaskRef, ClaimId)> {
    (claim.platform_url == platform_url).then(|| {
        (
            SyncTaskRef {
                number: claim.task_number,
                project: claim.task_project,
            },
            ClaimId::new(claim.claim_id),
        )
    })
}

/// Returns the notice of a session that holds the task `start` answered,
/// with the repository's other work in progress, as of `now_ms`.
fn held_notice(start: &SyncStart, now_ms: u64) -> String {
    let task = &start.task;
    let mut notice = if start.created {
        format!(
            "The platform's Sync recorded this session's work as task #{}: {}",
            task.number, task.title
        )
    } else {
        format!(
            "The platform's Sync gave this session the open task #{}: {}",
            task.number, task.title
        )
    };
    if let Some(url) = &task.url {
        notice.push('\n');
        notice.push_str(url);
    }
    if !start.in_progress.is_empty() {
        notice.push_str("\n\nOther work in progress in this repository:");
        for work in &start.in_progress {
            let _ = write!(
                notice,
                "\n- #{} {}, held by {}, active {}",
                work.number,
                work.title,
                work.session.as_deref().unwrap_or(UNLABELLED_HOLDER),
                last_active_text(now_ms, work.last_activity_at_ms)
            );
        }
    }

    notice
}

/// What a conflict notice names: the task and the session holding it.
struct ConflictView<'a> {
    /// Label of the holding session, when it has one.
    holder: Option<&'a str>,
    /// Last activity of the holding session, in milliseconds since the
    /// Unix epoch, when known.
    last_active_ms: Option<u64>,
    /// Task number.
    number: u64,
    /// Task title.
    title: &'a str,
    /// Task page on the platform, when it is one of the platform's pages.
    url: Option<&'a str>,
}

impl<'a> ConflictView<'a> {
    /// Returns what the conflict `task` records.
    fn from_task(task: &'a PlatformTaskRow) -> Self {
        Self {
            holder: task.holder_session.as_deref(),
            last_active_ms: task
                .holder_last_active_at
                .and_then(|seconds| u64::try_from(seconds).ok())
                .map(|seconds| seconds.saturating_mul(1000)),
            number: task.task_number.unwrap_or_default(),
            title: task.task_title.as_deref().unwrap_or_default(),
            url: task.task_url.as_deref(),
        }
    }
}

/// Returns the notice of a session another session's task holds back, as
/// of `now_ms`.
fn conflict_notice(conflict: &ConflictView<'_>, now_ms: u64) -> String {
    let number = conflict.number;
    let title = conflict.title;
    let holder = conflict.holder.unwrap_or(UNLABELLED_HOLDER);
    let last_active = conflict.last_active_ms.map_or_else(
        || "at an unknown time".to_string(),
        |at_ms| last_active_text(now_ms, at_ms),
    );
    let url = conflict
        .url
        .map_or_else(String::new, |url| format!("\n{url}"));

    format!(
        "Another session already works on this on the platform's Sync, so this session has not \
         started.\nTask #{number}: {title}\nHeld by {holder}, last active \
         {last_active}{url}\n\nPress t to take the task over, which ends the other session's \
         claim, or n to start this as separate work. Your saved prompt is retained."
    )
}

#[cfg(test)]
#[path = "platform_sync_test.rs"]
mod tests;
