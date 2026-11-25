//! Background sync orchestration for remote git and forge status.
//!
//! One [`SyncOrchestrator`] task per app owns all recurring remote
//! interaction: the periodic read-only status pass of the active project
//! (`git fetch`, branch tracking statuses, review-request refreshes), the
//! git-status pass of each project whose session page session-API callers
//! view, and the user-triggered mutating main-branch sync. Commands arrive
//! through one queue. Each main-branch sync and each viewed project's pass
//! runs its Git work in its own task, so syncs of different projects proceed
//! independently and a slow viewed project never holds up the active one.
//! Fetches of one project never overlap: the background fetch of a syncing
//! project pauses until its sync finishes, and a sync waits for its project's
//! running viewed pass.
//!
//! The app publishes versioned [`SyncContext`] snapshots, one for the active
//! project and one per viewed project, through `tokio::sync::watch` channels,
//! so target changes (new sessions, linked review requests, project switches,
//! pages loaded or dropped) take effect on the next pass without restarting
//! the task. Emitted status events carry their project and the context
//! generation, which no other published context shares, so the reducer can
//! discard completions computed from stale targets or for another role of the
//! project.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ag_forge::ReviewRequestClient;
use ag_git::{BranchTrackingMap, GitClient};
use ag_telemetry::{FutureExt as _, KeyValue, Outcome, Span};
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::review_watch_config::ReviewWatchConfig;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppEvent, session, spawn};
use crate::domain::agent::AgentModel;
use crate::domain::session::{ReviewRequestState, SessionId};
use crate::infra::fs::FsClient;

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
/// Upper bound of status passes the background fetch of one failing project
/// is skipped between retries, so an unreachable remote is retried about
/// every eight minutes at the periodic cadence.
const FETCH_MAX_BACKOFF_PASSES: u64 = 15;
/// Consecutive review passes that must return the previous result again
/// before a target drops to the idle cadence.
const REVIEW_SYNC_IDLE_AFTER_UNCHANGED_PASSES: u32 = 3;
/// Review passes between two polls of an idle target.
///
/// Review passes run about once a minute, so an external merge, close, new
/// review comment, or check result on an idle review request is seen at most
/// five passes, about five minutes, late. Watched review requests with running
/// checks keep the full cadence, and explicit refreshes poll idle targets at
/// once.
const REVIEW_SYNC_IDLE_INTERVAL_PASSES: u64 = 5;

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
/// Owns the command queue sender plus the versioned context publishers used
/// to hand fresh polling targets, of the active project and of each viewed
/// project, to the running task.
pub(crate) struct SyncHandle {
    command_tx: mpsc::UnboundedSender<SyncCommand>,
    context_tx: watch::Sender<SyncContext>,
    /// Latest generation handed to a published context, active or viewed,
    /// so no two publications with different polling inputs share one.
    last_generation: AtomicU64,
    viewed_context_tx: watch::Sender<Vec<SyncContext>>,
}

impl SyncHandle {
    /// Returns the app-side handle and the orchestrator it controls, which
    /// does not run until [`SyncOrchestrator::spawn`].
    ///
    /// Contexts published and commands sent before the spawn wait for the
    /// orchestrator, so the app can finish startup reconciliation first. Its
    /// first pass then reads the reconciled polling targets instead of the
    /// initial ones.
    pub(crate) fn unstarted(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        initial_context: SyncContext,
    ) -> (Self, SyncOrchestrator) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (context_tx, context_rx) = watch::channel(initial_context);
        let (sync_handle, viewed_context_rx) = Self::new(command_tx, context_tx);
        let orchestrator =
            SyncOrchestrator::new(app_event_tx, command_rx, context_rx, viewed_context_rx);

        (sync_handle, orchestrator)
    }

    /// Creates a handle around the orchestrator command channel and the
    /// active context channel, with a viewed context channel that holds no
    /// viewed project yet, and returns it with that channel's receiver.
    pub(crate) fn new(
        command_tx: mpsc::UnboundedSender<SyncCommand>,
        context_tx: watch::Sender<SyncContext>,
    ) -> (Self, watch::Receiver<Vec<SyncContext>>) {
        let last_generation = context_tx.borrow().generation;
        let (viewed_context_tx, viewed_context_rx) = watch::channel(Vec::new());
        let sync_handle = Self {
            command_tx,
            context_tx,
            last_generation: AtomicU64::new(last_generation),
            viewed_context_tx,
        };

        (sync_handle, viewed_context_rx)
    }

    /// Returns the generation of the currently published sync context.
    ///
    /// The reducer compares this against the generation carried by status
    /// events to discard completions computed from stale targets.
    pub(crate) fn current_generation(&self) -> u64 {
        self.context_tx.borrow().generation
    }

    /// Returns the generation of the published context that polls the git
    /// status of `project_id`: the active context when it polls that
    /// project, otherwise the project's viewed context, or `None` when no
    /// published context polls it.
    ///
    /// Each publication that changes polling inputs takes a generation no
    /// other context had, so a status computed for a project's earlier role
    /// or targets, such as before its page reloaded, was dropped, or was
    /// promoted to the active page, never matches.
    pub(crate) fn generation_for(&self, project_id: i64) -> Option<u64> {
        {
            let active_context = self.context_tx.borrow();
            if active_context.project_id == project_id {
                return Some(active_context.generation);
            }
        }

        self.viewed_context_tx
            .borrow()
            .iter()
            .find(|context| context.project_id == project_id)
            .map(|context| context.generation)
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

    /// Publishes the git-status polling contexts of the viewed projects, one
    /// per project, replacing the previous set.
    ///
    /// A project keeps its generation while its polling inputs are
    /// unchanged; a changed, added, or re-added project takes a new one.
    /// The orchestrator hears of the set only when a project or a
    /// generation changed, and then polls each project not polled yet at
    /// once.
    pub(crate) fn publish_viewed_contexts(&self, contexts: Vec<SyncContext>) {
        self.publish_viewed_contexts_with_policy(contexts, false);
    }

    /// Publishes the viewed projects' polling contexts while forcing a new
    /// generation for each, as [`Self::publish_refresh_context`] does for
    /// the active one.
    pub(crate) fn publish_refresh_viewed_contexts(&self, contexts: Vec<SyncContext>) {
        self.publish_viewed_contexts_with_policy(contexts, true);
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
            self.next_generation()
        };

        // Fire-and-forget: the orchestrator only stops at app shutdown.
        let _ = self.context_tx.send(context.clone());
    }

    /// Publishes the viewed `contexts`, optionally forcing a new generation
    /// for each even when its polling inputs are unchanged.
    fn publish_viewed_contexts_with_policy(
        &self,
        mut contexts: Vec<SyncContext>,
        force_generation_bump: bool,
    ) {
        {
            let published_contexts = self.viewed_context_tx.borrow();
            for context in &mut contexts {
                let previous_context = published_contexts
                    .iter()
                    .find(|published| published.project_id == context.project_id);
                context.generation = match previous_context {
                    Some(previous_context)
                        if !force_generation_bump
                            && previous_context.same_polling_inputs(context) =>
                    {
                        previous_context.generation
                    }
                    _ => self.next_generation(),
                };
            }
        }

        self.viewed_context_tx
            .send_if_modified(|published_contexts| {
                let is_modified =
                    polling_versions(published_contexts) != polling_versions(&contexts);
                *published_contexts = contexts;

                is_modified
            });
    }

    /// Takes the next generation, one no published context had before.
    fn next_generation(&self) -> u64 {
        self.last_generation
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    /// Requests one immediate read-only refresh pass outside the periodic
    /// cadence.
    pub(crate) fn request_refresh(&self) {
        // Fire-and-forget: the orchestrator only stops at app shutdown.
        let _ = self.command_tx.send(SyncCommand::RefreshNow);
    }
}

/// Versioned polling inputs of one project published by the app to the
/// orchestrator.
///
/// The cache key for emitted status events is `generation`; the app bumps it
/// whenever the comparable polling inputs change and the reducer discards
/// events carrying an older generation. A viewed project's context polls
/// only its git status, so it carries no review-request targets.
#[derive(Clone)]
pub(crate) struct SyncContext {
    /// Worker submission handle captured with this project snapshot.
    pub(crate) run_client: Arc<dyn ag_worker::RunClient>,
    /// Monotonic snapshot version used for stale-completion rejection.
    pub(crate) generation: u64,
    /// Filesystem boundary used to read the project's review-watch
    /// configuration.
    pub(crate) fs_client: Arc<dyn FsClient>,
    /// Git boundary used for fetch, ahead/behind, and merge-conflict queries.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// Project branch, or `None` when the project has no git branch and
    /// polling should be skipped.
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
    /// Project working directory used to resolve the repository root.
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
    /// Whether the session rests in `Watching`, so each pass also reports the
    /// forge facts that may need automatic follow-up.
    pub(crate) is_watched: bool,
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
            self.is_watched,
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
    bool,
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

/// Returns each context's project and generation, which change exactly when
/// a viewed project is added, dropped, or polled from new inputs.
fn polling_versions(contexts: &[SyncContext]) -> Vec<(i64, u64)> {
    contexts
        .iter()
        .map(|context| (context.project_id, context.generation))
        .collect()
}

/// Single background task owning all recurring remote sync work.
///
/// Driven by a command queue plus one tick interval. Periodic passes stay
/// read-only (fetch and forge queries); the mutating main-branch sync runs
/// only for explicit [`SyncCommand::SyncMain`] requests. Each such sync runs
/// its Git phases in a detached task, so later commands and periodic passes
/// keep running alongside it. Only the syncing project's background fetch is
/// skipped until the task reports back on the finished channel.
///
/// Each pass also starts a git-status pass for every viewed project in its
/// own task, at most one per project at a time, beside the active project's
/// pass in this loop. Viewed passes share the active pass's fetch-pass
/// count and per-project fetch backoff.
pub(crate) struct SyncOrchestrator {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_comparison_cache: BaseComparisonCache,
    command_rx: mpsc::UnboundedReceiver<SyncCommand>,
    context_rx: watch::Receiver<SyncContext>,
    fetch_failures: HashMap<i64, FailureBackoff>,
    /// Index of the next refresh pass, counted once per pass for the fetch
    /// backoff of every project.
    fetch_pass_index: u64,
    review_pass_index: u64,
    review_sync_cadences: HashMap<SessionId, ReviewSyncCadenceState>,
    review_sync_failures: HashMap<SessionId, FailureBackoff>,
    sync_main_tasks: SyncMainTasks,
    tick_index: u64,
    viewed_context_rx: watch::Receiver<Vec<SyncContext>>,
    viewed_passes: ViewedPasses,
}

impl SyncOrchestrator {
    /// Creates an orchestrator that reads commands from `command_rx`, the
    /// active project's polling context from `context_rx`, and the viewed
    /// projects' from `viewed_context_rx`, and reports to `app_event_tx`.
    fn new(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        command_rx: mpsc::UnboundedReceiver<SyncCommand>,
        context_rx: watch::Receiver<SyncContext>,
        viewed_context_rx: watch::Receiver<Vec<SyncContext>>,
    ) -> Self {
        Self {
            app_event_tx,
            base_comparison_cache: BaseComparisonCache::default(),
            command_rx,
            context_rx,
            fetch_failures: HashMap::new(),
            fetch_pass_index: 0,
            review_pass_index: 0,
            review_sync_cadences: HashMap::new(),
            review_sync_failures: HashMap::new(),
            sync_main_tasks: SyncMainTasks::default(),
            tick_index: 0,
            viewed_context_rx,
            viewed_passes: ViewedPasses::default(),
        }
    }

    /// Spawns the orchestrator loop on the runtime.
    ///
    /// The first pass starts at once and reads the latest published context.
    /// The task exits when the command channel closes, which happens when the
    /// app drops its [`SyncHandle`] at shutdown. The host execution context
    /// follows the task so conflict assistance retains its capture policy.
    pub(crate) fn spawn(self) {
        spawn::spawn_traced(self.run());
    }

    /// Runs the command/tick loop until the command channel closes.
    ///
    /// Reports from finished syncs come first, so a sync requested in
    /// reaction to one finishing stays marked running, then reports from
    /// finished viewed passes and changes of the viewed projects. Commands
    /// take priority over ticks, and every command-triggered pass resets the
    /// tick timer so one refresh is never immediately duplicated by the
    /// periodic cadence.
    async fn run(mut self) {
        let mut tick = tokio::time::interval(Duration::from_secs(SYNC_TICK_INTERVAL_SECONDS));
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                biased;
                Some(finished) = self.sync_main_tasks.finished_rx.recv() => {
                    self.finish_sync_main(&finished);
                }
                Some(finished) = self.viewed_passes.finished_rx.recv() => {
                    self.finish_viewed_pass(finished).await;
                }
                Ok(()) = self.viewed_context_rx.changed() => {
                    self.start_new_viewed_passes();
                }
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };

                    match command {
                        SyncCommand::RefreshNow => self.run_refresh_pass(ReviewPass::Forced).await,
                        SyncCommand::SyncMain(request) => self.run_sync_main(*request).await,
                    }
                    tick.reset();
                }
                _ = tick.tick() => {
                    let review_pass = ReviewPass::for_tick(self.tick_index);
                    self.tick_index = self.tick_index.wrapping_add(1);
                    self.run_refresh_pass(review_pass).await;
                }
            }
        }
    }

    /// Runs one read-only status pass over the current context snapshot as
    /// one independent `sync.refresh` trace, after starting each viewed
    /// project's git-status pass beside it.
    ///
    /// Only periodic passes fetch viewed projects' remotes, once per tick;
    /// a requested refresh follows local changes, so their passes compare
    /// against the refs fetched last. The active project's pass skips
    /// entirely, without a trace, when the project has no git branch. The
    /// trace fails when any phase failed and is canceled when newer polling
    /// targets superseded the pass.
    async fn run_refresh_pass(&mut self, review_pass: ReviewPass) {
        let fetch_pass_index = self.fetch_pass_index;
        self.fetch_pass_index = self.fetch_pass_index.wrapping_add(1);
        let context = self.context_rx.borrow().clone();
        self.start_viewed_passes((review_pass != ReviewPass::Forced).then_some(fetch_pass_index));
        if context.project_branch_name.is_none() {
            return;
        }

        let refresh = Span::root(
            "sync.refresh",
            vec![KeyValue::new(
                "agentty.sync.review_pass",
                review_pass != ReviewPass::Skip,
            )],
        );
        let trace = refresh.context();
        let mut outcome = self
            .run_git_status_pass(&context, fetch_pass_index)
            .with_context(trace.clone())
            .await;
        if review_pass != ReviewPass::Skip {
            let review_outcome = self
                .run_review_request_pass(&context, review_pass == ReviewPass::Forced)
                .with_context(trace)
                .await;
            outcome = combined_outcome(outcome, review_outcome);
        }
        refresh.finish(outcome);
    }

    /// Fetches the remote and emits one combined ahead/behind snapshot for
    /// the project branch plus all active session branches.
    ///
    /// A failed fetch or tracking-status query still emits the snapshot
    /// computed from the remaining data, but fails its `git.fetch` or
    /// `sync.branch_status` phase and the returned outcome. A fetch skipped
    /// by failure backoff does not fail the pass. Session base comparisons
    /// whose commits did not move since the previous pass are reused without
    /// spawning Git.
    async fn run_git_status_pass(
        &mut self,
        context: &SyncContext,
        fetch_pass_index: u64,
    ) -> Outcome {
        let repo_root = repo_root(context).await;

        // Background fetch failure is non-critical: tracking statuses still
        // compare against the last fetched remote refs.
        let fetch_plan = self.fetch_plan(context.project_id, fetch_pass_index);
        let (fetch_outcome, fetch_succeeded) =
            run_fetch_plan(context, &repo_root, fetch_plan).await;
        if let Some(fetch_succeeded) = fetch_succeeded {
            self.record_fetch_result(context.project_id, fetch_pass_index, fetch_succeeded);
        }

        let (status, session_statuses, status_outcome) =
            branch_status_snapshot(context, &repo_root, &mut self.base_comparison_cache).await;
        let outcome = combined_outcome(fetch_outcome, status_outcome);
        if self.context_is_stale(context) {
            return combined_outcome(outcome, Outcome::Canceled);
        }

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = self.app_event_tx.send(AppEvent::GitStatusUpdated {
            generation: context.generation,
            project_id: context.project_id,
            session_statuses,
            status,
        });

        outcome
    }

    /// Decides whether fetch pass `fetch_pass_index` fetches `project_id`'s
    /// remote: not while the project's explicit sync or another pass of the
    /// project runs, so two Git transfers of one project never overlap, and
    /// not while repeated failures keep the project in backoff.
    fn fetch_plan(&self, project_id: i64, fetch_pass_index: u64) -> FetchPlan {
        if self
            .sync_main_tasks
            .running_project_ids
            .contains(&project_id)
        {
            return FetchPlan::DeferredForSync;
        }
        if self.viewed_passes.is_running(project_id) {
            return FetchPlan::DeferredForPass;
        }

        let backoff = self.fetch_failures.get(&project_id);
        if backoff.is_some_and(|backoff| backoff.is_backed_off(fetch_pass_index)) {
            return FetchPlan::BackedOff;
        }

        FetchPlan::Fetch {
            consecutive_failures: backoff.map_or(0, |backoff| backoff.consecutive_failures),
        }
    }

    /// Records the result of `project_id`'s fetch during fetch pass
    /// `fetch_pass_index`: a success clears the project's backoff, and a
    /// failure extends its window exponentially.
    fn record_fetch_result(&mut self, project_id: i64, fetch_pass_index: u64, succeeded: bool) {
        if succeeded {
            self.fetch_failures.remove(&project_id);

            return;
        }

        self.fetch_failures
            .entry(project_id)
            .or_default()
            .record_failure(fetch_pass_index, FETCH_MAX_BACKOFF_PASSES);
    }

    /// Starts a git-status pass for each published viewed project, counted
    /// as fetch pass `fetch_pass_index` or fetching nothing when it is
    /// `None`, and forgets the polling state of projects no longer viewed.
    fn start_viewed_passes(&mut self, fetch_pass_index: Option<u64>) {
        let viewed_contexts = self.viewed_context_rx.borrow_and_update().clone();
        self.viewed_passes.retain_viewed(&viewed_contexts);

        for context in viewed_contexts {
            self.start_viewed_pass(context, fetch_pass_index);
        }
    }

    /// Starts a pass at once for each viewed project not polled yet, such as
    /// one whose page just loaded, so its counts do not wait for the next
    /// tick, and forgets the polling state of projects no longer viewed.
    ///
    /// The pass counts as the next fetch pass.
    fn start_new_viewed_passes(&mut self) {
        let viewed_contexts = self.viewed_context_rx.borrow_and_update().clone();
        self.viewed_passes.retain_viewed(&viewed_contexts);

        for context in viewed_contexts {
            if !self
                .viewed_passes
                .projects
                .contains_key(&context.project_id)
            {
                self.start_viewed_pass(context, Some(self.fetch_pass_index));
            }
        }
    }

    /// Starts one git-status pass of a viewed project in its own task,
    /// counted as fetch pass `fetch_pass_index`, or comparing against the
    /// refs fetched last without fetching when it is `None`.
    ///
    /// Does nothing while the project's previous pass runs, when it has no
    /// git branch, or when it is the active project, whose own pass polls
    /// it.
    fn start_viewed_pass(&mut self, context: SyncContext, fetch_pass_index: Option<u64>) {
        if context.project_branch_name.is_none()
            || context.project_id == self.context_rx.borrow().project_id
            || self.viewed_passes.is_running(context.project_id)
        {
            return;
        }

        let fetch_plan = fetch_pass_index.map_or(FetchPlan::Reuse, |fetch_pass_index| {
            self.fetch_plan(context.project_id, fetch_pass_index)
        });
        let project_poll = self
            .viewed_passes
            .projects
            .entry(context.project_id)
            .or_default();
        project_poll.is_running = true;
        let viewed_pass = ViewedPass {
            app_event_tx: self.app_event_tx.clone(),
            base_comparison_cache: std::mem::take(&mut project_poll.base_comparison_cache),
            context,
            fetch_pass_index: fetch_pass_index.unwrap_or(self.fetch_pass_index),
            fetch_plan,
            finished_tx: self.viewed_passes.finished_tx.clone(),
            viewed_context_rx: self.viewed_context_rx.clone(),
        };

        spawn::spawn_detached(viewed_pass.run());
    }

    /// Takes back the state of one finished viewed pass and records its
    /// fetch result.
    ///
    /// An explicit sync that waited for the pass starts now. Otherwise a
    /// pass whose inputs the project's published context no longer has,
    /// even if they changed only after it emitted, runs again at once from
    /// the new ones, comparing against the refs it just fetched, so a forced
    /// refresh or a reloaded page is reflected without waiting for the next
    /// tick.
    async fn finish_viewed_pass(&mut self, finished: ViewedPassFinished) {
        let ViewedPassFinished {
            base_comparison_cache,
            fetch_pass_index,
            fetch_succeeded,
            generation,
            project_id,
        } = finished;
        if let Some(fetch_succeeded) = fetch_succeeded {
            self.record_fetch_result(project_id, fetch_pass_index, fetch_succeeded);
        }
        let viewed_context = self
            .viewed_context_rx
            .borrow()
            .iter()
            .find(|context| context.project_id == project_id)
            .cloned();
        if viewed_context.is_some() {
            let project_poll = self.viewed_passes.projects.entry(project_id).or_default();
            project_poll.base_comparison_cache = base_comparison_cache;
            project_poll.is_running = false;
        } else {
            self.viewed_passes.projects.remove(&project_id);
        }

        let waiting_sync_mains = self.viewed_passes.take_waiting_sync_mains(project_id);
        if !waiting_sync_mains.is_empty() {
            for request in waiting_sync_mains {
                self.run_sync_main(request).await;
            }

            return;
        }
        if let Some(viewed_context) = viewed_context
            && viewed_context.generation != generation
        {
            self.start_viewed_pass(viewed_context, None);
        }
    }

    /// Refreshes linked or published review-request state for all targets in
    /// the context snapshot.
    ///
    /// Emits one [`AppEvent::ReviewRequestStatusUpdated`] per target so the
    /// reducer can persist refreshed summaries and transition externally
    /// merged or closed sessions. Targets in failure backoff are skipped
    /// until their retry pass, and idle targets until their next idle poll
    /// unless `poll_idle_targets` is set. Returns the outcome of the
    /// `sync.review_requests` phase.
    async fn run_review_request_pass(
        &mut self,
        context: &SyncContext,
        poll_idle_targets: bool,
    ) -> Outcome {
        let updates = self
            .collect_review_request_pass_updates(context, true, poll_idle_targets)
            .await;
        let outcome = self.review_request_pass_outcome(context, true, &updates);

        self.emit_review_request_pass_updates(context, updates);

        outcome
    }

    /// Refreshes review-request status for all eligible targets in one
    /// `sync.review_requests` phase and returns reducer-ready updates without
    /// emitting them yet.
    ///
    /// Manual main sync uses this to defer merged/closed session transitions
    /// until after the local default branch has been updated successfully.
    async fn collect_review_request_pass_updates(
        &mut self,
        context: &SyncContext,
        reject_stale_context: bool,
        poll_idle_targets: bool,
    ) -> Vec<ReviewRequestPassUpdate> {
        let phase = Span::child("sync.review_requests");
        phase.attribute(
            "agentty.sync.targets",
            attribute_count(context.review_request_sync_targets.len()),
        );
        let (updates, idle_targets) = self
            .collect_review_request_updates(context, reject_stale_context, poll_idle_targets)
            .with_context(phase.context())
            .await;
        phase.attribute("agentty.sync.idle_targets", attribute_count(idle_targets));
        phase.attribute(
            "agentty.sync.failures",
            attribute_count(
                updates
                    .iter()
                    .filter(|update| update.result.is_err())
                    .count(),
            ),
        );
        phase.finish(self.review_request_pass_outcome(context, reject_stale_context, &updates));

        updates
    }

    /// Classifies one review-request pass: canceled when newer polling
    /// targets superseded it, failed when any target failed to refresh.
    fn review_request_pass_outcome(
        &self,
        context: &SyncContext,
        reject_stale_context: bool,
        updates: &[ReviewRequestPassUpdate],
    ) -> Outcome {
        if reject_stale_context && self.context_is_stale(context) {
            Outcome::Canceled
        } else if updates.iter().any(|update| update.result.is_err()) {
            Outcome::Failed
        } else {
            Outcome::Completed
        }
    }

    /// Refreshes review-request status for every eligible target, stopping
    /// early when `reject_stale_context` is set and newer targets arrive.
    /// A completed pass with `reject_stale_context` set, a polling pass,
    /// also forgets the cadence and backoff of targets it no longer holds.
    ///
    /// Returns the updates with the number of idle targets left for a later
    /// pass.
    async fn collect_review_request_updates(
        &mut self,
        context: &SyncContext,
        reject_stale_context: bool,
        poll_idle_targets: bool,
    ) -> (Vec<ReviewRequestPassUpdate>, usize) {
        let review_pass_index = self.review_pass_index;
        self.review_pass_index = self.review_pass_index.wrapping_add(1);
        let mut idle_targets = 0;
        let mut updates = Vec::new();
        let review_watch_config = if context
            .review_request_sync_targets
            .iter()
            .any(|target| target.is_watched)
        {
            ReviewWatchConfig::load(context.fs_client.as_ref(), &context.working_dir).await
        } else {
            Ok(ReviewWatchConfig::default())
        };

        for review_request_sync_target in &context.review_request_sync_targets {
            if reject_stale_context && self.context_is_stale(context) {
                return (updates, idle_targets);
            }
            if self.is_target_backed_off(&review_request_sync_target.session_id, review_pass_index)
            {
                continue;
            }
            if !poll_idle_targets
                && self.is_target_idle(review_request_sync_target, review_pass_index)
            {
                idle_targets += 1;

                continue;
            }

            let result = sync_review_request_status(
                review_request_sync_target,
                context.git_client.as_ref(),
                context.review_request_client.as_ref(),
                &review_watch_config,
            )
            .await;

            if reject_stale_context && self.context_is_stale(context) {
                return (updates, idle_targets);
            }

            self.record_review_sync_outcome(review_request_sync_target, &result, review_pass_index);
            updates.push(ReviewRequestPassUpdate {
                result,
                target: review_request_sync_target.clone(),
            });
        }

        // Only a polling pass, the one that rejects a stale context, holds
        // every polled target. A project sync may hold another project's
        // targets, so it keeps the polled targets' states.
        if reject_stale_context {
            self.retain_active_target_states(&context.review_request_sync_targets);
        }

        (updates, idle_targets)
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

    /// Starts one user-triggered main-branch sync.
    ///
    /// Review-request state is refreshed before the mutating git phases, but
    /// successful terminal updates are emitted only after the main branch
    /// sync succeeds. That keeps externally merged session finalization behind
    /// the updated local default branch for the manual `s` workflow.
    ///
    /// The Git phases, which can wait minutes on assisted conflict
    /// resolution, run in their own task so periodic passes and syncs of
    /// other projects continue. The project's background fetch pauses until
    /// that task reports back to [`Self::finish_sync_main`].
    ///
    /// Both phases and any conflict-assistance utilities nest beneath one
    /// independent `project.sync_main` trace, which fails with the sync.
    ///
    /// A sync of a project whose viewed pass is running, such as one the
    /// terminal just switched to, waits for that pass, so the pass's fetch
    /// cannot race the sync's pull and push.
    async fn run_sync_main(&mut self, request: SyncMainRequest) {
        if self
            .viewed_passes
            .is_running(request.sync_context.project_id)
        {
            self.viewed_passes.waiting_sync_mains.push(request);

            return;
        }

        let SyncMainRequest {
            app_event_tx,
            operation,
            session_model,
            sync_context,
        } = request;
        let trace = Span::root("project.sync_main", Vec::new());
        let review_request_updates = self
            .collect_review_request_pass_updates(&sync_context, false, true)
            .with_context(trace.context())
            .await;
        self.sync_main_tasks
            .running_project_ids
            .insert(sync_context.project_id);
        let finished_tx = self.sync_main_tasks.finished_tx.clone();

        spawn::spawn_detached(async move {
            let result = async {
                // Scoping inside the trace parents conflict assistance beneath
                // it.
                let run_client = ag_worker::scoped_client(
                    Arc::clone(&sync_context.run_client),
                    ag_worker::RunScope {
                        project_id: Some(sync_context.project_id),
                        parent_id: Some(format!(
                            "sync-{}-{}",
                            operation.project_id, operation.operation_id
                        )),
                        purpose: Some("sync conflict assistance".to_string()),
                        ..ag_worker::RunScope::default()
                    },
                );

                session::SessionManager::sync_main_for_project(
                    sync_context.project_branch_name.clone(),
                    sync_context.working_dir.clone(),
                    Some(SyncMainEventContext {
                        app_event_tx: app_event_tx.clone(),
                        operation: operation.clone(),
                    }),
                    Arc::clone(&sync_context.git_client),
                    session_model,
                    run_client,
                )
                .await
            }
            .with_context(trace.context())
            .await;
            trace.finish(if result.is_ok() {
                Outcome::Completed
            } else {
                Outcome::Failed
            });
            let review_request_updates = if result.is_ok() {
                review_request_updates
                    .into_iter()
                    .map(|update| SyncMainReviewUpdate {
                        result: update.result,
                        session_id: update.target.session_id,
                    })
                    .collect()
            } else {
                Vec::new()
            };

            // Fire-and-forget: the orchestrator and the app may stop at
            // shutdown. The orchestrator hears first so a sync requested in
            // reaction to this completion cannot be marked finished early.
            let _ = finished_tx.send(SyncMainFinished {
                project_id: sync_context.project_id,
                succeeded: result.is_ok(),
            });
            let _ = app_event_tx.send(AppEvent::SyncMainCompleted {
                completion: SyncMainCompletion {
                    operation,
                    result,
                    review_request_updates,
                },
            });
        });
    }

    /// Resumes the project's background fetch after its explicit sync.
    ///
    /// A successful sync shows the remote is reachable again, so it also
    /// clears the fetch backoff and the next status pass fetches at once.
    fn finish_sync_main(&mut self, finished: &SyncMainFinished) {
        self.sync_main_tasks
            .running_project_ids
            .remove(&finished.project_id);
        if finished.succeeded {
            self.fetch_failures.remove(&finished.project_id);
        }
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
            .is_some_and(|backoff| backoff.is_backed_off(review_pass_index))
    }

    /// Returns whether one unchanged target waits for its next idle poll.
    ///
    /// A changed polling key, such as a toggled watch or a newly linked
    /// review request, polls the target at once.
    fn is_target_idle(
        &self,
        review_request_sync_target: &ReviewRequestSyncTarget,
        review_pass_index: u64,
    ) -> bool {
        self.review_sync_cadences
            .get(&review_request_sync_target.session_id)
            .is_some_and(|cadence_state| {
                review_pass_index < cadence_state.next_poll_pass
                    && cadence_state.target.polling_key()
                        == review_request_sync_target.polling_key()
            })
    }

    /// Updates per-target failure and cadence tracking after one
    /// review-request sync.
    ///
    /// Successes clear the failure state and update the idle cadence.
    /// Failures restart the cadence, extend the backoff window exponentially,
    /// and surface one workflow notice when the consecutive failure count
    /// reaches the notice threshold, instead of retrying at full rate forever
    /// with no user-visible signal.
    fn record_review_sync_outcome(
        &mut self,
        review_request_sync_target: &ReviewRequestSyncTarget,
        result: &Result<SyncReviewRequestTaskResult, String>,
        review_pass_index: u64,
    ) {
        let session_id = &review_request_sync_target.session_id;
        match result {
            Ok(task_result) => {
                self.review_sync_failures.remove(session_id);
                self.record_review_sync_cadence(
                    review_request_sync_target,
                    task_result,
                    review_pass_index,
                );
            }
            Err(error) => {
                self.review_sync_cadences.remove(session_id);
                let backoff = self
                    .review_sync_failures
                    .entry(session_id.clone())
                    .or_default();
                backoff.record_failure(review_pass_index, REVIEW_SYNC_MAX_BACKOFF_PASSES);

                tracing::warn!(
                    session_id = %session_id,
                    consecutive_failures = backoff.consecutive_failures,
                    "review request sync failed: {error}",
                );
                if backoff.consecutive_failures == REVIEW_SYNC_FAILURE_NOTICE_THRESHOLD {
                    // Fire-and-forget: receiver may be dropped during
                    // shutdown.
                    let _ = self
                        .app_event_tx
                        .send(AppEvent::SessionWorkflowNoticeUpdated {
                            notice: format!(
                                "[Review request sync] {} consecutive failures; retrying with \
                                 backoff. Last error: {error}",
                                backoff.consecutive_failures
                            ),
                            session_id: session_id.clone(),
                        });
                }
            }
        }
    }

    /// Updates the idle cadence of one target after a successful sync.
    ///
    /// A result equal to the previous one for the same polling key extends
    /// the unchanged run. After [`REVIEW_SYNC_IDLE_AFTER_UNCHANGED_PASSES`]
    /// such passes the target is polled once every
    /// [`REVIEW_SYNC_IDLE_INTERVAL_PASSES`] passes. Any change, running checks
    /// on a watched review request, or a merge-queue entry keeps the full
    /// cadence.
    fn record_review_sync_cadence(
        &mut self,
        review_request_sync_target: &ReviewRequestSyncTarget,
        task_result: &SyncReviewRequestTaskResult,
        review_pass_index: u64,
    ) {
        let session_id = &review_request_sync_target.session_id;
        let unchanged_passes = self
            .review_sync_cadences
            .get(session_id)
            .filter(|cadence_state| {
                cadence_state.result == *task_result
                    && cadence_state.target.polling_key()
                        == review_request_sync_target.polling_key()
            })
            .map_or(0, |cadence_state| {
                cadence_state.unchanged_passes.saturating_add(1)
            });
        let skipped_passes = if unchanged_passes >= REVIEW_SYNC_IDLE_AFTER_UNCHANGED_PASSES
            && !has_imminent_change(task_result)
        {
            REVIEW_SYNC_IDLE_INTERVAL_PASSES - 1
        } else {
            0
        };

        self.review_sync_cadences.insert(
            session_id.clone(),
            ReviewSyncCadenceState {
                next_poll_pass: review_pass_index
                    .saturating_add(1)
                    .saturating_add(skipped_passes),
                result: task_result.clone(),
                target: review_request_sync_target.clone(),
                unchanged_passes,
            },
        );
    }

    /// Drops failure and cadence tracking for sessions no longer in the
    /// target list.
    fn retain_active_target_states(
        &mut self,
        review_request_sync_targets: &[ReviewRequestSyncTarget],
    ) {
        let is_active = |session_id: &SessionId| {
            review_request_sync_targets
                .iter()
                .any(|target| &target.session_id == session_id)
        };
        self.review_sync_cadences
            .retain(|session_id, _| is_active(session_id));
        self.review_sync_failures
            .retain(|session_id, _| is_active(session_id));
    }
}

/// Explicit project syncs whose Git work runs beside the orchestrator loop.
struct SyncMainTasks {
    /// Reports from finished sync tasks, read by the orchestrator loop.
    finished_rx: mpsc::UnboundedReceiver<SyncMainFinished>,
    /// Sender cloned into each spawned sync task.
    finished_tx: mpsc::UnboundedSender<SyncMainFinished>,
    /// Projects whose sync is still running, so their background fetch
    /// cannot race the sync's own pull and push.
    running_project_ids: HashSet<i64>,
}

impl Default for SyncMainTasks {
    fn default() -> Self {
        let (finished_tx, finished_rx) = mpsc::unbounded_channel();

        Self {
            finished_rx,
            finished_tx,
            running_project_ids: HashSet::new(),
        }
    }
}

/// Report one spawned project sync sends back to the orchestrator.
struct SyncMainFinished {
    project_id: i64,
    succeeded: bool,
}

/// Git-status passes of viewed projects, which run beside the orchestrator
/// loop so a slow or failing project never holds up the active project's
/// pass or the command queue.
struct ViewedPasses {
    /// Reports from finished passes, read by the orchestrator loop.
    finished_rx: mpsc::UnboundedReceiver<ViewedPassFinished>,
    /// Sender cloned into each spawned pass.
    finished_tx: mpsc::UnboundedSender<ViewedPassFinished>,
    /// Polling state of each viewed project polled since it became viewed,
    /// kept while it stays viewed or its pass runs.
    projects: HashMap<i64, ViewedProjectPoll>,
    /// Explicit syncs waiting for their project's running pass.
    waiting_sync_mains: Vec<SyncMainRequest>,
}

impl ViewedPasses {
    /// Returns whether a pass of `project_id` is running.
    fn is_running(&self, project_id: i64) -> bool {
        self.projects
            .get(&project_id)
            .is_some_and(|project_poll| project_poll.is_running)
    }

    /// Forgets the polling state of every project that `viewed_contexts`
    /// no longer holds, unless its pass still runs.
    fn retain_viewed(&mut self, viewed_contexts: &[SyncContext]) {
        self.projects.retain(|project_id, project_poll| {
            project_poll.is_running
                || viewed_contexts
                    .iter()
                    .any(|context| context.project_id == *project_id)
        });
    }

    /// Removes and returns the explicit syncs of `project_id` that wait for
    /// its pass, in request order.
    fn take_waiting_sync_mains(&mut self, project_id: i64) -> Vec<SyncMainRequest> {
        let (ready, waiting) = std::mem::take(&mut self.waiting_sync_mains)
            .into_iter()
            .partition(|request| request.sync_context.project_id == project_id);
        self.waiting_sync_mains = waiting;

        ready
    }
}

impl Default for ViewedPasses {
    fn default() -> Self {
        let (finished_tx, finished_rx) = mpsc::unbounded_channel();

        Self {
            finished_rx,
            finished_tx,
            projects: HashMap::new(),
            waiting_sync_mains: Vec::new(),
        }
    }
}

/// Polling state of one viewed project between its passes.
#[derive(Default)]
struct ViewedProjectPoll {
    /// Base comparisons memoized for the project's repository, moved into
    /// its pass while one runs.
    base_comparison_cache: BaseComparisonCache,
    /// Whether a pass of the project is running.
    is_running: bool,
}

/// One viewed project's git-status pass, run in its own task.
struct ViewedPass {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    base_comparison_cache: BaseComparisonCache,
    /// Polling context the pass reads.
    context: SyncContext,
    /// Fetch pass the pass counts as for the project's fetch backoff, read
    /// only when it fetches.
    fetch_pass_index: u64,
    fetch_plan: FetchPlan,
    finished_tx: mpsc::UnboundedSender<ViewedPassFinished>,
    /// Published viewed contexts, read to tell whether newer inputs
    /// superseded the pass.
    viewed_context_rx: watch::Receiver<Vec<SyncContext>>,
}

impl ViewedPass {
    /// Runs the pass as one independent `sync.refresh` trace marked with
    /// `agentty.sync.viewed_project`, emits the project's snapshot unless
    /// newer polling inputs superseded the pass, and reports back to the
    /// orchestrator.
    ///
    /// Phases, failures, and cancellation follow the active project's pass;
    /// review requests are not refreshed.
    async fn run(self) {
        let Self {
            app_event_tx,
            mut base_comparison_cache,
            context,
            fetch_pass_index,
            fetch_plan,
            finished_tx,
            viewed_context_rx,
        } = self;
        let refresh = Span::root(
            "sync.refresh",
            vec![
                KeyValue::new("agentty.sync.review_pass", false),
                KeyValue::new("agentty.sync.viewed_project", true),
            ],
        );
        let (outcome, fetch_succeeded) = async {
            let repo_root = repo_root(&context).await;
            let (fetch_outcome, fetch_succeeded) =
                run_fetch_plan(&context, &repo_root, fetch_plan).await;
            let (status, session_statuses, status_outcome) =
                branch_status_snapshot(&context, &repo_root, &mut base_comparison_cache).await;
            let outcome = combined_outcome(fetch_outcome, status_outcome);
            let is_superseded = viewed_context_rx
                .borrow()
                .iter()
                .find(|published| published.project_id == context.project_id)
                .is_none_or(|published| published.generation != context.generation);
            if is_superseded {
                return (
                    combined_outcome(outcome, Outcome::Canceled),
                    fetch_succeeded,
                );
            }

            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(AppEvent::GitStatusUpdated {
                generation: context.generation,
                project_id: context.project_id,
                session_statuses,
                status,
            });

            (outcome, fetch_succeeded)
        }
        .with_context(refresh.context())
        .await;
        refresh.finish(outcome);

        // Fire-and-forget: the orchestrator only stops at app shutdown.
        let _ = finished_tx.send(ViewedPassFinished {
            base_comparison_cache,
            fetch_pass_index,
            fetch_succeeded,
            generation: context.generation,
            project_id: context.project_id,
        });
    }
}

/// Report one viewed project's pass sends back to the orchestrator.
struct ViewedPassFinished {
    /// Base comparisons the pass memoized, kept for the project's next pass.
    base_comparison_cache: BaseComparisonCache,
    /// Fetch pass the pass counted as.
    fetch_pass_index: u64,
    /// Whether the pass's fetch succeeded, or `None` when it did not fetch.
    fetch_succeeded: Option<bool>,
    /// Generation of the context the pass read, which tells the
    /// orchestrator whether newer polling inputs superseded it.
    generation: u64,
    project_id: i64,
}

/// What one pass does about its project's remote before reading branch
/// statuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FetchPlan {
    /// Fetches after `consecutive_failures` failed fetches in a row.
    Fetch { consecutive_failures: u32 },
    /// Skips the fetch while the project's explicit sync runs.
    DeferredForSync,
    /// Skips the fetch while another pass of the project runs.
    DeferredForPass,
    /// Skips the fetch while repeated failures keep the project in backoff.
    BackedOff,
    /// Skips the fetch because the pass follows a requested refresh or
    /// reruns a superseded pass, comparing against the refs fetched last.
    Reuse,
}

/// Review-request work included in one read-only refresh pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReviewPass {
    /// Refreshes git status only.
    Skip,
    /// Periodic pass that leaves idle targets until their next idle poll.
    Scheduled,
    /// Explicit refresh that also polls idle targets.
    Forced,
}

impl ReviewPass {
    /// Returns the review-request work of periodic tick `tick_index`, which
    /// polls forge CLIs on every [`REVIEW_REQUEST_PASS_TICKS`]th tick.
    fn for_tick(tick_index: u64) -> Self {
        if tick_index.is_multiple_of(REVIEW_REQUEST_PASS_TICKS) {
            Self::Scheduled
        } else {
            Self::Skip
        }
    }
}

/// Per-session unchanged-result tracking used for the idle cadence.
struct ReviewSyncCadenceState {
    /// Review-pass index from which the target is polled again.
    next_poll_pass: u64,
    /// Latest successful result, compared with the next one.
    result: SyncReviewRequestTaskResult,
    /// Target that produced `result`; a different polling key ends the idle
    /// cadence.
    target: ReviewRequestSyncTarget,
    /// Consecutive passes whose result matched the one before.
    unchanged_passes: u32,
}

/// Consecutive-failure tracking for one polled target, used to skip passes
/// between retries.
///
/// Review-request targets count review passes and project fetches count
/// fetch passes.
#[derive(Default)]
struct FailureBackoff {
    /// Number of failures since the last success.
    consecutive_failures: u32,
    /// Pass index at which the target may be retried.
    retry_after_pass: u64,
}

impl FailureBackoff {
    /// Returns whether `pass_index` is still inside the backoff window.
    fn is_backed_off(&self, pass_index: u64) -> bool {
        pass_index < self.retry_after_pass
    }

    /// Records one failure during `pass_index` and extends the backoff
    /// window by [`Self::skipped_passes`].
    fn record_failure(&mut self, pass_index: u64, max_skipped_passes: u64) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.retry_after_pass = pass_index
            .saturating_add(1)
            .saturating_add(Self::skipped_passes(
                self.consecutive_failures,
                max_skipped_passes,
            ));
    }

    /// Returns how many passes a failing target is skipped before retry.
    ///
    /// Grows exponentially (`0, 1, 3, 7, ...`) with the consecutive failure
    /// count and is capped at `max_skipped_passes`.
    fn skipped_passes(consecutive_failures: u32, max_skipped_passes: u64) -> u64 {
        let exponent = consecutive_failures.saturating_sub(1).min(u64::BITS - 1);

        ((1_u64 << exponent) - 1).min(max_skipped_passes)
    }
}

/// One review-request refresh result and its polling target.
struct ReviewRequestPassUpdate {
    /// Result emitted to the reducer for review-request persistence and
    /// terminal state transitions.
    result: Result<SyncReviewRequestTaskResult, String>,
    /// Polling target that produced this result.
    target: ReviewRequestSyncTarget,
}

/// Returns the repository root of the context's project, or its working
/// directory when Git finds none.
async fn repo_root(context: &SyncContext) -> PathBuf {
    let working_dir = context.working_dir.clone();

    context
        .git_client
        .find_git_repo_root(working_dir.clone())
        .await
        .unwrap_or(working_dir)
}

/// Carries out `fetch_plan` for the project at `repo_root` and returns the
/// fetch outcome with whether a fetch ran and succeeded.
///
/// A skipped fetch starts no phase and completes; one skipped for the
/// project's sync, another pass of the project, or backoff marks the
/// enclosing trace with `agentty.sync.fetch_deferred_for_sync`,
/// `agentty.sync.fetch_deferred_for_pass`, or
/// `agentty.sync.fetch_backed_off`. A failed fetch ends its `git.fetch`
/// phase with the bounded [`ag_git::GitError::failure_class`] as
/// `error.type` plus the consecutive failure count it reaches.
async fn run_fetch_plan(
    context: &SyncContext,
    repo_root: &Path,
    fetch_plan: FetchPlan,
) -> (Outcome, Option<bool>) {
    let consecutive_failures = match fetch_plan {
        FetchPlan::Fetch {
            consecutive_failures,
        } => consecutive_failures,
        FetchPlan::DeferredForSync => {
            ag_telemetry::current_attribute("agentty.sync.fetch_deferred_for_sync", true);

            return (Outcome::Completed, None);
        }
        FetchPlan::DeferredForPass => {
            ag_telemetry::current_attribute("agentty.sync.fetch_deferred_for_pass", true);

            return (Outcome::Completed, None);
        }
        FetchPlan::BackedOff => {
            ag_telemetry::current_attribute("agentty.sync.fetch_backed_off", true);

            return (Outcome::Completed, None);
        }
        FetchPlan::Reuse => return (Outcome::Completed, None),
    };

    let fetch = Span::child("git.fetch");
    let result = context
        .git_client
        .fetch_remote(repo_root.to_path_buf())
        .with_context(fetch.context())
        .await;
    let Err(error) = result else {
        fetch.finish(Outcome::Completed);

        return (Outcome::Completed, Some(true));
    };

    let consecutive_failures = consecutive_failures.saturating_add(1);
    let failure_class = error.failure_class();
    tracing::warn!(
        project_id = context.project_id,
        consecutive_failures,
        failure_class,
        "background git fetch failed: {error}",
    );
    fetch.attribute(
        "agentty.sync.consecutive_failures",
        i64::from(consecutive_failures),
    );
    fetch.fail(failure_class);

    (Outcome::Failed, Some(false))
}

/// Reads the project branch's ahead/behind counts against its upstream and
/// each target session's git status in one `sync.branch_status` phase.
///
/// A failed tracking-status query fails the returned outcome but still
/// compares sessions by branch name. Session base comparisons whose commits
/// did not move since `base_comparison_cache` recorded them are reused
/// without spawning Git.
async fn branch_status_snapshot(
    context: &SyncContext,
    repo_root: &Path,
    base_comparison_cache: &mut BaseComparisonCache,
) -> (
    Option<(u32, u32)>,
    HashMap<SessionId, SessionGitStatus>,
    Outcome,
) {
    let branch_status = Span::child("sync.branch_status");
    branch_status.attribute(
        "agentty.sync.targets",
        attribute_count(context.session_git_status_targets.len()),
    );
    let (branch_tracking_statuses, session_statuses, status_outcome) = async {
        let (branch_tracking_statuses, status_outcome) = match context
            .git_client
            .branch_tracking_statuses(repo_root.to_path_buf())
            .await
        {
            Ok(branch_tracking_statuses) => (branch_tracking_statuses, Outcome::Completed),
            Err(_) => (HashMap::new(), Outcome::Failed),
        };
        let session_statuses = base_comparison_cache
            .session_git_statuses(
                &branch_tracking_statuses,
                repo_root,
                &context.session_git_status_targets,
                context.git_client.as_ref(),
            )
            .await;

        (branch_tracking_statuses, session_statuses, status_outcome)
    }
    .with_context(branch_status.context())
    .await;
    branch_status.finish(status_outcome);
    let project_branch_name = context.project_branch_name.as_deref().unwrap_or_default();
    let status = branch_tracking_statuses
        .get(project_branch_name)
        .and_then(|branch_status| branch_status.upstream_status);

    (status, session_statuses, status_outcome)
}

/// Returns the more severe of two phase outcomes: any failure fails the
/// pass, otherwise any cancellation cancels it.
fn combined_outcome(first: Outcome, second: Outcome) -> Outcome {
    match (first, second) {
        (Outcome::Failed, _) | (_, Outcome::Failed) => Outcome::Failed,
        (Outcome::Canceled, _) | (_, Outcome::Canceled) => Outcome::Canceled,
        (Outcome::Completed, Outcome::Completed) => Outcome::Completed,
    }
}

/// Converts one collection size to an integer span attribute.
fn attribute_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

/// Returns whether a review request is expected to change soon: a watched
/// one reported checks that are still running, whose results review watching
/// acts on once they finish, or one waits in the forge's merge queue, which
/// merges or removes it.
fn has_imminent_change(task_result: &SyncReviewRequestTaskResult) -> bool {
    let has_running_checks =
        matches!(&task_result.attention, Some(Ok(attention)) if attention.has_pending_checks);
    let is_in_merge_queue = task_result
        .summary
        .as_ref()
        .is_some_and(crate::domain::session::ReviewRequestSummary::is_github_pr_in_merge_queue);

    has_running_checks || is_in_merge_queue
}

/// Session base comparisons memoized by the commits they were computed from.
///
/// Ahead/behind counts and merge conflicts between two commits never change,
/// so a session whose branch tip and base tip did not move since the previous
/// pass reuses its comparison instead of spawning `git rev-list` and
/// `git merge-tree` again. Each pass keeps only the commit pairs it targeted,
/// and a different repository root starts from an empty cache.
#[derive(Default)]
struct BaseComparisonCache {
    /// Complete comparisons from the latest pass, keyed by commit pair.
    comparisons: HashMap<CommitPair, BaseComparison>,
    /// Repository root the cached comparisons were computed in.
    repo_root: PathBuf,
}

impl BaseComparisonCache {
    /// Resolves ahead/behind and merge-conflict snapshots for all tracked
    /// session branches, combining each branch's base comparison with any
    /// tracked-remote snapshot already available from the repo-wide status
    /// query.
    ///
    /// Branches whose tip and base tip both appear in
    /// `branch_tracking_statuses` are compared by commit and memoized;
    /// identical tips compare as even without Git. Other branches are compared
    /// by name on every pass.
    async fn session_git_statuses(
        &mut self,
        branch_tracking_statuses: &BranchTrackingMap,
        repo_root: &Path,
        session_git_status_targets: &[SessionGitStatusTarget],
        git_client: &dyn GitClient,
    ) -> HashMap<SessionId, SessionGitStatus> {
        let previous_comparisons = if self.repo_root == repo_root {
            std::mem::take(&mut self.comparisons)
        } else {
            HashMap::new()
        };
        let mut comparisons = HashMap::new();
        let mut session_git_statuses = HashMap::with_capacity(session_git_status_targets.len());

        for session_git_status_target in session_git_status_targets {
            let base_comparison =
                match CommitPair::for_target(branch_tracking_statuses, session_git_status_target) {
                    Some(commit_pair) => {
                        let cached_comparison = comparisons
                            .get(&commit_pair)
                            .or_else(|| previous_comparisons.get(&commit_pair))
                            .copied();
                        let base_comparison = match cached_comparison {
                            Some(base_comparison) => base_comparison,
                            None => commit_pair.compare(git_client, repo_root).await,
                        };
                        if base_comparison.is_complete() {
                            comparisons.insert(commit_pair, base_comparison);
                        }

                        base_comparison
                    }
                    None => {
                        BaseComparison::compute(
                            git_client,
                            repo_root,
                            &session_git_status_target.branch_name,
                            &session_git_status_target.base_branch,
                        )
                        .await
                    }
                };
            let remote_status = branch_tracking_statuses
                .get(&session_git_status_target.branch_name)
                .and_then(|branch_status| branch_status.upstream_status);
            session_git_statuses.insert(
                session_git_status_target.session_id.clone(),
                SessionGitStatus {
                    base_status: base_comparison.base_status,
                    has_merge_conflict: base_comparison.has_merge_conflict,
                    remote_status,
                },
            );
        }

        self.comparisons = comparisons;
        repo_root.clone_into(&mut self.repo_root);

        session_git_statuses
    }
}

/// Tip commits of one session branch and its base branch.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CommitPair {
    /// Commit the base branch points to.
    base_commit: String,
    /// Commit the session branch points to.
    head_commit: String,
}

impl CommitPair {
    /// Returns the tip commits of `target`'s branch and base branch, or
    /// `None` when either is not a local branch in `branch_tracking_statuses`.
    fn for_target(
        branch_tracking_statuses: &BranchTrackingMap,
        target: &SessionGitStatusTarget,
    ) -> Option<Self> {
        let branch_status = branch_tracking_statuses.get(&target.branch_name)?;
        let base_status = branch_tracking_statuses.get(&target.base_branch)?;

        Some(Self {
            base_commit: base_status.commit_hash.clone(),
            head_commit: branch_status.commit_hash.clone(),
        })
    }

    /// Compares the two commits, treating identical commits as even without
    /// running Git.
    async fn compare(&self, git_client: &dyn GitClient, repo_root: &Path) -> BaseComparison {
        if self.head_commit == self.base_commit {
            return BaseComparison {
                base_status: Some((0, 0)),
                has_merge_conflict: Some(false),
            };
        }

        BaseComparison::compute(git_client, repo_root, &self.head_commit, &self.base_commit).await
    }
}

/// Ahead/behind counts and merge-conflict state of one session branch
/// against its base, each `None` when Git could not compute it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BaseComparison {
    /// Commits the session branch is ahead of and behind its base.
    base_status: Option<(u32, u32)>,
    /// Whether merging the session branch into its base would conflict.
    has_merge_conflict: Option<bool>,
}

impl BaseComparison {
    /// Compares `head_ref` with `base_ref`, probing for merge conflicts only
    /// when the two have diverged.
    async fn compute(
        git_client: &dyn GitClient,
        repo_root: &Path,
        head_ref: &str,
        base_ref: &str,
    ) -> Self {
        let base_status = git_client
            .get_ref_ahead_behind(
                repo_root.to_path_buf(),
                head_ref.to_string(),
                base_ref.to_string(),
            )
            .await
            .ok();
        let has_merge_conflict = match base_status {
            Some((ahead, behind)) if ahead > 0 && behind > 0 => git_client
                .has_merge_conflicts(
                    repo_root.to_path_buf(),
                    head_ref.to_string(),
                    base_ref.to_string(),
                )
                .await
                .ok(),
            Some(_) => Some(false),
            None => None,
        };

        Self {
            base_status,
            has_merge_conflict,
        }
    }

    /// Returns whether every Git query succeeded, so the comparison can be
    /// reused while its commits stay unchanged. A conflict state exists only
    /// after a successful ahead/behind query.
    fn is_complete(self) -> bool {
        self.has_merge_conflict.is_some()
    }
}

/// Runs one review-request sync against the forge.
///
/// When the session has a linked review request, this refreshes it by display
/// id. Otherwise, when the branch was published, this searches for an
/// externally created review request by source branch name. An open linked
/// review request of a watched target also reports the forge facts that may
/// need automatic follow-up, trusting the review bots named by
/// `review_watch_config`. A failed lookup of those facts, or an invalid
/// configuration, is reported next to the refreshed summary instead of
/// failing the refresh.
async fn sync_review_request_status(
    target: &ReviewRequestSyncTarget,
    git_client: &dyn GitClient,
    review_request_client: &dyn ReviewRequestClient,
    review_watch_config: &Result<ReviewWatchConfig, String>,
) -> Result<SyncReviewRequestTaskResult, String> {
    let folder = target.folder.clone();
    let remote = review_request_remote(
        folder.clone(),
        git_client,
        target.linked_review_request.as_ref(),
        review_request_client,
    )
    .await?;

    if let Some(review_request) = &target.linked_review_request {
        let display_id = review_request.summary.display_id.clone();
        let refreshed_summary = review_request_client
            .refresh_review_request(remote.clone(), display_id.clone())
            .await
            .map_err(|error| error.detail_message())?;
        let session_head_hash =
            session_head_hash_for_summary(git_client, &folder, &refreshed_summary).await;
        let attention = if target.is_watched && refreshed_summary.state == ReviewRequestState::Open
        {
            Some(match review_watch_config {
                Ok(config) => review_request_client
                    .fetch_review_request_attention(remote, display_id, config.trusted_bots.clone())
                    .await
                    .map_err(|error| error.detail_message()),
                Err(error) => Err(error.clone()),
            })
        } else {
            None
        };
        let mut task_result = sync_task_result_from_summary(refreshed_summary, session_head_hash);
        task_result.attention = attention;

        return Ok(task_result);
    }

    let upstream_ref = target
        .published_upstream_ref
        .clone()
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
            attention: None,
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
pub(crate) async fn review_request_remote(
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
        attention: None,
        outcome,
        summary: Some(summary),
    }
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
