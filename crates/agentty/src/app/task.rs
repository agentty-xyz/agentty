//! App-wide background task helpers for status polling, version checks, and
//! app-server turns.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use askama::Template;
use tokio::sync::mpsc;

use crate::app::AppEvent;
use crate::domain::agent::{AgentModel, ReasoningLevel};
use crate::infra::agent;
use crate::infra::git::GitClient;
#[cfg(not(test))]
use crate::version;

/// Stateless helpers for app-scoped background pollers and app-server
/// session execution.
pub(super) struct TaskService;

/// Inputs needed to generate review assist text in the background.
pub(super) struct FocusedReviewAssistTaskInput {
    pub(super) app_event_tx: mpsc::UnboundedSender<AppEvent>,
    pub(super) focused_review_diff: String,
    pub(super) review_model: AgentModel,
    pub(super) session_folder: PathBuf,
    pub(super) session_id: String,
    pub(super) session_summary: Option<String>,
}

/// Askama view model for rendering review assist prompts.
#[derive(Template)]
#[template(path = "review_assist_prompt.md", escape = "none")]
struct FocusedReviewAssistPromptTemplate<'a> {
    focused_review_diff: &'a str,
    session_summary: &'a str,
}

impl TaskService {
    /// Spawns a background loop that periodically refreshes ahead/behind info.
    ///
    /// The task emits [`AppEvent::GitStatusUpdated`] snapshots instead of
    /// mutating app state directly.
    pub(super) fn spawn_git_status_task(
        working_dir: &Path,
        cancel: Arc<AtomicBool>,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
        git_client: Arc<dyn GitClient>,
    ) {
        let dir = working_dir.to_path_buf();
        tokio::spawn(async move {
            let repo_root = git_client
                .find_git_repo_root(dir.clone())
                .await
                .unwrap_or(dir);
            loop {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }

                {
                    let root = repo_root.clone();
                    let _ = git_client.fetch_remote(root).await;
                }

                let status = {
                    let root = repo_root.clone();
                    git_client.get_ahead_behind(root).await.ok()
                };
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = app_event_tx.send(AppEvent::GitStatusUpdated { status });
                for _ in 0..30 {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        });
    }

    /// Spawns a one-shot background check for newer `agentty` versions on
    /// npmjs.
    ///
    /// The task emits [`AppEvent::VersionAvailabilityUpdated`] with
    /// `Some("vX.Y.Z")` only when a newer version is detected.
    ///
    /// In tests, it emits an immediate `None` update instead of spawning the
    /// network check so test runs stay deterministic and offline.
    pub(super) fn spawn_version_check_task(app_event_tx: &mpsc::UnboundedSender<AppEvent>) {
        #[cfg(test)]
        {
            let _ = app_event_tx.send(AppEvent::VersionAvailabilityUpdated {
                latest_available_version: None,
            });
        }

        #[cfg(not(test))]
        let app_event_tx = app_event_tx.clone();

        #[cfg(not(test))]
        tokio::spawn(async move {
            let latest_available_version =
                version::latest_npm_version_tag()
                    .await
                    .filter(|latest_version| {
                        version::is_newer_than_current_version(
                            env!("CARGO_PKG_VERSION"),
                            latest_version,
                        )
                    });

            let _ = app_event_tx.send(AppEvent::VersionAvailabilityUpdated {
                latest_available_version,
            });
        });
    }

    /// Spawns one background review assist generation task and emits
    /// an event with either final review text or a failure description.
    pub(super) fn spawn_focused_review_assist_task(input: FocusedReviewAssistTaskInput) {
        let FocusedReviewAssistTaskInput {
            app_event_tx,
            focused_review_diff,
            review_model,
            session_folder,
            session_id,
            session_summary,
        } = input;

        tokio::spawn(async move {
            let focused_review_result = Self::focused_review_assist_text(
                &session_folder,
                review_model,
                &focused_review_diff,
                session_summary.as_deref(),
            )
            .await;

            let app_event = match focused_review_result {
                Ok(review_text) => AppEvent::FocusedReviewPrepared {
                    review_text,
                    session_id,
                },
                Err(error) => AppEvent::FocusedReviewPreparationFailed { error, session_id },
            };
            let _ = app_event_tx.send(app_event);
        });
    }

    /// Generates review assist text by running one model command with
    /// read-only review constraints and parsing the final assistant response
    /// content.
    async fn focused_review_assist_text(
        session_folder: &Path,
        review_model: AgentModel,
        focused_review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, String> {
        let focused_review_prompt =
            Self::focused_review_assist_prompt(focused_review_diff, session_summary)?;
        let agent_response = agent::submit_one_shot(agent::OneShotRequest {
            child_pid: None,
            folder: session_folder,
            model: review_model,
            prompt: &focused_review_prompt,
            reasoning_level: ReasoningLevel::default(),
        })
        .await?;
        let focused_review_text = agent_response.to_display_text();
        let focused_review_text = focused_review_text.trim();
        if focused_review_text.is_empty() {
            return Err("Review assist returned empty output".to_string());
        }

        Ok(focused_review_text.to_string())
    }

    /// Renders the review assist prompt from the markdown template.
    ///
    /// # Errors
    /// Returns an error when Askama template rendering fails.
    fn focused_review_assist_prompt(
        focused_review_diff: &str,
        session_summary: Option<&str>,
    ) -> Result<String, String> {
        let template = FocusedReviewAssistPromptTemplate {
            focused_review_diff: focused_review_diff.trim(),
            session_summary: session_summary.map_or("", str::trim),
        };

        template
            .render()
            .map_err(|error| format!("Failed to render `review_assist_prompt.md`: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Ensures review prompt rendering includes read-only constraints
    /// while keeping internet and non-editing verification options available.
    fn test_focused_review_assist_prompt_enforces_read_only_constraints() {
        // Arrange
        let focused_review_diff = "diff --git a/src/lib.rs b/src/lib.rs";
        let session_summary = Some("Refactor parser error mapping.");

        // Act
        let prompt =
            TaskService::focused_review_assist_prompt(focused_review_diff, session_summary)
                .expect("review prompt should render");

        // Assert
        assert!(prompt.contains("You are in read-only review mode."));
        assert!(prompt.contains("Do not create, modify, rename, or delete files."));
        assert!(prompt.contains("You may browse the internet when needed."));
        assert!(prompt.contains("You may run non-editing CLI commands"));
    }

    #[test]
    /// Verifies that structured `AgentResponse` JSON is unwrapped to plain
    /// display text for focused review rendering.
    fn test_structured_agent_response_is_unwrapped_to_display_text() {
        // Arrange
        let structured_json = r#"{"messages":[{"type":"answer","text":"Review looks good."}]}"#;

        // Act
        let agent_response = agent::protocol::parse_agent_response(structured_json);
        let display_text = agent_response.to_display_text();

        // Assert
        assert_eq!(display_text.trim(), "Review looks good.");
    }
}
