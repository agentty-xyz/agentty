//! Runtime event loop and terminal rendering orchestration.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend};
use tokio::sync::mpsc;

use crate::app::App;
use crate::infra::clock::Clock;
use crate::runtime::{FRAME_INTERVAL, event, terminal};

/// Fallback redraw cadence for visible spinner and timer UI when no new
/// events arrive.
const FORCED_REDRAW_INTERVAL: Duration = Duration::from_millis(200);

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
    let shutdown = Arc::new(AtomicBool::new(false));
    event::spawn_event_reader(event_tx, shutdown.clone());

    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let run_result = run_main_loop(app, &mut terminal, &mut event_rx, &mut tick).await;
    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    app.wait_for_background_cleanup_tasks().await;
    terminal.show_cursor()?;

    run_result
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
async fn run_main_loop<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    tick: &mut tokio::time::Interval,
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let clock = app.services.clock();
    let last_draw_at = clock.now_instant();
    let mut main_loop_state = MainLoopState {
        app,
        clock,
        event_rx,
        last_draw_at,
        terminal,
        tick,
    };

    run_until_quit(&mut main_loop_state, |state| Box::pin(state.run_cycle())).await
}

/// Borrowed runtime state required to process one main-loop cycle.
struct MainLoopState<'a, B: Backend> {
    app: &'a mut App,
    clock: Arc<dyn Clock>,
    event_rx: &'a mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    last_draw_at: Instant,
    terminal: &'a mut Terminal<B>,
    tick: &'a mut tokio::time::Interval,
}

impl<B: Backend> MainLoopState<'_, B>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    /// Runs one render/event cycle and returns the continuation result.
    ///
    /// Pending app events are reduced before draw so touched sessions refresh
    /// from their live handles without a full per-frame session sweep.
    async fn run_cycle(&mut self) -> io::Result<EventResult> {
        self.app.process_pending_app_events().await;
        render_frame(
            self.app,
            self.terminal,
            self.clock.as_ref(),
            &mut self.last_draw_at,
        )?;

        event::process_events(self.app, self.terminal, self.event_rx, self.tick).await
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
) -> io::Result<()>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let forced_redraw_due =
        app.has_visible_tick_driven_ui() && forced_redraw_elapsed(clock, *last_draw_at);
    if !app.needs_redraw() && !forced_redraw_due {
        return Ok(());
    }

    terminal
        .draw(|frame| app.draw(frame))
        .map_err(backend_err)?;
    app.clear_redraw();
    *last_draw_at = clock.now_instant();

    Ok(())
}

/// Returns whether the injected clock has advanced past `last_draw_at` by at
/// least `FORCED_REDRAW_INTERVAL`.
fn forced_redraw_elapsed(clock: &dyn Clock, last_draw_at: Instant) -> bool {
    clock.now_instant().saturating_duration_since(last_draw_at) >= FORCED_REDRAW_INTERVAL
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
    use ratatui::buffer::Cell;
    use ratatui::layout::{Position, Size};
    use tempfile::tempdir;

    use super::*;
    use crate::app::AppEvent;
    use crate::db::Database;
    use crate::domain::session::tests::SessionFixtureBuilder;
    use crate::domain::session::{SessionHandles, Status};
    use crate::ui::state::app_mode::AppMode;

    /// Test-only loop state that records call counts and scripted outcomes.
    struct TestLoopState {
        cycle_count: usize,
        results: VecDeque<io::Result<EventResult>>,
    }

    impl TestLoopState {
        /// Runs one scripted test cycle.
        fn run_cycle(&mut self) -> io::Result<EventResult> {
            self.cycle_count += 1;

            self.results
                .pop_front()
                .expect("test should provide one result per cycle")
        }
    }

    /// Builds one client bundle with deterministic agent availability for
    /// test app startup.
    fn test_app_clients() -> crate::app::AppClients {
        crate::app::AppClients::new().with_agent_availability_probe(std::sync::Arc::new(
            crate::infra::agent::StaticAgentAvailabilityProbe {
                available_agent_kinds: crate::domain::agent::AgentKind::ALL.to_vec(),
            },
        ))
    }

    #[tokio::test]
    async fn run_until_quit_stops_after_first_quit_result() {
        // Arrange
        let mut state = TestLoopState {
            cycle_count: 0,
            results: VecDeque::from([
                Ok(EventResult::Continue),
                Ok(EventResult::Quit),
                Ok(EventResult::Continue),
            ]),
        };

        // Act
        let loop_result = run_until_quit(&mut state, |loop_state| {
            Box::pin(async move { loop_state.run_cycle() })
        })
        .await;

        // Assert
        assert!(loop_result.is_ok());
        assert_eq!(state.cycle_count, 2);
    }

    #[tokio::test]
    async fn run_until_quit_returns_cycle_error_without_extra_iterations() {
        // Arrange
        let mut state = TestLoopState {
            cycle_count: 0,
            results: VecDeque::from([Err(io::Error::other("cycle failed"))]),
        };

        // Act
        let loop_result = run_until_quit(&mut state, |loop_state| {
            Box::pin(async move { loop_state.run_cycle() })
        })
        .await;

        // Assert
        let error = loop_result.expect_err("loop should return the cycle error");
        assert_eq!(error.to_string(), "cycle failed");
        assert_eq!(state.cycle_count, 1);
    }

    /// Builds a test app rooted at a temporary directory.
    ///
    /// Returns both the `App` and the `TempDir` guard so the caller keeps the
    /// temporary directory alive for the full test lifetime.
    async fn new_test_app() -> (App, tempfile::TempDir) {
        let base_dir = tempdir().expect("failed to create temp dir");
        let base_path = base_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        let app = App::new_with_clients(
            base_path.clone(),
            base_path,
            None,
            database,
            test_app_clients(),
        )
        .await
        .expect("failed to build test app");

        (app, base_dir)
    }

    /// Flattens a test terminal buffer into one searchable string.
    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        buffer
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    /// Test backend wrapper that counts each `Terminal::draw()` flush.
    struct CountingBackend {
        draw_count: Arc<AtomicUsize>,
        inner: TestBackend,
    }

    impl CountingBackend {
        /// Creates a counting wrapper around one `TestBackend`.
        fn new(width: u16, height: u16) -> (Self, Arc<AtomicUsize>) {
            let draw_count = Arc::new(AtomicUsize::new(0));

            (
                Self {
                    draw_count: Arc::clone(&draw_count),
                    inner: TestBackend::new(width, height),
                },
                draw_count,
            )
        }
    }

    impl Backend for CountingBackend {
        type Error = Infallible;

        fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
        where
            I: Iterator<Item = (u16, u16, &'a Cell)>,
        {
            self.draw_count.fetch_add(1, Ordering::Relaxed);
            self.inner.draw(content)
        }

        fn hide_cursor(&mut self) -> Result<(), Self::Error> {
            self.inner.hide_cursor()
        }

        fn show_cursor(&mut self) -> Result<(), Self::Error> {
            self.inner.show_cursor()
        }

        fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
            self.inner.get_cursor_position()
        }

        fn set_cursor_position<P: Into<Position>>(
            &mut self,
            position: P,
        ) -> Result<(), Self::Error> {
            self.inner.set_cursor_position(position)
        }

        fn clear(&mut self) -> Result<(), Self::Error> {
            self.inner.clear()
        }

        fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
            self.inner.clear_region(clear_type)
        }

        fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
            self.inner.append_lines(n)
        }

        fn size(&self) -> Result<Size, Self::Error> {
            self.inner.size()
        }

        fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
            self.inner.window_size()
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            self.inner.flush()
        }
    }

    /// Verifies that `run_with_backend` drives the main loop with a
    /// `TestBackend` and exits cleanly when quit key events are injected.
    #[tokio::test]
    async fn run_with_backend_exits_on_quit_key() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        // Send `q` to open the quit confirmation, then `y` to confirm.
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send quit key");
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send confirm key");

        // Act
        let result = run_with_backend(&mut app, &mut terminal, &mut event_rx).await;

        // Assert
        assert!(
            result.is_ok(),
            "run_with_backend should exit cleanly on quit"
        );
    }

    #[tokio::test]
    async fn run_with_backend_waits_for_cleanup_tasks_after_quit() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (release_tx, mut release_rx) = mpsc::unbounded_channel::<()>();
        let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();
        app.services.track_cleanup_task(tokio::spawn(async move {
            let _ = release_rx.recv().await;
            let _ = done_tx.send(());
        }));

        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send quit key");
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send confirm key");
        let run_future = run_with_backend(&mut app, &mut terminal, &mut event_rx);
        tokio::pin!(run_future);

        // Act / Assert — the runtime should not complete while the tracked
        // cleanup task is still pending.
        tokio::select! {
            result = &mut run_future => {
                unreachable!("runtime exited before cleanup task finished: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        // Act
        release_tx.send(()).expect("failed to release cleanup task");
        let result = run_future.await;

        // Assert
        assert!(
            result.is_ok(),
            "run_with_backend should exit cleanly after cleanup"
        );
        assert!(done_rx.try_recv().is_ok());
    }

    #[tokio::test]
    /// Verifies idle `run_with_backend` redraws stay throttled when no visible
    /// spinner or timer is active.
    async fn run_with_backend_skips_idle_redraws_without_tick_driven_ui() {
        // Arrange
        let (mut app, _base_dir) = new_test_app().await;
        let (backend, draw_count) = CountingBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let run_future = run_with_backend(&mut app, &mut terminal, &mut event_rx);
        tokio::pin!(run_future);

        // Act
        tokio::select! {
            result = &mut run_future => {
                unreachable!("runtime exited before idle window elapsed: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(1100)) => {}
        }
        let idle_draw_count = draw_count.load(Ordering::Relaxed);
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send quit key");
        event_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
            )))
            .expect("failed to send confirm key");
        let result = run_future.await;

        // Assert
        assert!(
            result.is_ok(),
            "run_with_backend should exit cleanly on quit"
        );
        assert!(
            idle_draw_count <= 2,
            "expected at most two idle draws per second, observed {idle_draw_count}"
        );
    }

    #[tokio::test]
    /// Verifies one queued `SessionUpdated` event syncs the touched session
    /// before the next render without scanning all session handles.
    async fn run_cycle_renders_pending_session_update_before_waiting_for_events() {
        // Arrange
        let (mut app, base_dir) = new_test_app().await;
        let session_id = "session-1".to_string();
        let mut event_rx = mpsc::unbounded_channel().1;
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
        let mut tick = tokio::time::interval(Duration::from_millis(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let session = SessionFixtureBuilder::new()
            .id(session_id.clone())
            .folder(base_dir.path().to_path_buf())
            .status(Status::InProgress)
            .build();
        app.sessions.push_session(session);
        app.sessions.session_handles_mut().insert(
            session_id.clone().into(),
            SessionHandles::new(String::new(), Status::InProgress),
        );

        app.mode = AppMode::View {
            review_status_message: None,
            review_text: None,
            session_id: session_id.clone().into(),
            scroll_offset: None,
        };
        if let Some(session) = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.status = Status::InProgress;
        }
        if let Some(handles) = app.sessions.session_handles().get(session_id.as_str()) {
            if let Ok(mut output) = handles.output.lock() {
                output.push_str("synced output");
            }
            if let Ok(mut status) = handles.status.lock() {
                *status = Status::InProgress;
            }
        }
        app.services.emit_app_event(AppEvent::SessionUpdated {
            session_id: session_id.clone().into(),
            version: 1,
        });

        let clock: Arc<dyn Clock> = Arc::new(crate::infra::clock::RealClock);
        let last_draw_at = clock.now_instant();
        let mut main_loop_state = MainLoopState {
            app: &mut app,
            clock,
            event_rx: &mut event_rx,
            last_draw_at,
            terminal: &mut terminal,
            tick: &mut tick,
        };

        // Act
        let cycle_result = main_loop_state.run_cycle().await;
        let rendered_text = buffer_text(terminal.backend().buffer());

        // Assert
        assert!(matches!(cycle_result, Ok(EventResult::Continue)));
        assert!(
            rendered_text.contains("synced output"),
            "expected rendered session output to contain synced handle text: {rendered_text}"
        );
    }

    /// Stationary clock whose `now_instant()` value never advances.
    ///
    /// Used to verify that `forced_redraw_elapsed` reads elapsed time through
    /// the injected `Clock` rather than the host wall clock.
    struct FrozenInstantClock {
        instant: std::time::Instant,
    }

    impl Clock for FrozenInstantClock {
        fn now_instant(&self) -> std::time::Instant {
            self.instant
        }

        fn now_system_time(&self) -> std::time::SystemTime {
            std::time::SystemTime::now()
        }
    }

    /// Verifies the forced-redraw cadence reads elapsed time through the
    /// injected `Clock`. The frozen clock and `last_draw_at` are anchored to
    /// the same host instant, while host wall time has already drifted past
    /// `FORCED_REDRAW_INTERVAL` since the anchor instant. A correct
    /// implementation reports zero virtual elapsed time and skips the draw.
    #[test]
    fn forced_redraw_elapsed_uses_injected_clock_not_host_time() {
        // Arrange
        let anchor = std::time::Instant::now()
            .checked_sub(FORCED_REDRAW_INTERVAL * 10)
            .expect("anchor instant should fit before host now");
        let frozen = FrozenInstantClock { instant: anchor };

        // Act
        let elapsed = forced_redraw_elapsed(&frozen, anchor);

        // Assert
        assert!(
            !elapsed,
            "forced-redraw cadence must read elapsed time through the injected clock, not the \
             host wall clock"
        );
    }

    /// Verifies the forced-redraw cadence fires once the injected clock has
    /// advanced past `FORCED_REDRAW_INTERVAL`.
    #[test]
    fn forced_redraw_elapsed_fires_after_injected_clock_advances() {
        // Arrange
        let now = std::time::Instant::now();
        let last_draw_at = now
            .checked_sub(FORCED_REDRAW_INTERVAL)
            .expect("last_draw_at should fit before now");
        let frozen = FrozenInstantClock { instant: now };

        // Act
        let elapsed = forced_redraw_elapsed(&frozen, last_draw_at);

        // Assert
        assert!(
            elapsed,
            "forced-redraw cadence should trigger once the injected clock has advanced past \
             FORCED_REDRAW_INTERVAL"
        );
    }
}
