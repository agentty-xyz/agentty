//! Background sync orchestration for remote git and forge status.
//!
//! One [`SyncOrchestrator`] task per app owns all recurring remote
//! interaction for the active project: the periodic read-only status pass
//! (`git fetch`, branch tracking statuses, review-request refreshes) and the
//! user-triggered mutating main-branch sync. Routing both through one command
//! queue serializes git operations without explicit locking.
//!
//! The app publishes versioned [`SyncContext`] snapshots through a
//! `tokio::sync::watch` channel, so target changes (new sessions, linked
//! review requests, project switches) take effect on the next pass without
//! restarting the task. Emitted status events carry the context generation so
//! the reducer can discard completions computed from stale targets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppEvent, session};
use crate::domain::agent::AgentModel;
use crate::domain::session::{ReviewRequestState, SessionId};

/// Seconds between background read-only sync passes.
const SYNC_TICK_INTERVAL_SECONDS: u64 = 30;
/// Duration terminal project-sync results remain visible in the status bar.
pub(crate) const PROJECT_SYNC_STATUS_VISIBLE_DURATION: Duration = Duration::from_secs(10);
/// Number of ticks between review-request refresh passes, so forge CLIs are
/// polled at half the git-status cadence.
const REVIEW_REQUEST_PASS_TICKS: u64 = 2;
/// Consecutive per-target failures after which one workflow notice is
/// surfaced to the session transcript.
const REVIEW_SYNC_FAILURE_NOTICE_THRESHOLD: u32 = 3;
/// Upper bound of review passes one failing target is skipped between
/// retries.
const REVIEW_SYNC_MAX_BACKOFF_PASSES: u64 = 7;

/// Starts project sync work and emits operation-scoped progress events.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait SyncMainRunner: Send + Sync {
    /// Starts sync for one project and emits one
    /// [`AppEvent::SyncMainCompleted`] when work finishes.
    fn start_sync_main(
        &self,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        operation: ProjectSyncContext,
        session_model: AgentModel,
        sync_context: SyncContext,
    );
}

/// Production [`SyncMainRunner`] that routes manual sync through the
/// orchestrator command queue so the mutating pull/push phases serialize with
/// background status passes instead of racing them.
pub(crate) struct OrchestratorSyncMainRunner {
    command_tx: mpsc::UnboundedSender<SyncCommand>,
}

impl OrchestratorSyncMainRunner {
    /// Creates a runner that forwards sync requests to the orchestrator
    /// command queue.
    pub(crate) fn new(command_tx: mpsc::UnboundedSender<SyncCommand>) -> Self {
        Self { command_tx }
    }
}

impl SyncMainRunner for OrchestratorSyncMainRunner {
    fn start_sync_main(
        &self,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        operation: ProjectSyncContext,
        session_model: AgentModel,
        sync_context: SyncContext,
    ) {
        // Fire-and-forget: the orchestrator only stops at app shutdown.
        let _ = self
            .command_tx
            .send(SyncCommand::SyncMain(Box::new(SyncMainRequest {
                app_event_tx,
                operation,
                session_model,
                sync_context,
            })));
    }
}

/// App-side handle for the background sync orchestrator.
///
/// Owns the command queue sender plus the versioned context publisher used to
/// hand fresh polling targets to the running task.
pub(crate) struct SyncHandle {
    command_tx: mpsc::UnboundedSender<SyncCommand>,
    context_tx: watch::Sender<SyncContext>,
}

impl SyncHandle {
    /// Spawns the sync orchestrator and returns the app-side handle for its
    /// command and context channels.
    pub(crate) fn spawn(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        initial_context: SyncContext,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (context_tx, context_rx) = watch::channel(initial_context);
        SyncOrchestrator::spawn(app_event_tx, command_rx, context_rx);

        Self::new(command_tx, context_tx)
    }

    /// Creates a handle around the orchestrator command and context channels.
    pub(crate) fn new(
        command_tx: mpsc::UnboundedSender<SyncCommand>,
        context_tx: watch::Sender<SyncContext>,
    ) -> Self {
        Self {
            command_tx,
            context_tx,
        }
    }

    /// Returns the generation of the currently published sync context.
    ///
    /// The reducer compares this against the generation carried by status
    /// events to discard completions computed from stale targets.
    pub(crate) fn current_generation(&self) -> u64 {
        self.context_tx.borrow().generation
    }

    /// Returns one immutable snapshot for an explicitly requested sync.
    ///
    /// Unlike recurring polling, a manual sync must keep using the project
    /// selected when the user pressed `s`, even if the visible project changes
    /// before the queued command runs.
    pub(crate) fn context_snapshot(&self) -> SyncContext {
        self.context_tx.borrow().clone()
    }

    /// Builds the production sync-main runner backed by this handle's
    /// orchestrator command sender.
    pub(crate) fn sync_main_runner(&self) -> Arc<dyn SyncMainRunner> {
        Arc::new(OrchestratorSyncMainRunner::new(self.command_tx.clone()))
    }

    /// Publishes one fresh sync context for the next orchestrator pass.
    ///
    /// The generation is bumped only when the polling inputs actually
    /// changed, so client-only refreshes do not invalidate in-flight results.
    pub(crate) fn publish_context(&self, mut context: SyncContext) {
        self.publish_context_with_policy(&mut context, false);
    }

    /// Publishes one fresh sync context while forcing a generation bump.
    ///
    /// Used for explicit refresh requests after mutating workflows so queued
    /// status events computed before the requested refresh cannot overwrite
    /// the post-mutation snapshot.
    pub(crate) fn publish_refresh_context(&self, mut context: SyncContext) {
        self.publish_context_with_policy(&mut context, true);
    }

    /// Publishes `context`, optionally forcing a generation bump even when
    /// the comparable polling inputs are unchanged.
    fn publish_context_with_policy(&self, context: &mut SyncContext, force_generation_bump: bool) {
        let (previous_generation, same_inputs) = {
            let current_context = self.context_tx.borrow();

            (
                current_context.generation,
                current_context.same_polling_inputs(context),
            )
        };
        context.generation = if same_inputs && !force_generation_bump {
            previous_generation
        } else {
            previous_generation.saturating_add(1)
        };

        // Fire-and-forget: the orchestrator only stops at app shutdown.
        let _ = self.context_tx.send(context.clone());
    }

    /// Requests one immediate read-only refresh pass outside the periodic
    /// cadence.
    pub(crate) fn request_refresh(&self) {
        // Fire-and-forget: the orchestrator only stops at app shutdown.
        let _ = self.command_tx.send(SyncCommand::RefreshNow);
    }
}

/// Versioned polling inputs published by the app to the orchestrator.
///
/// The cache key for emitted status events is `generation`; the app bumps it
/// whenever the comparable polling inputs change and the reducer discards
/// events carrying an older generation.
#[derive(Clone)]
pub(crate) struct SyncContext {
    /// Monotonic snapshot version used for stale-completion rejection.
    pub(crate) generation: u64,
    /// Git boundary used for fetch, ahead/behind, and merge-conflict queries.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// Active project branch, or `None` when the project has no git branch
    /// and polling should be skipped.
    pub(crate) project_branch_name: Option<String>,
    /// Stable identifier of the project that owns this context.
    pub(crate) project_id: i64,
    /// User-visible project name captured with this context.
    pub(crate) project_name: String,
    /// Forge boundary used for review-request refreshes.
    pub(crate) review_request_client: Arc<dyn ReviewRequestClient>,
    /// Review-request refresh targets for active sessions.
    pub(crate) review_request_sync_targets: Vec<ReviewRequestSyncTarget>,
    /// Ahead/behind polling targets for active session branches.
    pub(crate) session_git_status_targets: Vec<SessionGitStatusTarget>,
    /// Active project working directory used to resolve the repository root.
    pub(crate) working_dir: PathBuf,
}

impl SyncContext {
    /// Returns whether two contexts describe the same polling inputs.
    ///
    /// Review-request targets are compared through their stable polling keys
    /// so refresh-timestamp churn on linked review requests does not bump the
    /// generation on every successful pass.
    fn same_polling_inputs(&self, other: &SyncContext) -> bool {
        self.project_id == other.project_id
            && self.project_branch_name == other.project_branch_name
            && self.working_dir == other.working_dir
            && self.session_git_status_targets == other.session_git_status_targets
            && review_target_polling_keys(&self.review_request_sync_targets)
                == review_target_polling_keys(&other.review_request_sync_targets)
    }
}

/// Commands accepted by the orchestrator task.
pub(crate) enum SyncCommand {
    /// Runs one immediate read-only status refresh pass.
    RefreshNow,
    /// Runs the user-triggered mutating main-branch sync.
    SyncMain(Box<SyncMainRequest>),
}

/// Inputs for one user-triggered main-branch sync.
pub(crate) struct SyncMainRequest {
    /// Event sender used to emit [`AppEvent::SyncMainCompleted`].
    pub(crate) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Immutable user-request identity and display context.
    pub(crate) operation: ProjectSyncContext,
    /// Model used for agent-assisted conflict resolution.
    pub(crate) session_model: AgentModel,
    /// Immutable project and review-target snapshot captured at enqueue time.
    pub(crate) sync_context: SyncContext,
}

/// Stable identity and display context for one explicit project sync.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectSyncContext {
    /// Branch updated by this operation.
    pub(crate) default_branch: String,
    /// Monotonic app-local operation identifier used to reject stale events.
    pub(crate) operation_id: u64,
    /// Project that owns the target checkout and any follow-up reconciliation.
    pub(crate) project_id: i64,
    /// User-visible project label retained across project switches.
    pub(crate) project_name: String,
}

/// Current user-visible phase of the latest explicit project sync.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProjectSyncPhase {
    /// Pull/rebase/push and preflight work is running.
    Running,
    /// Assisted conflict resolution is running for the reported file count.
    ResolvingConflicts { conflicted_file_count: usize },
    /// The project branch was synchronized successfully.
    Complete {
        deferred_session_count: usize,
        pulled_commits: Option<u32>,
        pushed_commits: Option<u32>,
        resolved_conflict_count: usize,
    },
    /// A recoverable project policy prevented sync from starting.
    Blocked { message: String },
    /// Git, authentication, or assisted conflict resolution failed.
    Failed { message: String },
}

/// Non-modal project sync state rendered independently from the active app
/// mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProjectSyncStatus {
    /// Immutable project and operation identity.
    pub(crate) context: ProjectSyncContext,
    /// Latest accepted lifecycle phase.
    pub(crate) phase: ProjectSyncPhase,
}

impl ProjectSyncStatus {
    /// Returns whether another base-checkout operation must wait.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self.phase,
            ProjectSyncPhase::Running | ProjectSyncPhase::ResolvingConflicts { .. }
        )
    }
}

/// One review refresh computed from the project snapshot owned by a manual
/// sync.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncMainReviewUpdate {
    pub(crate) result: Result<SyncReviewRequestTaskResult, String>,
    pub(crate) session_id: SessionId,
}

/// Terminal payload for one operation-scoped project sync.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SyncMainCompletion {
    pub(crate) operation: ProjectSyncContext,
    pub(crate) result: Result<session::SyncMainOutcome, session::SyncSessionStartError>,
    pub(crate) review_request_updates: Vec<SyncMainReviewUpdate>,
}

/// Event channel and operation identity used by assisted sync progress.
#[derive(Clone)]
pub(crate) struct SyncMainEventContext {
    pub(crate) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(crate) operation: ProjectSyncContext,
}

/// Per-session git-status polling target for one active session branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SessionGitStatusTarget {
    /// Base branch the session branch should be compared against, for example
    /// `main`.
    pub(crate) base_branch: String,
    /// Local branch name tracked for the session, for example
    /// `wt/1234abcd`.
    pub(crate) branch_name: String,
    /// Stable session identifier used as the reducer map key.
    pub(crate) session_id: SessionId,
}

/// Per-session forge-sync polling target for one active review-request
/// candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRequestSyncTarget {
    /// Session worktree directory used to resolve the repository remote and
    /// forge command working directory.
    pub(crate) folder: PathBuf,
    /// Previously linked review request, when Agentty already knows the forge
    /// display id.
    pub(crate) linked_review_request: Option<crate::domain::session::ReviewRequest>,
    /// Published upstream branch tracked by the session, used to discover an
    /// externally created review request when no link has been persisted yet.
    pub(crate) published_upstream_ref: Option<String>,
    /// Stable session identifier used to route reducer updates.
    pub(crate) session_id: SessionId,
}

impl ReviewRequestSyncTarget {
    /// Returns the stable polling identity of this target.
    ///
    /// Excludes the linked review request's refresh timestamp and status text
    /// so routine refreshes do not register as target changes.
    fn polling_key(&self) -> ReviewTargetPollingKey<'_> {
        (
            self.folder.as_path(),
            self.session_id.as_str(),
            self.published_upstream_ref.as_deref(),
            self.linked_review_request
                .as_ref()
                .map(|linked| (linked.summary.display_id.as_str(), linked.summary.state)),
        )
    }
}

/// Stable comparable identity for one review-request polling target.
type ReviewTargetPollingKey<'a> = (
    &'a Path,
    &'a str,
    Option<&'a str>,
    Option<(&'a str, ReviewRequestState)>,
);

/// Returns the stable polling keys for one target list, used for generation
/// comparisons.
fn review_target_polling_keys(
    review_request_sync_targets: &[ReviewRequestSyncTarget],
) -> Vec<ReviewTargetPollingKey<'_>> {
    review_request_sync_targets
        .iter()
        .map(ReviewRequestSyncTarget::polling_key)
        .collect()
}

/// Single background task owning all recurring remote sync work.
///
/// Driven by a command queue plus one tick interval. Periodic passes stay
/// read-only (fetch and forge queries); the mutating main-branch sync runs
/// only for explicit [`SyncCommand::SyncMain`] requests and serializes with
/// the periodic passes through the same queue.
pub(crate) struct SyncOrchestrator {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    command_rx: mpsc::UnboundedReceiver<SyncCommand>,
    context_rx: watch::Receiver<SyncContext>,
    review_pass_index: u64,
    review_sync_failures: HashMap<SessionId, ReviewSyncFailureState>,
    tick_index: u64,
}

impl SyncOrchestrator {
    /// Spawns the orchestrator loop on the runtime.
    ///
    /// The task exits when the command channel closes, which happens when the
    /// app drops its [`SyncHandle`] at shutdown.
    pub(crate) fn spawn(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        command_rx: mpsc::UnboundedReceiver<SyncCommand>,
        context_rx: watch::Receiver<SyncContext>,
    ) {
        let orchestrator = Self {
            app_event_tx,
            command_rx,
            context_rx,
            review_pass_index: 0,
            review_sync_failures: HashMap::new(),
            tick_index: 0,
        };

        tokio::spawn(orchestrator.run());
    }

    /// Runs the command/tick loop until the command channel closes.
    ///
    /// Commands take priority over ticks, and every command-triggered pass
    /// resets the tick timer so one refresh is never immediately duplicated
    /// by the periodic cadence.
    async fn run(mut self) {
        let mut tick = tokio::time::interval(Duration::from_secs(SYNC_TICK_INTERVAL_SECONDS));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };

                    match command {
                        SyncCommand::RefreshNow => self.run_refresh_pass(true).await,
                        SyncCommand::SyncMain(request) => self.run_sync_main(*request).await,
                    }
                    tick.reset();
                }
                _ = tick.tick() => {
                    let include_review_pass = self.tick_index.is_multiple_of(REVIEW_REQUEST_PASS_TICKS);
                    self.tick_index = self.tick_index.wrapping_add(1);
                    self.run_refresh_pass(include_review_pass).await;
                }
            }
        }
    }

    /// Runs one read-only status pass over the current context snapshot.
    ///
    /// Skips entirely when the active project has no git branch.
    async fn run_refresh_pass(&mut self, include_review_pass: bool) {
        let context = self.context_rx.borrow().clone();
        if context.project_branch_name.is_none() {
            return;
        }

        self.run_git_status_pass(&context).await;
        if include_review_pass {
            self.run_review_request_pass(&context).await;
        }
    }

    /// Fetches the remote and emits one combined ahead/behind snapshot for
    /// the project branch plus all active session branches.
    async fn run_git_status_pass(&self, context: &SyncContext) {
        let working_dir = context.working_dir.clone();
        let repo_root = context
            .git_client
            .find_git_repo_root(working_dir.clone())
            .await
            .unwrap_or(working_dir);

        {
            let repo_root = repo_root.clone();
            // Best-effort: background fetch failure is non-critical.
            let _ = context.git_client.fetch_remote(repo_root).await;
        }

        let branch_tracking_statuses = {
            let repo_root = repo_root.clone();
            context
                .git_client
                .branch_tracking_statuses(repo_root)
                .await
                .unwrap_or_default()
        };
        let project_branch_name = context.project_branch_name.as_deref().unwrap_or_default();
        let status = branch_tracking_statuses
            .get(project_branch_name)
            .copied()
            .flatten();
        let session_statuses = session_git_statuses(
            &branch_tracking_statuses,
            &repo_root,
            &context.session_git_status_targets,
            context.git_client.as_ref(),
        )
        .await;

        if self.context_is_stale(context) {
            return;
        }

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = self.app_event_tx.send(AppEvent::GitStatusUpdated {
            generation: context.generation,
            session_statuses,
            status,
        });
    }

    /// Refreshes linked or published review-request state for all targets in
    /// the context snapshot.
    ///
    /// Emits one [`AppEvent::ReviewRequestStatusUpdated`] per target so the
    /// reducer can persist refreshed summaries and transition externally
    /// merged or closed sessions. Targets in failure backoff are skipped
    /// until their retry pass.
    async fn run_review_request_pass(&mut self, context: &SyncContext) {
        let updates = self
            .collect_review_request_pass_updates(context, true)
            .await;

        self.emit_review_request_pass_updates(context, updates);
    }

    /// Refreshes review-request status for all eligible targets and returns
    /// reducer-ready updates without emitting them yet.
    ///
    /// Manual main sync uses this to defer merged/closed session transitions
    /// until after the local default branch has been updated successfully.
    async fn collect_review_request_pass_updates(
        &mut self,
        context: &SyncContext,
        reject_stale_context: bool,
    ) -> Vec<ReviewRequestPassUpdate> {
        let review_pass_index = self.review_pass_index;
        self.review_pass_index = self.review_pass_index.wrapping_add(1);
        let mut updates = Vec::new();

        for review_request_sync_target in &context.review_request_sync_targets {
            if reject_stale_context && self.context_is_stale(context) {
                return updates;
            }
            if self.is_target_backed_off(&review_request_sync_target.session_id, review_pass_index)
            {
                continue;
            }

            let result = sync_review_request_status(
                review_request_sync_target.folder.clone(),
                context.git_client.as_ref(),
                review_request_sync_target.linked_review_request.clone(),
                review_request_sync_target.published_upstream_ref.clone(),
                context.review_request_client.as_ref(),
            )
            .await;

            if reject_stale_context && self.context_is_stale(context) {
                return updates;
            }

            self.record_review_sync_outcome(review_request_sync_target, &result, review_pass_index);
            updates.push(ReviewRequestPassUpdate {
                result,
                target: review_request_sync_target.clone(),
            });
        }

        self.retain_active_failure_states(&context.review_request_sync_targets);

        updates
    }

    /// Emits previously collected review-request updates.
    fn emit_review_request_pass_updates(
        &self,
        context: &SyncContext,
        updates: Vec<ReviewRequestPassUpdate>,
    ) {
        for update in updates {
            let ReviewRequestPassUpdate { result, target } = update;
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = self
                .app_event_tx
                .send(AppEvent::ReviewRequestStatusUpdated {
                    generation: context.generation,
                    result,
                    session_id: target.session_id,
                });
        }
    }

    /// Runs one user-triggered main-branch sync.
    ///
    /// Review-request state is refreshed before the mutating git phases, but
    /// successful terminal updates are emitted only after the main branch
    /// sync succeeds. That keeps externally merged child restacking behind the
    /// updated local default branch for the manual `s` workflow.
    async fn run_sync_main(&mut self, request: SyncMainRequest) {
        let SyncMainRequest {
            app_event_tx,
            operation,
            session_model,
            sync_context,
        } = request;

        let review_request_updates = self
            .collect_review_request_pass_updates(&sync_context, false)
            .await;

        let result = session::SessionManager::sync_main_for_project(
            sync_context.project_branch_name.clone(),
            sync_context.working_dir.clone(),
            Some(SyncMainEventContext {
                app_event_tx: app_event_tx.clone(),
                operation: operation.clone(),
            }),
            Arc::clone(&sync_context.git_client),
            session_model,
        )
        .await;
        let review_request_updates = result.is_ok().then(|| {
            review_request_updates
                .into_iter()
                .map(|update| SyncMainReviewUpdate {
                    result: update.result,
                    session_id: update.target.session_id,
                })
                .collect()
        });

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = app_event_tx.send(AppEvent::SyncMainCompleted {
            completion: SyncMainCompletion {
                operation,
                result,
                review_request_updates: review_request_updates.unwrap_or_default(),
            },
        });
    }

    /// Returns whether the app published a newer context since this pass
    /// started, in which case remaining work targets stale inputs.
    fn context_is_stale(&self, context: &SyncContext) -> bool {
        self.context_rx.borrow().generation != context.generation
    }

    /// Returns whether one failing target is still inside its backoff window.
    fn is_target_backed_off(&self, session_id: &SessionId, review_pass_index: u64) -> bool {
        self.review_sync_failures
            .get(session_id)
            .is_some_and(|failure_state| review_pass_index < failure_state.retry_after_pass)
    }

    /// Updates per-target failure tracking after one review-request sync.
    ///
    /// Successes clear the failure state. Failures extend the backoff window
    /// exponentially and surface one workflow notice when the consecutive
    /// failure count reaches the notice threshold, instead of retrying at
    /// full rate forever with no user-visible signal.
    fn record_review_sync_outcome(
        &mut self,
        review_request_sync_target: &ReviewRequestSyncTarget,
        result: &Result<SyncReviewRequestTaskResult, String>,
        review_pass_index: u64,
    ) {
        let session_id = &review_request_sync_target.session_id;
        match result {
            Ok(_) => {
                self.review_sync_failures.remove(session_id);
            }
            Err(error) => {
                let failure_state = self
                    .review_sync_failures
                    .entry(session_id.clone())
                    .or_default();
                failure_state.consecutive_failures =
                    failure_state.consecutive_failures.saturating_add(1);
                failure_state.retry_after_pass = review_pass_index
                    .saturating_add(1)
                    .saturating_add(review_sync_backoff_passes(
                        failure_state.consecutive_failures,
                    ));

                tracing::warn!(
                    session_id = %session_id,
                    consecutive_failures = failure_state.consecutive_failures,
                    "review request sync failed: {error}",
                );
                if failure_state.consecutive_failures == REVIEW_SYNC_FAILURE_NOTICE_THRESHOLD {
                    // Fire-and-forget: receiver may be dropped during
                    // shutdown.
                    let _ = self
                        .app_event_tx
                        .send(AppEvent::SessionWorkflowNoticeUpdated {
                            notice: format!(
                                "[Review request sync] {} consecutive failures; retrying with \
                                 backoff. Last error: {error}",
                                failure_state.consecutive_failures
                            ),
                            session_id: session_id.clone(),
                        });
                }
            }
        }
    }

    /// Drops failure tracking for sessions no longer in the target list.
    fn retain_active_failure_states(
        &mut self,
        review_request_sync_targets: &[ReviewRequestSyncTarget],
    ) {
        self.review_sync_failures.retain(|session_id, _| {
            review_request_sync_targets
                .iter()
                .any(|target| &target.session_id == session_id)
        });
    }
}

/// Per-session consecutive review-sync failure tracking used for backoff.
#[derive(Default)]
struct ReviewSyncFailureState {
    /// Number of review-sync failures since the last success.
    consecutive_failures: u32,
    /// Review-pass index at which the target may be retried.
    retry_after_pass: u64,
}

/// One review-request refresh result and its polling target.
struct ReviewRequestPassUpdate {
    /// Result emitted to the reducer for review-request persistence and
    /// terminal state transitions.
    result: Result<SyncReviewRequestTaskResult, String>,
    /// Polling target that produced this result.
    target: ReviewRequestSyncTarget,
}

/// Returns how many review passes a failing target is skipped before retry.
///
/// Grows exponentially with the consecutive failure count and is capped at
/// [`REVIEW_SYNC_MAX_BACKOFF_PASSES`].
fn review_sync_backoff_passes(consecutive_failures: u32) -> u64 {
    let exponent = consecutive_failures.saturating_sub(1).min(3);

    ((1_u64 << exponent) - 1).min(REVIEW_SYNC_MAX_BACKOFF_PASSES)
}

/// Resolves ahead/behind and merge-conflict snapshots for all tracked session
/// branches, combining each branch's base comparison with any tracked-remote
/// snapshot already available from the repo-wide status query.
async fn session_git_statuses(
    branch_tracking_statuses: &HashMap<String, Option<(u32, u32)>>,
    repo_root: &Path,
    session_git_status_targets: &[SessionGitStatusTarget],
    git_client: &dyn GitClient,
) -> HashMap<SessionId, SessionGitStatus> {
    let mut session_git_statuses = HashMap::with_capacity(session_git_status_targets.len());

    for session_git_status_target in session_git_status_targets {
        let base_status = git_client
            .get_ref_ahead_behind(
                repo_root.to_path_buf(),
                session_git_status_target.branch_name.clone(),
                session_git_status_target.base_branch.clone(),
            )
            .await
            .ok();
        let has_merge_conflict = match base_status {
            Some((ahead, behind)) if ahead > 0 && behind > 0 => git_client
                .has_merge_conflicts(
                    repo_root.to_path_buf(),
                    session_git_status_target.branch_name.clone(),
                    session_git_status_target.base_branch.clone(),
                )
                .await
                .ok(),
            Some(_) => Some(false),
            None => None,
        };
        let remote_status = branch_tracking_statuses
            .get(&session_git_status_target.branch_name)
            .copied()
            .flatten();
        session_git_statuses.insert(
            session_git_status_target.session_id.clone(),
            SessionGitStatus {
                base_status,
                has_merge_conflict,
                remote_status,
            },
        );
    }

    session_git_statuses
}

/// Runs one review-request sync against the forge.
///
/// When the session has a linked review request, this refreshes it by display
/// id. Otherwise, when the branch was published, this searches for an
/// externally created review request by source branch name.
async fn sync_review_request_status(
    folder: PathBuf,
    git_client: &dyn GitClient,
    linked_review_request: Option<crate::domain::session::ReviewRequest>,
    published_upstream_ref: Option<String>,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<SyncReviewRequestTaskResult, String> {
    let remote = review_request_remote(
        folder.clone(),
        git_client,
        linked_review_request.as_ref(),
        review_request_client,
    )
    .await?;

    if let Some(review_request) = linked_review_request {
        let refreshed_summary = review_request_client
            .refresh_review_request(remote, review_request.summary.display_id)
            .await
            .map_err(|error| error.detail_message())?;
        let session_head_hash =
            session_head_hash_for_summary(git_client, &folder, &refreshed_summary).await;

        return Ok(sync_task_result_from_summary(
            refreshed_summary,
            session_head_hash,
        ));
    }

    let upstream_ref = published_upstream_ref
        .ok_or_else(|| "Session branch has not been published yet".to_string())?;
    let source_branch = session::remote_branch_name_from_upstream_ref(&upstream_ref);
    let found_summary = review_request_client
        .find_by_source_branch(remote, source_branch)
        .await
        .map_err(|error| error.detail_message())?;

    match found_summary {
        Some(summary) => {
            let session_head_hash =
                session_head_hash_for_summary(git_client, &folder, &summary).await;
            Ok(sync_task_result_from_summary(summary, session_head_hash))
        }
        None => Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
            summary: None,
        }),
    }
}

/// Resolves the forge remote used for one background review-request refresh.
///
/// Active sessions prefer the live worktree remote so forge CLI commands
/// inherit local repository context. Linked review requests can fall back to
/// their stored web URL when the worktree has already disappeared or no longer
/// resolves as a repository, allowing terminal PR/MR state to still be
/// observed.
async fn review_request_remote(
    folder: PathBuf,
    git_client: &dyn GitClient,
    linked_review_request: Option<&crate::domain::session::ReviewRequest>,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<ag_forge::ForgeRemote, String> {
    match git_client.repo_url(folder.clone()).await {
        Ok(repo_url) => review_request_client
            .detect_remote(repo_url)
            .map(|remote| remote.with_command_working_directory(folder))
            .map_err(|error| error.detail_message()),
        Err(repo_url_error) => {
            let repo_url = linked_review_request
                .and_then(review_request_repo_url)
                .ok_or_else(|| format!("Failed to resolve repository remote: {repo_url_error}"))?;

            review_request_client
                .detect_remote(repo_url)
                .map_err(|error| error.detail_message())
        }
    }
}

/// Derives a repository URL from one persisted review-request web URL.
fn review_request_repo_url(
    review_request: &crate::domain::session::ReviewRequest,
) -> Option<String> {
    let web_url = review_request.summary.web_url.trim_end_matches('/');

    match review_request.summary.forge_kind {
        ag_forge::ForgeKind::GitHub => web_url
            .split_once("/pull/")
            .map(|(repo_url, _)| repo_url.to_string()),
        ag_forge::ForgeKind::GitLab => web_url
            .split_once("/-/merge_requests/")
            .or_else(|| web_url.split_once("/merge_requests/"))
            .map(|(repo_url, _)| repo_url.to_string()),
    }
}

/// Captures the local session branch `HEAD` hash only when the review request
/// is merged upstream so continuation can seed future work with a specific
/// commit.
async fn session_head_hash_for_summary(
    git_client: &dyn GitClient,
    folder: &Path,
    summary: &crate::domain::session::ReviewRequestSummary,
) -> Option<String> {
    if summary.state != crate::domain::session::ReviewRequestState::Merged {
        return None;
    }

    git_client.head_hash(folder.to_path_buf()).await.ok()
}

/// Builds one sync result from a normalized review-request summary.
fn sync_task_result_from_summary(
    summary: crate::domain::session::ReviewRequestSummary,
    session_head_hash: Option<String>,
) -> SyncReviewRequestTaskResult {
    let display_id = summary.display_id.clone();
    let outcome = match summary.state {
        crate::domain::session::ReviewRequestState::Open => {
            session::SyncReviewRequestOutcome::Open {
                display_id,
                status_summary: summary.status_summary.clone(),
            }
        }
        crate::domain::session::ReviewRequestState::Merged => {
            session::SyncReviewRequestOutcome::Merged {
                display_id,
                session_head_hash,
            }
        }
        crate::domain::session::ReviewRequestState::Closed => {
            session::SyncReviewRequestOutcome::Closed { display_id }
        }
    };

    SyncReviewRequestTaskResult {
        outcome,
        summary: Some(summary),
    }
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
