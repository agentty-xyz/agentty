//! Runtime event loop and terminal rendering orchestration.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ag_orchestration::{OrchestrationCoordinator, OrchestrationSchedule};
use async_trait::async_trait;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use tokio::sync::mpsc;

use crate::app::App;
use crate::infra::clock::Clock;
use crate::runtime::{FRAME_INTERVAL, PresentationState, event, terminal};

/// Fallback redraw cadence for visible spinner and timer UI when no new
/// events arrive.
const FORCED_REDRAW_INTERVAL: Duration = Duration::from_millis(200);
/// Coordinator polling cadence while the terminal runtime is active.
const ORCHESTRATION_RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

/// Tokio-backed production schedule for orchestration reconciliation.
struct RuntimeOrchestrationSchedule {
    interval: tokio::time::Interval,
}

impl RuntimeOrchestrationSchedule {
    fn new() -> Self {
        let mut interval = tokio::time::interval(ORCHESTRATION_RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        Self { interval }
    }
}

#[async_trait]
impl OrchestrationSchedule for RuntimeOrchestrationSchedule {
    async fn wait_for_reconciliation(&mut self) {
        self.interval.tick().await;
    }
}

/// Concrete terminal type used by the production runtime entry point.
pub(crate) type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Converts a backend-specific error into `io::Error`.
///
/// This enables generic functions to use `?` with `Terminal` methods that
/// return `Result<_, B::Error>` for any backend, including `TestBackend`
/// whose error type is `Infallible`.
pub(crate) fn backend_err<E: std::error::Error + Send + Sync + 'static>(error: E) -> io::Error {
    io::Error::other(error)
}

/// Event-loop continuation outcome after processing one input/tick cycle.
pub(crate) enum EventResult {
    /// Continue running the runtime loop.
    Continue,
    /// Exit the runtime loop and terminate the TUI session.
    Quit,
}

/// Owns the blocking terminal-reader thread and its shutdown signal.
struct EventReaderTask {
    join_handle: std::thread::JoinHandle<()>,
    shutdown: Arc<AtomicBool>,
}

impl EventReaderTask {
    /// Starts the production terminal reader for `event_tx`.
    fn spawn(event_tx: mpsc::UnboundedSender<io::Result<crossterm::event::Event>>) -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let join_handle = event::spawn_event_reader(event_tx, Arc::clone(&shutdown));

        Self {
            join_handle,
            shutdown,
        }
    }

    /// Requests reader shutdown and waits for the blocking thread to finish.
    async fn shutdown(self) -> io::Result<()> {
        self.shutdown.store(true, Ordering::Relaxed);
        let join_result = tokio::task::spawn_blocking(move || self.join_handle.join()).await;

        Self::map_join_result(join_result)
    }

    /// Maps the blocking join task and reader thread outcomes into one runtime
    /// error surface.
    fn map_join_result(
        join_result: Result<std::thread::Result<()>, tokio::task::JoinError>,
    ) -> io::Result<()> {
        let reader_result = join_result.map_err(|error| {
            io::Error::other(format!("failed to join event reader task: {error}"))
        })?;

        reader_result.map_err(|_| io::Error::other("terminal event reader panicked"))
    }
}

/// Runs the TUI event/render loop until the user exits.
///
/// # Errors
/// Returns an error if terminal setup, rendering, or event processing fails.
pub async fn run(app: &mut App) -> io::Result<()> {
    let terminal_guard = terminal::TerminalGuard::new();
    let mut terminal = terminal::setup_terminal(&terminal_guard)?;

    // Spawn a dedicated thread for crossterm event reading so the main async
    // loop can yield to tokio between iterations.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let event_reader_task = EventReaderTask::spawn(event_tx);

    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let run_result = run_main_loop(app, &mut terminal, &mut event_rx, &mut tick).await;
    let reader_shutdown_result = event_reader_task.shutdown().await;
    app.wait_for_background_cleanup_tasks().await;
    let cursor_result = terminal.show_cursor().map_err(backend_err);

    run_result.and(reader_shutdown_result).and(cursor_result)
}

/// Runs the TUI event/render loop with an externally provided backend and
/// event channel.
///
/// Tests use this to drive the full runtime with a `TestBackend` and injected
/// `crossterm::event::Event` values, bypassing terminal setup and the
/// background event-reader thread.
///
/// # Errors
/// Returns an error if rendering or event processing fails.
pub async fn run_with_backend<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let run_result = run_main_loop(app, terminal, event_rx, &mut tick).await;
    app.wait_for_background_cleanup_tasks().await;

    run_result
}

/// Drives the main render/event loop until quit or error.
///
/// Reads the runtime clock from `app.services.clock()` so render-throttle
/// timing is sourced through the same `Clock` trait used by session refresh
/// logic, keeping `runtime` free of direct `Instant::now()` calls.
async fn run_main_loop<B: Backend, Message: event::TerminalEventMessage>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<Message>,
    tick: &mut tokio::time::Interval,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let _session_runtime_consumer = app.sessions.foreground_consumer();
    let orchestration_coordinator = OrchestrationCoordinator::new(
        Arc::new(app.services.clone()),
        app.services.db().orchestration_repository(),
        app.coordinator_session_service(),
    );
    let orchestration_task =
        tokio::spawn(orchestration_coordinator.run(RuntimeOrchestrationSchedule::new()));
    let clock = app.services.clock();
    let last_draw_at = clock.now_instant();
    let mut main_loop_state = MainLoopState {
        app,
        clock,
        event_rx,
        last_draw_at,
        presentation: Rc::new(PresentationState::default()),
        terminal,
        tick,
    };

    let result = run_until_quit(&mut main_loop_state, |state| Box::pin(state.run_cycle())).await;
    let orchestration_shutdown_result = stop_orchestration_task(orchestration_task).await;

    result.and(orchestration_shutdown_result)
}

/// Cancels the coordinator and observes its final task result.
async fn stop_orchestration_task(
    orchestration_task: tokio::task::JoinHandle<()>,
) -> io::Result<()> {
    orchestration_task.abort();

    match orchestration_task.await {
        Ok(()) => Ok(()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(io::Error::other(format!(
            "orchestration coordinator task failed: {error}"
        ))),
    }
}

/// Borrowed runtime state required to process one main-loop cycle.
struct MainLoopState<'a, B: Backend, Message> {
    app: &'a mut App,
    clock: Arc<dyn Clock>,
    event_rx: &'a mut mpsc::UnboundedReceiver<Message>,
    last_draw_at: Instant,
    presentation: Rc<PresentationState>,
    terminal: &'a mut Terminal<B>,
    tick: &'a mut tokio::time::Interval,
}

impl<B: Backend, Message: event::TerminalEventMessage> MainLoopState<'_, B, Message>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Runs one render/event cycle and returns the continuation result.
    ///
    /// Pending app events are reduced before draw so touched sessions refresh
    /// from their live handles without a full per-frame session sweep. The open
    /// session view then reconciles into the clarification panel when its
    /// session has reached `Status::Question`, covering cases where the live
    /// `AgentResponseReceived` projection did not flip the view.
    async fn run_cycle(&mut self) -> io::Result<EventResult> {
        self.app.process_pending_app_events().await;
        self.app.reconcile_open_session_question_mode().await;
        self.app
            .expire_project_sync_status(self.clock.now_instant());
        render_frame(
            self.app,
            self.terminal,
            self.clock.as_ref(),
            &mut self.last_draw_at,
            self.presentation.as_ref(),
        )?;

        event::process_events(
            self.app,
            Rc::clone(&self.presentation),
            self.terminal,
            self.event_rx,
            self.tick,
        )
        .await
    }
}

/// Repeats an async runtime cycle until one cycle returns `EventResult::Quit`.
async fn run_until_quit<State, CycleFn>(state: &mut State, mut cycle: CycleFn) -> io::Result<()>
where
    CycleFn: for<'state> FnMut(
        &'state mut State,
    )
        -> Pin<Box<dyn Future<Output = io::Result<EventResult>> + 'state>>,
{
    loop {
        if matches!(cycle(state).await?, EventResult::Quit) {
            break;
        }
    }

    Ok(())
}

/// Renders one frame of the TUI application into the terminal buffer.
///
/// Idle redraws are skipped unless the app explicitly requested a fresh frame
/// or one visible spinner/timer has reached the forced redraw cadence. Both
/// the elapsed-time comparison and the `last_draw_at` stamp read through the
/// injected `Clock` so test runs can virtualize the render-throttle clock
/// without mutating production timing behavior.
fn render_frame<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    clock: &dyn Clock,
    last_draw_at: &mut Instant,
    presentation: &PresentationState,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let forced_redraw_due =
        app.has_visible_tick_driven_ui() && forced_redraw_elapsed(clock, *last_draw_at);
    if !app.needs_redraw() && !forced_redraw_due {
        return Ok(());
    }

    let snapshot = app.view_snapshot();
    if presentation.terminal_clear_needed(&snapshot) {
        clear_terminal_for_surface_change(terminal)?;
    }
    terminal
        .draw(|frame| {
            presentation.render(&snapshot, frame);
        })
        .map_err(backend_err)?;
    presentation.record_rendered_surface(&snapshot);
    app.clear_redraw();
    *last_draw_at = clock.now_instant();

    Ok(())
}

/// Clears the fullscreen backend and invalidates Ratatui's previous frame.
///
/// `Terminal::clear()` snapshots the cursor through an interactive terminal
/// query. Agentty renders in the alternate fullscreen buffer, so clearing the
/// full backend directly preserves the cursor while avoiding that query. The
/// extra buffer swap resets Ratatui's previous frame and forces the following
/// draw to repaint every rendered cell.
fn clear_terminal_for_surface_change<B: Backend>(terminal: &mut Terminal<B>) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal
        .backend_mut()
        .clear_region(ClearType::All)
        .map_err(backend_err)?;
    terminal.swap_buffers();

    Ok(())
}

/// Returns whether the injected clock has advanced past `last_draw_at` by at
/// least `FORCED_REDRAW_INTERVAL`.
fn forced_redraw_elapsed(clock: &dyn Clock, last_draw_at: Instant) -> bool {
    clock.now_instant().saturating_duration_since(last_draw_at) >= FORCED_REDRAW_INTERVAL
}

#[cfg(test)]
#[path = "core_test.rs"]
mod tests;
