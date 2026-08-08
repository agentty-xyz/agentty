//! App-wide background task helpers for requested-review loads, version
//! checks, and review-assist generation.
//!
//! Recurring git-status and review-request polling lives in the sync
//! orchestrator (`app/sync.rs`); this module keeps one-shot background tasks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ag_agent::{self as agent, OneShotClient};
use ag_forge::{ForgeRemote, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewRequestClient};
use ag_git::GitClient;
use ag_protocol::AgentResponse;
use askama::Template;
use tokio::sync::mpsc;
use tracing::warn;

use crate::app::error::AppError;
use crate::app::review::FocusedReviewPersistenceRetry;
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
/// Monotonic counter used to distinguish stale and current at-mention loads.
static NEXT_AT_MENTION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Stateless helpers for app-scoped one-shot background tasks and app-server
/// session execution.
pub(crate) struct TaskService;

/// Payload needed to run one requested-review comment snapshot task and route
/// its completion back to the matching list generation.
pub(super) struct RequestedReviewCommentSnapshotTask {
    /// Provider display id such as GitHub `#123` or GitLab `!123`.
    pub(super) display_id: String,
    /// Requested-review list generation visible when the comment load began.
    pub(super) generation: u64,
    /// Project id that owns the requested-review row.
    pub(super) project_id: i64,
    /// Browser-openable review-request URL used to disambiguate rows.
    pub(super) web_url: String,
    /// Project working directory used for git remote and forge CLI context.
    pub(super) working_dir: PathBuf,
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
}

/// Askama view model for rendering review assist prompts.
#[derive(Template)]
#[template(path = "review_assist_prompt.md", escape = "none")]
struct ReviewAssistPromptTemplate<'a> {
    /// Full diff payload wrapped in a Markdown fence sized for its content.
    fenced_diff: &'a str,
    /// User and assistant transcript context for the reviewed session.
    session_chat_history: &'a str,
}

impl TaskService {
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

    /// Spawns an assigned GitHub issue refresh for the active project.
    pub(super) fn spawn_assigned_issues_task(
        generation: u64,
        project_id: i64,
        working_dir: PathBuf,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        review_request_client: Arc<dyn ReviewRequestClient>,
    ) {
        tokio::spawn(async move {
            let result = load_assigned_issues(
                working_dir,
                git_client.as_ref(),
                review_request_client.as_ref(),
            )
            .await;
            let _ = app_event_tx.send(AppEvent::AssignedIssuesLoaded {
                generation,
                project_id,
                result,
            });
        });
    }

    /// Spawns a base-detail load for one selected GitHub issue.
    pub(super) fn spawn_issue_detail_task(
        display_id: String,
        generation: u64,
        project_id: i64,
        working_dir: PathBuf,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        review_request_client: Arc<dyn ReviewRequestClient>,
    ) {
        tokio::spawn(async move {
            let result = load_issue_detail(
                working_dir,
                &display_id,
                git_client.as_ref(),
                review_request_client.as_ref(),
            )
            .await;
            let _ = app_event_tx.send(AppEvent::IssueDetailLoaded {
                display_id,
                generation,
                project_id,
                result,
            });
        });
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

    /// Spawns one requested-review refresh for the active project.
    ///
    /// The task resolves the project remote through the injected git boundary,
    /// asks the forge client for open PRs/MRs requesting the authenticated
    /// user's review, and reports the result through [`AppEvent`] with the
    /// generation that lets the reducer discard stale completions.
    pub(super) fn spawn_requested_reviews_task(
        generation: u64,
        project_id: i64,
        working_dir: PathBuf,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        review_request_client: Arc<dyn ReviewRequestClient>,
    ) {
        tokio::spawn(async move {
            let result = load_requested_reviews(
                working_dir,
                git_client.as_ref(),
                review_request_client.as_ref(),
            )
            .await;
            let _ = app_event_tx.send(AppEvent::RequestedReviewsLoaded {
                generation,
                project_id,
                result,
            });
        });
    }

    /// Spawns one requested-review comment snapshot load for the open detail
    /// page without blocking key handling or terminal redraws.
    pub(super) fn spawn_requested_review_comment_snapshot_task(
        task: RequestedReviewCommentSnapshotTask,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        review_request_client: Arc<dyn ReviewRequestClient>,
    ) {
        tokio::spawn(async move {
            let result = Self::load_requested_review_comment_snapshot(
                task.working_dir,
                task.display_id.clone(),
                git_client.as_ref(),
                review_request_client.as_ref(),
            )
            .await;
            let _ = app_event_tx.send(AppEvent::RequestedReviewCommentSnapshotLoaded {
                display_id: task.display_id,
                generation: task.generation,
                project_id: task.project_id,
                result,
                web_url: task.web_url,
            });
        });
    }

    /// Spawns one linked session review-comment load without blocking terminal
    /// input or redraws.
    pub(super) fn spawn_session_review_comment_snapshot_task(
        task: SessionReviewCommentSnapshotTask,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
        review_request_client: Arc<dyn ReviewRequestClient>,
    ) {
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
                result,
                session_id: task.session_id,
            });
        });
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

    /// Loads and normalizes the comment snapshot for one requested review.
    ///
    /// This is triggered lazily when users open a specific review detail page
    /// so the top-level Inbox tab does not perform one comment API request
    /// per listed PR or MR.
    pub(super) async fn load_requested_review_comment_snapshot(
        working_dir: PathBuf,
        display_id: String,
        git_client: &dyn GitClient,
        review_request_client: &dyn ReviewRequestClient,
    ) -> Result<ReviewCommentSnapshot, String> {
        let remote = review_request_remote(working_dir, git_client, review_request_client).await?;

        load_review_comment_snapshot(remote, display_id, review_request_client).await
    }

    /// Spawns a one-shot background check for newer `agentty` versions on
    /// npmjs, optionally followed by an automatic `npm i -g agentty@latest`
    /// update.
    ///
    /// The task emits [`AppEvent::VersionAvailabilityUpdated`] with
    /// `Some("vX.Y.Z")` only when a newer version is detected. When
    /// `auto_update` is `true` and a newer version exists, the task
    /// subsequently emits [`AppEvent::UpdateStatusChanged`] with
    /// `InProgress`, then `Complete` or `Failed` depending on the npm
    /// install outcome.
    ///
    /// In tests, it emits an immediate `None` update instead of spawning the
    /// network check so test runs stay deterministic and offline.
    pub(super) fn spawn_version_check_task(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        auto_update: bool,
    ) {
        #[cfg(test)]
        {
            let _ = auto_update;
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(Self::version_availability_event(None));
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let latest_version_tag = version::latest_npm_version_tag().await;
            let version_event = Self::version_availability_event(latest_version_tag);

            let newer_version = match &version_event {
                AppEvent::VersionAvailabilityUpdated {
                    latest_available_version: Some(version),
                } => Some(version.clone()),
                _ => None,
            };

            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(version_event);

            if let Some(newer_version) = newer_version
                && auto_update
            {
                Self::run_background_update(&app_event_tx, &newer_version).await;
            }
        });
    }

    /// Runs `npm i -g agentty@latest` in a background blocking task and
    /// emits update progress events.
    #[cfg(not(test))]
    async fn run_background_update(
        app_event_tx: &mpsc::UnboundedSender<AppEvent>,
        newer_version: &str,
    ) {
        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: newer_version.to_string(),
            },
        });

        let update_result = tokio::task::spawn_blocking(move || {
            let update_runner = version::RealUpdateRunner;
            version::run_npm_update_sync(&update_runner)
        })
        .await;

        let update_status = match update_result {
            Ok(Ok(_)) => UpdateStatus::Complete {
                version: newer_version.to_string(),
            },
            Ok(Err(_)) | Err(_) => UpdateStatus::Failed {
                version: newer_version.to_string(),
            },
        };

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = app_event_tx.send(AppEvent::UpdateStatusChanged { update_status });
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
        } = input;

        tokio::spawn(async move {
            let review_result = Self::review_assist_text_with_client(
                &session_folder,
                review_selection,
                reasoning_level,
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

    /// Generates review assist text using an injected one-shot boundary so
    /// failure paths can be tested without subprocess execution.
    async fn review_assist_text_with_client(
        session_folder: &Path,
        review_selection: AgentSelection,
        reasoning_level: ReasoningLevel,
        review_diff: &str,
        session_chat_history: Option<&str>,
        one_shot_client: &dyn OneShotClient,
    ) -> Result<String, AppError> {
        let review_prompt = Self::review_assist_prompt(review_diff, session_chat_history)?;
        let submission = one_shot_client
            .submit(agent::OneShotRequest {
                agent_kind: review_selection.kind(),
                child_pid: None,
                folder: session_folder.to_path_buf(),
                model: review_selection.model(),
                prompt: review_prompt,
                request_kind: ag_agent::AgentRequestKind::UtilityPrompt,
                reasoning_level,
            })
            .await
            .map_err(AppError::from)?;

        Self::review_output_text(&submission.response)
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

    /// Extracts one non-empty review string from the agent response payload.
    fn review_output_text(agent_response: &AgentResponse) -> Result<String, AppError> {
        let review_text = agent_response.to_display_text();
        let review_text = review_text.trim();
        if review_text.is_empty() {
            return Err(AppError::Workflow(
                "Review assist returned empty output".to_string(),
            ));
        }

        Ok(review_text.to_string())
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
        let template = ReviewAssistPromptTemplate {
            fenced_diff: &fenced_diff,
            session_chat_history: session_chat_history.map_or("", str::trim_end),
        };

        template.render().map_err(|error| {
            AppError::Workflow(format!(
                "Failed to render `review_assist_prompt.md`: {error}"
            ))
        })
    }
}

/// Resolves the active project remote and loads PRs/MRs requesting the current
/// authenticated user's review without detail comment snapshots.
async fn load_requested_reviews(
    working_dir: PathBuf,
    git_client: &dyn GitClient,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<Vec<ag_forge::RequestedReview>, String> {
    let remote = review_request_remote(working_dir, git_client, review_request_client).await?;

    review_request_client
        .list_requested_reviews(remote)
        .await
        .map_err(|error| error.detail_message())
}

/// Resolves the active project remote and loads its assigned GitHub issues.
async fn load_assigned_issues(
    working_dir: PathBuf,
    git_client: &dyn GitClient,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<Vec<ag_forge::AssignedIssue>, String> {
    let remote = review_request_remote(working_dir, git_client, review_request_client).await?;

    review_request_client
        .list_assigned_issues(remote)
        .await
        .map_err(|error| error.detail_message())
}

/// Resolves the active project remote and loads one issue without comments.
async fn load_issue_detail(
    working_dir: PathBuf,
    display_id: &str,
    git_client: &dyn GitClient,
    review_request_client: &dyn ReviewRequestClient,
) -> Result<ag_forge::IssueDetail, String> {
    let remote = review_request_remote(working_dir, git_client, review_request_client).await?;

    review_request_client
        .fetch_issue_detail(remote, display_id.to_string())
        .await
        .map_err(|error| error.detail_message())
}

/// Resolves the active project remote for requested-review list and detail
/// loading.
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
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use ag_forge::{
        AssignedIssue, ForgeKind, MockReviewRequestClient, RequestedReview,
        RequestedReviewAudience, ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot,
        ReviewCommentThread,
    };
    use ag_git::MockGitClient;
    use ag_protocol::{AgentResponse, parse_agent_response_strict};

    use super::*;
    use crate::domain::agent::AgentModel;

    struct PanickingAgentAvailabilityProbe;

    impl agent::AgentAvailabilityProbe for PanickingAgentAvailabilityProbe {
        fn available_agent_kinds(&self) -> Vec<AgentKind> {
            vec![AgentKind::Claude]
        }

        fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
            std::panic::resume_unwind(Box::new("version probe failed".to_string()));
        }
    }

    #[tokio::test]
    async fn join_at_mention_entries_returns_empty_index_for_panicking_worker() {
        // Arrange
        let load_handle = tokio::task::spawn_blocking(|| -> Vec<FileEntry> {
            std::panic::resume_unwind(Box::new("file index failed".to_string()));
        });

        // Act
        let entries = TaskService::join_at_mention_entries(load_handle, &"session-id".into()).await;

        // Assert
        assert_eq!(entries, [] as [crate::domain::file_entry::FileEntry; 0]);
    }

    #[test]
    fn publish_at_mention_entries_tolerates_closed_event_receiver() {
        // Arrange
        let (app_event_tx, app_event_rx) = mpsc::unbounded_channel();
        drop(app_event_rx);

        // Act
        TaskService::publish_at_mention_entries(
            &app_event_tx,
            Vec::new(),
            &"session-id".into(),
            "cached",
        );

        // Assert
        assert!(app_event_tx.is_closed());
    }

    #[tokio::test]
    /// Ensures requested-review list loading does not fetch detail comment
    /// snapshots for every listed PR or MR.
    async fn load_requested_reviews_lists_metadata_without_comment_snapshots() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_repo_url().times(1).returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut mock_review_request_client = MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .returning(|_| Ok(forge_remote()));
        mock_review_request_client
            .expect_list_requested_reviews()
            .times(1)
            .returning(|_| Box::pin(async { Ok(vec![requested_review("#42")]) }));
        mock_review_request_client
            .expect_fetch_review_comment_snapshot()
            .times(0);

        // Act
        let requested_reviews = load_requested_reviews(
            PathBuf::from("/tmp/project"),
            &mock_git_client,
            &mock_review_request_client,
        )
        .await
        .expect("requested reviews should load");

        // Assert
        assert_eq!(requested_reviews.len(), 1);
        assert_eq!(requested_reviews[0].comment_snapshot, None);
    }

    #[tokio::test]
    async fn load_assigned_issues_scopes_query_to_active_project_remote() {
        // Arrange
        let working_dir = PathBuf::from("/tmp/project");
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_repo_url().times(1).returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut mock_review_request_client = MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .returning(|_| Ok(forge_remote()));
        mock_review_request_client
            .expect_list_assigned_issues()
            .times(1)
            .withf({
                let working_dir = working_dir.clone();

                move |remote| {
                    remote.project_path() == "agentty-xyz/agentty"
                        && remote.command_working_directory.as_ref() == Some(&working_dir)
                }
            })
            .returning(|_| {
                Box::pin(async {
                    Ok(vec![AssignedIssue {
                        display_id: "#124".to_string(),
                        repository: "agentty-xyz/agentty".to_string(),
                        title: "Keep issue list compact".to_string(),
                        updated_at: None,
                        web_url: "https://github.com/agentty-xyz/agentty/issues/124".to_string(),
                    }])
                })
            });

        // Act
        let assigned_issues =
            load_assigned_issues(working_dir, &mock_git_client, &mock_review_request_client)
                .await
                .expect("assigned issues should load");

        // Assert
        assert_eq!(assigned_issues.len(), 1);
        assert_eq!(assigned_issues[0].repository, "agentty-xyz/agentty");
    }

    #[tokio::test]
    /// Ensures lazy requested-review detail loading fetches and stores the
    /// selected PR or MR comment snapshot.
    async fn load_requested_review_comment_snapshot_fetches_selected_review_comments() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_repo_url().times(1).returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut mock_review_request_client = MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .returning(|_| Ok(forge_remote()));
        mock_review_request_client
            .expect_fetch_review_comment_snapshot()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot()) }));

        // Act
        let comment_snapshot = TaskService::load_requested_review_comment_snapshot(
            PathBuf::from("/tmp/project"),
            "#42".to_string(),
            &mock_git_client,
            &mock_review_request_client,
        )
        .await;

        // Assert
        assert_eq!(comment_snapshot, Ok(review_comment_snapshot()));
    }

    #[tokio::test]
    async fn load_session_review_comment_snapshot_uses_persisted_url_without_worktree() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_repo_url().times(1).returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "session worktree was removed",
                )))
            })
        });
        let mut mock_review_request_client = MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty")
            .returning(|_| Ok(forge_remote()));
        mock_review_request_client
            .expect_fetch_review_comment_snapshot()
            .times(1)
            .withf(|remote, display_id| {
                remote.command_working_directory.is_none() && display_id == "#42"
            })
            .returning(|_, _| Box::pin(async { Ok(review_comment_snapshot()) }));

        // Act
        let comment_snapshot = TaskService::load_session_review_comment_snapshot(
            PathBuf::from("/tmp/missing-session"),
            Some("https://github.com/agentty-xyz/agentty".to_string()),
            "#42".to_string(),
            &mock_git_client,
            &mock_review_request_client,
        )
        .await;

        // Assert
        assert_eq!(comment_snapshot, Ok(review_comment_snapshot()));
    }

    #[tokio::test]
    async fn load_session_review_comment_snapshot_returns_worktree_error_without_fallback() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_repo_url().once().returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "session worktree was removed",
                )))
            })
        });
        let mock_review_request_client = MockReviewRequestClient::new();

        // Act
        let result = TaskService::load_session_review_comment_snapshot(
            PathBuf::from("/tmp/missing-session"),
            None,
            "#42".to_string(),
            &mock_git_client,
            &mock_review_request_client,
        )
        .await;

        // Assert
        assert!(matches!(result, Err(error) if error.contains("session worktree was removed")));
    }

    #[tokio::test]
    /// Ensures requested-review comment snapshots are sorted before they are
    /// stored for render-time reuse.
    async fn load_requested_review_comment_snapshot_sorts_threads_once() {
        // Arrange
        let mut mock_git_client = MockGitClient::new();
        mock_git_client.expect_repo_url().times(1).returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
        let mut mock_review_request_client = MockReviewRequestClient::new();
        mock_review_request_client
            .expect_detect_remote()
            .times(1)
            .returning(|_| Ok(forge_remote()));
        mock_review_request_client
            .expect_fetch_review_comment_snapshot()
            .times(1)
            .returning(|_, _| Box::pin(async { Ok(unsorted_review_comment_snapshot()) }));

        // Act
        let comment_snapshot = TaskService::load_requested_review_comment_snapshot(
            PathBuf::from("/tmp/project"),
            "#42".to_string(),
            &mock_git_client,
            &mock_review_request_client,
        )
        .await
        .expect("comments should load");

        // Assert
        let threads = &comment_snapshot.threads;
        assert_eq!(threads[0].path, "crates/agentty/src/app.rs");
        assert_eq!(threads[1].path, "crates/agentty/src/ui.rs");
    }

    #[tokio::test]
    /// Ensures test-mode version checks still emit one reducer event without
    /// touching the network.
    async fn spawn_version_check_task_emits_none_update_in_tests() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_version_check_task(&app_event_tx, true);
        let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
            .await
            .expect("timed out waiting for version-check event")
            .expect("version-check task should emit one event");

        // Assert
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            }
        );
    }

    #[tokio::test]
    /// Ensures the `--no-update` flag (`auto_update=false`) still emits a
    /// version availability event without triggering an update.
    async fn spawn_version_check_task_with_no_update_emits_version_event() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

        // Act
        TaskService::spawn_version_check_task(&app_event_tx, false);
        let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
            .await
            .expect("timed out waiting for version-check event")
            .expect("version-check task should emit one event");

        // Assert — still emits version check, no update events
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            }
        );
    }

    #[tokio::test]
    /// Ensures CLI update/version fallback rows preserve the
    /// startup-discovered availability subset when the blocking probe panics.
    async fn load_agent_cli_availability_uses_startup_kinds_when_probe_panics() {
        // Arrange
        let fallback_agent_kinds = vec![AgentKind::Claude];

        // Act
        let agent_clis = TaskService::load_agent_cli_availability(
            Arc::new(PanickingAgentAvailabilityProbe),
            fallback_agent_kinds,
        )
        .await;

        // Assert
        assert_eq!(agent_clis, vec![AgentCliInfo::new(AgentKind::Claude, None)]);
    }

    #[test]
    /// Verifies version availability keeps only tags newer than the current
    /// crate version.
    fn version_availability_event_keeps_newer_version_tags() {
        // Arrange
        let latest_version_tag = Some("v999.0.0".to_string());

        // Act
        let app_event = TaskService::version_availability_event(latest_version_tag);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: Some("v999.0.0".to_string()),
            }
        );
    }

    #[test]
    /// Verifies version availability suppresses current-version tags so the
    /// UI only announces true upgrades.
    fn version_availability_event_ignores_current_version_tag() {
        // Arrange
        let latest_version_tag = Some(format!("v{}", env!("CARGO_PKG_VERSION")));

        // Act
        let app_event = TaskService::version_availability_event(latest_version_tag);

        // Assert
        assert_eq!(
            app_event,
            AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            }
        );
    }

    #[tokio::test]
    /// Ensures the detached review-assist task emits the completed review
    /// through the app event channel.
    async fn spawn_review_assist_task_with_client_emits_completed_review() {
        // Arrange
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        let mut one_shot_client = agent::MockOneShotClient::new();
        one_shot_client
            .expect_submit()
            .times(1)
            .returning(|request| {
                assert_eq!(request.agent_kind, AgentKind::Claude);
                assert_eq!(request.reasoning_level, ReasoningLevel::XHigh);
                assert!(
                    request
                        .prompt
                        .contains("diff --git a/src/lib.rs b/src/lib.rs")
                );

                Ok(agent::OneShotSubmission {
                    response: AgentResponse::plain("Review completed."),
                    stats: agent::SessionStats::default(),
                })
            });
        let input = ReviewAssistTaskInput {
            app_event_tx,
            diff_hash: 42,
            reasoning_level: ReasoningLevel::XHigh,
            review_diff: "diff --git a/src/lib.rs b/src/lib.rs".to_string(),
            review_selection: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
            session_chat_history: None,
            session_folder: PathBuf::from("/tmp/review-assist"),
            session_id: "session-42".into(),
        };

        // Act
        TaskService::spawn_review_assist_task_with_client(input, Arc::new(one_shot_client));
        let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
            .await
            .expect("timed out waiting for review-assist event")
            .expect("review-assist task should emit one event");

        // Assert
        assert_eq!(
            app_event,
            AppEvent::ReviewPrepared {
                diff_hash: 42,
                review_text: "Review completed.".to_string(),
                session_id: "session-42".into(),
            }
        );
    }

    #[test]
    fn focused_review_persistence_retries_use_capped_exponential_backoff() {
        // Arrange / Act
        let delays = [1, 2, 3, 4].map(TaskService::focused_review_persistence_retry_delay);

        // Assert
        assert_eq!(
            delays,
            [
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(1),
            ]
        );
    }

    #[tokio::test]
    /// Ensures review assist preserves typed one-shot submission failures
    /// without invoking a real subprocess.
    async fn review_assist_text_with_client_returns_one_shot_error_on_submit_failure() {
        // Arrange
        let session_folder = Path::new("/tmp/review-assist-submit-error");
        let review_selection = AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5);
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let mut one_shot_client = agent::MockOneShotClient::new();
        one_shot_client
            .expect_submit()
            .returning(|_| Err(agent::OneShotError::new("submit failed")));

        // Act
        let result = TaskService::review_assist_text_with_client(
            session_folder,
            review_selection,
            ReasoningLevel::XHigh,
            review_diff,
            None,
            &one_shot_client,
        )
        .await;

        // Assert
        let error = result.expect_err("submit failure should be returned");
        assert!(
            matches!(error, AppError::OneShot(_)),
            "expected AppError::OneShot, got: {error:?}"
        );
        assert_eq!(error.to_string(), "submit failed");
    }

    #[tokio::test]
    /// Ensures review assist keeps the selected provider for shared Gemini
    /// model ids instead of resolving the model to the first available
    /// provider.
    async fn review_assist_text_with_client_preserves_review_selection_provider() {
        // Arrange
        let session_folder = Path::new("/tmp/review-assist-provider");
        let review_selection =
            AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini36Flash);
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let mut one_shot_client = agent::MockOneShotClient::new();
        one_shot_client.expect_submit().returning(|request| {
            assert_eq!(request.agent_kind, AgentKind::Antigravity);
            assert_eq!(request.model, AgentModel::Gemini36Flash);
            assert_eq!(request.reasoning_level, ReasoningLevel::Low);

            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain("Review completed."),
                stats: agent::SessionStats::default(),
            })
        });

        // Act
        let result = TaskService::review_assist_text_with_client(
            session_folder,
            review_selection,
            ReasoningLevel::Low,
            review_diff,
            None,
            &one_shot_client,
        )
        .await;

        // Assert
        assert_eq!(
            result.expect("review output should be returned"),
            "Review completed."
        );
    }

    #[test]
    /// Verifies review-assist event mapping preserves successful review text.
    fn review_app_event_maps_successful_review_output() {
        // Arrange
        let diff_hash = 7;
        let review_result = Ok("Flagged one missing error branch.".to_string());
        let session_id = "session-7".to_string();

        // Act
        let app_event = TaskService::review_app_event(diff_hash, review_result, session_id.into());

        // Assert
        assert_eq!(
            app_event,
            AppEvent::ReviewPrepared {
                diff_hash: 7,
                review_text: "Flagged one missing error branch.".to_string(),
                session_id: "session-7".into(),
            }
        );
    }

    #[test]
    /// Verifies review-assist event mapping preserves failure details for the
    /// reducer and view-mode status text.
    fn review_app_event_maps_failure_output() {
        // Arrange
        let diff_hash = 9;
        let review_result = Err(AppError::Workflow("empty response".to_string()));
        let session_id = "session-9".to_string();

        // Act
        let app_event = TaskService::review_app_event(diff_hash, review_result, session_id.into());

        // Assert
        assert_eq!(
            app_event,
            AppEvent::ReviewPreparationFailed {
                diff_hash: 9,
                error: "empty response".to_string(),
                session_id: "session-9".into(),
            }
        );
    }

    #[test]
    /// Verifies review output text is trimmed before it is stored in app
    /// state.
    fn review_output_text_trims_agent_response_text() {
        // Arrange
        let agent_response = AgentResponse::plain("  Review looks good.  \n");

        // Act
        let review_text = TaskService::review_output_text(&agent_response)
            .expect("non-empty output should be accepted");

        // Assert
        assert_eq!(review_text, "Review looks good.");
    }

    #[test]
    /// Verifies whitespace-only review output is rejected as
    /// [`AppError::Workflow`] so users see a clear error instead of a blank
    /// review pane.
    fn review_output_text_rejects_blank_agent_response_text() {
        // Arrange
        let agent_response = AgentResponse::plain(" \n\t ");

        // Act
        let result = TaskService::review_output_text(&agent_response);

        // Assert
        let error = result.expect_err("blank output should be rejected");
        assert!(
            matches!(error, AppError::Workflow(_)),
            "expected AppError::Workflow, got: {error:?}"
        );
        assert_eq!(error.to_string(), "Review assist returned empty output");
    }

    #[test]
    /// Ensures review prompt rendering includes inspection-only review
    /// constraints.
    fn test_review_assist_prompt_enforces_read_only_constraints() {
        // Arrange
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";

        // Act
        let prompt = TaskService::review_assist_prompt(review_diff, None)
            .expect("review prompt should render");
        let normalized_prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");

        // Assert
        assert!(normalized_prompt.contains("Markdown review body in `answer`"));
        assert!(normalized_prompt.contains("leave `questions` empty"));
        assert!(normalized_prompt.contains("set `summary` to null"));
        assert!(!prompt.contains("Return Markdown only."));
        assert!(prompt.contains("You are in read-only review mode."));
        assert!(prompt.contains("Do not create, modify, rename, or delete files."));
        assert!(prompt.contains("Do not run build, test, formatter, linter"));
        assert!(prompt.contains("You may browse the internet when needed."));
        assert!(prompt.contains("Use inspection only: file reads, file searches"));
        assert!(normalized_prompt.contains("never treat absence from the diff as absence"));
        assert!(normalized_prompt.contains(
            "Never suggest a missing import, declaration, dependency, or registration unless you \
             verified it is absent in the current worktree"
        ));
        assert!(normalized_prompt.contains(
            "phrase it as a suggestion for the agent to run the exact command in a follow-up turn"
        ));
        assert!(normalized_prompt.contains("never tell the user to run commands themselves"));
        assert!(!prompt.contains("You may run non-editing CLI commands"));
        assert!(prompt.contains("Format this section as a Markdown bullet list."));
        assert!(normalized_prompt.contains(
            "Format each suggestion as `- [Severity]: Issue details`, using `[High]` or `[Medium]`"
        ));
        assert!(normalized_prompt.contains("Treat high severity as correctness"));
        assert!(normalized_prompt.contains("concrete practical impact"));
        let fenced_diff = format!("```diff\n{review_diff}\n```");
        assert!(
            prompt.contains(&fenced_diff),
            "review prompt must wrap the diff in a ```diff``` fence so `@`-prefixed decorator \
             tokens are not misread as file mentions"
        );
    }

    #[test]
    /// Ensures review prompt rendering includes prior user and assistant
    /// messages as decision context without adding stale session-summary
    /// context.
    fn test_review_assist_prompt_includes_session_chat_history() {
        // Arrange
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs\n+new behavior";
        let session_chat_history = Some(" › Add focused review context\n\nDone.\n\n");

        // Act
        let prompt = TaskService::review_assist_prompt(review_diff, session_chat_history)
            .expect("review prompt should render");
        let normalized_prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");

        // Assert
        assert!(
            prompt.contains("Session chat history (user and agent messages only; may be empty):")
        );
        assert!(prompt.contains(" › Add focused review context\n\nDone."));
        assert!(!prompt.contains("Existing session summary context"));
        assert!(normalized_prompt.contains(
            "Use the session chat history as decision context, not just background information"
        ));
        assert!(normalized_prompt.contains(
            "Treat explicit user decisions, accepted tradeoffs, and explanations in the history \
             as review constraints"
        ));
        assert!(normalized_prompt.contains(
            "Do not repeat a suggestion already resolved in the history unless the current diff \
             contradicts that resolution or inspection reveals a new high- or medium-severity risk"
        ));
        assert!(normalized_prompt.contains(
            "When reopening a resolved suggestion, acknowledge the prior resolution and state the \
             new evidence"
        ));
    }

    #[test]
    /// Ensures the review prompt widens the outer code fence when the diff
    /// contains a triple-backtick sequence of its own so the Markdown boundary
    /// cannot be terminated by the diff content itself.
    fn test_review_assist_prompt_escapes_triple_backtick_fence_in_diff() {
        // Arrange
        let review_diff = concat!(
            "diff --git a/notes.md b/notes.md\n",
            "+```\n",
            "+example fenced block\n",
            "+```\n",
        );

        // Act
        let prompt = TaskService::review_assist_prompt(review_diff, None)
            .expect("review prompt should render");

        // Assert
        assert!(
            prompt.contains("````diff\n"),
            "outer fence must be longer than the longest backtick run in the diff to preserve \
             prompt boundaries"
        );
        let matches = prompt.matches("\n````").count();
        assert!(
            matches >= 2,
            "prompt must contain an opening and closing 4-backtick fence, got {matches} \
             occurrences"
        );
        assert!(prompt.contains("+```\n"));
    }

    /// Builds one GitHub remote fixture for requested-review task tests.
    fn forge_remote() -> ForgeRemote {
        ForgeRemote {
            command_working_directory: None,
            forge_kind: ForgeKind::GitHub,
            host: "github.com".to_string(),
            namespace: "agentty-xyz".to_string(),
            project: "agentty".to_string(),
            repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty".to_string(),
        }
    }

    /// Builds one requested review fixture for task tests.
    fn requested_review(display_id: &str) -> RequestedReview {
        RequestedReview {
            audience: RequestedReviewAudience::Personal,
            author: "octocat".to_string(),
            body: Some("Implements requested-review detail comments.".to_string()),
            comment_snapshot: None,
            display_id: display_id.to_string(),
            forge_kind: ForgeKind::GitHub,
            repository: "agentty-xyz/agentty".to_string(),
            status_summary: None,
            title: "Show requested-review comments".to_string(),
            updated_at: Some("2026-06-10T04:00:00Z".to_string()),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        }
    }

    /// Builds one review-comment snapshot fixture for task tests.
    fn review_comment_snapshot() -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: "Looks ready.".to_string(),
            }],
            threads: Vec::new(),
        }
    }

    /// Builds one unsorted review-comment snapshot fixture for task tests.
    fn unsorted_review_comment_snapshot() -> ReviewCommentSnapshot {
        ReviewCommentSnapshot {
            pr_level_comments: Vec::new(),
            threads: vec![
                review_comment_thread("crates/agentty/src/ui.rs"),
                review_comment_thread("crates/agentty/src/app.rs"),
            ],
        }
    }

    /// Builds one inline review-comment thread fixture for task tests.
    fn review_comment_thread(path: &str) -> ReviewCommentThread {
        ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "alice".to_string(),
                body: "Please check this.".to_string(),
            }],
            id: "thread-id".to_string(),
            is_outdated: Some(false),
            is_resolved: false,
            line: Some(7),
            path: path.to_string(),
            start_line: None,
        }
    }

    #[test]
    fn review_comment_anchor_side_order_places_file_before_old_and_new_lines() {
        // Arrange, Act
        let file_order = review_comment_anchor_side_order(ReviewCommentAnchorSide::File);
        let old_order = review_comment_anchor_side_order(ReviewCommentAnchorSide::Old);
        let new_order = review_comment_anchor_side_order(ReviewCommentAnchorSide::New);

        // Assert
        assert!(file_order < old_order);
        assert!(old_order < new_order);
    }

    #[test]
    /// Verifies that structured `AgentResponse` JSON is unwrapped to plain
    /// display text for focused review rendering.
    fn test_structured_agent_response_is_unwrapped_to_display_text() {
        // Arrange
        let structured_json = r#"{"answer":"Review looks good.","questions":[],"summary":null}"#;

        // Act
        let agent_response =
            parse_agent_response_strict(structured_json).expect("structured response should parse");
        let display_text = agent_response.to_display_text();

        // Assert
        assert_eq!(display_text.trim(), "Review looks good.");
    }

    #[test]
    /// Verifies that `UpdateStatusChanged` events for in-progress, complete,
    /// and failed states can be constructed and compared.
    fn update_status_changed_event_roundtrips_all_variants() {
        // Arrange / Act
        let in_progress = AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::InProgress {
                version: "v1.0.0".to_string(),
            },
        };
        let complete = AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::Complete {
                version: "v1.0.0".to_string(),
            },
        };
        let failed = AppEvent::UpdateStatusChanged {
            update_status: UpdateStatus::Failed {
                version: "v1.0.0".to_string(),
            },
        };

        // Assert
        assert_ne!(in_progress, complete);
        assert_ne!(complete, failed);
        assert_ne!(in_progress, failed);
    }
}
