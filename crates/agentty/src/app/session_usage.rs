//! Per-session usage panel: loads recorded usage, keeps each running turn's
//! live usage, and refreshes the open panel whenever the store records new
//! usage for its session.
//!
//! The panel only reads the store and the live usage the turn-event
//! consumer reports; it never starts model work.

use std::collections::HashSet;
use std::sync::Arc;

use ag_contracts::{OneShotError, TurnUsage, UsagePurpose};
use ag_worker::{RunInfo, RunRepository, RunState};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{App, AppEvent};
use crate::domain::session::SessionId;
use crate::domain::session_usage::{
    ContextNow, LiveUsageKey, LiveUsageRun, SessionUsage, UsageRecord, parse_cost_basis,
    parse_usage_purpose,
};
use crate::infra::db::{SessionUsageContextRow, SessionUsageRow, UsageRepository};
use crate::presentation::app_mode::{AppMode, UsagePanel, UsageRestore};

/// One change to live usage, applied in arrival order so a run recorded
/// in the same reducer batch as the next run's first update keeps that
/// update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LiveUsageOp {
    /// Replaces one running turn's cumulative usage.
    Update {
        /// Running turn the usage belongs to.
        key: LiveUsageKey,
        /// Cumulative usage of that turn so far.
        usage: TurnUsage,
    },
    /// The store recorded usage for a session; drops the live usage of
    /// `run`, when given, whose usage is now stored or whose turn ended.
    Recorded {
        /// Run whose live usage the store now holds or that ended.
        run: Option<LiveUsageRun>,
        /// Session whose stored usage changed.
        session_id: SessionId,
    },
}

impl App {
    /// Opens the usage panel for the session `restore` returns to, loading
    /// its recorded usage.
    pub(crate) async fn open_usage_panel(&mut self, restore: UsageRestore) {
        let usage = load_session_usage(self.services.db().usage(), restore.session_id()).await;
        self.mode = AppMode::Usage(Box::new(UsagePanel {
            restore,
            scroll_offset: 0,
            usage,
        }));
    }

    /// Applies one batch's live usage changes in order, then reloads the
    /// open panel when the store recorded usage for its session.
    pub(super) async fn apply_live_usage_ops(&mut self, ops: Vec<LiveUsageOp>) {
        let mut recorded_session_ids = HashSet::new();
        for op in ops {
            match op {
                LiveUsageOp::Update { key, usage } => {
                    self.session_live_usage.insert(key, usage);
                }
                LiveUsageOp::Recorded { run, session_id } => {
                    if let Some(run) = run {
                        self.session_live_usage.remove(&LiveUsageKey {
                            run,
                            session_id: session_id.clone(),
                        });
                    }
                    recorded_session_ids.insert(session_id);
                }
            }
        }
        let db = self.services.db().clone();
        let AppMode::Usage(panel) = &mut self.mode else {
            return;
        };
        if recorded_session_ids.contains(panel.restore.session_id()) {
            panel.usage = load_session_usage(db.usage(), panel.restore.session_id()).await;
        }
    }
}

/// Run repository that reports each recorded utility run's usage to the
/// app, so an open usage panel refreshes when a title, commit message,
/// review, rebase, or other utility saves its usage.
pub(crate) struct UsageReportingRunRepository {
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    inner: Arc<dyn RunRepository>,
}

impl UsageReportingRunRepository {
    /// Wraps `inner`, reporting recorded usage through `app_event_tx`.
    pub(crate) fn new(
        inner: Arc<dyn RunRepository>,
        app_event_tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            app_event_tx,
            inner,
        }
    }
}

#[async_trait]
impl RunRepository for UsageReportingRunRepository {
    async fn create(&self, run: &RunInfo) -> Result<(), OneShotError> {
        self.inner.create(run).await
    }

    async fn claim_queued(&self, run: &RunInfo) -> Result<bool, OneShotError> {
        self.inner.claim_queued(run).await
    }

    async fn close_session(&self, session_id: &str) -> Result<(), OneShotError> {
        self.inner.close_session(session_id).await
    }

    async fn transition(
        &self,
        id: &str,
        state: RunState,
        error: Option<&str>,
    ) -> Result<(), OneShotError> {
        self.inner.transition(id, state, error).await
    }

    async fn heartbeat(&self, id: &str) -> Result<(), OneShotError> {
        self.inner.heartbeat(id).await
    }

    async fn record_usage(
        &self,
        session_id: &str,
        purpose: UsagePurpose,
        usage: &TurnUsage,
    ) -> Result<(), OneShotError> {
        self.inner.record_usage(session_id, purpose, usage).await?;
        // The receiver is gone only during shutdown.
        let _ = self.app_event_tx.send(AppEvent::SessionUsageRecorded {
            run: None,
            session_id: SessionId::from(session_id),
        });

        Ok(())
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        self.inner.recover().await
    }
}

/// Loads the recorded usage of `session_id`; a failed read shows no usage
/// rather than blocking the panel.
async fn load_session_usage(usage: &dyn UsageRepository, session_id: &str) -> SessionUsage {
    let rows = usage
        .load_session_usage(session_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(session_id, error = %error, "failed to load session usage");

            Vec::new()
        });
    let context = usage
        .load_session_usage_context(session_id)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(session_id, error = %error, "failed to load session context");

            SessionUsageContextRow::default()
        });

    session_usage_from_rows(rows, &context)
}

/// Converts stored usage rows and the session context into the domain
/// aggregate.
fn session_usage_from_rows(
    rows: Vec<SessionUsageRow>,
    context: &SessionUsageContextRow,
) -> SessionUsage {
    let records = rows.into_iter().map(usage_record_from_row).collect();
    let mut usage = SessionUsage {
        context: context.context_tokens.map(|tokens| ContextNow {
            tokens: stored_count(tokens),
            window_tokens: context.context_window_tokens.map(stored_count),
        }),
        records,
        turn_count: stored_count(context.turn_count),
    };
    usage.sort_records();

    usage
}

/// Converts one stored usage row.
fn usage_record_from_row(row: SessionUsageRow) -> UsageRecord {
    UsageRecord {
        cache_read_tokens: row.cache_read_tokens.map(stored_count),
        cache_write_tokens: row.cache_write_tokens.map(stored_count),
        context_peak_tokens: row.context_peak_tokens.map(stored_count),
        context_window_tokens: row.context_window_tokens.map(stored_count),
        cost_basis: row.cost_basis.as_deref().map(parse_cost_basis),
        cost_micro_usd: row.cost_micro_usd.map(stored_count),
        detailed_invocation_count: stored_count(row.detailed_invocation_count),
        input_tokens: stored_count(row.input_tokens),
        invocation_count: stored_count(row.invocation_count),
        model: row.model,
        output_tokens: stored_count(row.output_tokens),
        purpose: parse_usage_purpose(&row.purpose),
        reasoning_tokens: row.reasoning_tokens.map(stored_count),
        request_count: stored_count(row.request_count),
        unpriced_invocation_count: stored_count(row.unpriced_invocation_count),
        unreported_request_count: stored_count(row.unreported_request_count),
    }
}

/// Reads a stored nonnegative count; a negative value reads as zero.
fn stored_count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or_default()
}

#[cfg(test)]
#[path = "session_usage_test.rs"]
mod tests;
