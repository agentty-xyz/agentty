use tracing::warn;

use crate::app::{App, ReviewCacheEntry, SessionStatsUsage, diff_content_hash};
use crate::domain::agent::{AgentKind, AgentModel, ReasoningLevel};
use crate::domain::review;
use crate::domain::session::{SessionId, Status};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::{TurnPrompt, TurnPromptAttachment, TurnPromptTextSource};
use crate::ui::state::app_mode::AppMode;
use crate::ui::state::prompt::{
    PromptSlashStage, PromptSuggestionSelection, drain_prompt_submission,
    resolve_prompt_slash_selection,
};
use crate::ui::text_util::format_token_count;

/// Checked-in prompt body submitted by the `/qe:check` slash command.
const QE_CHECK_PROMPT: &str = include_str!("qe_check_prompt.md");

/// App-layer prompt intent context derived from prompt-mode runtime state.
///
/// Runtime key handling constructs this snapshot when a key maps to an
/// application workflow. The app layer then owns the execution decision:
/// prompt submission routing, slash-command selection, cancellation cleanup,
/// and view restoration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PromptIntentContext {
    /// Active prompt input shape used for app-layer submit/cancel routing.
    pub(crate) input_mode: PromptIntentInputMode,
    /// Session transcript scroll offset to restore when returning to view.
    pub(crate) scroll_offset: Option<u16>,
    /// Stable identifier for the active prompt session.
    pub(crate) session_id: SessionId,
    /// Current list index for the active prompt session.
    pub(crate) session_index: usize,
    /// Session lifecycle shape used for app-layer submit/cancel routing.
    pub(crate) session_mode: PromptIntentSessionMode,
}

impl PromptIntentContext {
    /// Returns whether `Esc` should delete the blank backing session instead
    /// of restoring session view.
    fn can_delete_on_cancel(&self) -> bool {
        self.session_mode == PromptIntentSessionMode::NewDeletable
    }

    /// Returns whether this prompt belongs to a draft session still staging
    /// messages.
    fn is_draft_session(&self) -> bool {
        self.session_mode == PromptIntentSessionMode::NewDraft
    }

    /// Returns whether this prompt belongs to a session that has not started
    /// yet.
    fn is_new_session(&self) -> bool {
        self.session_mode != PromptIntentSessionMode::Existing
    }

    /// Returns whether the active prompt input currently represents a slash
    /// command.
    fn is_slash_command(&self) -> bool {
        self.input_mode == PromptIntentInputMode::SlashCommand
    }
}

/// Active prompt input shape used by app-layer prompt intent routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptIntentInputMode {
    /// Prompt text starts with a slash-command prefix.
    SlashCommand,
    /// Prompt text is normal user input.
    Text,
}

/// Session lifecycle shape used by app-layer prompt intent routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptIntentSessionMode {
    /// Existing session receiving a follow-up reply.
    Existing,
    /// New non-draft session that can be deleted when prompt composition is
    /// canceled.
    NewDeletable,
    /// Draft-mode session that stages prompt text instead of starting a turn.
    NewDraft,
    /// New non-draft session that should be preserved on cancel because it
    /// has staged drafts.
    NewRegular,
}

impl App {
    /// Handles the submit intent emitted by prompt-mode key routing.
    ///
    /// Slash-command submissions are resolved inside the app layer. Text
    /// submissions drain the prompt composer, choose the correct session
    /// workflow, and return to session view after a non-empty prompt.
    pub(crate) async fn handle_prompt_submit_intent(&mut self, context: &PromptIntentContext) {
        if context.is_slash_command() {
            self.handle_prompt_slash_submit_intent(context).await;

            return;
        }

        let prompt = self.take_submitted_turn_prompt();
        if prompt.is_empty() {
            return;
        }

        if context.is_draft_session() {
            if let Err(error) = self.stage_draft_message(&context.session_id, prompt).await {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::Error.format(error),
                )
                .await;
            }
        } else if context.is_new_session() {
            if let Err(error) = self.start_session(&context.session_id, prompt).await {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::Error.format(error),
                )
                .await;
            }
        } else if self.session_is_in_progress(&context.session_id) {
            if let Err(error) = self.enqueue_message(&context.session_id, prompt) {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::QueueError.format(error),
                )
                .await;
            }
        } else {
            self.reply(&context.session_id, prompt).await;
        }

        self.mode = AppMode::View {
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: context.session_id.clone(),
        };
    }

    /// Executes the active slash-command selection for prompt mode.
    pub(crate) async fn handle_prompt_slash_submit_intent(
        &mut self,
        context: &PromptIntentContext,
    ) {
        let session_agent_kind = self
            .session_at(context.session_index)
            .map_or(AgentKind::Codex, |session| session.model.kind());
        let selection = match &self.mode {
            AppMode::Prompt {
                input, slash_state, ..
            } => resolve_prompt_slash_selection(
                input.text(),
                slash_state,
                session_agent_kind,
                self.prompt_apply_command_is_available_for_session(&context.session_id),
            ),
            _ => None,
        };

        match selection {
            Some(PromptSuggestionSelection::Command("/apply")) => {
                if self.handle_apply_prompt_command(context).await {
                    self.reset_prompt_slash_input();
                } else {
                    self.reset_prompt_slash_state();
                }
            }
            Some(PromptSuggestionSelection::Command("/stats")) => {
                self.reset_prompt_slash_input();
                self.handle_stats_prompt_command(context).await;
            }
            Some(PromptSuggestionSelection::Command("/qe:check")) => {
                self.reset_prompt_slash_input();
                self.handle_qe_check_prompt_command(context).await;
            }
            Some(PromptSuggestionSelection::Command("/reasoning")) => {
                let selected_reasoning_level = self
                    .session_at(context.session_index)
                    .map_or(self.settings.reasoning_level, |session| {
                        session.effective_reasoning_level(self.settings.reasoning_level)
                    });
                let selected_index = ReasoningLevel::ALL
                    .iter()
                    .position(|level| *level == selected_reasoning_level)
                    .unwrap_or(0);

                if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
                    slash_state.stage = PromptSlashStage::Reasoning;
                    slash_state.selected_agent = None;
                    slash_state.selected_index = selected_index;
                }
            }
            Some(PromptSuggestionSelection::Command(_)) => {
                if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
                    slash_state.stage = PromptSlashStage::Agent;
                    slash_state.selected_agent = None;
                    slash_state.selected_index = 0;
                }
            }
            Some(PromptSuggestionSelection::Agent(selected_agent)) => {
                if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
                    slash_state.selected_agent = Some(selected_agent);
                    slash_state.stage = PromptSlashStage::Model;
                    slash_state.selected_index = 0;
                }
            }
            Some(PromptSuggestionSelection::Model(selected_model)) => {
                self.reset_prompt_slash_input();
                self.update_prompt_session_model(context, selected_model)
                    .await;
            }
            Some(PromptSuggestionSelection::Reasoning(reasoning_level)) => {
                self.reset_prompt_slash_input();
                self.update_prompt_session_reasoning_level(context, reasoning_level)
                    .await;
            }
            None => {}
        }
    }

    /// Handles the cancel intent emitted by prompt-mode key routing.
    ///
    /// Slash-command cancellation only clears the slash input. Prompt
    /// cancellation cleans up composer-owned attachments, optionally deletes
    /// a blank backing session, and otherwise restores session view.
    pub(crate) async fn handle_prompt_cancel_intent(&mut self, context: &PromptIntentContext) {
        if context.is_slash_command() {
            self.reset_prompt_slash_input();

            return;
        }

        self.cleanup_prompt_attachment_state().await;

        if context.can_delete_on_cancel() {
            self.delete_selected_session_deferred_cleanup().await;
            self.mode = AppMode::List;

            return;
        }

        self.mode = AppMode::View {
            review_status_message: self.prompt_review_status_message(),
            review_text: self.prompt_review_text(),
            scroll_offset: context.scroll_offset,
            session_id: context.session_id.clone(),
        };
    }

    /// Returns whether the active prompt session has cached focused-review
    /// suggestions that make `/apply` selectable.
    pub(crate) fn prompt_apply_command_is_available(&self) -> bool {
        let Some(session_id) = self.prompt_session_id() else {
            return false;
        };

        self.prompt_apply_command_is_available_for_session(&session_id)
    }

    /// Drains the prompt composer into the structured turn payload sent to
    /// the session workflow.
    ///
    /// Attachments are filtered against the submitted text so manually
    /// deleted `[Image #n]` placeholders do not leave orphaned image inputs in
    /// the final turn payload.
    pub(crate) fn take_submitted_turn_prompt(&mut self) -> TurnPrompt {
        match &mut self.mode {
            AppMode::Prompt {
                attachment_state,
                input,
                ..
            } => {
                let submission = drain_prompt_submission(attachment_state, input);
                let attachments = submission
                    .attachments
                    .into_iter()
                    .map(|attachment| TurnPromptAttachment {
                        local_image_path: attachment.local_image_path,
                        placeholder: attachment.placeholder,
                    })
                    .collect();

                TurnPrompt {
                    attachments,
                    text: submission.text,
                    text_source: TurnPromptTextSource::UserPrompt,
                }
            }
            _ => TurnPrompt::from_text(String::new()),
        }
    }

    /// Returns whether the targeted session is currently `InProgress`, used
    /// to route non-slash submissions into the in-memory message queue
    /// instead of the live reply path.
    fn session_is_in_progress(&self, session_id: &str) -> bool {
        self.sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| session.status == Status::InProgress)
    }

    /// Clears the slash-command buffer after one prompt slash action is
    /// accepted.
    fn reset_prompt_slash_input(&mut self) {
        if let AppMode::Prompt {
            input, slash_state, ..
        } = &mut self.mode
        {
            input.take_text();
            slash_state.reset();
        }
    }

    /// Resets the slash-command menu without clearing the user's input text.
    fn reset_prompt_slash_state(&mut self) {
        if let AppMode::Prompt { slash_state, .. } = &mut self.mode {
            slash_state.reset();
        }
    }

    /// Persists one slash-selected model change and logs any failure with
    /// session context.
    async fn update_prompt_session_model(
        &mut self,
        context: &PromptIntentContext,
        selected_model: AgentModel,
    ) {
        if let Err(error) = self
            .set_session_model(&context.session_id, selected_model)
            .await
        {
            warn!(
                session_id = %context.session_id,
                model = %selected_model.as_str(),
                error = %error,
                "failed to switch session model from prompt slash command"
            );
        }
    }

    /// Persists one slash-selected reasoning override and logs any failure
    /// with session context.
    async fn update_prompt_session_reasoning_level(
        &mut self,
        context: &PromptIntentContext,
        reasoning_level: ReasoningLevel,
    ) {
        if let Err(error) = self
            .set_session_reasoning_level(&context.session_id, Some(reasoning_level))
            .await
        {
            warn!(
                session_id = %context.session_id,
                reasoning_level = ?reasoning_level,
                error = %error,
                "failed to update session reasoning level from prompt slash command"
            );
        }
    }

    /// Returns the preserved focused-review status text stored in prompt
    /// mode.
    fn prompt_review_status_message(&self) -> Option<String> {
        match &self.mode {
            AppMode::Prompt {
                review_status_message,
                ..
            } => review_status_message.clone(),
            _ => None,
        }
    }

    /// Returns the preserved focused-review output stored in prompt mode.
    fn prompt_review_text(&self) -> Option<String> {
        match &self.mode {
            AppMode::Prompt { review_text, .. } => review_text.clone(),
            _ => None,
        }
    }

    /// Drops the preserved focused-review text and status message held in
    /// prompt mode so a subsequent cancel cannot restore invalidated review
    /// content.
    fn clear_prompt_review_state(&mut self) {
        if let AppMode::Prompt {
            review_status_message,
            review_text,
            ..
        } = &mut self.mode
        {
            *review_status_message = None;
            *review_text = None;
        }
    }

    /// Returns the active prompt session id without mutating prompt state.
    fn prompt_session_id(&self) -> Option<SessionId> {
        match &self.mode {
            AppMode::Prompt { session_id, .. } => Some(session_id.clone()),
            _ => None,
        }
    }

    /// Returns whether cached focused-review text contains actionable
    /// suggestions for one session.
    fn prompt_apply_command_is_available_for_session(&self, session_id: &str) -> bool {
        let Some(ReviewCacheEntry::Ready { text, .. }) = self.review_cache.get(session_id) else {
            return false;
        };

        review::has_actionable_review_suggestions(Some(text))
    }

    /// Appends one prompt-mode status line to the session transcript shown
    /// above the composer.
    async fn append_prompt_status_line(&self, session_id: &str, label: &str, message: &str) {
        self.append_output_for_session(session_id, &format!("\n[{label}] {message}\n"))
            .await;
    }

    /// Removes any prompt attachment files still owned by the active composer
    /// and resets attachment state before leaving prompt mode.
    async fn cleanup_prompt_attachment_state(&mut self) {
        let prompt = match &mut self.mode {
            AppMode::Prompt {
                attachment_state, ..
            } => {
                let attachments = attachment_state
                    .attachments
                    .iter()
                    .map(|attachment| TurnPromptAttachment {
                        local_image_path: attachment.local_image_path.clone(),
                        placeholder: attachment.placeholder.clone(),
                    })
                    .collect::<Vec<_>>();
                attachment_state.reset();

                TurnPrompt {
                    attachments,
                    text: String::new(),
                    text_source: TurnPromptTextSource::UserPrompt,
                }
            }
            _ => return,
        };

        self.cleanup_prompt_attachment_files(&prompt).await;
    }

    /// Handles `/apply` by extracting suggestions from the focused review and
    /// submitting them as a verification-gated prompt to the agent.
    ///
    /// Returns `true` when the command consumed the slash-command input. A
    /// validation failure that leaves actionable cached suggestions available
    /// returns `false`, preserving the visible `/apply` text for correction or
    /// retry.
    async fn handle_apply_prompt_command(&mut self, context: &PromptIntentContext) -> bool {
        let Some((session_status, session_folder, base_branch)) =
            self.session_at(context.session_index).map(|session| {
                (
                    session.status,
                    session.folder.clone(),
                    session.base_branch.clone(),
                )
            })
        else {
            return false;
        };

        if session_status != Status::Review {
            self.append_prompt_status_line(
                &context.session_id,
                "Apply",
                "Apply is only available after a focused review completes (session status must be \
                 Review).",
            )
            .await;

            return true;
        }

        let (cached_hash, cached_text) = if let Some(ReviewCacheEntry::Ready { diff_hash, text }) =
            self.review_cache.get(context.session_id.as_str())
        {
            (*diff_hash, text.clone())
        } else {
            self.append_prompt_status_line(
                &context.session_id,
                "Apply",
                "No actionable suggestions available. Run a focused review first (f key).",
            )
            .await;

            return true;
        };

        let current_diff = match self
            .services
            .git_client()
            .diff(session_folder, base_branch)
            .await
        {
            Ok(diff) => diff,
            Err(err) => {
                self.append_prompt_status_line(
                    &context.session_id,
                    "Apply",
                    &format!(
                        "Failed to read worktree diff: {err}. Review cache preserved; try /apply \
                         again."
                    ),
                )
                .await;

                return true;
            }
        };
        let current_hash = diff_content_hash(&current_diff);

        if current_hash != cached_hash {
            self.review_cache.remove(context.session_id.as_str());
            self.clear_prompt_review_state();
            self.append_prompt_status_line(
                &context.session_id,
                "Apply",
                "Review is stale; the worktree changed since it was generated. Run focused review \
                 again (f key).",
            )
            .await;

            return true;
        }

        let Some(suggestions) = review::review_suggestions(&cached_text) else {
            self.append_prompt_status_line(
                &context.session_id,
                "Apply",
                "No actionable suggestions found in the current review.",
            )
            .await;

            return false;
        };

        let prompt = build_apply_review_prompt(&suggestions);

        self.cleanup_prompt_attachment_state().await;
        self.review_cache.remove(context.session_id.as_str());
        self.reply(&context.session_id, prompt).await;

        self.mode = AppMode::View {
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: context.session_id.clone(),
        };

        true
    }

    /// Handles `/stats` by loading stats through the app layer and appending
    /// the rendered output to the session transcript.
    async fn handle_stats_prompt_command(&self, context: &PromptIntentContext) {
        let session_stats = self.stats_for_session(&context.session_id).await;
        let session_time = session_stats
            .session_duration_seconds
            .map_or_else(|| "Unavailable".to_string(), format_duration);
        let usage_rows_result = build_token_usage_rows(session_stats.usage_rows_result);
        let stats_output =
            build_stats_markdown(&context.session_id, &session_time, usage_rows_result);

        self.append_output_for_session(&context.session_id, &stats_output)
            .await;
    }

    /// Handles `/qe:check` by submitting the checked-in quality-enforcement
    /// prompt as the next agent turn.
    async fn handle_qe_check_prompt_command(&mut self, context: &PromptIntentContext) {
        let prompt = build_qe_check_prompt();

        self.cleanup_prompt_attachment_state().await;

        if context.is_draft_session() {
            if let Err(error) = self.stage_draft_message(&context.session_id, prompt).await {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::Error.format(error),
                )
                .await;
            }
        } else if context.is_new_session() {
            if let Err(error) = self.start_session(&context.session_id, prompt).await {
                self.append_output_for_session(
                    &context.session_id,
                    &TranscriptNotice::Error.format(error),
                )
                .await;
            }
        } else {
            self.reply(&context.session_id, prompt).await;
        }

        self.mode = AppMode::View {
            review_status_message: None,
            review_text: None,
            scroll_offset: None,
            session_id: context.session_id.clone(),
        };
    }
}

/// Builds the agent-facing `/qe:check` prompt from the checked-in markdown
/// template.
pub(crate) fn build_qe_check_prompt() -> TurnPrompt {
    TurnPrompt::from_text(QE_CHECK_PROMPT.trim_end().to_string())
}

/// Builds the agent-facing `/apply` prompt from focused-review suggestions.
///
/// The prompt explicitly asks the agent to verify each suggestion against the
/// current code before making changes, then apply only suggestions that remain
/// correct and relevant.
pub(crate) fn build_apply_review_prompt(suggestions: &str) -> TurnPrompt {
    TurnPrompt::from_text(format!(
        "Verify the following focused-review suggestions against the current code before changing \
         anything. Apply only the suggestions that are still correct and relevant; explain any \
         suggestions you leave unapplied.\n\n{suggestions}"
    ))
}

/// One rendered token-usage row used by the `/stats` command transcript.
pub(crate) struct TokenUsageRow {
    /// Formatted input-token count.
    pub(crate) in_tokens: String,
    /// Provider model name.
    pub(crate) model: String,
    /// Formatted output-token count.
    pub(crate) out_tokens: String,
}

/// Converts loaded token usage data into rendered table rows.
pub(crate) fn build_token_usage_rows(
    usage_rows_result: Result<Vec<SessionStatsUsage>, String>,
) -> Result<Vec<TokenUsageRow>, String> {
    match usage_rows_result {
        Ok(usage_rows) => {
            let rows = usage_rows
                .into_iter()
                .map(|row| TokenUsageRow {
                    in_tokens: format_token_count(row.input_tokens),
                    model: row.model,
                    out_tokens: format_token_count(row.output_tokens),
                })
                .collect();

            Ok(rows)
        }
        Err(error) => Err(error),
    }
}

/// Renders one `/stats` transcript block.
pub(crate) fn build_stats_markdown(
    session_id: &str,
    session_time: &str,
    usage_rows_result: Result<Vec<TokenUsageRow>, String>,
) -> String {
    let mut lines = vec![
        format_stats_metric_line("Session ID", session_id),
        format_stats_metric_line("Session Time", session_time),
        String::new(),
        "Tokens Usage".to_string(),
    ];

    lines.extend(build_token_usage_lines(usage_rows_result));

    format!(
        "\n## Session Stats\n\n```stats\n{}\n```\n",
        lines.join("\n")
    )
}

/// Renders one tab-delimited metric line for the `/stats` block.
pub(crate) fn format_stats_metric_line(metric: &str, value: &str) -> String {
    format!("{metric}\t{value}")
}

/// Renders token usage lines or a compact fallback message.
pub(crate) fn build_token_usage_lines(
    usage_rows_result: Result<Vec<TokenUsageRow>, String>,
) -> Vec<String> {
    match usage_rows_result {
        Ok(usage_rows) if usage_rows.is_empty() => vec!["No token usage recorded.".to_string()],
        Ok(usage_rows) => render_token_usage_table_lines(&usage_rows),
        Err(error) => vec![
            "Usage unavailable.".to_string(),
            format_stats_metric_line("Error", &error),
        ],
    }
}

/// Renders the aligned token usage table body without box-drawing
/// characters.
pub(crate) fn render_token_usage_table_lines(usage_rows: &[TokenUsageRow]) -> Vec<String> {
    let model_width = usage_rows
        .iter()
        .map(|row| row.model.chars().count())
        .max()
        .unwrap_or_default()
        .max("Model".chars().count());
    let in_width = usage_rows
        .iter()
        .map(|row| row.in_tokens.chars().count())
        .max()
        .unwrap_or_default()
        .max("In".chars().count());
    let out_width = usage_rows
        .iter()
        .map(|row| row.out_tokens.chars().count())
        .max()
        .unwrap_or_default()
        .max("Out".chars().count());

    let mut lines = vec![format!(
        "{:<model_width$}  {:>in_width$}  {:>out_width$}",
        "Model", "In", "Out"
    )];

    lines.extend(usage_rows.iter().map(|row| {
        format!(
            "{:<model_width$}  {:>in_width$}  {:>out_width$}",
            row.model, row.in_tokens, row.out_tokens
        )
    }));

    lines
}

/// Formats elapsed seconds as `HH:MM:SS` for `/stats` output.
pub(crate) fn format_duration(total_seconds: i64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies `/qe:check` submits the checked-in markdown prompt without
    /// attachments or generated-data transport semantics.
    #[test]
    fn test_build_qe_check_prompt_uses_checked_in_template() {
        // Arrange, Act
        let prompt = build_qe_check_prompt();

        // Assert
        assert!(prompt.text.starts_with("# /qe:check"));
        assert!(prompt.text.contains("change repository state"));
        assert!(prompt.text.contains("Agent instruction files"));
        assert!(prompt.text.contains("Suggested Next Prompt"));
        assert!(prompt.attachments.is_empty());
        assert_eq!(prompt.text_source, TurnPromptTextSource::UserPrompt);
    }
}
