//! Shared app dependency container for managers and background workflows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent::{AppServerClient, OneShotClient, RealOneShotClient};
use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use ag_orchestration::{OrchestrationEvent, OrchestrationEventSink};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant};
use tracing::{debug, warn};

use crate::app::AppEvent;
use crate::db::AppRepositories;
use crate::domain::agent::{AgentCliInfo, AgentKind};
use crate::domain::session::SessionId;
use crate::infra::clipboard_image::{ClipboardImageClient, RealClipboardImageClient};
use crate::infra::clock::Clock;
use crate::infra::fs::FsClient;
use crate::infra::personality::{PersonalityCatalogClient, RealPersonalityCatalogClient};

/// Shared per-app session redraw version counters keyed by session id.
pub(crate) type SessionUpdateVersionMap = Arc<Mutex<HashMap<SessionId, u64>>>;

/// Maximum graceful-shutdown wait shared by all background cleanup tasks.
const CLEANUP_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// External clients and cached machine-scoped availability injected into
/// [`AppServices`].
pub(crate) struct AppServiceDeps {
    /// Shared provider-owned app-server client override used by tests and
    /// injected environments.
    pub(crate) app_server_client_override: Option<Arc<dyn AppServerClient>>,
    /// Cached locally runnable backends used to scope model selection.
    pub(crate) available_agent_kinds: Vec<AgentKind>,
    /// Optional clipboard image client override used by tests and injected
    /// environments.
    pub(crate) clipboard_image_client_override: Option<Arc<dyn ClipboardImageClient>>,
    /// Shared filesystem client for async filesystem operations.
    pub(crate) fs_client: Arc<dyn FsClient>,
    /// Shared git client for async git operations.
    pub(crate) git_client: Arc<dyn GitClient>,
    /// Optional isolated-prompt client override used by tests and injected
    /// environments.
    pub(crate) one_shot_client_override: Option<Arc<dyn OneShotClient>>,
    /// Optional workspace personality catalog override used by tests.
    pub(crate) personality_catalog_client_override: Option<Arc<dyn PersonalityCatalogClient>>,
    /// Shared repository bundle used by app workflows.
    pub(crate) repositories: AppRepositories,
    /// Shared forge review-request client.
    pub(crate) review_request_client: Arc<dyn ReviewRequestClient>,
}

/// Shared app dependencies used by managers and background workflows.
#[derive(Clone)]
pub struct AppServices {
    app_server_client_override: Option<Arc<dyn AppServerClient>>,
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
    one_shot_client: Arc<dyn OneShotClient>,
    personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    repositories: AppRepositories,
    review_request_client: Arc<dyn ReviewRequestClient>,
    session_update_versions: SessionUpdateVersionMap,
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
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override,
            fs_client,
            git_client,
            one_shot_client_override,
            personality_catalog_client_override,
            repositories,
            review_request_client,
        } = deps;
        let clipboard_image_client = clipboard_image_client_override.unwrap_or_else(|| {
            Arc::new(RealClipboardImageClient::new(
                Arc::clone(&clock),
                Arc::clone(&fs_client),
            ))
        });
        let one_shot_client = one_shot_client_override.unwrap_or_else(|| {
            Arc::new(RealOneShotClient::new(
                app_server_client_override.as_ref().map(Arc::clone),
            ))
        });
        let personality_catalog_client = personality_catalog_client_override
            .unwrap_or_else(|| Arc::new(RealPersonalityCatalogClient));

        Self {
            available_agent_clis: Arc::new(Mutex::new(available_agent_clis)),
            available_agent_kinds: Arc::<[AgentKind]>::from(available_agent_kinds),
            app_server_client_override,
            base_path,
            cleanup_task_handles: Arc::default(),
            creation_task_handles: Arc::default(),
            clipboard_image_client,
            clock,
            event_tx,
            fs_client,
            git_client,
            one_shot_client,
            personality_catalog_client,
            repositories,
            review_request_client,
            session_update_versions: Arc::default(),
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

    /// Returns the cached locally runnable agent CLIs and detected versions.
    pub(crate) fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
        self.available_agent_clis
            .lock()
            .map(|agent_clis| agent_clis.clone())
            .unwrap_or_default()
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

    /// Returns the shared client for isolated structured agent prompts.
    pub(crate) fn one_shot_client(&self) -> Arc<dyn OneShotClient> {
        Arc::clone(&self.one_shot_client)
    }

    /// Returns the workspace personality discovery client.
    pub(crate) fn personality_catalog_client(&self) -> Arc<dyn PersonalityCatalogClient> {
        Arc::clone(&self.personality_catalog_client)
    }

    /// Enqueues an app event onto the internal event bus with debug
    /// instrumentation for producer-side event volume.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        let event_label = app_event_label(&event);
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
    /// finishes graceful shutdown.
    pub(crate) fn track_cleanup_task(&self, join_handle: JoinHandle<()>) {
        if let Ok(mut cleanup_task_handles) = self.cleanup_task_handles.lock() {
            cleanup_task_handles.push(join_handle);
        }
    }

    /// Settles worktree creation, then waits for tracked cleanup tasks.
    ///
    /// The task list is drained before awaiting so the synchronous mutex guard
    /// is never held across an `.await`. The loop repeats in case a cleanup
    /// task registers additional cleanup work before it exits. Cleanup tasks
    /// share one shutdown deadline; unfinished tasks are canceled after it
    /// expires.
    pub(crate) async fn wait_for_cleanup_tasks(&self) {
        let creation_tasks = self
            .creation_task_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in creation_tasks {
            if let Err(error) = task.await {
                warn!(error = %error, "session creation task failed during shutdown");
            }
        }

        wait_for_cleanup_task_handles(
            self.cleanup_task_handles.as_ref(),
            CLEANUP_TASK_SHUTDOWN_TIMEOUT,
        )
        .await;
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

    /// Returns the optional app-server client override used by tests and
    /// injected environments.
    pub(crate) fn app_server_client_override(&self) -> Option<Arc<dyn AppServerClient>> {
        self.app_server_client_override.as_ref().map(Arc::clone)
    }
}

impl OrchestrationEventSink for AppServices {
    fn emit(&self, event: OrchestrationEvent) {
        let event = match event {
            OrchestrationEvent::RefreshSessions => AppEvent::RefreshSessions,
            OrchestrationEvent::ProgressUpdated {
                progress,
                session_id,
            } => AppEvent::SessionOrchestrationProgressUpdated {
                progress,
                session_id,
            },
        };
        self.emit_app_event(event);
    }
}

/// Waits for tracked cleanup tasks until one shared deadline, then cancels
/// every unfinished task so terminal shutdown can continue.
async fn wait_for_cleanup_task_handles(
    cleanup_task_handles: &Mutex<Vec<JoinHandle<()>>>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;

    loop {
        let task_handles = cleanup_task_handles
            .lock()
            .map(|mut task_handles| task_handles.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();

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
                        timeout_seconds = timeout.as_secs(),
                        "background cleanup task exceeded the shutdown deadline and was canceled"
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

/// Returns a stable instrumentation label for one app event variant.
fn app_event_label(event: &AppEvent) -> &'static str {
    match event {
        AppEvent::SessionCreationCompleted { .. } => "SessionCreationCompleted",
        AppEvent::AtMentionEntriesLoaded { .. } => "AtMentionEntriesLoaded",
        AppEvent::DiffPreviewLoaded { .. } => "DiffPreviewLoaded",
        AppEvent::SessionDiffLoaded { .. } => "SessionDiffLoaded",
        AppEvent::GitStatusUpdated { .. } => "GitStatusUpdated",
        AppEvent::VersionAvailabilityUpdated { .. } => "VersionAvailabilityUpdated",
        AppEvent::AgentCliVersionsUpdated { .. } => "AgentCliVersionsUpdated",
        AppEvent::UpdateStatusChanged { .. } => "UpdateStatusChanged",
        AppEvent::SessionModelUpdated { .. } => "SessionModelUpdated",
        AppEvent::SessionPersonalityUpdated { .. } => "SessionPersonalityUpdated",
        AppEvent::SessionPermissionModeUpdated { .. } => "SessionPermissionModeUpdated",
        AppEvent::SessionReasoningLevelUpdated { .. } => "SessionReasoningLevelUpdated",
        AppEvent::SessionResponseStyleUpdated { .. } => "SessionResponseStyleUpdated",
        AppEvent::SessionSpeedModeUpdated { .. } => "SessionSpeedModeUpdated",
        AppEvent::RefreshSessions => "RefreshSessions",
        AppEvent::RefreshProjects => "RefreshProjects",
        AppEvent::RefreshGitStatus => "RefreshGitStatus",
        AppEvent::SessionReviewCommentSnapshotLoaded { .. } => "SessionReviewCommentSnapshotLoaded",
        AppEvent::SessionProgressUpdated { .. } => "SessionProgressUpdated",
        AppEvent::SyncMainCompleted { .. } => "SyncMainCompleted",
        AppEvent::SyncMainConflictResolutionStarted { .. } => "SyncMainConflictResolutionStarted",
        AppEvent::SessionDiffStatsUpdated { .. } => "SessionDiffStatsUpdated",
        AppEvent::SessionTitleGenerationFinished { .. } => "SessionTitleGenerationFinished",
        AppEvent::BranchPublishActionCompleted { .. } => "BranchPublishActionCompleted",
        AppEvent::BranchPublishActionResolved { .. } => "BranchPublishActionResolved",
        AppEvent::BranchPublishActionStarted { .. } => "BranchPublishActionStarted",
        AppEvent::SessionQueuedSyncResolved { .. } => "SessionQueuedSyncResolved",
        AppEvent::SessionTurnStarted { .. } => "SessionTurnStarted",
        AppEvent::ReviewPrepared { .. } => "ReviewPrepared",
        AppEvent::ReviewPreparationFailed { .. } => "ReviewPreparationFailed",
        AppEvent::DeferredAutoReviewPersistenceRetry { .. } => "DeferredAutoReviewPersistenceRetry",
        AppEvent::FocusedReviewPersistenceRetry { .. } => "FocusedReviewPersistenceRetry",
        AppEvent::SessionUpdated { .. } => "SessionUpdated",
        AppEvent::AgentResponseReceived { .. } => "AgentResponseReceived",
        AppEvent::StackedParentTurnCompleted { .. } => "StackedParentTurnCompleted",
        AppEvent::StackedParentSyncCompleted { .. } => "StackedParentSyncCompleted",
        AppEvent::StackedParentMergeCompleted { .. } => "StackedParentMergeCompleted",
        AppEvent::SessionWorkflowNoticeUpdated { .. } => "SessionWorkflowNoticeUpdated",
        AppEvent::SessionOrchestrationProgressUpdated { .. } => {
            "SessionOrchestrationProgressUpdated"
        }
        AppEvent::PublishedBranchSyncUpdated { .. } => "PublishedBranchSyncUpdated",
        AppEvent::ReviewRequestStatusUpdated { .. } => "ReviewRequestStatusUpdated",
    }
}

#[cfg(test)]
#[path = "service_test.rs"]
mod tests;
