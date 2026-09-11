//! App-wide background task helpers for session review-comment loads, periodic
//! version checks, and review-assist generation.
//!
//! Recurring git-status and review-request polling lives in the sync
//! orchestrator (`app/sync.rs`); this module keeps the remaining background
//! tasks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ag_agent::{self as agent, OneShotClient};
use ag_forge::{ForgeRemote, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewRequestClient};
use ag_git::GitClient;
use ag_protocol::focused_review_json_schema_json;
use askama::Template;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::warn;

use crate::app::error::AppError;
use crate::app::review::FocusedReviewPersistenceRetry;
use crate::app::session_diff::DeferredAutoReviewPersistenceRetry;
use crate::app::{AppEvent, UpdateStatus, at_mention_task};
use crate::domain::agent::{AgentCliInfo, AgentKind, AgentSelection, ReasoningLevel};
use crate::domain::file_entry::FileEntry;
use crate::domain::session::SessionId;
use crate::infra::{file_index, version};

/// Delay applied before a fresh `@`-mention filesystem walk starts.
const AT_MENTION_LOAD_DEBOUNCE: Duration = Duration::from_millis(75);

/// Delay before a failed focused-review persistence write is retried through
/// the foreground event reducer.
const FOCUSED_REVIEW_PERSISTENCE_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);

/// Interval between background checks for a newer Agentty release.
const VERSION_CHECK_INTERVAL: Duration = Duration::from_hours(1);

/// Test-only environment override for the version-check interval in
/// milliseconds.
const VERSION_CHECK_INTERVAL_MS_ENV_VAR: &str = "AGENTTY_TEST_VERSION_CHECK_INTERVAL_MS";

/// Monotonic counter used to distinguish stale and current at-mention loads.
static NEXT_AT_MENTION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Monotonic counter used to distinguish stale review-comment loads.
static NEXT_REVIEW_COMMENT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Monotonic counter used to distinguish stale session-diff loads.
static NEXT_SESSION_DIFF_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// External version lookup and package-install boundary used by the periodic
/// update task.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub(crate) trait VersionTaskRunner: Send + Sync {
    /// Returns the latest published Agentty version tag.
    async fn latest_version_tag(&self) -> Option<String>;

    /// Installs the latest Agentty package and reports whether it succeeded.
    async fn run_update(&self) -> bool;
}

/// Production version task runner backed by npm/curl infrastructure.
pub(crate) struct RealVersionTaskRunner;

#[async_trait]
impl VersionTaskRunner for RealVersionTaskRunner {
    async fn latest_version_tag(&self) -> Option<String> {
        version::latest_npm_version_tag().await
    }

    async fn run_update(&self) -> bool {
        version::run_npm_update().await.is_ok()
    }
}

/// Payload needed to load comments for a linked session review request.
pub(super) struct SessionReviewCommentSnapshotTask {
    /// Provider display id such as GitHub `#123` or GitLab `!123`.
    pub(super) display_id: String,
    /// Repository URL reconstructed from the persisted review-request link.
    pub(super) fallback_repo_url: Option<String>,
    /// Session whose comments should receive the completed snapshot.
    pub(super) session_id: SessionId,
    /// Session worktree used for remote detection and forge CLI context.
    pub(super) working_dir: PathBuf,
}

/// Source used by one background session-diff load.
pub(super) enum SessionDiffTaskSource {
    /// Load a retained diff after a managed session worktree was reclaimed.
    Archived {
        repositories: crate::infra::db::AppRepositories,
    },
    /// Compute a live worktree diff against the session base branch.
    Worktree {
        /// Archived diff used when managed merge cleanup wins the live-load
        /// race.
        archived_fallback: Option<crate::infra::db::AppRepositories>,
        base_branch: String,
        git_client: Arc<dyn GitClient>,
    },
}

/// Inputs needed to load one session diff without blocking the foreground UI.
pub(super) struct SessionDiffTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) folder: PathBuf,
    pub(super) session_id: SessionId,
    pub(super) source: SessionDiffTaskSource,
}

/// Inputs needed to generate review assist text in the background.
pub(super) struct ReviewAssistTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Hash of the diff that triggered this review, threaded back in the
    /// completion event so the reducer can store it without re-reading cache.
    pub(super) diff_hash: u64,
    pub(super) reasoning_level: ReasoningLevel,
    pub(super) review_diff: String,
    pub(super) review_selection: AgentSelection,
    pub(super) session_chat_history: Option<String>,
    pub(super) session_folder: PathBuf,
    pub(super) session_id: SessionId,
    pub(super) speed_mode: crate::domain::agent::SpeedMode,
}

/// Askama view model for rendering review assist prompts.
#[derive(Template)]
#[template(path = "review_assist_prompt.md", escape = "none")]
struct ReviewAssistPromptTemplate<'a> {
    /// Full diff payload wrapped in a Markdown fence sized for its content.
    fenced_diff: &'a str,
    /// Self-descriptive schema for the review object returned in `answer`.
    focused_review_json_schema: &'a str,
    /// Transcript context wrapped in a Markdown fence sized for its content.
    session_chat_history: &'a str,
}

/// Stateless helpers for app-scoped one-shot background tasks and app-server
/// session execution.
pub(crate) struct TaskService;

impl TaskService {
    /// Spawns one session-diff load and returns its stale-safe request
    /// generation without waiting for Git or persistence I/O.
    pub(super) fn spawn_session_diff_task(input: SessionDiffTaskInput) -> u64 {
        let request_id = NEXT_SESSION_DIFF_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let result = match input.source {
                SessionDiffTaskSource::Archived { repositories } => repositories
                    .sessions()
                    .load_session_archived_diff(&input.session_id)
                    .await
                    .map(Option::unwrap_or_default)
                    .map_err(|error| format!("Failed to load archived diff: {error}")),
                SessionDiffTaskSource::Worktree {
                    archived_fallback,
                    base_branch,
                    git_client,
                } => match git_client.diff(input.folder, base_branch).await {
                    Ok(diff) => Ok(diff),
                    Err(error @ ag_git::GitError::RepositoryUnavailable { .. }) => {
                        let archived_diff_result = if let Some(repositories) = archived_fallback {
                            repositories
                                .sessions()
                                .load_session_archived_diff(&input.session_id)
                                .await
                                .map_err(|error| format!("Failed to load archived diff: {error}"))
                        } else {
                            Ok(None)
                        };

                        archived_diff_result.and_then(|archived_diff| {
                            archived_diff.ok_or_else(|| format!("Failed to run git diff: {error}"))
                        })
                    }
                    Err(error) => Err(format!("Failed to run git diff: {error}")),
                },
            };
            let _ = input.app_event_tx.send(AppEvent::SessionDiffLoaded {
                request_id,
                result,
                session_id: input.session_id,
            });
        });

        request_id
    }

    /// Publishes cached `@`-mention entries immediately or starts one
    /// debounced filesystem-index task for a cache miss.
    pub(crate) fn spawn_at_mention_entries_task(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        cached_entries: Option<Vec<FileEntry>>,
        lookup_root: PathBuf,
        session_id: SessionId,
    ) {
        if let Some(entries) = cached_entries {
            Self::publish_at_mention_entries(&app_event_tx, entries, &session_id, "cached");

            return;
        }

        let request_id = NEXT_AT_MENTION_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        let tracked_session_id = session_id.clone();
        let task_session_id = session_id.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(AT_MENTION_LOAD_DEBOUNCE).await;

            let load_handle =
                tokio::task::spawn_blocking(move || file_index::list_files(&lookup_root));
            let entries = Self::join_at_mention_entries(load_handle, &session_id).await;

            Self::publish_at_mention_entries(&app_event_tx, entries, &session_id, "loaded");
            at_mention_task::finish_pending_load(&task_session_id, request_id);
        });

        at_mention_task::track_pending_load(tracked_session_id, request_id, handle);
    }

    /// Resolves one blocking file-index task, falling back to an empty index
    /// when the worker cannot be joined.
    async fn join_at_mention_entries(
        load_handle: tokio::task::JoinHandle<Vec<FileEntry>>,
        session_id: &SessionId,
    ) -> Vec<FileEntry> {
        match load_handle.await {
            Ok(entries) => entries,
            Err(error) => {
                warn!(
                    session_id = %session_id,
                    error = %error,
                    "failed to join at-mention file index task"
                );

                Vec::new()
            }
        }
    }

    /// Publishes one at-mention index snapshot through the app event bus.
    fn publish_at_mention_entries(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        entries: Vec<FileEntry>,
        session_id: &SessionId,
        source: &str,
    ) {
        if app_event_tx
            .send(AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id: session_id.clone(),
            })
            .is_err()
        {
            warn!(
                session_id = %session_id,
                source,
                "failed to publish at-mention entries because the app event receiver is closed"
            );
        }
    }

    /// Loads one fresh machine-scoped snapshot of locally runnable agent
    /// kinds without probing CLI versions.
    pub(super) async fn load_agent_availability(
        availability_probe: Arc<dyn agent::AgentAvailabilityProbe>,
    ) -> Vec<AgentKind> {
        tokio::task::spawn_blocking(move || availability_probe.available_agent_kinds())
            .await
            .unwrap_or_else(|_| AgentKind::ALL.to_vec())
    }

    /// Loads one fresh machine-scoped snapshot of locally runnable agent CLIs
    /// after running their startup update commands behind the injected
    /// availability boundary.
    pub(super) async fn load_agent_cli_availability(
        availability_probe: Arc<dyn agent::AgentAvailabilityProbe>,
        fallback_agent_kinds: Vec<AgentKind>,
    ) -> Vec<AgentCliInfo> {
        tokio::task::spawn_blocking(move || availability_probe.available_agent_clis())
            .await
            .unwrap_or_else(|_| AgentCliInfo::from_kinds(&fallback_agent_kinds))
    }

    /// Spawns background agent CLI update/version refresh and emits the
    /// completed snapshot through the app event bus.
    pub(super) fn spawn_agent_cli_version_task(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        availability_probe: Arc<dyn agent::AgentAvailabilityProbe>,
        fallback_agent_kinds: Vec<AgentKind>,
    ) {
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let agent_clis =
                Self::load_agent_cli_availability(availability_probe, fallback_agent_kinds).await;
            let _ = app_event_tx.send(AppEvent::AgentCliVersionsUpdated { agent_clis });
        });
    }

    /// Spawns one linked session review-comment load without blocking terminal
    /// input or redraws and returns its stale-completion request generation.
    pub(super) fn spawn_session_review_comment_snapshot_task(
        task: SessionReviewCommentSnapshotTask,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        review_request_client: Arc<dyn ReviewRequestClient>,
    ) -> u64 {
        let request_id = NEXT_REVIEW_COMMENT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let result = Self::load_session_review_comment_snapshot(
                task.working_dir,
                task.fallback_repo_url,
                task.display_id,
                git_client.as_ref(),
                review_request_client.as_ref(),
            )
            .await;
            let _ = app_event_tx.send(AppEvent::SessionReviewCommentSnapshotLoaded {
                request_id,
                result,
                session_id: task.session_id,
            });
        });

        request_id
    }

    /// Loads comments for one linked session review request, falling back to
    /// its persisted forge URL when terminal-session cleanup removed the
    /// worktree.
    async fn load_session_review_comment_snapshot(
        working_dir: PathBuf,
        fallback_repo_url: Option<String>,
        display_id: String,
        git_client: &dyn GitClient,
        review_request_client: &dyn ReviewRequestClient,
    ) -> Result<ReviewCommentSnapshot, String> {
        let remote =
            match review_request_remote(working_dir, git_client, review_request_client).await {
                Ok(remote) => remote,
                Err(working_dir_error) => {
                    let Some(repo_url) = fallback_repo_url else {
                        return Err(working_dir_error);
                    };

                    review_request_client
                        .detect_remote(repo_url)
                        .map_err(|error| error.detail_message())?
                }
            };

        load_review_comment_snapshot(remote, display_id, review_request_client).await
    }

    /// Spawns an immediate and then hourly background check for newer
    /// `agentty` versions on npmjs, optionally followed by an automatic
    /// `npm i -g agentty@latest` update.
    ///
    /// The task emits [`AppEvent::VersionAvailabilityUpdated`] with
    /// `Some("vX.Y.Z")` only when a newer version is detected. When
    /// `auto_update` is `true` and a newer version exists, the task
    /// subsequently emits [`AppEvent::UpdateStatusChanged`] with
    /// `InProgress`, then `Complete` or `Failed` depending on the npm
    /// install outcome.
    ///
    /// The injected runner controls external lookups and updates; unit tests
    /// supply an offline runner.
    pub(super) fn spawn_version_check_task(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        auto_update: bool,
        version_task_runner: Arc<dyn VersionTaskRunner>,
    ) {
        std::mem::drop(Self::spawn_version_check_task_with_interval(
            app_event_tx,
            auto_update,
            Self::version_check_interval(),
            version_task_runner,
        ));
    }

    /// Resolves the production interval with an optional E2E-only override.
    fn version_check_interval() -> Duration {
        let override_value = std::env::var(VERSION_CHECK_INTERVAL_MS_ENV_VAR).ok();

        Self::version_check_interval_from_override(override_value.as_deref())
    }

    /// Parses a positive millisecond override or returns the hourly default.
    fn version_check_interval_from_override(override_value: Option<&str>) -> Duration {
        override_value
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|milliseconds| *milliseconds > 0)
            .map_or(VERSION_CHECK_INTERVAL, Duration::from_millis)
    }

    /// Spawns recurring version checks at one caller-provided interval.
    fn spawn_version_check_task_with_interval(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        auto_update: bool,
        check_interval: Duration,
        version_task_runner: Arc<dyn VersionTaskRunner>,
    ) -> JoinHandle<()> {
        let app_event_tx = app_event_tx.clone();
        tokio::spawn(async move {
            let mut completed_update_version = None;
            let mut tick = tokio::time::interval(check_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

            loop {
                tick.tick().await;

                let latest_version_tag = version_task_runner.latest_version_tag().await;

                let version_event = Self::version_availability_event(latest_version_tag);
                let newer_version = match &version_event {
                    AppEvent::VersionAvailabilityUpdated {
                        latest_available_version: Some(version),
                    } => Some(version.clone()),
                    _ => None,
                };

                // The receiver closes when the app shuts down, so there is no
                // further work for this task to schedule.
                if app_event_tx.send(version_event).is_err() {
                    break;
                }

                if let Some(newer_version) = newer_version
                    && auto_update
                    && completed_update_version.as_deref() != Some(newer_version.as_str())
                    && Self::run_background_update(
                        &app_event_tx,
                        &newer_version,
                        version_task_runner.as_ref(),
                    )
                    .await
                {
                    completed_update_version = Some(newer_version);
                }
            }
        })
    }

    /// Runs `npm i -g agentty@latest` in a bounded background task and emits
    /// update progress events.
    async fn run_background_update(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        newer_version: &str,
        version_task_runner: &dyn VersionTaskRunner,
    ) -> bool {
        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: newer_version.to_string(),
            },
        });

        let update_completed = version_task_runner.run_update().await;
        let update_status = if update_completed {
            UpdateStatus::Complete {
                version: newer_version.to_string(),
            }
        } else {
            UpdateStatus::Failed {
                version: newer_version.to_string(),
            }
        };

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged { update_status });

        update_completed
    }

    /// Spawns one background review assist generation task and emits
    /// an event with either final review text or a failure description.
    pub(super) fn spawn_review_assist_task(input: ReviewAssistTaskInput) {
        let one_shot_client: Arc<dyn OneShotClient> = Arc::new(agent::RealOneShotClient::new(None));

        Self::spawn_review_assist_task_with_client(input, one_shot_client);
    }

    /// Requeues one failed focused-review persistence write after a bounded
    /// delay so transient database errors cannot strand orchestration review.
    pub(crate) fn spawn_focused_review_persistence_retry(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        retry: FocusedReviewPersistenceRetry,
    ) {
        tokio::spawn(async move {
            tokio::time::sleep(Self::focused_review_persistence_retry_delay(retry.attempt)).await;

            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(AppEvent::FocusedReviewPersistenceRetry { retry });
        });
    }

    /// Requeues one failed automatic-review deferral write after a bounded
    /// delay so transient database errors cannot drop the trigger.
    pub(crate) fn spawn_deferred_auto_review_persistence_retry(
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        retry: DeferredAutoReviewPersistenceRetry,
    ) {
        tokio::spawn(async move {
            tokio::time::sleep(Self::focused_review_persistence_retry_delay(retry.attempt)).await;

            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(AppEvent::DeferredAutoReviewPersistenceRetry { retry });
        });
    }

    /// Returns exponential focused-review persistence backoff for one bounded
    /// retry attempt.
    fn focused_review_persistence_retry_delay(attempt: u8) -> Duration {
        let exponent = attempt.saturating_sub(1).min(2);

        FOCUSED_REVIEW_PERSISTENCE_RETRY_BASE_DELAY.saturating_mul(1_u32 << exponent)
    }

    /// Spawns review assist generation through the provided one-shot boundary.
    fn spawn_review_assist_task_with_client(
        input: ReviewAssistTaskInput,
        one_shot_client: Arc<dyn OneShotClient>,
    ) {
        let ReviewAssistTaskInput {
            app_event_tx,
            diff_hash,
            reasoning_level,
            review_diff,
            review_selection,
            session_chat_history,
            session_folder,
            session_id,
            speed_mode,
        } = input;

        tokio::spawn(async move {
            let review_result = Self::review_assist_text_with_client(
                &session_folder,
                review_selection,
                reasoning_level,
                speed_mode,
                &review_diff,
                session_chat_history.as_deref(),
                one_shot_client.as_ref(),
            )
            .await;

            let app_event = Self::review_app_event(diff_hash, review_result, session_id);
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(app_event);
        });
    }

    /// Converts a raw version lookup result into the reducer event consumed by
    /// app state.
    fn version_availability_event(latest_version_tag: Option<String>) -> AppEvent {
        let latest_available_version = latest_version_tag.filter(|latest_version| {
            version::is_newer_than_current_version(env!("CARGO_PKG_VERSION"), latest_version)
        });

        AppEvent::VersionAvailabilityUpdated {
            latest_available_version,
        }
    }

    /// Generates review assist text through a provider-enforced read-only
    /// one-shot boundary so review generation cannot modify the worktree.
    async fn review_assist_text_with_client(
        session_folder: &Path,
        review_selection: AgentSelection,
        reasoning_level: ReasoningLevel,
        speed_mode: crate::domain::agent::SpeedMode,
        review_diff: &str,
        session_chat_history: Option<&str>,
        one_shot_client: &dyn OneShotClient,
    ) -> Result<String, AppError> {
        let review = crate::app::review_prompt::submit(
            one_shot_client,
            agent::OneShotRequest {
                provider_call_budget: None,
                agent_kind: review_selection.kind(),
                child_pid: None,
                folder: session_folder.to_path_buf(),
                model: review_selection.model(),
                permission_mode: ag_agent::PermissionMode::ReadOnly,
                prompt: String::new(),
                request_kind: ag_agent::AgentRequestKind::FocusedReview,
                reasoning_level,
                speed_mode,
            },
            review_diff,
            session_chat_history.unwrap_or_default(),
            |diff, history| {
                Self::review_assist_prompt(diff, Some(history))
                    .map_err(|error| agent::OneShotError::new(error.to_string()))
            },
        )
        .await?;
        Ok(review)
    }

    /// Builds the final reducer event for one review-assist task outcome.
    ///
    /// Converts the typed [`AppError`] to a display string at the event
    /// boundary because [`AppEvent`] requires `Clone` + `Eq`, which
    /// [`AppError`] cannot satisfy due to non-cloneable inner IO errors.
    fn review_app_event(
        diff_hash: u64,
        review_result: Result<String, AppError>,
        session_id: SessionId,
    ) -> AppEvent {
        match review_result {
            Ok(review_text) => AppEvent::ReviewPrepared {
                diff_hash,
                review_text,
                session_id,
            },
            Err(error) => AppEvent::ReviewPreparationFailed {
                diff_hash,
                error: error.to_string(),
                session_id,
            },
        }
    }

    /// Renders the review assist prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error when Askama template rendering fails.
    fn review_assist_prompt(
        review_diff: &str,
        session_chat_history: Option<&str>,
    ) -> Result<String, AppError> {
        let trimmed_diff = review_diff.trim();
        let fence = agent::diff_fence(trimmed_diff);
        let fenced_diff = format!("{fence}diff\n{trimmed_diff}\n{fence}");
        let session_chat_history = session_chat_history.map_or("", str::trim_end);
        let history_fence = agent::diff_fence(session_chat_history);
        let fenced_session_chat_history =
            format!("{history_fence}text\n{session_chat_history}\n{history_fence}");
        let focused_review_json_schema = focused_review_json_schema_json();
        let template = ReviewAssistPromptTemplate {
            fenced_diff: &fenced_diff,
            focused_review_json_schema: &focused_review_json_schema,
            session_chat_history: &fenced_session_chat_history,
        };

        template.render().map_err(|error| {
            AppError::Workflow(format!(
                "Failed to render `review_assist_prompt.md`: {error}"
            ))
        })
    }
}

/// Resolves the active project remote for session review-comment loading.
async fn review_request_remote(
    working_dir: PathBuf,
    git_client: &dyn GitClient,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<ForgeRemote, String> {
    let repo_url = git_client
        .repo_url(working_dir.clone())
        .await
        .map_err(|error| format!("Failed to resolve repository remote: {error}"))?;

    review_request_client
        .detect_remote(repo_url)
        .map(|remote| remote.with_command_working_directory(working_dir))
        .map_err(|error| error.detail_message())
}

/// Fetches and normalizes one review-comment snapshot from an already
/// resolved forge remote.
async fn load_review_comment_snapshot(
    remote: ForgeRemote,
    display_id: String,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<ReviewCommentSnapshot, String> {
    review_request_client
        .fetch_review_comment_snapshot(remote, display_id)
        .await
        .map(sorted_review_comment_snapshot)
        .map_err(|error| error.detail_message())
}

/// Sorts inline review-comment threads once before storing them for rendering.
fn sorted_review_comment_snapshot(
    mut review_comment_snapshot: ReviewCommentSnapshot,
) -> ReviewCommentSnapshot {
    review_comment_snapshot.threads.sort_by(|left, right| {
        (
            left.path.as_str(),
            left.line.unwrap_or(u32::MAX),
            review_comment_anchor_side_order(left.anchor_side),
        )
            .cmp(&(
                right.path.as_str(),
                right.line.unwrap_or(u32::MAX),
                review_comment_anchor_side_order(right.anchor_side),
            ))
    });

    review_comment_snapshot
}

/// Returns a deterministic sort order for comment anchor sides.
fn review_comment_anchor_side_order(anchor_side: ReviewCommentAnchorSide) -> u8 {
    match anchor_side {
        ReviewCommentAnchorSide::File => 0,
        ReviewCommentAnchorSide::Old => 1,
        ReviewCommentAnchorSide::New => 2,
    }
}

#[cfg(test)]
#[path = "task_test.rs"]
pub(crate) mod tests;
