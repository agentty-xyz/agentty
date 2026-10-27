//! Shared app dependency container for managers and background workflows.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use ag_session::{AgentAvailabilityProbe, StaticAgentAvailabilityProbe};
use ag_worker::{RunClient, RunWorker, RuntimeConfig, SessionRunClient, SessionWorkerTask};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tracing::{debug, warn};

use crate::analytics::{Analytics, SessionType, TurnOutcome};
use crate::app::AppEvent;
use crate::app::prompt_intent::PromptImagePasteUpdate;
use crate::app::session::SessionManager;
use crate::db::AppRepositories;
use crate::domain::agent::{
    AgentCliInfo, AgentKind, AgentModel, AgentSelection, HarnessModelOption,
};
use crate::domain::harness::{HarnessAvailability, HarnessModelCatalog};
use crate::domain::session::SessionId;
use crate::infra::agent_profile::{AgentCatalogClient, RealAgentCatalogClient};
use crate::infra::clipboard_image::{ClipboardImageClient, RealClipboardImageClient};
use crate::infra::clock::Clock;
use crate::infra::fs::FsClient;

/// Shared app dependencies used by managers and background workflows.
#[derive(Clone)]
pub struct AppServices {
    pub(super) graph_drivers: Arc<Mutex<HashMap<String, crate::app::execution_graph::GraphDriver>>>,
    pub(super) graph_resources: Arc<ag_worker::GraphResources>,
    pub(super) graph_wake: tokio::sync::watch::Sender<u64>,
    analytics: Option<Analytics>,
    available_agent_clis: Arc<Mutex<Vec<AgentCliInfo>>>,
    available_agent_kinds: Arc<[AgentKind]>,
    base_path: PathBuf,
    cleanup_task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    clipboard_image_client: Arc<dyn ClipboardImageClient>,
    clock: Arc<dyn Clock>,
    creation_task_handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    fs_client: Arc<dyn FsClient>,
    git_client: Arc<dyn GitClient>,
    harness_model_probe: Arc<dyn AgentAvailabilityProbe>,
    harness_models: Arc<Mutex<HarnessModelCatalog>>,
    agent_profile_catalog_client: Arc<dyn AgentCatalogClient>,
    repositories: AppRepositories,
    review_request_client: Arc<dyn ReviewRequestClient>,
    run_client: Arc<dyn RunClient>,
    run_worker: Arc<RunWorker>,
    session_run_factory: Arc<dyn SessionRunFactory>,
    session_update_versions: SessionUpdateVersionMap,
    session_worker_tasks: Arc<Mutex<Vec<(String, SessionWorkerTask)>>>,
    telemetry_task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl AppServices {
    /// Creates a shared service container with versioned agent CLI
    /// availability captured at startup.
    pub(crate) fn new_with_agent_clis(
        base_path: PathBuf,
        clock: Arc<dyn Clock>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        deps: AppServiceDeps,
        available_agent_clis: Vec<AgentCliInfo>,
    ) -> Self {
        let AppServiceDeps {
            runtime_config,
            available_agent_kinds,
            clipboard_image_client_override,
            fs_client,
            git_client,
            run_client_override,
            agent_profile_catalog_client_override,
            repositories,
            review_request_client,
            session_run_factory,
        } = deps;
        let clipboard_image_client = clipboard_image_client_override.unwrap_or_else(|| {
            Arc::new(RealClipboardImageClient::new(
                Arc::clone(&clock),
                Arc::clone(&fs_client),
            ))
        });
        let run_worker = Arc::new(RunWorker::new(
            &runtime_config,
            repositories.runs(),
            Arc::new(ag_worker::HeartbeatClock),
            NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
        ));
        let run_client = run_client_override.unwrap_or_else(|| run_worker.clone());
        let agent_profile_catalog_client = agent_profile_catalog_client_override
            .unwrap_or_else(|| Arc::new(RealAgentCatalogClient));

        Self {
            graph_resources: Arc::new(ag_worker::GraphResources::new(
                NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN),
                HashMap::new(),
            )),
            graph_wake: tokio::sync::watch::channel(0).0,
            graph_drivers: Arc::default(),
            analytics: None,
            available_agent_clis: Arc::new(Mutex::new(available_agent_clis)),
            available_agent_kinds: Arc::<[AgentKind]>::from(available_agent_kinds),
            base_path,
            cleanup_task_handles: Arc::default(),
            creation_task_handles: Arc::default(),
            clipboard_image_client,
            clock,
            event_tx,
            fs_client,
            git_client,
            harness_model_probe: Arc::new(StaticAgentAvailabilityProbe {
                available_agent_kinds: Vec::new(),
            }),
            harness_models: Arc::default(),
            run_client,
            run_worker,
            session_run_factory,
            agent_profile_catalog_client,
            repositories,
            review_request_client,
            session_update_versions: Arc::default(),
            session_worker_tasks: Arc::default(),
            telemetry_task_handles: Arc::default(),
        }
    }

    /// Records the harness models this launch offers and the probe that
    /// refreshes them.
    pub(crate) fn set_harness_models(
        &mut self,
        catalog: HarnessModelCatalog,
        probe: Arc<dyn AgentAvailabilityProbe>,
    ) {
        self.harness_models = Arc::new(Mutex::new(catalog));
        self.harness_model_probe = probe;
    }

    /// Returns whether and on which model this launch offers Harness
    /// sessions.
    pub(crate) fn harness_availability(&self) -> HarnessAvailability {
        self.with_harness_models(|catalog| catalog.availability())
    }

    /// Returns the cached harness models in picker order.
    pub(crate) fn harness_model_options(&self) -> Vec<HarnessModelOption> {
        self.with_harness_models(|catalog| catalog.options().to_vec())
    }

    /// Replaces the cached harness models after a background refresh.
    pub(crate) fn replace_harness_model_options(&self, options: Vec<HarnessModelOption>) {
        self.with_harness_models(|catalog| catalog.replace_options(options));
    }

    /// Records the user's explicit harness model choice for new sessions.
    pub(crate) fn remember_harness_model(&self, model: AgentModel) {
        self.with_harness_models(|catalog| catalog.remember(model));
    }

    /// Lists the harness models again without blocking the caller and emits
    /// [`AppEvent::HarnessModelsUpdated`] when the listing completes. Launches
    /// without `--experimental-harness` skip the refresh.
    pub(crate) fn refresh_harness_models(&self) {
        if !self.with_harness_models(|catalog| catalog.is_enabled()) {
            return;
        }
        let probe = Arc::clone(&self.harness_model_probe);
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            let options = probe.native_harness_models().await;
            let _ = event_tx.send(AppEvent::HarnessModelsUpdated { options });
        });
    }

    /// Runs `action` on the shared harness catalog, which stays usable
    /// after a panic.
    fn with_harness_models<T>(&self, action: impl FnOnce(&mut HarnessModelCatalog) -> T) -> T {
        action(
            &mut self
                .harness_models
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Replaces the optional telemetry sender before session work starts.
    pub(crate) fn set_analytics(&mut self, analytics: Option<Analytics>) {
        self.analytics = analytics;
    }

    /// Reports a session reservation without delaying the lifecycle workflow.
    pub(crate) fn record_session_start(&self, session_type: SessionType, agent: AgentSelection) {
        if let Some(analytics) = self.analytics.clone() {
            self.track_telemetry_task(tokio::spawn(async move {
                analytics.record_session_start(session_type, agent).await;
            }));
        }
    }

    /// Reports an accepted turn without delaying foreground event reduction.
    pub(crate) fn record_turn_start(&self, agent: AgentSelection) {
        if let Some(analytics) = self.analytics.clone() {
            self.track_telemetry_task(tokio::spawn(async move {
                analytics.record_turn_start(agent).await;
            }));
        }
    }

    /// Reports a settled turn without delaying foreground event reduction.
    pub(crate) fn record_turn_end(&self, agent: AgentSelection, outcome: TurnOutcome) {
        if let Some(analytics) = self.analytics.clone() {
            self.track_telemetry_task(tokio::spawn(async move {
                analytics.record_turn_end(agent, outcome).await;
            }));
        }
    }

    /// Returns the session base path.
    pub(crate) fn base_path(&self) -> &Path {
        self.base_path.as_path()
    }

    /// Returns the cached locally runnable agent kinds.
    pub(crate) fn available_agent_kinds(&self) -> Vec<AgentKind> {
        self.available_agent_kinds.as_ref().to_vec()
    }

    /// Returns whether any agent CLI can run new sessions.
    ///
    /// Reads the startup-validated agent kinds that session defaults and
    /// `/model` use, not the refreshed CLI rows, which also list an `agy`
    /// too old to run.
    pub(crate) fn has_agent_cli(&self) -> bool {
        !self.available_agent_kinds.is_empty()
    }

    /// Returns the cached locally runnable agent CLIs and detected versions.
    pub(crate) fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        self.available_agent_clis
            .lock()
            .map_or_default(|agent_clis| agent_clis.clone())
    }

    /// Replaces the cached CLI rows after background version detection
    /// completes.
    pub(crate) fn replace_available_agent_clis(&self, available_agent_clis: Vec<AgentCliInfo>) {
        if let Ok(mut agent_clis) = self.available_agent_clis.lock() {
            *agent_clis = available_agent_clis;
        }
    }

    /// Returns the application repository bundle.
    pub(crate) fn db(&self) -> &AppRepositories {
        &self.repositories
    }

    /// Returns the shared wall-clock used by session workflows.
    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.clock)
    }

    /// Returns the shared clipboard-image client for pasted image capture.
    pub(crate) fn clipboard_image_client(&self) -> Arc<dyn ClipboardImageClient> {
        Arc::clone(&self.clipboard_image_client)
    }

    /// Obtains a worker execution handle without accessing its runtime adapter.
    pub(crate) fn session_run(&self, session_id: &SessionId, kind: AgentKind) -> SessionRunClient {
        self.session_run_factory.create(session_id, kind)
    }

    /// Captures task runtime composition for serialized execution.
    pub(crate) fn task_run_factory(&self) -> Arc<dyn SessionRunFactory> {
        Arc::clone(&self.session_run_factory)
    }

    /// Stops detached and nested utility runs owned by a canceled session.
    pub(crate) fn cancel_agent_runs(&self, session_id: &str) {
        self.stop_graph_drivers(Some(session_id));
        self.run_worker.cancel_session(session_id);
        let worker = Arc::clone(&self.run_worker);
        let session_id = session_id.to_string();
        self.track_cleanup_task(tokio::spawn(async move {
            worker.cancel_session_and_wait(&session_id).await;
        }));
    }

    /// Stops the owning serial worker and utilities before removing resources.
    pub(crate) async fn finish_session_agent_runs(&self, session_id: &str) {
        self.stop_graph_drivers(Some(session_id));
        let tasks: Vec<_> = self
            .session_worker_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(owner, _)| owner == session_id)
            .map(|(_, task)| task.clone())
            .collect();
        for task in &tasks {
            task.request_shutdown();
        }
        for task in tasks {
            task.shutdown().await;
        }
        self.run_worker.cancel_session_and_wait(session_id).await;
    }

    /// Returns the worker submission handle for isolated agent runs.
    pub(crate) fn run_client(&self) -> Arc<dyn RunClient> {
        Arc::clone(&self.run_client)
    }

    pub(super) fn graph_run_worker(&self) -> Arc<RunWorker> {
        Arc::clone(&self.run_worker)
    }

    /// Captures session ownership before handing work to a background task.
    pub(crate) fn session_run_client(&self, session_id: &str) -> Arc<dyn RunClient> {
        ag_worker::scoped_client(
            self.run_client(),
            ag_worker::RunScope {
                session_id: Some(session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        )
    }

    /// Returns the workspace agent discovery client.
    pub(crate) fn agent_profile_catalog_client(&self) -> Arc<dyn AgentCatalogClient> {
        Arc::clone(&self.agent_profile_catalog_client)
    }

    /// Enqueues an app event onto the internal event bus with debug
    /// instrumentation for producer-side event volume.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        let event_label = Self::app_event_label(&event);
        debug!(
            event = event_label,
            "enqueueing app event through app services"
        );

        // Fire-and-forget: receiver may be dropped during shutdown.
        if self.event_tx.send(event).is_err() {
            warn!(
                event = event_label,
                "failed to send app event because the receiver is closed"
            );
        }
    }

    /// Enqueues refresh events for workflows that changed both session
    /// snapshots and project-level session aggregates.
    pub(crate) fn emit_session_and_project_refresh_events(&self) {
        self.emit_app_event(AppEvent::RefreshSessions);
        self.emit_app_event(AppEvent::RefreshProjects);
    }

    /// Tracks worktree creation, which must settle before cleanup because its
    /// blocking Git operation cannot safely be canceled midway through setup.
    pub(crate) fn track_session_creation_task(
        &self,
        request_id: String,
        join_handle: JoinHandle<()>,
    ) {
        self.creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request_id, join_handle);
    }

    /// Releases a completed request's handle before acknowledging its result.
    /// Completion is emitted after external work, so joining only settles the
    /// task's return. Other requests remain tracked for shutdown.
    pub(crate) async fn finish_session_creation_task(&self, request_id: &str) {
        let task = self
            .creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(request_id);
        if let Some(task) = task
            && let Err(error) = task.await
        {
            warn!(error = %error, "session creation task failed during completion");
        }
    }

    /// Tracks one best-effort cleanup task that should complete before the app
    /// finishes graceful shutdown, reaping already completed tasks.
    pub(crate) fn track_cleanup_task(&self, join_handle: JoinHandle<()>) {
        if let Ok(mut cleanup_task_handles) = self.cleanup_task_handles.lock() {
            cleanup_task_handles.retain(|task| !task.is_finished());
            cleanup_task_handles.push(join_handle);
        }
    }

    /// Retains shutdown ownership even after a session mailbox is removed.
    pub(crate) fn track_session_worker(&self, session_id: &str, task: SessionWorkerTask) {
        let mut tasks = self
            .session_worker_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tasks.retain(|(_, task)| !task.is_finished());
        tasks.push((session_id.to_string(), task));
    }

    /// Settles worker execution, worktree creation, cleanup, and telemetry
    /// tasks within one deadline. Escalates worker shutdown when grace expires.
    /// When supplied, pending app events are drained for turn telemetry after
    /// background work settles, before waiting for telemetry delivery.
    /// Clipboard images captured but never applied to a composer are removed.
    /// Other events are discarded so shutdown cannot start new workflow work.
    ///
    /// The task list is drained before awaiting so the synchronous mutex guard
    /// is never held across an `.await`. The loop repeats in case a cleanup
    /// task registers additional cleanup work before it exits. Cleanup tasks
    /// share one shutdown deadline; unfinished tasks are canceled after it
    /// expires.
    pub(crate) async fn wait_for_cleanup_tasks(
        &self,
        pending_events: Option<&mut mpsc::UnboundedReceiver<AppEvent>>,
    ) {
        self.stop_graph_drivers(None);
        let deadline = Instant::from_std(self.clock.now_instant()) + CLEANUP_TASK_SHUTDOWN_TIMEOUT;
        let session_tasks = std::mem::take(
            &mut *self
                .session_worker_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (_, task) in &session_tasks {
            task.request_shutdown();
        }
        for (_, task) in session_tasks {
            if time::timeout_at(deadline, task.clone().shutdown())
                .await
                .is_err()
            {
                task.force_shutdown().await;
                warn!("session worker exceeded the shutdown deadline; dropping stuck execution");
            }
        }
        if time::timeout_at(deadline, self.run_worker.shutdown())
            .await
            .is_err()
        {
            self.run_worker.force_shutdown();
            warn!("agent run worker exceeded the shutdown deadline; forcing runtime shutdown");
        }
        let creation_tasks = self
            .creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        Self::wait_for_cleanup_task_handles(&Mutex::new(creation_tasks), deadline).await;

        Self::wait_for_cleanup_task_handles(self.cleanup_task_handles.as_ref(), deadline).await;

        if let Some(event_rx) = pending_events {
            let mut unapplied_image_paths = Vec::new();
            while let Ok(event) = event_rx.try_recv() {
                match event {
                    AppEvent::SessionTurnStarted { agent, .. } => self.record_turn_start(agent),
                    AppEvent::SessionTurnEnded { agent, outcome } => {
                        self.record_turn_end(agent, outcome);
                    }
                    AppEvent::PromptImagePasted {
                        update:
                            PromptImagePasteUpdate {
                                result: Ok(local_image_path),
                                ..
                            },
                    } => unapplied_image_paths.push(local_image_path),
                    _ => {}
                }
            }
            SessionManager::cleanup_prompt_attachment_paths(
                self.fs_client(),
                unapplied_image_paths,
            )
            .await;
        }

        Self::wait_for_cleanup_task_handles(self.telemetry_task_handles.as_ref(), deadline).await;
    }

    /// Returns a clone of the app event sender.
    pub(crate) fn event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.event_tx.clone()
    }

    /// Returns the shared filesystem client for async filesystem operations.
    pub(crate) fn fs_client(&self) -> Arc<dyn FsClient> {
        Arc::clone(&self.fs_client)
    }

    /// Returns the shared git client for async git operations.
    pub(crate) fn git_client(&self) -> Arc<dyn GitClient> {
        Arc::clone(&self.git_client)
    }

    /// Returns the shared forge review-request client.
    pub(crate) fn review_request_client(&self) -> Arc<dyn ReviewRequestClient> {
        Arc::clone(&self.review_request_client)
    }

    /// Returns the shared per-app session update version counters.
    pub(crate) fn session_update_versions(&self) -> SessionUpdateVersionMap {
        Arc::clone(&self.session_update_versions)
    }

    /// Reaps completed telemetry before retaining new work for graceful
    /// shutdown.
    fn track_telemetry_task(&self, join_handle: JoinHandle<()>) {
        if let Ok(mut telemetry_task_handles) = self.telemetry_task_handles.lock() {
            telemetry_task_handles.retain(|task| !task.is_finished());
            telemetry_task_handles.push(join_handle);
        }
    }

    /// Returns a stable instrumentation label for one app event variant.
    fn app_event_label(event: &AppEvent) -> &'static str {
        match event {
            AppEvent::TaskChainGraphLoaded { .. } => "TaskChainGraphLoaded",
            AppEvent::ProjectDiscoveryCompleted { .. } => "ProjectDiscoveryCompleted",
            AppEvent::SessionCreationCompleted { .. } => "SessionCreationCompleted",
            AppEvent::AtMentionEntriesLoaded { .. } => "AtMentionEntriesLoaded",
            AppEvent::PromptImagePasted { .. } => "PromptImagePasted",
            AppEvent::DiffPreviewLoaded { .. } => "DiffPreviewLoaded",
            AppEvent::SessionDiffLoaded { .. } => "SessionDiffLoaded",
            AppEvent::GitStatusUpdated { .. } => "GitStatusUpdated",
            AppEvent::VersionAvailabilityUpdated { .. } => "VersionAvailabilityUpdated",
            AppEvent::AgentCliVersionsUpdated { .. } => "AgentCliVersionsUpdated",
            AppEvent::HarnessModelsUpdated { .. } => "HarnessModelsUpdated",
            AppEvent::UpdateStatusChanged { .. } => "UpdateStatusChanged",
            AppEvent::SessionModelUpdated { .. } => "SessionModelUpdated",
            AppEvent::SessionPermissionModeUpdated { .. } => "SessionPermissionModeUpdated",
            AppEvent::SessionReasoningLevelUpdated { .. } => "SessionReasoningLevelUpdated",
            AppEvent::SessionResponseStyleUpdated { .. } => "SessionResponseStyleUpdated",
            AppEvent::SessionSpeedModeUpdated { .. } => "SessionSpeedModeUpdated",
            AppEvent::RefreshSessions => "RefreshSessions",
            AppEvent::RefreshProjects => "RefreshProjects",
            AppEvent::RefreshGitStatus => "RefreshGitStatus",
            AppEvent::SessionReviewCommentSnapshotLoaded { .. } => {
                "SessionReviewCommentSnapshotLoaded"
            }
            AppEvent::SessionActivityReady { .. } => "SessionActivityReady",
            AppEvent::SessionActivityUpdated { .. } => "SessionActivityUpdated",
            AppEvent::SessionProgressUpdated { .. } => "SessionProgressUpdated",
            AppEvent::SyncMainCompleted { .. } => "SyncMainCompleted",
            AppEvent::SyncMainConflictResolutionStarted { .. } => {
                "SyncMainConflictResolutionStarted"
            }
            AppEvent::SessionDiffStatsUpdated { .. } => "SessionDiffStatsUpdated",
            AppEvent::SessionTitleGenerationFinished { .. } => "SessionTitleGenerationFinished",
            AppEvent::BranchPublishActionCompleted { .. } => "BranchPublishActionCompleted",
            AppEvent::BranchPublishActionResolved { .. } => "BranchPublishActionResolved",
            AppEvent::BranchPublishActionStarted { .. } => "BranchPublishActionStarted",
            AppEvent::SessionQueuedSyncResolved { .. } => "SessionQueuedSyncResolved",
            AppEvent::SessionRebaseReviewInvalidated { .. } => "SessionRebaseReviewInvalidated",
            AppEvent::SessionReviewCommentResolutionUpdated { .. } => {
                "SessionReviewCommentResolutionUpdated"
            }
            AppEvent::SessionTurnStarted { .. } => "SessionTurnStarted",
            AppEvent::SessionTurnEnded { .. } => "SessionTurnEnded",
            AppEvent::ReviewPrepared { .. } => "ReviewPrepared",
            AppEvent::ReviewProgressUpdated { .. } => "ReviewProgressUpdated",
            AppEvent::ReviewPreparationFailed { .. } => "ReviewPreparationFailed",
            AppEvent::DeferredAutoReviewPersistenceRetry { .. } => {
                "DeferredAutoReviewPersistenceRetry"
            }
            AppEvent::FocusedReviewPersistenceRetry { .. } => "FocusedReviewPersistenceRetry",
            AppEvent::SessionUpdated { .. } => "SessionUpdated",
            AppEvent::AgentResponseReceived { .. } => "AgentResponseReceived",
            AppEvent::ImplementerAttemptSucceeded { .. } => "ImplementerAttemptSucceeded",
            AppEvent::StackedParentTurnCompleted { .. } => "StackedParentTurnCompleted",
            AppEvent::StackedParentSyncCompleted { .. } => "StackedParentSyncCompleted",
            AppEvent::StackedParentMergeCompleted { .. } => "StackedParentMergeCompleted",
            AppEvent::SessionWorkflowNoticeUpdated { .. } => "SessionWorkflowNoticeUpdated",
            AppEvent::PublishedBranchSyncUpdated { .. } => "PublishedBranchSyncUpdated",
            AppEvent::ReviewRequestStatusUpdated { .. } => "ReviewRequestStatusUpdated",
        }
    }

    /// Waits for tracked cleanup tasks until one shared deadline, then cancels
    /// every unfinished task so terminal shutdown can continue.
    async fn wait_for_cleanup_task_handles(
        cleanup_task_handles: &Mutex<Vec<JoinHandle<()>>>,
        deadline: Instant,
    ) {
        loop {
            let task_handles = cleanup_task_handles
                .lock()
                .map_or_default(|mut task_handles| task_handles.drain(..).collect::<Vec<_>>());

            if task_handles.is_empty() {
                break;
            }

            for mut task_handle in task_handles {
                match time::timeout_at(deadline, &mut task_handle).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        warn!(
                            error = %error,
                            "background cleanup task failed during shutdown"
                        );
                    }
                    Err(_) => {
                        task_handle.abort();
                        warn!(
                            "background cleanup task exceeded the shutdown deadline and was \
                             canceled"
                        );

                        if let Err(error) = task_handle.await
                            && !error.is_cancelled()
                        {
                            warn!(
                                error = %error,
                                "background cleanup task failed while being canceled"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Shared per-app session redraw version counters keyed by session id.
pub(crate) type SessionUpdateVersionMap = Arc<Mutex<HashMap<SessionId, u64>>>;

/// External clients and cached machine-scoped availability injected into
/// [`AppServices`].
pub(crate) struct AppServiceDeps {
    /// Cached locally runnable backends used to scope model selection.
    pub(crate) available_agent_kinds: Vec<AgentKind>,
    /// Optional clipboard image client override used by tests and injected
    /// environments.
    pub(crate) clipboard_image_client_override: Option<Arc<dyn ClipboardImageClient>>,
    /// Shared filesystem client for async filesystem operations.
    pub(crate) fs_client: Arc<dyn FsClient>,
    /// Shared git client for async git operations.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// Optional workspace agent catalog override used by tests.
    pub(crate) agent_profile_catalog_client_override: Option<Arc<dyn AgentCatalogClient>>,
    /// Shared repository bundle used by app workflows.
    pub(crate) repositories: AppRepositories,
    /// Shared forge review-request client.
    pub(crate) review_request_client: Arc<dyn ReviewRequestClient>,
    /// Optional isolated-prompt client override used by tests and injected
    /// environments.
    pub(crate) run_client_override: Option<Arc<dyn RunClient>>,
    /// Worker configuration shared by utility runtime composition.
    pub(crate) runtime_config: RuntimeConfig,
    /// Supplies an execution client whenever a session worker is created.
    pub(crate) session_run_factory: Arc<dyn SessionRunFactory>,
}

/// Supplies worker execution handles without exposing runtime adapters.
///
/// The caller invokes this only for a new worker. That worker owns channel
/// reuse, cancellation, and shutdown; per-turn model settings remain in its
/// requests.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait SessionRunFactory: Send + Sync {
    /// Uses the durable graph claim's host UUID for isolated runtime resources.
    fn create_graph_task(
        &self,
        context_id: &str,
        session_id: &SessionId,
        kind: AgentKind,
    ) -> Result<SessionRunClient, ag_contracts::AgentError>;
    /// Creates a worker client for the session and selected provider.
    fn create(&self, session_id: &SessionId, kind: AgentKind) -> SessionRunClient;

    /// Creates a task-owned runtime retaining the owner session's admission.
    fn create_task(
        &self,
        context: &ag_session::TaskAgentContext,
    ) -> Result<SessionRunClient, ag_contracts::AgentError>;
}

/// Production worker configuration for session runtime composition.
pub(crate) struct RealSessionRunFactory {
    config: RuntimeConfig,
}

impl RealSessionRunFactory {
    /// Captures worker configuration without starting a provider process.
    pub(crate) fn new(config: RuntimeConfig) -> Self {
        Self { config }
    }
}

impl SessionRunFactory for RealSessionRunFactory {
    fn create_graph_task(
        &self,
        context_id: &str,
        session_id: &SessionId,
        kind: AgentKind,
    ) -> Result<SessionRunClient, ag_contracts::AgentError> {
        SessionRunClient::for_isolated_context(context_id, session_id, kind, &self.config)
    }

    fn create(&self, session_id: &SessionId, kind: AgentKind) -> SessionRunClient {
        SessionRunClient::new(session_id.to_string(), kind, &self.config)
    }

    fn create_task(
        &self,
        context: &ag_session::TaskAgentContext,
    ) -> Result<SessionRunClient, ag_contracts::AgentError> {
        SessionRunClient::for_task_context(context, &self.config)
    }
}

/// Maximum graceful-shutdown wait shared by all background cleanup tasks.
const CLEANUP_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;

#[cfg(test)]
#[path = "service_channel_test.rs"]
mod channel_tests;

#[cfg(test)]
#[path = "service_test_support_test.rs"]
pub(crate) mod test_support;
