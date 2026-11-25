//! Usage panel loading, live-usage bookkeeping, refresh, and utility usage
//! reporting tests.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::test_support::token_usage;
use ag_contracts::{CostBasis, OneShotError, TurnUsage, UsagePurpose};
use ag_worker::{RunInfo, RunRepository, RunState};
use async_trait::async_trait;
use tokio::sync::mpsc;

use super::{
    LiveUsageOp, UsageReportingRunRepository, load_session_usage, session_usage_from_rows,
};
use crate::app::{App, AppEvent, SessionManager};
use crate::domain::session::{SessionId, Status};
use crate::domain::session_usage::{ContextNow, LiveUsageKey, LiveUsageRun};
use crate::infra::db::{DbError, SessionUsageContextRow, SessionUsageRow, UsageRepository};
use crate::presentation::app_mode::{AppMode, ConfirmationViewMode, UsageRestore};
use crate::test_support::{new_test_app, session_fixture, session_manager_with_sessions};

/// Usage repository whose reads always fail.
struct FailingUsageRepository;

#[async_trait]
impl UsageRepository for FailingUsageRepository {
    async fn load_session_usage(&self, _session_id: &str) -> Result<Vec<SessionUsageRow>, DbError> {
        Err(DbError::InvalidData {
            entity: "session usage",
            reason: "unavailable".into(),
        })
    }

    async fn load_session_usage_context(
        &self,
        _session_id: &str,
    ) -> Result<SessionUsageContextRow, DbError> {
        Err(DbError::InvalidData {
            entity: "session context",
            reason: "unavailable".into(),
        })
    }

    async fn upsert_session_usage(
        &self,
        _session_id: &str,
        _purpose: UsagePurpose,
        _usage: &TurnUsage,
    ) -> Result<(), DbError> {
        Ok(())
    }
}

/// Run repository that records each call and fails usage recording when
/// asked to.
#[derive(Default)]
struct RecordingRunRepository {
    calls: Mutex<Vec<String>>,
    fails_usage: bool,
}

impl RecordingRunRepository {
    /// Records one delegated call.
    fn record(&self, call: &str) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call.to_string());
    }
}

#[async_trait]
impl RunRepository for RecordingRunRepository {
    async fn create(&self, _run: &RunInfo) -> Result<(), OneShotError> {
        self.record("create");

        Ok(())
    }

    async fn claim_queued(&self, _run: &RunInfo) -> Result<bool, OneShotError> {
        self.record("claim_queued");

        Ok(true)
    }

    async fn close_session(&self, _session_id: &str) -> Result<(), OneShotError> {
        self.record("close_session");

        Ok(())
    }

    async fn transition(
        &self,
        _id: &str,
        _state: RunState,
        _error: Option<&str>,
    ) -> Result<(), OneShotError> {
        self.record("transition");

        Ok(())
    }

    async fn heartbeat(&self, _id: &str) -> Result<(), OneShotError> {
        self.record("heartbeat");

        Ok(())
    }

    async fn record_usage(
        &self,
        _session_id: &str,
        _purpose: UsagePurpose,
        _usage: &TurnUsage,
    ) -> Result<(), OneShotError> {
        self.record("record_usage");
        if self.fails_usage {
            return Err(OneShotError::new("usage unavailable"));
        }

        Ok(())
    }

    async fn recover(&self) -> Result<(), OneShotError> {
        self.record("recover");

        Ok(())
    }
}

/// Seeds `sess` in the store and the session manager, and records one turn
/// of 100 input and 20 output tokens.
async fn seed_session(app: &mut App) {
    let db = app.services.db().clone();
    let project = db
        .projects()
        .upsert_project("project", None)
        .await
        .expect("project");
    db.sessions()
        .insert_session("sess", "gpt-6.1-sol", "main", "Review", project)
        .await
        .expect("session");
    record_turn_usage(app, 100).await;
    let manager: &mut SessionManager = &mut app.sessions;
    *manager = session_manager_with_sessions(vec![session_fixture("sess", Status::InProgress)]);
}

/// Records one turn of `input_tokens` for `sess` in the store.
async fn record_turn_usage(app: &App, input_tokens: u64) {
    app.services
        .db()
        .usage()
        .upsert_session_usage(
            "sess",
            UsagePurpose::Turn,
            &token_usage("model", input_tokens, 1),
        )
        .await
        .expect("usage");
}

/// Returns the restore target of the session view of `sess`.
fn view_restore() -> UsageRestore {
    UsageRestore::View(ConfirmationViewMode {
        scroll_offset: None,
        session_id: "sess".into(),
    })
}

/// Returns the live-usage key of `run` in `session_id`.
fn live_key(session_id: &str, run: LiveUsageRun) -> LiveUsageKey {
    LiveUsageKey {
        run,
        session_id: session_id.into(),
    }
}

/// Returns an update of `run` in `session_id` to `input_tokens`.
fn update(session_id: &str, run: LiveUsageRun, input_tokens: u64) -> LiveUsageOp {
    LiveUsageOp::Update {
        key: live_key(session_id, run),
        usage: token_usage("model", input_tokens, 1),
    }
}

/// Returns the input tokens of the open panel's first record.
fn panel_input_tokens(app: &App) -> Option<u64> {
    match &app.mode {
        AppMode::Usage(panel) => panel
            .usage
            .records
            .first()
            .map(|record| record.input_tokens),
        _ => None,
    }
}

#[test]
fn test_session_usage_from_rows_converts_and_sorts_records() {
    // Arrange
    let rows = vec![
        SessionUsageRow {
            model: "model".to_string(),
            purpose: "title".to_string(),
            input_tokens: -5,
            ..SessionUsageRow::default()
        },
        SessionUsageRow {
            cache_read_tokens: Some(4),
            cost_basis: Some("list".to_string()),
            cost_micro_usd: Some(9),
            model: "model".to_string(),
            purpose: "turn".to_string(),
            input_tokens: 10,
            ..SessionUsageRow::default()
        },
    ];
    let context = SessionUsageContextRow {
        context_tokens: Some(70),
        context_window_tokens: Some(100),
        turn_count: 3,
    };

    // Act
    let usage = session_usage_from_rows(rows, &context);

    // Assert
    assert_eq!(usage.records[0].purpose, UsagePurpose::Turn);
    assert_eq!(usage.records[0].cache_read_tokens, Some(4));
    assert_eq!(usage.records[0].cost_basis, Some(CostBasis::ListPrice));
    assert_eq!(usage.records[1].purpose, UsagePurpose::Title);
    assert_eq!(usage.records[1].input_tokens, 0);
    assert_eq!(
        usage.context,
        Some(ContextNow {
            tokens: 70,
            window_tokens: Some(100),
        })
    );
    assert_eq!(usage.turn_count, 3);
}

#[tokio::test]
async fn test_load_session_usage_shows_nothing_when_reads_fail() {
    // Arrange
    let repository = FailingUsageRepository;

    // Act
    let usage = load_session_usage(&repository, "sess").await;
    let upserted = repository
        .upsert_session_usage("sess", UsagePurpose::Turn, &TurnUsage::default())
        .await;

    // Assert
    assert_eq!(usage.records, []);
    assert_eq!(usage.context, None);
    assert_eq!(usage.turn_count, 0);
    assert!(upserted.is_ok());
}

#[tokio::test]
async fn test_open_usage_panel_loads_recorded_usage_unscrolled() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app).await;

    // Act
    app.open_usage_panel(view_restore()).await;

    // Assert
    assert_eq!(panel_input_tokens(&app), Some(100));
    assert!(matches!(&app.mode, AppMode::Usage(panel) if panel.scroll_offset == 0));
}

#[tokio::test]
async fn test_live_usage_is_keyed_per_run_and_dropped_when_its_run_is_recorded() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app).await;
    let first_attempt = LiveUsageRun::Attempt("attempt-1".to_string());
    let second_attempt = LiveUsageRun::Attempt("attempt-2".to_string());

    // Act: concurrent graph tasks keep separate entries, and one recorded
    // attempt drops only its own entry.
    app.apply_live_usage_ops(vec![
        update("sess", first_attempt.clone(), 5),
        update("sess", second_attempt.clone(), 7),
        update("sess", LiveUsageRun::SessionTurn, 9),
        LiveUsageOp::Recorded {
            run: Some(first_attempt.clone()),
            session_id: "sess".into(),
        },
    ])
    .await;

    // Assert
    assert!(
        !app.session_live_usage
            .contains_key(&live_key("sess", first_attempt))
    );
    assert!(
        app.session_live_usage
            .contains_key(&live_key("sess", second_attempt))
    );
    assert!(
        app.session_live_usage
            .contains_key(&live_key("sess", LiveUsageRun::SessionTurn))
    );
}

#[tokio::test]
async fn test_recorded_run_keeps_a_later_update_in_the_same_batch() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app).await;

    // Act
    app.apply_live_usage_ops(vec![
        update("sess", LiveUsageRun::SessionTurn, 5),
        LiveUsageOp::Recorded {
            run: Some(LiveUsageRun::SessionTurn),
            session_id: "sess".into(),
        },
        update("sess", LiveUsageRun::SessionTurn, 3),
    ])
    .await;

    // Assert
    assert_eq!(
        app.session_live_usage
            .get(&live_key("sess", LiveUsageRun::SessionTurn))
            .map(TurnUsage::input_tokens),
        Some(3)
    );
}

#[tokio::test]
async fn test_recorded_usage_reloads_only_the_open_panel_of_its_session() {
    // Arrange
    let (mut app, _directory) = new_test_app().await;
    seed_session(&mut app).await;
    app.open_usage_panel(view_restore()).await;
    record_turn_usage(&app, 5).await;

    // Act
    app.apply_live_usage_ops(vec![LiveUsageOp::Recorded {
        run: None,
        session_id: "other".into(),
    }])
    .await;
    let other_session_tokens = panel_input_tokens(&app);
    app.apply_live_usage_ops(vec![LiveUsageOp::Recorded {
        run: None,
        session_id: "sess".into(),
    }])
    .await;
    let own_session_tokens = panel_input_tokens(&app);
    app.mode = AppMode::List;
    app.apply_live_usage_ops(vec![LiveUsageOp::Recorded {
        run: None,
        session_id: "sess".into(),
    }])
    .await;

    // Assert
    assert_eq!(other_session_tokens, Some(100));
    assert_eq!(own_session_tokens, Some(105));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_usage_reporting_run_repository_delegates_and_reports_recorded_usage() {
    // Arrange
    let inner = Arc::new(RecordingRunRepository::default());
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let repository = UsageReportingRunRepository::new(inner.clone(), app_event_tx);
    let run = RunInfo {
        folder: PathBuf::from("/tmp"),
        id: "run".to_string(),
        parent_id: None,
        project_id: None,
        purpose: "title".to_string(),
        session_id: Some("sess".to_string()),
    };

    // Act
    repository.create(&run).await.expect("create");
    let claimed = repository.claim_queued(&run).await.expect("claim");
    repository.close_session("sess").await.expect("close");
    repository
        .transition("run", RunState::Completed, None)
        .await
        .expect("transition");
    repository.heartbeat("run").await.expect("heartbeat");
    repository
        .record_usage("sess", UsagePurpose::Title, &TurnUsage::default())
        .await
        .expect("record usage");
    repository.recover().await.expect("recover");

    // Assert
    assert!(claimed);
    assert_eq!(
        *inner
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [
            "create",
            "claim_queued",
            "close_session",
            "transition",
            "heartbeat",
            "record_usage",
            "recover"
        ]
    );
    assert_eq!(
        app_event_rx.try_recv().ok(),
        Some(AppEvent::SessionUsageRecorded {
            run: None,
            session_id: SessionId::from("sess"),
        })
    );
    assert!(app_event_rx.try_recv().is_err());
}

#[tokio::test]
async fn test_usage_reporting_run_repository_reports_nothing_when_recording_fails() {
    // Arrange
    let inner = Arc::new(RecordingRunRepository {
        fails_usage: true,
        ..RecordingRunRepository::default()
    });
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let repository = UsageReportingRunRepository::new(inner, app_event_tx);

    // Act
    let recorded = repository
        .record_usage("sess", UsagePurpose::Review, &TurnUsage::default())
        .await;

    // Assert
    assert!(recorded.is_err());
    assert!(app_event_rx.try_recv().is_err());
}
