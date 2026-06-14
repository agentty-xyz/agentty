//! App-wide background task helpers for requested-review loads, version
//! checks, and review-assist generation.
//!
//! Recurring git-status and review-request polling lives in the sync
//! orchestrator (`app/sync.rs`); this module keeps one-shot background tasks.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use ag_forge::{ForgeRemote, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewRequestClient};
use askama::Template;
use tokio::sync::mpsc;

use crate::app::error::AppError;
use crate::app::{AppEvent, UpdateStatus};
use crate::domain::agent::{AgentCliInfo, AgentKind, AgentModel, ReasoningLevel};
use crate::domain::session::SessionId;
use crate::domain::system_log::{SystemLogCategory, SystemLogEvent, SystemLogLevel};
use crate::infra::agent;
use crate::infra::git::GitClient;
use crate::version;

/// Stateless helpers for app-scoped one-shot background tasks and app-server
/// session execution.
pub(super) struct TaskService;

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

/// Inputs needed to generate review assist text in the background.
pub(super) struct ReviewAssistTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Hash of the diff that triggered this review, threaded back in the
    /// completion event so the reducer can store it without re-reading cache.
    pub(super) diff_hash: u64,
    pub(super) review_diff: String,
    pub(super) review_model: AgentModel,
    pub(super) session_folder: PathBuf,
    pub(super) session_id: SessionId,
    pub(super) session_summary: Option<String>,
}

/// Askama view model for rendering review assist prompts.
#[derive(Template)]
#[template(path = "review_assist_prompt.md", escape = "none")]
struct ReviewAssistPromptTemplate<'a> {
    /// Full diff payload wrapped in a Markdown fence sized for its content.
    fenced_diff: &'a str,
    /// Prior session summary used to give the review model continuity.
    session_summary: &'a str,
}

impl TaskService {
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
    /// and their versions behind the injected availability boundary.
    pub(super) async fn load_agent_cli_availability(
        availability_probe: Arc<dyn agent::AgentAvailabilityProbe>,
        fallback_agent_kinds: Vec<AgentKind>,
    ) -> Vec<AgentCliInfo> {
        tokio::task::spawn_blocking(move || availability_probe.available_agent_clis())
            .await
            .unwrap_or_else(|_| AgentCliInfo::from_kinds(&fallback_agent_kinds))
    }

    /// Spawns background agent CLI version detection and emits the completed
    /// snapshot through the app event bus.
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
            let _ = app_event_tx.send(AppEvent::SystemLog {
                event: SystemLogEvent::new(
                    SystemLogLevel::Info,
                    SystemLogCategory::Forge,
                    "Requested reviews forge query started",
                )
                .with_detail(format!("project #{project_id}")),
            });
            let result = load_requested_reviews(
                working_dir,
                git_client.as_ref(),
                review_request_client.as_ref(),
            )
            .await;
            let event = match &result {
                Ok(items) => SystemLogEvent::new(
                    SystemLogLevel::Success,
                    SystemLogCategory::Forge,
                    "Requested reviews forge query completed",
                )
                .with_detail(format!("project #{project_id}: {} reviews", items.len())),
                Err(error) => SystemLogEvent::new(
                    SystemLogLevel::Warning,
                    SystemLogCategory::Forge,
                    "Requested reviews forge query failed",
                )
                .with_detail(format!("project #{project_id}: {error}")),
            };
            let _ = app_event_tx.send(AppEvent::SystemLog { event });
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

    /// Loads and normalizes the comment snapshot for one requested review.
    ///
    /// This is triggered lazily when users open a specific review detail page
    /// so the top-level Review tab does not perform one comment API request
    /// per listed PR or MR.
    pub(super) async fn load_requested_review_comment_snapshot(
        working_dir: PathBuf,
        display_id: String,
        git_client: &dyn GitClient,
        review_request_client: &dyn ReviewRequestClient,
    ) -> Result<ReviewCommentSnapshot, String> {
        let remote = review_request_remote(working_dir, git_client, review_request_client).await?;

        review_request_client
            .fetch_review_comment_snapshot(remote, display_id)
            .await
            .map(sorted_review_comment_snapshot)
            .map_err(|error| error.detail_message())
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
        let ReviewAssistTaskInput {
            app_event_tx,
            diff_hash,
            review_diff,
            review_model,
            session_folder,
            session_id,
            session_summary,
        } = input;

        tokio::spawn(async move {
            let review_result = Self::review_assist_text(
                &session_folder,
                review_model,
                &review_diff,
                session_summary.as_deref(),
            )
            .await;

            let app_event = Self::review_app_event(diff_hash, review_result, session_id);
            // Fire-and-forget: receiver may be dropped during shutdown.
            let _ = app_event_tx.send(app_event);
        });
    }

    /// Generates review assist text by running one model command with
    /// read-only review constraints and parsing the final assistant response
    /// content.
    async fn review_assist_text(
        session_folder: &Path,
        review_model: AgentModel,
        review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, AppError> {
        Self::review_assist_text_with_submitter(
            session_folder,
            review_model,
            review_diff,
            session_summary,
            |review_folder, review_model, review_prompt| {
                Box::pin(async move {
                    agent::submit_one_shot(agent::OneShotRequest {
                        child_pid: None,
                        folder: review_folder,
                        model: review_model,
                        prompt: review_prompt,
                        request_kind: crate::infra::channel::AgentRequestKind::UtilityPrompt,
                        reasoning_level: ReasoningLevel::default(),
                    })
                    .await
                })
            },
        )
        .await
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

    /// Generates review assist text using an injected one-shot submitter so
    /// failure paths can be tested without subprocess execution.
    async fn review_assist_text_with_submitter<Submitter>(
        session_folder: &Path,
        review_model: AgentModel,
        review_diff: &str,
        session_summary: Option<&str>,
        submitter: Submitter,
    ) -> Result<String, AppError>
    where
        Submitter: for<'submit> FnOnce(
            &'submit Path,
            AgentModel,
            &'submit str,
        ) -> Pin<
            Box<dyn Future<Output = Result<agent::AgentResponse, String>> + Send + 'submit>,
        >,
    {
        let review_prompt = Self::review_assist_prompt(review_diff, session_summary)?;
        let agent_response = submitter(session_folder, review_model, &review_prompt)
            .await
            .map_err(AppError::Workflow)?;

        Self::review_output_text(&agent_response)
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
    fn review_output_text(agent_response: &agent::AgentResponse) -> Result<String, AppError> {
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
        session_summary: Option<&str>,
    ) -> Result<String, AppError> {
        let trimmed_diff = review_diff.trim();
        let fence = agent::diff_fence(trimmed_diff);
        let fenced_diff = format!("{fence}diff\n{trimmed_diff}\n{fence}");
        let template = ReviewAssistPromptTemplate {
            fenced_diff: &fenced_diff,
            session_summary: session_summary.map_or("", str::trim),
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
        ForgeKind, MockReviewRequestClient, RequestedReview, RequestedReviewAudience,
        ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
    };

    use super::*;
    use crate::infra::agent::protocol::AgentResponse;
    use crate::infra::git::MockGitClient;

    struct PanickingAgentAvailabilityProbe;

    impl crate::infra::agent::AgentAvailabilityProbe for PanickingAgentAvailabilityProbe {
        fn available_agent_kinds(&self) -> Vec<AgentKind> {
            vec![AgentKind::Claude]
        }

        fn available_agent_clis(&self) -> Vec<AgentCliInfo> {
            std::panic::resume_unwind(Box::new("version probe failed".to_string()));
        }
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
    /// Ensures CLI version fallback rows preserve the startup-discovered
    /// availability subset when the blocking probe panics.
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
    /// Ensures review assist surfaces one-shot submission failures as
    /// [`AppError::Workflow`] without invoking a real subprocess.
    async fn review_assist_text_with_submitter_returns_workflow_error_on_submit_failure() {
        // Arrange
        let session_folder = Path::new("/tmp/review-assist-submit-error");
        let review_model = AgentModel::ClaudeSonnet46;
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";

        // Act
        let result = TaskService::review_assist_text_with_submitter(
            session_folder,
            review_model,
            review_diff,
            None,
            |_, _, _| Box::pin(async { Err("submit failed".to_string()) }),
        )
        .await;

        // Assert
        let error = result.expect_err("submit failure should be returned");
        assert!(
            matches!(error, AppError::Workflow(_)),
            "expected AppError::Workflow, got: {error:?}"
        );
        assert_eq!(error.to_string(), "submit failed");
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
    /// Ensures review prompt rendering includes read-only constraints
    /// while keeping internet and non-editing verification options available.
    fn test_review_assist_prompt_enforces_read_only_constraints() {
        // Arrange
        let review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let session_summary = Some("Refactor parser error mapping.");

        // Act
        let prompt = TaskService::review_assist_prompt(review_diff, session_summary)
            .expect("review prompt should render");

        // Assert
        assert!(prompt.contains("put the Markdown review body in `answer`"));
        assert!(prompt.contains("leave `questions` empty"));
        assert!(prompt.contains("set `summary` to null"));
        assert!(!prompt.contains("Return Markdown only."));
        assert!(prompt.contains("You are in read-only review mode."));
        assert!(prompt.contains("Do not create, modify, rename, or delete files."));
        assert!(prompt.contains("You may browse the internet when needed."));
        assert!(prompt.contains("You may run non-editing CLI commands"));
        assert!(prompt.contains("Use the surrounding Agentty protocol for file-reference"));
        let fenced_diff = format!("```diff\n{review_diff}\n```");
        assert!(
            prompt.contains(&fenced_diff),
            "review prompt must wrap the diff in a ```diff``` fence so `@`-prefixed decorator \
             tokens are not misread as file mentions"
        );
        assert!(prompt.contains("`@`-prefixed tokens inside the diff"));
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
        let session_summary: Option<&str> = None;

        // Act
        let prompt = TaskService::review_assist_prompt(review_diff, session_summary)
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
            is_outdated: Some(false),
            is_resolved: false,
            line: Some(7),
            path: path.to_string(),
            start_line: None,
        }
    }

    #[test]
    /// Verifies that structured `AgentResponse` JSON is unwrapped to plain
    /// display text for focused review rendering.
    fn test_structured_agent_response_is_unwrapped_to_display_text() {
        // Arrange
        let structured_json = r#"{"answer":"Review looks good.","questions":[],"summary":null}"#;

        // Act
        let agent_response = agent::protocol::parse_agent_response_strict(structured_json)
            .expect("structured response should parse");
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
