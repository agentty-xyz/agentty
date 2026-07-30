//! Multi-session orchestration planning, reconciliation, and fan-in.
//!
//! Controller turns persist validated plans before approval. A background
//! coordinator reads child state directly from `SQLite` and uses the foreground
//! session runtime only for mutations, keeping polling off the bounded actor
//! mailbox used by interactive commands.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ag_protocol::{AgentResponse, QuestionItem, SubtaskItem, TurnPrompt, TurnPromptTextSource};
use ag_session::{
    CoordinatorMessageRequest, CreateSessionMode, CreateSessionRequest, QuestionAnswer, SessionId,
    SessionRole, SessionService, SessionStatus,
};
use askama::Template;
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::warn;

use crate::app::AppEvent;
use crate::app::session::session_branch;
use crate::domain::orchestration::{OrchestrationStatus, OrchestrationTaskStatus};
use crate::domain::setting::{
    DEFAULT_ORCHESTRATION_PARALLELISM, MAX_ORCHESTRATION_PARALLELISM, SettingName,
};
use crate::infra::db::{
    AppRepositories, DbError, OrchestrationRepository, PersistedOrchestrationTask,
    SessionOrchestrationMetadataRow, SessionOrchestrationRow, SessionOrchestrationTaskRow,
};

/// Exact question text used to recognize orchestration approval answers.
pub(crate) const APPROVAL_QUESTION: &str = "Approve this orchestration plan?";
/// Maximum child summary length persisted into a roll-up.
const RESULT_SUMMARY_MAX_CHARS: usize = 800;

/// Askama view model for controller turns.
#[derive(Template)]
#[template(path = "orchestrator_controller_prompt.md", escape = "none")]
struct OrchestratorControllerPromptTemplate<'a> {
    prompt: &'a str,
}

/// Askama view model for child first turns.
#[derive(Template)]
#[template(path = "orchestration_child_prompt.md", escape = "none")]
struct OrchestrationChildPromptTemplate<'a> {
    prompt: &'a str,
    task_key: &'a str,
    title: &'a str,
    touched_areas: &'a str,
}

/// Derived list metadata for one controller or child session.
#[derive(Clone, Default)]
pub(crate) struct OrchestrationSessionMetadata {
    pub(crate) controller_session_id: Option<SessionId>,
    pub(crate) progress: Option<String>,
}

/// Applies controller-only instructions to a turn prompt.
pub(crate) async fn controller_prompt(
    db: &AppRepositories,
    session_id: &str,
    prompt: TurnPrompt,
) -> TurnPrompt {
    let is_orchestrator = db
        .sessions()
        .load_session(session_id)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.role)
        .and_then(|role| role.parse::<SessionRole>().ok())
        == Some(SessionRole::Orchestrator);
    if !is_orchestrator {
        return prompt;
    }

    let agent_prompt = prompt.agent_text();
    let rendered = OrchestratorControllerPromptTemplate {
        prompt: &agent_prompt,
    }
    .render()
    .unwrap_or(agent_prompt);

    TurnPrompt {
        attachments: prompt.attachments,
        text: rendered,
        text_source: TurnPromptTextSource::AgentData,
    }
}

/// Persists a validated controller plan before the turn parks for approval.
pub(crate) async fn persist_controller_plan(
    db: &AppRepositories,
    controller_session_id: &str,
    response: &mut AgentResponse,
) -> Result<(), DbError> {
    let is_orchestrator = db
        .sessions()
        .load_session(controller_session_id)
        .await?
        .and_then(|row| row.role)
        .and_then(|role| role.parse::<SessionRole>().ok())
        == Some(SessionRole::Orchestrator);
    if !is_orchestrator {
        return Ok(());
    }

    let existing = db
        .orchestrations()
        .load_orchestration_for_controller(controller_session_id)
        .await?;
    if existing.as_ref().is_some_and(|orchestration| {
        orchestration
            .status
            .parse::<OrchestrationStatus>()
            .is_ok_and(OrchestrationStatus::is_active)
    }) {
        response.subtasks.clear();
        response
            .questions
            .retain(|question| question.text != APPROVAL_QUESTION);

        return Ok(());
    }
    if response.subtasks.is_empty() {
        return Ok(());
    }

    let subtasks = response.subtask_items();
    let retry_orchestration_id =
        reusable_retry_orchestration_id(db, existing.as_ref(), &subtasks).await?;
    if let Err(reason) = validate_subtasks(&subtasks, retry_orchestration_id.is_some()) {
        response.subtasks.clear();
        response.questions = vec![QuestionItem::new(format!(
            "The orchestration plan cannot run yet: {reason} Revise the plan?"
        ))];

        return Ok(());
    }

    let orchestration_id = if let Some(retry_orchestration_id) = retry_orchestration_id {
        retry_orchestration_id
    } else {
        db.orchestrations()
            .insert_orchestration(
                controller_session_id,
                &OrchestrationStatus::AwaitingApproval.to_string(),
                load_max_parallelism(db).await,
            )
            .await?
    };
    db.orchestrations()
        .update_orchestration_status(
            orchestration_id,
            &OrchestrationStatus::AwaitingApproval.to_string(),
        )
        .await?;

    for subtask in subtasks {
        db.orchestrations()
            .upsert_orchestration_task(PersistedOrchestrationTask {
                prompt: subtask.prompt,
                session_orchestration_id: orchestration_id,
                task_key: subtask.task_key,
                title: subtask.title,
                touched_areas: serde_json::to_string(&subtask.touched_areas)
                    .unwrap_or_else(|_| "[]".to_string()),
            })
            .await?;
    }

    response.questions = vec![QuestionItem::with_options(
        APPROVAL_QUESTION,
        vec!["Approve".to_string(), "Revise".to_string()],
    )];

    Ok(())
}

/// Applies approval or revision intent to the latest parked plan.
pub(crate) async fn apply_plan_answer(
    db: &AppRepositories,
    controller_session_id: &str,
    answers: &[QuestionAnswer],
) -> Result<(), DbError> {
    let Some(orchestration) = db
        .orchestrations()
        .load_orchestration_for_controller(controller_session_id)
        .await?
    else {
        return Ok(());
    };
    if orchestration.status != OrchestrationStatus::AwaitingApproval.to_string()
        || !answers
            .iter()
            .any(|answer| answer.question == APPROVAL_QUESTION)
    {
        return Ok(());
    }

    let approved = answers
        .iter()
        .filter(|answer| answer.question == APPROVAL_QUESTION)
        .any(|answer| answer.answer.trim().eq_ignore_ascii_case("approve"));
    let next = if approved {
        OrchestrationStatus::Running
    } else {
        OrchestrationStatus::Canceled
    };
    db.orchestrations()
        .update_orchestration_status(orchestration.id, &next.to_string())
        .await
}

/// Bulk-loads controller-child adjacency and controller progress for one
/// project's session-list refresh.
pub(crate) async fn session_metadata_for_project(
    db: &AppRepositories,
    project_id: i64,
) -> HashMap<String, OrchestrationSessionMetadata> {
    db.orchestrations()
        .load_session_metadata_for_project(project_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            let session_id = row.session_id.clone();

            (session_id, session_metadata_from_row(row))
        })
        .collect()
}

/// Returns the active child count shown in cascade-cancel confirmation.
pub(crate) async fn running_child_count(
    db: &AppRepositories,
    controller_session_id: &str,
) -> usize {
    let Ok(Some(orchestration)) = db
        .orchestrations()
        .load_orchestration_for_controller(controller_session_id)
        .await
    else {
        return 0;
    };
    let tasks = db
        .orchestrations()
        .load_orchestration_tasks(orchestration.id)
        .await
        .unwrap_or_default();

    let mut active_child_count = 0;
    for task in tasks.into_iter().filter(|task| {
        task.status
            .parse::<OrchestrationTaskStatus>()
            .is_ok_and(OrchestrationTaskStatus::occupies_parallelism_slot)
    }) {
        let has_child = if task.child_session_id.is_some() {
            true
        } else {
            db.orchestrations()
                .load_child_session_id_for_task(task.id)
                .await
                .is_ok_and(|child_session_id| child_session_id.is_some())
        };
        active_child_count += usize::from(has_child);
    }

    active_child_count
}

/// Runtime-owned schedule that wakes orchestration reconciliation.
#[async_trait]
pub(crate) trait OrchestrationSchedule: Send {
    /// Waits until the coordinator should reconcile its next persisted
    /// snapshot.
    async fn wait_for_reconciliation(&mut self);
}

/// Reconciles all active orchestrations while the foreground runtime is
/// available.
pub(crate) struct OrchestrationCoordinator {
    event_tx: mpsc::UnboundedSender<AppEvent>,
    live_statuses: Mutex<HashMap<i64, String>>,
    repository: Arc<dyn OrchestrationRepository>,
    session_service: SessionService,
}

impl OrchestrationCoordinator {
    /// Creates a coordinator from cloneable persistence and session ports.
    pub(crate) fn new(
        event_tx: mpsc::UnboundedSender<AppEvent>,
        repository: Arc<dyn OrchestrationRepository>,
        session_service: SessionService,
    ) -> Self {
        Self {
            event_tx,
            live_statuses: Mutex::new(HashMap::new()),
            repository,
            session_service,
        }
    }

    /// Runs reconciliation on an injected schedule until the terminal loop
    /// cancels the task.
    pub(crate) async fn run(self, mut schedule: impl OrchestrationSchedule) {
        loop {
            schedule.wait_for_reconciliation().await;
            if let Err(error) = self.reconcile_once().await {
                warn!(%error, "orchestration reconciliation failed");
            }
        }
    }

    /// Reconciles one snapshot of every active orchestration.
    pub(crate) async fn reconcile_once(&self) -> Result<(), String> {
        let orchestrations = self
            .repository
            .load_active_orchestrations()
            .await
            .map_err(|error| error.to_string())?;
        for orchestration in orchestrations {
            if orchestration.status == OrchestrationStatus::Running.to_string() {
                self.reconcile_orchestration(&orchestration).await?;

                continue;
            }
            if orchestration.status == OrchestrationStatus::Submitting.to_string() {
                let tasks = self
                    .repository
                    .load_orchestration_tasks(orchestration.id)
                    .await
                    .map_err(|error| error.to_string())?;
                self.reconcile_rollup(&orchestration, &tasks).await?;

                continue;
            }
            if orchestration.status == OrchestrationStatus::Canceling.to_string() {
                self.reconcile_cancellation(&orchestration).await?;
            }
        }

        Ok(())
    }

    async fn reconcile_cancellation(
        &self,
        orchestration: &SessionOrchestrationRow,
    ) -> Result<(), String> {
        let mut tasks = self
            .repository
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| error.to_string())?;
        let mut first_cancellation_error = None;
        for task in &mut tasks {
            if task_status(task).is_some_and(OrchestrationTaskStatus::is_settled) {
                continue;
            }
            if child_session_is_stopped(task.child_status.as_deref()) {
                self.update_task_status(task, OrchestrationTaskStatus::Canceled, None)
                    .await?;

                continue;
            }

            let child_session_id = if task.child_session_id.is_some() {
                task.child_session_id.clone()
            } else {
                self.repository
                    .load_child_session_id_for_task(task.id)
                    .await
                    .map_err(|error| error.to_string())?
            };
            if let Some(child_session_id) = child_session_id {
                let child_session_id = SessionId::from(child_session_id);
                if let Err(error) = self.session_service.cancel_session(&child_session_id).await {
                    first_cancellation_error.get_or_insert_with(|| error.to_string());

                    continue;
                }
            }
            self.update_task_status(task, OrchestrationTaskStatus::Canceled, None)
                .await?;
        }
        if let Some(error) = first_cancellation_error {
            return Err(error);
        }
        if tasks
            .iter()
            .all(|task| task_status(task).is_some_and(OrchestrationTaskStatus::is_settled))
        {
            self.repository
                .update_orchestration_status(
                    orchestration.id,
                    &OrchestrationStatus::Canceled.to_string(),
                )
                .await
                .map_err(|error| error.to_string())?;
            self.clear_live_status(orchestration);
            let _ = self.event_tx.send(AppEvent::RefreshSessions);
        }

        Ok(())
    }

    async fn reconcile_orchestration(
        &self,
        orchestration: &SessionOrchestrationRow,
    ) -> Result<(), String> {
        let mut tasks = self
            .repository
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| error.to_string())?;
        for task in &mut tasks {
            self.reconcile_task(task).await?;
        }

        let occupied_slots = tasks
            .iter()
            .filter(|task| {
                task_status(task).is_some_and(OrchestrationTaskStatus::occupies_parallelism_slot)
            })
            .count();
        let available_slots = usize::try_from(orchestration.max_parallelism)
            .unwrap_or_default()
            .saturating_sub(occupied_slots);
        for task in tasks
            .iter_mut()
            .filter(|task| task_status(task) == Some(OrchestrationTaskStatus::Planned))
            .take(available_slots)
        {
            self.spawn_task(orchestration, task).await?;
        }

        let refreshed = self
            .repository
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| error.to_string())?;
        if !refreshed.is_empty()
            && refreshed
                .iter()
                .all(|task| task_status(task).is_some_and(OrchestrationTaskStatus::is_settled))
        {
            self.clear_live_status(orchestration);
            let claimed = self
                .repository
                .claim_orchestration_rollup(orchestration.id)
                .await
                .map_err(|error| error.to_string())?;
            if claimed {
                self.submit_rollup(orchestration, &refreshed).await?;
            }
        } else {
            self.emit_live_status(orchestration, &refreshed);
        }

        Ok(())
    }

    async fn reconcile_rollup(
        &self,
        orchestration: &SessionOrchestrationRow,
        tasks: &[SessionOrchestrationTaskRow],
    ) -> Result<(), String> {
        let operation_id = rollup_operation_id(orchestration.id);
        let operation_status = self
            .repository
            .load_rollup_operation_status(&operation_id)
            .await
            .map_err(|error| error.to_string())?;
        match operation_status.as_deref() {
            Some("done") => {
                self.repository
                    .complete_orchestration_rollup(orchestration.id)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Some("queued" | "running") => {}
            None | Some("failed" | "canceled") => {
                self.submit_rollup(orchestration, tasks).await?;
            }
            Some(status) => {
                return Err(format!(
                    "Unknown roll-up operation status `{status}` for orchestration {}",
                    orchestration.id
                ));
            }
        }

        Ok(())
    }

    async fn reconcile_task(&self, task: &mut SessionOrchestrationTaskRow) -> Result<(), String> {
        if task.child_session_id.is_none()
            && task_status(task) == Some(OrchestrationTaskStatus::Creating)
        {
            task.child_session_id = self
                .repository
                .load_child_session_id_for_task(task.id)
                .await
                .map_err(|error| error.to_string())?;
            if let Some(child_session_id) = task.child_session_id.as_deref() {
                let linked = self
                    .repository
                    .link_orchestration_task_child(task.id, child_session_id)
                    .await
                    .map_err(|error| error.to_string())?;
                if !linked {
                    self.cancel_unclaimed_child(child_session_id).await?;

                    return Ok(());
                }
                task.status = OrchestrationTaskStatus::Running.to_string();

                return Ok(());
            }
            self.update_task_status(
                task,
                OrchestrationTaskStatus::Failed,
                Some("Child creation did not complete".to_string()),
            )
            .await?;

            return Ok(());
        }
        if task.child_session_id.is_none() {
            return Ok(());
        }
        let child_status = task
            .child_status
            .as_deref()
            .and_then(|status| status.parse::<SessionStatus>().ok())
            .unwrap_or(SessionStatus::Canceled);
        let next = task_status_from_child(child_status);
        self.update_task_status(task, next, None).await?;
        if next == OrchestrationTaskStatus::Ready {
            let summary = bounded_summary(task.child_summary.as_deref().unwrap_or("Completed"));
            if task.result_summary.as_deref() != Some(summary.as_str()) {
                self.repository
                    .update_orchestration_task_result_summary(task.id, &summary)
                    .await
                    .map_err(|error| error.to_string())?;
                task.result_summary = Some(summary);
            }
        }

        Ok(())
    }

    async fn update_task_status(
        &self,
        task: &mut SessionOrchestrationTaskRow,
        next: OrchestrationTaskStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        let current = task_status(task).unwrap_or(OrchestrationTaskStatus::Failed);
        if current == next {
            return Ok(());
        }
        self.repository
            .update_orchestration_task_status(task.id, &next.to_string(), last_error.clone())
            .await
            .map_err(|error| error.to_string())?;
        task.status = next.to_string();
        task.last_error = last_error;

        Ok(())
    }

    async fn spawn_task(
        &self,
        orchestration: &SessionOrchestrationRow,
        task: &mut SessionOrchestrationTaskRow,
    ) -> Result<(), String> {
        let claimed = self
            .repository
            .claim_orchestration_task(task.id)
            .await
            .map_err(|error| error.to_string())?;
        if !claimed {
            return Ok(());
        }
        task.status = OrchestrationTaskStatus::Creating.to_string();
        task.last_error = None;
        let controller_session_id = SessionId::from(orchestration.controller_session_id.clone());
        let child_session_id = match self
            .session_service
            .create_session(CreateSessionRequest {
                inherit_from_session_id: Some(controller_session_id),
                mode: CreateSessionMode::OrchestrationChild { task_id: task.id },
                project_id: orchestration.controller_project_id,
            })
            .await
        {
            Ok(child_session_id) => child_session_id,
            Err(error) => {
                self.fail_task_spawn(task, error.to_string()).await?;

                return Ok(());
            }
        };
        let linked = self
            .repository
            .link_orchestration_task_child(task.id, child_session_id.as_str())
            .await
            .map_err(|error| error.to_string())?;
        if !linked {
            self.cancel_unclaimed_child(child_session_id.as_str())
                .await?;

            return Ok(());
        }
        task.child_session_id = Some(child_session_id.as_str().to_string());
        task.status = OrchestrationTaskStatus::Running.to_string();
        let prompt = child_prompt(task);
        if let Err(error) = self
            .session_service
            .send_message(&child_session_id, prompt)
            .await
        {
            self.fail_task_spawn(task, error.to_string()).await?;
        }
        let _ = self.event_tx.send(AppEvent::RefreshSessions);

        Ok(())
    }

    async fn cancel_unclaimed_child(&self, child_session_id: &str) -> Result<(), String> {
        let child_session_id = SessionId::from(child_session_id);
        self.session_service
            .cancel_session(&child_session_id)
            .await
            .map_err(|error| error.to_string())?;
        let _ = self.event_tx.send(AppEvent::RefreshSessions);

        Ok(())
    }

    async fn fail_task_spawn(
        &self,
        task: &mut SessionOrchestrationTaskRow,
        error: String,
    ) -> Result<(), String> {
        self.update_task_status(task, OrchestrationTaskStatus::Failed, Some(error))
            .await
    }

    fn emit_live_status(
        &self,
        orchestration: &SessionOrchestrationRow,
        tasks: &[SessionOrchestrationTaskRow],
    ) {
        let message = live_status_message(tasks);
        let should_emit = self.live_statuses.lock().is_ok_and(|mut live_statuses| {
            if live_statuses.get(&orchestration.id) == Some(&message) {
                return false;
            }

            live_statuses.insert(orchestration.id, message.clone());

            true
        });
        if !should_emit {
            return;
        }

        let _ = self
            .event_tx
            .send(AppEvent::SessionOrchestrationProgressUpdated {
                progress: Some(message),
                session_id: SessionId::from(orchestration.controller_session_id.clone()),
            });
        let _ = self.event_tx.send(AppEvent::RefreshSessions);
    }

    fn clear_live_status(&self, orchestration: &SessionOrchestrationRow) {
        if let Ok(mut live_statuses) = self.live_statuses.lock() {
            live_statuses.remove(&orchestration.id);
        }
        let _ = self
            .event_tx
            .send(AppEvent::SessionOrchestrationProgressUpdated {
                progress: None,
                session_id: SessionId::from(orchestration.controller_session_id.clone()),
            });
    }

    async fn submit_rollup(
        &self,
        orchestration: &SessionOrchestrationRow,
        tasks: &[SessionOrchestrationTaskRow],
    ) -> Result<(), String> {
        let controller_session_id = SessionId::from(orchestration.controller_session_id.clone());
        let rollup = rollup_message(tasks);
        self.session_service
            .submit_coordinator_message(
                &controller_session_id,
                CoordinatorMessageRequest {
                    message: rollup,
                    operation_id: rollup_operation_id(orchestration.id),
                },
            )
            .await
            .map_err(|error| error.to_string())?;

        Ok(())
    }
}

fn validate_subtasks(subtasks: &[SubtaskItem], is_retry: bool) -> Result<(), String> {
    if subtasks.len() < 2 && !is_retry {
        return Err("a meaningful orchestration requires at least two subtasks.".to_string());
    }
    let mut task_keys = HashSet::new();
    let mut scopes = Vec::<(&str, String)>::new();
    for subtask in subtasks {
        if !is_kebab_case_task_key(&subtask.task_key)
            || !task_keys.insert(subtask.task_key.as_str())
        {
            return Err("every subtask needs a unique kebab-case task key.".to_string());
        }
        if subtask.prompt.trim().is_empty()
            || subtask.title.trim().is_empty()
            || subtask.touched_areas.is_empty()
        {
            return Err(format!(
                "subtask `{}` needs a title, standalone prompt, and touched areas.",
                subtask.task_key
            ));
        }
        for area in &subtask.touched_areas {
            let scope = normalized_scope(area).map_err(|reason| {
                format!(
                    "subtask `{}` has invalid touched area `{area}`: {reason}.",
                    subtask.task_key
                )
            })?;
            if let Some((other_key, _)) = scopes.iter().find(|(other_key, other_scope)| {
                *other_key != subtask.task_key && scopes_overlap(other_scope, &scope)
            }) {
                return Err(format!(
                    "subtasks `{other_key}` and `{}` overlap at `{area}`.",
                    subtask.task_key
                ));
            }
            scopes.push((subtask.task_key.as_str(), scope));
        }
    }

    Ok(())
}

fn is_kebab_case_task_key(task_key: &str) -> bool {
    !task_key.is_empty()
        && task_key.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

fn normalized_scope(area: &str) -> Result<String, &'static str> {
    let normalized = area.trim().trim_start_matches("./").trim_end_matches('/');
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.split('/').any(|part| part == "..")
    {
        return Err("use a non-empty repository-relative path");
    }
    if normalized.contains(['*', '?', '[', ']', '{', '}']) {
        return Err("use a literal file or directory path; wildcard patterns are not supported");
    }

    Ok(normalized.to_string())
}

fn scopes_overlap(left: &str, right: &str) -> bool {
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

async fn reusable_retry_orchestration_id(
    db: &AppRepositories,
    existing: Option<&SessionOrchestrationRow>,
    subtasks: &[SubtaskItem],
) -> Result<Option<i64>, DbError> {
    let Some(existing) = existing.filter(|row| row.status == OrchestrationStatus::Done.to_string())
    else {
        return Ok(None);
    };
    let tasks = db
        .orchestrations()
        .load_orchestration_tasks(existing.id)
        .await?;
    let retryable_keys = tasks
        .iter()
        .filter(|task| {
            matches!(
                task_status(task),
                Some(OrchestrationTaskStatus::Failed | OrchestrationTaskStatus::Canceled)
            )
        })
        .map(|task| task.task_key.as_str())
        .collect::<HashSet<_>>();
    let is_retry = !subtasks.is_empty()
        && subtasks
            .iter()
            .all(|subtask| retryable_keys.contains(subtask.task_key.as_str()));

    Ok(is_retry.then_some(existing.id))
}

async fn load_max_parallelism(db: &AppRepositories) -> i64 {
    db.settings()
        .get_setting(SettingName::OrchestrationParallelism)
        .await
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(i64::from(DEFAULT_ORCHESTRATION_PARALLELISM))
        .clamp(1, i64::from(MAX_ORCHESTRATION_PARALLELISM))
}

fn session_metadata_from_row(row: SessionOrchestrationMetadataRow) -> OrchestrationSessionMetadata {
    let progress = row
        .orchestration_status
        .as_deref()
        .and_then(|status| status.parse::<OrchestrationStatus>().ok())
        .and_then(|status| match status {
            OrchestrationStatus::AwaitingApproval => Some("Awaiting approval".to_string()),
            OrchestrationStatus::Canceling => Some("Canceling orchestration".to_string()),
            OrchestrationStatus::Running | OrchestrationStatus::Submitting => Some(format!(
                "{} running, {} waiting on you",
                row.running_task_count, row.waiting_task_count
            )),
            OrchestrationStatus::Done | OrchestrationStatus::Canceled => None,
        });

    OrchestrationSessionMetadata {
        controller_session_id: row.controller_session_id.map(SessionId::from),
        progress,
    }
}

fn task_status(task: &SessionOrchestrationTaskRow) -> Option<OrchestrationTaskStatus> {
    task.status.parse().ok()
}

/// Returns whether an observed child can no longer perform branch work.
pub(crate) fn child_session_is_stopped(status: Option<&str>) -> bool {
    status
        .and_then(|status| status.parse::<SessionStatus>().ok())
        .is_some_and(|status| {
            matches!(
                status,
                SessionStatus::Merged | SessionStatus::Done | SessionStatus::Canceled
            )
        })
}

fn task_status_from_child(status: SessionStatus) -> OrchestrationTaskStatus {
    match status {
        SessionStatus::Draft
        | SessionStatus::InProgress
        | SessionStatus::Queued
        | SessionStatus::Rebasing
        | SessionStatus::Merging => OrchestrationTaskStatus::Running,
        SessionStatus::Question => OrchestrationTaskStatus::WaitingForInput,
        SessionStatus::Review
        | SessionStatus::AgentReview
        | SessionStatus::Merged
        | SessionStatus::Done => OrchestrationTaskStatus::Ready,
        SessionStatus::Canceled => OrchestrationTaskStatus::Failed,
    }
}

fn child_prompt(task: &SessionOrchestrationTaskRow) -> String {
    OrchestrationChildPromptTemplate {
        prompt: &task.prompt,
        task_key: &task.task_key,
        title: &task.title,
        touched_areas: &task.touched_areas,
    }
    .render()
    .unwrap_or_else(|_| task.prompt.clone())
}

fn bounded_summary(summary: &str) -> String {
    let mut characters = summary.trim().chars();
    let mut bounded = characters
        .by_ref()
        .take(RESULT_SUMMARY_MAX_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        bounded.push('…');
    }

    bounded
}

fn live_status_message(tasks: &[SessionOrchestrationTaskRow]) -> String {
    let mut lines = vec!["Orchestrating...".to_string()];
    lines.extend(tasks.iter().map(|task| {
        let status = task_status(task).map_or("unknown", orchestration_task_status_label);

        format!("- {}: {status}", task.title)
    }));

    lines.join("\n")
}

fn orchestration_task_status_label(status: OrchestrationTaskStatus) -> &'static str {
    match status {
        OrchestrationTaskStatus::Planned => "waiting",
        OrchestrationTaskStatus::Creating => "starting",
        OrchestrationTaskStatus::Running => "running",
        OrchestrationTaskStatus::WaitingForInput => "waiting on you",
        OrchestrationTaskStatus::Ready => "ready",
        OrchestrationTaskStatus::Failed => "failed",
        OrchestrationTaskStatus::Canceled => "canceled",
    }
}

fn rollup_message(tasks: &[SessionOrchestrationTaskRow]) -> String {
    let mut lines = vec![
        "Orchestration roll-up. Summarize these results for the user and preserve the recommended \
         manual merge order."
            .to_string(),
        String::new(),
    ];
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut merge_order = Vec::new();
    for task in tasks {
        let branch = task
            .child_session_id
            .as_deref()
            .map_or_else(|| "none".to_string(), session_branch);
        input_tokens =
            input_tokens.saturating_add(u64::try_from(task.child_input_tokens).unwrap_or_default());
        output_tokens = output_tokens
            .saturating_add(u64::try_from(task.child_output_tokens).unwrap_or_default());
        if task_status(task) == Some(OrchestrationTaskStatus::Ready) {
            merge_order.push(branch.clone());
        }
        lines.extend([
            format!("Task `{}` — {}", task.task_key, task.status),
            format!("Branch: `{branch}`"),
            format!(
                "Summary: {}",
                task.result_summary
                    .as_deref()
                    .unwrap_or("No summary available")
            ),
            String::new(),
        ]);
    }
    lines.push(format!(
        "Total child token usage: {input_tokens} input, {output_tokens} output."
    ));
    lines.push("Recommended manual merge order:".to_string());
    lines.extend(
        merge_order
            .into_iter()
            .enumerate()
            .map(|(index, branch)| format!("{}. `{branch}`", index + 1)),
    );

    lines.join("\n")
}

fn rollup_operation_id(orchestration_id: i64) -> String {
    format!("orchestration-rollup-{orchestration_id}")
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::sync::Mutex;

    use ag_agent::{AgentKind, ReasoningLevel};
    use ag_session::{
        AnswerQuestionsRequest, ReviewRequest, Session, SessionBackend, SessionError,
    };
    use async_trait::async_trait;

    use super::*;
    use crate::domain::agent::SpeedMode;
    use crate::infra::db::{MockOrchestrationRepository, PersistedSessionCreation};

    #[derive(Clone, Default)]
    struct TestSessionBackend {
        state: Arc<Mutex<TestSessionBackendState>>,
    }

    #[derive(Default)]
    struct TestSessionBackendState {
        accepted_coordinator_operations: HashSet<String>,
        calls: Vec<String>,
        cancel_errors: VecDeque<SessionError>,
        create_results: VecDeque<SessionId>,
        send_errors: VecDeque<SessionError>,
    }

    impl TestSessionBackend {
        fn push_create_result(&self, session_id: impl Into<SessionId>) {
            self.state
                .lock()
                .expect("test backend state should remain available")
                .create_results
                .push_back(session_id.into());
        }

        fn calls(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("test backend state should remain available")
                .calls
                .clone()
        }

        fn push_cancel_error(&self, error: SessionError) {
            self.state
                .lock()
                .expect("test backend state should remain available")
                .cancel_errors
                .push_back(error);
        }

        fn push_send_error(&self, error: SessionError) {
            self.state
                .lock()
                .expect("test backend state should remain available")
                .send_errors
                .push_back(error);
        }

        fn service(&self) -> SessionService {
            SessionService::new(Arc::new(self.clone()))
        }
    }

    #[async_trait]
    impl SessionBackend for TestSessionBackend {
        async fn create_session(
            &self,
            request: CreateSessionRequest,
        ) -> Result<SessionId, SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("test backend state should remain available");
            state.calls.push(format!("create:{:?}", request.mode));

            state
                .create_results
                .pop_front()
                .ok_or_else(|| SessionError::Operation("missing create result".to_string()))
        }

        async fn get_session(
            &self,
            _session_id: &SessionId,
        ) -> Result<Option<Session>, SessionError> {
            Ok(None)
        }

        async fn send_message(
            &self,
            session_id: &SessionId,
            message: String,
        ) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("test backend state should remain available");
            state.calls.push(format!("send:{session_id}:{message}"));

            state.send_errors.pop_front().map_or(Ok(()), Err)
        }

        async fn submit_coordinator_message(
            &self,
            session_id: &SessionId,
            request: CoordinatorMessageRequest,
        ) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("test backend state should remain available");
            state.calls.push(format!(
                "rollup-attempt:{session_id}:{}",
                request.operation_id
            ));
            if state
                .accepted_coordinator_operations
                .insert(request.operation_id)
            {
                state
                    .calls
                    .push(format!("rollup:{session_id}:{}", request.message));
            }

            Ok(())
        }

        async fn answer_questions(
            &self,
            _session_id: &SessionId,
            _request: AnswerQuestionsRequest,
        ) -> Result<(), SessionError> {
            Ok(())
        }

        async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("test backend state should remain available");
            state.calls.push(format!("cancel:{session_id}"));

            state.cancel_errors.pop_front().map_or(Ok(()), Err)
        }

        async fn merge_session(&self, _session_id: &SessionId) -> Result<(), SessionError> {
            Ok(())
        }

        async fn create_review_request(
            &self,
            _session_id: &SessionId,
        ) -> Result<ReviewRequest, SessionError> {
            Err(SessionError::Operation(
                "review requests are not used by coordinator tests".to_string(),
            ))
        }
    }

    fn orchestration(max_parallelism: i64) -> SessionOrchestrationRow {
        SessionOrchestrationRow {
            controller_project_id: 1,
            controller_session_id: "controller".to_string(),
            id: 1,
            max_parallelism,
            status: OrchestrationStatus::Running.to_string(),
        }
    }

    fn task(
        id: i64,
        task_key: &str,
        status: OrchestrationTaskStatus,
        child_session_id: Option<&str>,
    ) -> SessionOrchestrationTaskRow {
        let child_status = child_session_id.map(|_| match status {
            OrchestrationTaskStatus::WaitingForInput => SessionStatus::Question,
            OrchestrationTaskStatus::Ready => SessionStatus::Review,
            OrchestrationTaskStatus::Failed | OrchestrationTaskStatus::Canceled => {
                SessionStatus::Canceled
            }
            OrchestrationTaskStatus::Planned
            | OrchestrationTaskStatus::Creating
            | OrchestrationTaskStatus::Running => SessionStatus::InProgress,
        });

        SessionOrchestrationTaskRow {
            attempt_count: i64::from(child_session_id.is_some()),
            child_input_tokens: i64::from(child_session_id.is_some()) * 10,
            child_output_tokens: i64::from(child_session_id.is_some()) * 5,
            child_session_id: child_session_id.map(str::to_string),
            child_status: child_status.map(|status| status.to_string()),
            child_summary: None,
            id,
            last_error: None,
            prompt: format!("Implement {task_key}"),
            result_summary: None,
            status: status.to_string(),
            task_key: task_key.to_string(),
            title: task_key.to_string(),
            touched_areas: format!("[\"{task_key}/\"]"),
        }
    }

    fn with_child_observation(
        mut task: SessionOrchestrationTaskRow,
        status: SessionStatus,
        summary: Option<&str>,
    ) -> SessionOrchestrationTaskRow {
        task.child_status = Some(status.to_string());
        task.child_summary = summary.map(str::to_string);

        task
    }

    fn mock_task_snapshots(
        mock: &mut MockOrchestrationRepository,
        snapshots: Vec<Vec<SessionOrchestrationTaskRow>>,
    ) {
        let snapshot_count = snapshots.len();
        let snapshots = Arc::new(Mutex::new(VecDeque::from(snapshots)));
        mock.expect_load_orchestration_tasks()
            .times(snapshot_count)
            .returning(move |_| {
                Ok(snapshots
                    .lock()
                    .expect("task snapshots should remain available")
                    .pop_front()
                    .expect("expected another task snapshot"))
            });
    }

    #[derive(Default)]
    struct OneShotSchedule {
        has_fired: bool,
    }

    #[async_trait]
    impl OrchestrationSchedule for OneShotSchedule {
        async fn wait_for_reconciliation(&mut self) {
            if self.has_fired {
                std::future::pending::<()>().await;
            }
            self.has_fired = true;
        }
    }

    fn expect_rollup_completion_failure_then_success(mock: &mut MockOrchestrationRepository) {
        let update_attempt = Arc::new(Mutex::new(0_u8));
        mock.expect_complete_orchestration_rollup()
            .withf(|id| *id == 1)
            .times(2)
            .returning({
                let update_attempt = Arc::clone(&update_attempt);

                move |_| {
                    let mut update_attempt = update_attempt
                        .lock()
                        .expect("update attempt should remain available");
                    *update_attempt += 1;
                    if *update_attempt == 1 {
                        return Err(DbError::Io(std::io::Error::other(
                            "injected post-submit failure",
                        )));
                    }

                    Ok(true)
                }
            });
    }

    async fn controller_database() -> (AppRepositories, i64) {
        let database = AppRepositories::in_memory().await;
        let project_id = database
            .projects()
            .upsert_project("/tmp/orchestration-project", Some("main".to_string()))
            .await
            .expect("failed to create orchestration test project");
        database
            .sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id: "controller",
                is_draft: false,
                model: AgentKind::Codex.default_model().as_str(),
                orchestration_task_id: None,
                parent_session_id: None,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                role: Some("Orchestrator"),
                speed_mode: SpeedMode::Normal,
                status: "Review",
            })
            .await
            .expect("failed to insert controller session");

        (database, project_id)
    }

    fn subtask(task_key: &str, touched_areas: &[&str]) -> SubtaskItem {
        SubtaskItem {
            prompt: format!("Implement {task_key}"),
            task_key: task_key.to_string(),
            title: task_key.to_string(),
            touched_areas: touched_areas
                .iter()
                .map(|area| (*area).to_string())
                .collect(),
        }
    }

    async fn persist_approved_two_task_plan(
        database: &AppRepositories,
    ) -> (
        SessionOrchestrationRow,
        Vec<SessionOrchestrationTaskRow>,
        AgentResponse,
        OrchestrationSessionMetadata,
    ) {
        let mut response = AgentResponse::plain("Plan");
        response.subtasks = vec![
            subtask("protocol", &["crates/ag-protocol/"]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ];
        persist_controller_plan(database, "controller", &mut response)
            .await
            .expect("plan should persist");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("failed to load plan")
            .expect("plan should exist");
        let tasks = database
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .expect("failed to load tasks");
        apply_plan_answer(
            database,
            "controller",
            &[QuestionAnswer {
                answer: "ignored".to_string(),
                question: "Different question".to_string(),
            }],
        )
        .await
        .expect("unrelated answer should be ignored");
        apply_plan_answer(
            database,
            "controller",
            &[QuestionAnswer {
                answer: "Approve".to_string(),
                question: APPROVAL_QUESTION.to_string(),
            }],
        )
        .await
        .expect("approval should start orchestration");
        let project_id = database
            .sessions()
            .load_session("controller")
            .await
            .expect("controller should load")
            .and_then(|controller| controller.project_id)
            .expect("controller should belong to a project");
        let metadata = session_metadata_for_project(database, project_id)
            .await
            .remove("controller")
            .expect("controller metadata should load");

        (orchestration, tasks, response, metadata)
    }

    fn assert_reconciled_rollup(
        backend: &TestSessionBackend,
        status_updates: &Arc<Mutex<Vec<(i64, String)>>>,
    ) {
        assert_eq!(
            *status_updates
                .lock()
                .expect("status updates should remain available"),
            vec![
                (2, OrchestrationTaskStatus::Failed.to_string()),
                (1, OrchestrationTaskStatus::Ready.to_string()),
            ]
        );
        let rollup = backend
            .calls()
            .into_iter()
            .find(|call| call.starts_with("rollup:controller:"))
            .expect("settled tasks should submit a rollup");
        assert!(rollup.contains("Task `protocol`"));
        assert!(rollup.contains("Task `ui`"));
        assert!(rollup.contains("20 input, 10 output"));
        assert!(rollup.contains("Recommended manual merge order"));
    }

    #[test]
    fn validates_file_disjoint_multi_task_plans() {
        // Arrange
        let tasks = [
            subtask("protocol", &["crates/ag-protocol/"]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ];

        // Act
        let result = validate_subtasks(&tasks, false);

        // Assert
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn derives_bulk_session_metadata_for_controller_and_child_rows() {
        // Arrange
        let controller_rows = [
            (
                OrchestrationStatus::AwaitingApproval,
                Some("Awaiting approval"),
            ),
            (
                OrchestrationStatus::Running,
                Some("2 running, 1 waiting on you"),
            ),
            (
                OrchestrationStatus::Canceling,
                Some("Canceling orchestration"),
            ),
            (
                OrchestrationStatus::Submitting,
                Some("2 running, 1 waiting on you"),
            ),
            (OrchestrationStatus::Done, None),
            (OrchestrationStatus::Canceled, None),
        ];

        // Act
        let progress = controller_rows.map(|(status, _)| {
            session_metadata_from_row(SessionOrchestrationMetadataRow {
                controller_session_id: None,
                orchestration_status: Some(status.to_string()),
                running_task_count: 2,
                session_id: "controller".to_string(),
                waiting_task_count: 1,
            })
            .progress
        });
        let child = session_metadata_from_row(SessionOrchestrationMetadataRow {
            controller_session_id: Some("controller".to_string()),
            orchestration_status: Some("invalid".to_string()),
            running_task_count: 0,
            session_id: "child".to_string(),
            waiting_task_count: 0,
        });

        // Assert
        assert_eq!(
            progress,
            controller_rows.map(|(_, expected)| expected.map(str::to_string))
        );
        assert_eq!(
            child.controller_session_id,
            Some(SessionId::from("controller"))
        );
        assert_eq!(child.progress, None);
    }

    #[test]
    fn rejects_single_overlapping_and_wildcard_task_plans() {
        // Arrange
        let single = [subtask("only", &["src/"])];
        let overlap = [
            subtask("all-ui", &["src/ui/"]),
            subtask("page", &["src/ui/page/session.rs"]),
        ];
        let wildcard_overlap = [
            subtask("pattern", &["src/foo*.rs"]),
            subtask("file", &["src/foobar.rs"]),
        ];
        let invalid_area = [
            subtask("outside", &["../Cargo.toml"]),
            subtask("inside", &["src/lib.rs"]),
        ];
        let invalid_key = [
            subtask("valid-key", &["src/valid.rs"]),
            subtask("Invalid Key", &["src/invalid.rs"]),
        ];
        let mut missing_details = [
            subtask("missing-details", &["src/missing.rs"]),
            subtask("valid-details", &["src/valid.rs"]),
        ];
        missing_details[0].prompt.clear();

        // Act
        let single_error =
            validate_subtasks(&single, false).expect_err("single task should be rejected");
        let retry_result = validate_subtasks(&single, true);
        let overlap_error =
            validate_subtasks(&overlap, false).expect_err("overlap should be rejected");
        let wildcard_error = validate_subtasks(&wildcard_overlap, false)
            .expect_err("wildcard touched areas should be rejected");
        let invalid_area_error = validate_subtasks(&invalid_area, false)
            .expect_err("non-relative touched areas should be rejected");
        let key_error = validate_subtasks(&invalid_key, false)
            .expect_err("invalid task key should be rejected");
        let details_error = validate_subtasks(&missing_details, false)
            .expect_err("incomplete task details should be rejected");

        // Assert
        assert!(single_error.contains("at least two"));
        assert_eq!(retry_result, Ok(()));
        assert!(overlap_error.contains("overlap"));
        assert!(wildcard_error.contains("wildcard patterns are not supported"));
        assert!(invalid_area_error.contains("repository-relative path"));
        assert!(key_error.contains("kebab-case"));
        assert!(details_error.contains("standalone prompt"));
    }

    #[test]
    fn maps_every_child_lifecycle_status_to_a_task_status() {
        // Arrange
        let expected = [
            (SessionStatus::Draft, OrchestrationTaskStatus::Running),
            (SessionStatus::InProgress, OrchestrationTaskStatus::Running),
            (SessionStatus::Queued, OrchestrationTaskStatus::Running),
            (SessionStatus::Rebasing, OrchestrationTaskStatus::Running),
            (SessionStatus::Merging, OrchestrationTaskStatus::Running),
            (
                SessionStatus::Question,
                OrchestrationTaskStatus::WaitingForInput,
            ),
            (SessionStatus::Review, OrchestrationTaskStatus::Ready),
            (SessionStatus::AgentReview, OrchestrationTaskStatus::Ready),
            (SessionStatus::Merged, OrchestrationTaskStatus::Ready),
            (SessionStatus::Done, OrchestrationTaskStatus::Ready),
            (SessionStatus::Canceled, OrchestrationTaskStatus::Failed),
        ];

        // Act / Assert
        for (status, task_status) in expected {
            assert_eq!(task_status_from_child(status), task_status);
        }
    }

    #[test]
    fn identifies_only_terminal_child_statuses_as_stopped() {
        // Arrange
        let statuses = [
            (None, false),
            (Some("invalid"), false),
            (Some("InProgress"), false),
            (Some("Merged"), true),
            (Some("Done"), true),
            (Some("Canceled"), true),
        ];

        // Act
        let observed = statuses.map(|(status, _)| child_session_is_stopped(status));

        // Assert
        assert_eq!(observed, statuses.map(|(_, expected)| expected));
    }

    #[test]
    fn bounds_child_summaries_for_fan_in() {
        // Arrange
        let summary = "x".repeat(RESULT_SUMMARY_MAX_CHARS + 1);

        // Act
        let bounded = bounded_summary(&summary);

        // Assert
        assert_eq!(bounded.chars().count(), RESULT_SUMMARY_MAX_CHARS + 1);
        assert!(bounded.ends_with('…'));
    }

    #[tokio::test]
    async fn coordinator_test_backend_covers_unneeded_session_ports() {
        // Arrange
        let backend = TestSessionBackend::default();
        let session_id = SessionId::from("unused");

        // Act / Assert
        assert!(
            backend
                .get_session(&session_id)
                .await
                .expect("session lookup should succeed")
                .is_none()
        );
        backend
            .answer_questions(
                &session_id,
                AnswerQuestionsRequest {
                    answers: Vec::new(),
                },
            )
            .await
            .expect("question answer should succeed");
        backend
            .cancel_session(&session_id)
            .await
            .expect("cancellation should succeed");
        backend
            .merge_session(&session_id)
            .await
            .expect("merge should succeed");
        assert!(backend.create_review_request(&session_id).await.is_err());
    }

    #[tokio::test]
    async fn coordinator_run_survives_one_reconciliation_error() {
        // Arrange
        let reconciliation_attempted = Arc::new(tokio::sync::Notify::new());
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_active_orchestrations()
            .once()
            .returning({
                let reconciliation_attempted = Arc::clone(&reconciliation_attempted);

                move || {
                    reconciliation_attempted.notify_one();

                    Err(DbError::Io(std::io::Error::other("injected failure")))
                }
            });
        let backend = TestSessionBackend::default();
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        let coordinator_task = tokio::spawn(coordinator.run(OneShotSchedule::default()));
        reconciliation_attempted.notified().await;
        tokio::task::yield_now().await;
        coordinator_task.abort();
        let join_result = coordinator_task.await;

        // Assert
        assert!(join_result.is_err());
    }

    #[tokio::test]
    async fn canceling_orchestration_recovers_every_task_shape_and_settles() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        let mut canceling = orchestration(1);
        canceling.status = OrchestrationStatus::Canceling.to_string();
        repository
            .expect_load_active_orchestrations()
            .once()
            .return_once(move || Ok(vec![canceling]));
        repository
            .expect_load_orchestration_tasks()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| {
                Ok(vec![
                    task(1, "protocol", OrchestrationTaskStatus::Creating, None),
                    with_child_observation(
                        task(
                            2,
                            "terminal",
                            OrchestrationTaskStatus::Running,
                            Some("child-2"),
                        ),
                        SessionStatus::Done,
                        None,
                    ),
                    task(3, "unstarted", OrchestrationTaskStatus::Planned, None),
                    task(
                        4,
                        "settled",
                        OrchestrationTaskStatus::Ready,
                        Some("child-4"),
                    ),
                ])
            });
        repository
            .expect_load_child_session_id_for_task()
            .times(2)
            .returning(|id| match id {
                1 => Ok(Some("child-1".to_string())),
                3 => Ok(None),
                _ => Err(DbError::Io(std::io::Error::other(format!(
                    "unexpected reverse-link lookup for task {id}"
                )))),
            });
        repository
            .expect_update_orchestration_task_status()
            .withf(|id, status, error| {
                [1, 2, 3].contains(id)
                    && status == OrchestrationTaskStatus::Canceled.to_string()
                    && error.is_none()
            })
            .times(3)
            .returning(|_, _, _| Ok(()));
        repository
            .expect_update_orchestration_status()
            .withf(|id, status| *id == 1 && status == OrchestrationStatus::Canceled.to_string())
            .once()
            .returning(|_, _| Ok(()));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        let result = coordinator.reconcile_once().await;

        // Assert
        assert_eq!(result, Ok(()));
        assert_eq!(backend.calls(), vec!["cancel:child-1".to_string()]);
    }

    #[tokio::test]
    async fn canceling_orchestration_retries_after_child_cancellation_error() {
        // Arrange
        let backend = TestSessionBackend::default();
        backend.push_cancel_error(SessionError::Operation("cancel failed".to_string()));
        let mut repository = MockOrchestrationRepository::new();
        let mut canceling = orchestration(1);
        canceling.status = OrchestrationStatus::Canceling.to_string();
        repository
            .expect_load_active_orchestrations()
            .times(2)
            .returning(move || Ok(vec![canceling.clone()]));
        repository
            .expect_load_orchestration_tasks()
            .withf(|id| *id == 1)
            .times(2)
            .returning(|_| {
                Ok(vec![task(
                    1,
                    "protocol",
                    OrchestrationTaskStatus::Running,
                    Some("child-1"),
                )])
            });
        repository
            .expect_update_orchestration_task_status()
            .withf(|id, status, error| {
                *id == 1
                    && status == OrchestrationTaskStatus::Canceled.to_string()
                    && error.is_none()
            })
            .once()
            .returning(|_, _, _| Ok(()));
        repository
            .expect_update_orchestration_status()
            .withf(|id, status| *id == 1 && status == OrchestrationStatus::Canceled.to_string())
            .once()
            .returning(|_, _| Ok(()));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        let first_result = coordinator.reconcile_once().await;
        let retry_result = coordinator.reconcile_once().await;

        // Assert
        assert_eq!(first_result, Err("cancel failed".to_string()));
        assert_eq!(retry_result, Ok(()));
        assert_eq!(
            backend.calls(),
            vec!["cancel:child-1".to_string(), "cancel:child-1".to_string()]
        );
    }

    #[tokio::test]
    async fn reconciliation_spawns_only_up_to_the_parallelism_cap() {
        // Arrange
        let backend = TestSessionBackend::default();
        backend.push_create_result("child-1");
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_active_orchestrations()
            .once()
            .returning(|| Ok(vec![orchestration(1)]));
        mock_task_snapshots(
            &mut repository,
            vec![
                vec![
                    task(1, "protocol", OrchestrationTaskStatus::Planned, None),
                    task(2, "ui", OrchestrationTaskStatus::Planned, None),
                ],
                vec![
                    with_child_observation(
                        task(
                            1,
                            "protocol",
                            OrchestrationTaskStatus::Running,
                            Some("child-1"),
                        ),
                        SessionStatus::Question,
                        None,
                    ),
                    task(2, "ui", OrchestrationTaskStatus::Planned, None),
                ],
            ],
        );
        repository
            .expect_claim_orchestration_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(true));
        repository
            .expect_link_orchestration_task_child()
            .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
            .once()
            .returning(|_, _| Ok(true));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        coordinator
            .reconcile_once()
            .await
            .expect("reconciliation should succeed");

        // Assert
        let calls = backend.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("create:"))
                .count(),
            1
        );
        assert!(calls.iter().any(|call| {
            call.starts_with("send:child-1:")
                && call.contains("You are one worker in an orchestration.")
                && call.contains("Task key: protocol")
                && call.contains("at most 800 characters")
        }));
    }

    #[tokio::test]
    async fn failed_child_creation_marks_pre_link_spawn_failed() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        let status_updates = Arc::new(Mutex::new(Vec::new()));
        repository
            .expect_claim_orchestration_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(true));
        repository
            .expect_update_orchestration_task_status()
            .once()
            .returning({
                let status_updates = Arc::clone(&status_updates);

                move |_, status, error| {
                    status_updates
                        .lock()
                        .expect("status updates should remain available")
                        .push((status.to_string(), error));

                    Ok(())
                }
            });
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

        // Act
        coordinator
            .spawn_task(&orchestration(2), &mut planned_task)
            .await
            .expect("failed creation should settle the task");

        // Assert
        assert_eq!(
            planned_task.status,
            OrchestrationTaskStatus::Failed.to_string()
        );
        assert_eq!(
            planned_task.last_error.as_deref(),
            Some("missing create result")
        );
        assert_eq!(
            *status_updates
                .lock()
                .expect("status updates should remain available"),
            vec![(
                OrchestrationTaskStatus::Failed.to_string(),
                Some("missing create result".to_string())
            )]
        );
    }

    #[tokio::test]
    async fn failed_child_prompt_marks_linked_spawn_failed() {
        // Arrange
        let backend = TestSessionBackend::default();
        backend.push_create_result("child-1");
        backend.push_send_error(SessionError::Operation("send failed".to_string()));
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_claim_orchestration_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(true));
        repository
            .expect_update_orchestration_task_status()
            .once()
            .returning(|_, _, _| Ok(()));
        repository
            .expect_link_orchestration_task_child()
            .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
            .once()
            .returning(|_, _| Ok(true));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

        // Act
        coordinator
            .spawn_task(&orchestration(2), &mut planned_task)
            .await
            .expect("failed prompt delivery should settle the task");

        // Assert
        assert_eq!(
            planned_task.status,
            OrchestrationTaskStatus::Failed.to_string()
        );
        assert_eq!(planned_task.last_error.as_deref(), Some("send failed"));
    }

    #[tokio::test]
    async fn cancellation_barrier_prevents_a_stale_planned_task_from_spawning() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_claim_orchestration_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(false));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

        // Act
        coordinator
            .spawn_task(&orchestration(2), &mut planned_task)
            .await
            .expect("a lost fan-out claim should be harmless");

        // Assert
        assert_eq!(
            planned_task.status,
            OrchestrationTaskStatus::Planned.to_string()
        );
        assert!(backend.calls().is_empty());
    }

    #[tokio::test]
    async fn cancellation_after_child_creation_stops_the_unclaimed_child() {
        // Arrange
        let backend = TestSessionBackend::default();
        backend.push_create_result("child-1");
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_claim_orchestration_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(true));
        repository
            .expect_link_orchestration_task_child()
            .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
            .once()
            .returning(|_, _| Ok(false));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let mut planned_task = task(1, "protocol", OrchestrationTaskStatus::Planned, None);

        // Act
        coordinator
            .spawn_task(&orchestration(2), &mut planned_task)
            .await
            .expect("the unclaimed child should be canceled");

        // Assert
        assert_eq!(
            backend.calls(),
            vec![
                "create:OrchestrationChild { task_id: 1 }".to_string(),
                "cancel:child-1".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn interrupted_creation_without_child_is_reconciled_as_failed() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_child_session_id_for_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(None));
        repository
            .expect_update_orchestration_task_status()
            .withf(|id, status, error| {
                *id == 1
                    && status == OrchestrationTaskStatus::Failed.to_string()
                    && error.as_deref() == Some("Child creation did not complete")
            })
            .once()
            .returning(|_, _, _| Ok(()));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let mut creating_task = task(1, "protocol", OrchestrationTaskStatus::Creating, None);

        // Act
        coordinator
            .reconcile_task(&mut creating_task)
            .await
            .expect("interrupted creation should settle the task");

        // Assert
        assert_eq!(
            creating_task.status,
            OrchestrationTaskStatus::Failed.to_string()
        );
        assert_eq!(
            creating_task.last_error.as_deref(),
            Some("Child creation did not complete")
        );
    }

    #[tokio::test]
    async fn restart_relink_cancels_a_child_after_losing_the_link_claim() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_child_session_id_for_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(Some("child-1".to_string())));
        repository
            .expect_link_orchestration_task_child()
            .withf(|id, child_session_id| *id == 1 && child_session_id == "child-1")
            .once()
            .returning(|_, _| Ok(false));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let mut creating_task = task(1, "protocol", OrchestrationTaskStatus::Creating, None);

        // Act
        coordinator
            .reconcile_task(&mut creating_task)
            .await
            .expect("a child that lost its link claim should be canceled");

        // Assert
        assert_eq!(creating_task.child_session_id.as_deref(), Some("child-1"));
        assert_eq!(backend.calls(), vec!["cancel:child-1".to_string()]);
    }

    #[tokio::test]
    async fn waiting_children_hold_parallelism_slots() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_active_orchestrations()
            .once()
            .returning(|| Ok(vec![orchestration(1)]));
        mock_task_snapshots(
            &mut repository,
            vec![
                vec![
                    with_child_observation(
                        task(
                            1,
                            "protocol",
                            OrchestrationTaskStatus::Running,
                            Some("child-1"),
                        ),
                        SessionStatus::Question,
                        None,
                    ),
                    task(2, "ui", OrchestrationTaskStatus::Planned, None),
                ],
                vec![
                    task(
                        1,
                        "protocol",
                        OrchestrationTaskStatus::WaitingForInput,
                        Some("child-1"),
                    ),
                    task(2, "ui", OrchestrationTaskStatus::Planned, None),
                ],
            ],
        );
        repository
            .expect_update_orchestration_task_status()
            .withf(|id, status, error| {
                *id == 1
                    && status == OrchestrationTaskStatus::WaitingForInput.to_string()
                    && error.is_none()
            })
            .once()
            .returning(|_, _, _| Ok(()));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        coordinator
            .reconcile_once()
            .await
            .expect("reconciliation should succeed");

        // Assert
        assert!(
            !backend
                .calls()
                .iter()
                .any(|call| call.starts_with("create:"))
        );
    }

    #[test]
    fn live_status_loader_is_deduplicated_and_clearable() {
        // Arrange
        let repository = MockOrchestrationRepository::new();
        let backend = TestSessionBackend::default();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());
        let orchestration = orchestration(2);
        let tasks = vec![
            task(
                1,
                "protocol",
                OrchestrationTaskStatus::Running,
                Some("child-1"),
            ),
            task(2, "ui", OrchestrationTaskStatus::Planned, None),
        ];

        // Act
        coordinator.emit_live_status(&orchestration, &tasks);
        coordinator.emit_live_status(&orchestration, &tasks);

        // Assert
        assert_eq!(
            event_rx.try_recv(),
            Ok(AppEvent::SessionOrchestrationProgressUpdated {
                progress: Some("Orchestrating...\n- protocol: running\n- ui: waiting".to_string()),
                session_id: SessionId::from("controller"),
            })
        );
        assert_eq!(event_rx.try_recv(), Ok(AppEvent::RefreshSessions));
        assert!(event_rx.try_recv().is_err());

        // Act
        coordinator.clear_live_status(&orchestration);

        // Assert
        assert_eq!(
            event_rx.try_recv(),
            Ok(AppEvent::SessionOrchestrationProgressUpdated {
                progress: None,
                session_id: SessionId::from("controller"),
            })
        );
    }

    #[test]
    fn live_status_loader_formats_every_task_state() {
        // Arrange
        let states = [
            (OrchestrationTaskStatus::Planned, "waiting"),
            (OrchestrationTaskStatus::Creating, "starting"),
            (OrchestrationTaskStatus::Running, "running"),
            (OrchestrationTaskStatus::WaitingForInput, "waiting on you"),
            (OrchestrationTaskStatus::Ready, "ready"),
            (OrchestrationTaskStatus::Failed, "failed"),
            (OrchestrationTaskStatus::Canceled, "canceled"),
        ];
        let mut tasks = (0_i64..)
            .zip(states)
            .map(|(index, (status, _))| task(index, &status.to_string(), status, None))
            .collect::<Vec<_>>();
        let mut invalid_task = task(8, "invalid", OrchestrationTaskStatus::Running, None);
        invalid_task.status = "invalid".to_string();
        tasks.push(invalid_task);

        // Act
        let message = live_status_message(&tasks);

        // Assert
        assert!(message.starts_with("Orchestrating...\n"));
        for (status, label) in states {
            assert!(message.contains(&format!("- {status}: {label}")));
        }
        assert!(message.contains("- invalid: unknown"));
    }

    #[tokio::test]
    async fn restart_relink_and_out_of_band_settlement_submit_rollup() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_active_orchestrations()
            .times(2)
            .returning(|| Ok(vec![orchestration(2)]));
        let observed_merged = with_child_observation(
            task(
                1,
                "protocol",
                OrchestrationTaskStatus::Running,
                Some("child-merged"),
            ),
            SessionStatus::Merged,
            Some("Merged result"),
        );
        let mut refreshed_merged = observed_merged.clone();
        refreshed_merged.status = OrchestrationTaskStatus::Ready.to_string();
        refreshed_merged.result_summary = Some("Merged result".to_string());
        let observed_canceled = with_child_observation(
            task(
                2,
                "ui",
                OrchestrationTaskStatus::Running,
                Some("child-canceled"),
            ),
            SessionStatus::Canceled,
            None,
        );
        let settled_canceled = task(
            2,
            "ui",
            OrchestrationTaskStatus::Failed,
            Some("child-canceled"),
        );
        mock_task_snapshots(
            &mut repository,
            vec![
                vec![
                    task(1, "protocol", OrchestrationTaskStatus::Creating, None),
                    observed_canceled,
                ],
                vec![observed_merged.clone(), settled_canceled.clone()],
                vec![observed_merged, settled_canceled.clone()],
                vec![refreshed_merged, settled_canceled],
            ],
        );
        repository
            .expect_load_child_session_id_for_task()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(Some("child-merged".to_string())));
        repository
            .expect_link_orchestration_task_child()
            .withf(|id, child_session_id| *id == 1 && child_session_id == "child-merged")
            .once()
            .returning(|_, _| Ok(true));
        let status_updates = Arc::new(Mutex::new(Vec::new()));
        repository
            .expect_update_orchestration_task_status()
            .times(2)
            .returning({
                let status_updates = Arc::clone(&status_updates);

                move |id, status, _| {
                    status_updates
                        .lock()
                        .expect("status updates should remain available")
                        .push((id, status.to_string()));

                    Ok(())
                }
            });
        repository
            .expect_update_orchestration_task_result_summary()
            .withf(|id, summary| *id == 1 && summary == "Merged result")
            .once()
            .returning(|_, _| Ok(()));
        repository
            .expect_claim_orchestration_rollup()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(true));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        coordinator
            .reconcile_once()
            .await
            .expect("restart re-link should succeed");
        coordinator
            .reconcile_once()
            .await
            .expect("settlement should succeed on the next snapshot");

        // Assert
        assert_reconciled_rollup(&backend, &status_updates);
    }

    #[tokio::test]
    async fn settled_rollup_claimed_elsewhere_is_not_submitted_twice() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        repository
            .expect_load_active_orchestrations()
            .once()
            .returning(|| Ok(vec![orchestration(2)]));
        let settled_tasks = vec![task(1, "protocol", OrchestrationTaskStatus::Failed, None)];
        mock_task_snapshots(&mut repository, vec![settled_tasks.clone(), settled_tasks]);
        repository
            .expect_claim_orchestration_rollup()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(false));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        coordinator
            .reconcile_once()
            .await
            .expect("an existing roll-up claim should be accepted");

        // Assert
        assert!(
            backend
                .calls()
                .iter()
                .all(|call| !call.starts_with("rollup"))
        );
    }

    #[tokio::test]
    async fn completed_rollup_retries_status_persistence_without_resubmitting() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        let mut submitting = orchestration(2);
        submitting.status = OrchestrationStatus::Submitting.to_string();
        let active_snapshots = Arc::new(Mutex::new(VecDeque::from([
            vec![orchestration(2)],
            vec![submitting.clone()],
            vec![submitting],
        ])));
        repository
            .expect_load_active_orchestrations()
            .times(3)
            .returning({
                let active_snapshots = Arc::clone(&active_snapshots);

                move || {
                    Ok(active_snapshots
                        .lock()
                        .expect("active snapshots should remain available")
                        .pop_front()
                        .expect("expected another active snapshot"))
                }
            });
        let mut ready_task = task(
            1,
            "protocol",
            OrchestrationTaskStatus::Ready,
            Some("child-ready"),
        );
        ready_task.child_summary = Some("Completed".to_string());
        ready_task.result_summary = Some("Completed".to_string());
        let task_snapshots = Arc::new(Mutex::new(VecDeque::from([
            vec![ready_task.clone()],
            vec![ready_task.clone()],
            vec![ready_task.clone()],
            vec![ready_task],
        ])));
        repository
            .expect_load_orchestration_tasks()
            .times(4)
            .returning({
                let task_snapshots = Arc::clone(&task_snapshots);

                move |_| {
                    Ok(task_snapshots
                        .lock()
                        .expect("task snapshots should remain available")
                        .pop_front()
                        .expect("expected another task snapshot"))
                }
            });
        repository
            .expect_claim_orchestration_rollup()
            .withf(|id| *id == 1)
            .once()
            .returning(|_| Ok(true));
        repository
            .expect_load_rollup_operation_status()
            .withf(|operation_id| operation_id == "orchestration-rollup-1")
            .times(2)
            .returning(|_| Ok(Some("done".to_string())));
        expect_rollup_completion_failure_then_success(&mut repository);
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        let first_result = coordinator.reconcile_once().await;
        let second_result = coordinator.reconcile_once().await;
        let third_result = coordinator.reconcile_once().await;

        // Assert
        assert_eq!(first_result, Ok(()));
        assert_eq!(
            second_result,
            Err("injected post-submit failure".to_string())
        );
        assert_eq!(third_result, Ok(()));
        let calls = backend.calls();
        assert_eq!(
            calls
                .iter()
                .filter(|call| {
                    call.as_str() == "rollup-attempt:controller:orchestration-rollup-1"
                })
                .count(),
            1
        );
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.starts_with("rollup:controller:"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn failed_rollup_operation_is_retried_with_the_same_identifier() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        let mut submitting = orchestration(2);
        submitting.status = OrchestrationStatus::Submitting.to_string();
        repository
            .expect_load_active_orchestrations()
            .once()
            .return_once(move || Ok(vec![submitting]));
        let ready_task = task(
            1,
            "protocol",
            OrchestrationTaskStatus::Ready,
            Some("child-ready"),
        );
        mock_task_snapshots(&mut repository, vec![vec![ready_task]]);
        repository
            .expect_load_rollup_operation_status()
            .withf(|operation_id| operation_id == "orchestration-rollup-1")
            .once()
            .returning(|_| Ok(Some("failed".to_string())));
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        coordinator
            .reconcile_once()
            .await
            .expect("failed roll-up delivery should be retried");

        // Assert
        assert!(
            backend
                .calls()
                .iter()
                .any(|call| { call == "rollup-attempt:controller:orchestration-rollup-1" })
        );
    }

    #[tokio::test]
    async fn unfinished_rollups_wait_and_unknown_operation_states_fail() {
        // Arrange
        let backend = TestSessionBackend::default();
        let mut repository = MockOrchestrationRepository::new();
        let mut submitting = orchestration(2);
        submitting.status = OrchestrationStatus::Submitting.to_string();
        repository
            .expect_load_active_orchestrations()
            .times(3)
            .returning(move || Ok(vec![submitting.clone()]));
        let ready_task = task(
            1,
            "protocol",
            OrchestrationTaskStatus::Ready,
            Some("child-ready"),
        );
        mock_task_snapshots(
            &mut repository,
            vec![
                vec![ready_task.clone()],
                vec![ready_task.clone()],
                vec![ready_task],
            ],
        );
        let statuses = Arc::new(Mutex::new(VecDeque::from([
            "queued".to_string(),
            "running".to_string(),
            "unexpected".to_string(),
        ])));
        repository
            .expect_load_rollup_operation_status()
            .times(3)
            .returning(move |_| {
                Ok(statuses
                    .lock()
                    .expect("operation statuses should remain available")
                    .pop_front())
            });
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let coordinator =
            OrchestrationCoordinator::new(event_tx, Arc::new(repository), backend.service());

        // Act
        let queued_result = coordinator.reconcile_once().await;
        let running_result = coordinator.reconcile_once().await;
        let unknown_result = coordinator.reconcile_once().await;

        // Assert
        assert_eq!(queued_result, Ok(()));
        assert_eq!(running_result, Ok(()));
        assert_eq!(
            unknown_result,
            Err("Unknown roll-up operation status `unexpected` for orchestration 1".to_string())
        );
        assert!(backend.calls().is_empty());
    }

    #[tokio::test]
    async fn controller_response_without_subtasks_does_not_create_plan() {
        // Arrange
        let (database, _) = controller_database().await;
        let mut response = AgentResponse::plain("Use a regular session");

        // Act
        persist_controller_plan(&database, "controller", &mut response)
            .await
            .expect("empty plan handling should succeed");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("orchestration lookup should succeed");

        // Assert
        assert!(orchestration.is_none());
        assert!(response.questions.is_empty());
    }

    #[tokio::test]
    async fn controller_plan_persists_before_approval() {
        // Arrange
        let (database, _) = controller_database().await;
        database
            .settings()
            .upsert_setting(SettingName::OrchestrationParallelism, "4")
            .await
            .expect("failed to seed orchestration parallelism");
        let unchanged_prompt = TurnPrompt::from_text("ordinary work".to_string());
        let controller_turn = controller_prompt(
            &database,
            "controller",
            TurnPrompt::from_text("Build it".to_string()),
        )
        .await;
        let ordinary_turn = controller_prompt(&database, "missing", unchanged_prompt.clone()).await;
        let mut invalid_response = AgentResponse::plain("Invalid plan");
        invalid_response.subtasks = vec![subtask("protocol", &["crates/ag-protocol/"])];
        persist_controller_plan(&database, "controller", &mut invalid_response)
            .await
            .expect("invalid plan handling should succeed");

        // Act
        let (orchestration, tasks, response, approved_metadata) =
            persist_approved_two_task_plan(&database).await;

        // Assert
        assert!(
            controller_turn
                .agent_text()
                .contains("controller for an Agentty")
        );
        assert_eq!(controller_turn.text_source, TurnPromptTextSource::AgentData);
        assert_eq!(ordinary_turn, unchanged_prompt);
        assert!(invalid_response.subtasks.is_empty());
        assert!(invalid_response.questions[0].text.contains("at least two"));
        assert_eq!(response.questions[0].text, APPROVAL_QUESTION);
        assert_eq!(
            response.questions[0].options,
            vec!["Approve".to_string(), "Revise".to_string()]
        );
        assert_eq!(orchestration.max_parallelism, 4);
        assert_eq!(tasks.len(), 2);
        assert_eq!(
            approved_metadata.progress.as_deref(),
            Some("0 running, 0 waiting on you")
        );
    }

    #[tokio::test]
    async fn revising_a_controller_plan_cancels_it_before_fan_out() {
        // Arrange
        let (database, _) = controller_database().await;
        let mut response = AgentResponse::plain("Plan");
        response.subtasks = vec![
            subtask("protocol", &["crates/ag-protocol/"]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ];
        persist_controller_plan(&database, "controller", &mut response)
            .await
            .expect("plan should persist");

        // Act
        apply_plan_answer(
            &database,
            "controller",
            &[QuestionAnswer {
                answer: "Revise".to_string(),
                question: APPROVAL_QUESTION.to_string(),
            }],
        )
        .await
        .expect("revision should cancel the plan");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("orchestration should load")
            .expect("orchestration should exist");

        // Assert
        assert_eq!(
            orchestration.status,
            OrchestrationStatus::Canceled.to_string()
        );
    }

    #[tokio::test]
    async fn active_orchestration_discards_repeated_plan_approval() {
        // Arrange
        let (database, _) = controller_database().await;
        let mut initial_response = AgentResponse::plain("Plan");
        initial_response.subtasks = vec![
            subtask("protocol", &["crates/ag-protocol/"]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ];
        persist_controller_plan(&database, "controller", &mut initial_response)
            .await
            .expect("initial plan should persist");
        let mut repeated_response = AgentResponse::plain("Approval received");
        repeated_response.subtasks = vec![
            subtask("protocol", &["crates/ag-protocol/"]),
            subtask("ui", &["crates/agentty/src/ui/"]),
        ];
        repeated_response.questions = vec![
            QuestionItem::with_options(
                APPROVAL_QUESTION,
                vec!["Approve".to_string(), "Revise".to_string()],
            ),
            QuestionItem::new("Which worker needs more context?"),
        ];

        // Act
        persist_controller_plan(&database, "controller", &mut repeated_response)
            .await
            .expect("active plan handling should succeed");
        let orchestration = database
            .orchestrations()
            .load_orchestration_for_controller("controller")
            .await
            .expect("orchestration should load")
            .expect("one orchestration should remain");
        let persisted_tasks = database
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .expect("orchestration tasks should load");

        // Assert
        assert!(repeated_response.subtasks.is_empty());
        assert_eq!(
            repeated_response
                .questions
                .iter()
                .map(|question| question.text.as_str())
                .collect::<Vec<_>>(),
            vec!["Which worker needs more context?"]
        );
        assert_eq!(
            persisted_tasks
                .iter()
                .map(|task| task.task_key.as_str())
                .collect::<Vec<_>>(),
            vec!["protocol", "ui"]
        );

        // Act
        apply_plan_answer(
            &database,
            "controller",
            &[QuestionAnswer {
                answer: "Approve".to_string(),
                question: APPROVAL_QUESTION.to_string(),
            }],
        )
        .await
        .expect("approval should start orchestration");
        repeated_response.questions = vec![QuestionItem::new(APPROVAL_QUESTION.to_string())];
        persist_controller_plan(&database, "controller", &mut repeated_response)
            .await
            .expect("approval-only response handling should succeed");

        // Assert
        assert!(repeated_response.questions.is_empty());
    }

    #[tokio::test]
    async fn failed_task_retry_reuses_key_and_exposes_child_metadata() {
        // Arrange
        let (database, project_id) = controller_database().await;
        let (orchestration, tasks, _, _) = persist_approved_two_task_plan(&database).await;
        database
            .orchestrations()
            .update_orchestration_task_status(
                tasks[0].id,
                &OrchestrationTaskStatus::Failed.to_string(),
                Some("failed".to_string()),
            )
            .await
            .expect("failed to settle failed task");
        database
            .orchestrations()
            .update_orchestration_task_status(
                tasks[1].id,
                &OrchestrationTaskStatus::Ready.to_string(),
                None,
            )
            .await
            .expect("failed to settle ready task");
        database
            .orchestrations()
            .update_orchestration_status(orchestration.id, &OrchestrationStatus::Done.to_string())
            .await
            .expect("failed to settle orchestration");
        let mut retry_response = AgentResponse::plain("Retry");
        retry_response.subtasks = vec![subtask("protocol", &["crates/ag-protocol/"])];

        // Act
        persist_controller_plan(&database, "controller", &mut retry_response)
            .await
            .expect("retry should persist");
        let retried_tasks = database
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .expect("failed to load retried tasks");
        apply_plan_answer(
            &database,
            "controller",
            &[QuestionAnswer {
                answer: "Approve".to_string(),
                question: APPROVAL_QUESTION.to_string(),
            }],
        )
        .await
        .expect("retry approval should start orchestration");
        let claimed = database
            .orchestrations()
            .claim_orchestration_task(tasks[0].id)
            .await
            .expect("failed to claim retried task");
        database
            .sessions()
            .insert_session(
                "child-protocol",
                AgentKind::Codex.default_model().as_str(),
                "main",
                "InProgress",
                project_id,
            )
            .await
            .expect("failed to insert orchestration child");
        let linked = database
            .orchestrations()
            .link_orchestration_task_child(tasks[0].id, "child-protocol")
            .await
            .expect("failed to link orchestration child");
        let mut session_metadata = session_metadata_for_project(&database, project_id).await;
        let controller_metadata = session_metadata
            .remove("controller")
            .expect("controller metadata should load");
        let child_metadata = session_metadata
            .remove("child-protocol")
            .expect("child metadata should load");
        let active_child_count = running_child_count(&database, "controller").await;

        // Assert
        assert!(claimed);
        assert!(linked);
        assert_eq!(retried_tasks.len(), 2);
        assert_eq!(retried_tasks[0].id, tasks[0].id);
        assert_eq!(
            retried_tasks[0].status,
            OrchestrationTaskStatus::Planned.to_string()
        );
        assert_eq!(
            retried_tasks[1].status,
            OrchestrationTaskStatus::Ready.to_string()
        );
        assert_eq!(
            controller_metadata.progress.as_deref(),
            Some("1 running, 0 waiting on you")
        );
        assert_eq!(
            child_metadata.controller_session_id,
            Some(SessionId::from("controller"))
        );
        assert_eq!(active_child_count, 1);
    }

    #[tokio::test]
    async fn running_child_count_includes_reverse_linked_child() {
        // Arrange
        let (database, project_id) = controller_database().await;
        let orchestration_id = database
            .orchestrations()
            .insert_orchestration("controller", &OrchestrationStatus::Running.to_string(), 2)
            .await
            .expect("orchestration should persist");
        let task_id = database
            .orchestrations()
            .upsert_orchestration_task(PersistedOrchestrationTask {
                prompt: "Implement protocol".to_string(),
                session_orchestration_id: orchestration_id,
                task_key: "protocol".to_string(),
                title: "Protocol".to_string(),
                touched_areas: r#"["crates/ag-protocol/"]"#.to_string(),
            })
            .await
            .expect("task should persist");
        assert!(
            database
                .orchestrations()
                .claim_orchestration_task(task_id)
                .await
                .expect("task should be claimed")
        );
        database
            .sessions()
            .insert_session_with_agent(PersistedSessionCreation {
                agent: "codex",
                base_branch: "main",
                id: "reverse-linked-child",
                is_draft: false,
                model: AgentKind::Codex.default_model().as_str(),
                orchestration_task_id: Some(task_id),
                parent_session_id: None,
                personality_id: None,
                project_id,
                reasoning_level: ReasoningLevel::default(),
                role: None,
                speed_mode: SpeedMode::Normal,
                status: "InProgress",
            })
            .await
            .expect("reverse-linked child should persist");

        // Act
        let active_child_count = running_child_count(&database, "controller").await;

        // Assert
        assert_eq!(active_child_count, 1);
    }
}
