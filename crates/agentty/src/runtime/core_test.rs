//! Runtime event loop, event-reader task, and frame-rendering unit tests.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::convert::Infallible;
use std::io;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Rect, Size};
use ratatui::style::Modifier;
use ratatui::{Terminal, TerminalOptions, Viewport};
use testty::session::PtySessionBuilder;
use tokio::sync::mpsc;

use super::{
    DrawStamp, EventReaderTask, EventResult, FORCED_REDRAW_INTERVAL, MainLoopState, backend_err,
    forced_redraw_elapsed, render_frame, run, run_until_quit, run_with_backend,
};
use crate::analytics::{TurnOutcome, TurnSelection};
use crate::app::{App, AppEvent};
use crate::domain::agent::{AgentKind, AgentSelection, AgentSelectionMetadata, ReasoningLevel};
use crate::domain::session::{SessionHandles, Status};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::infra::browser::MockBrowserClient;
use crate::infra::clock::Clock;
use crate::infra::terminal_clipboard::MockTerminalClipboard;
use crate::presentation::app_mode::AppMode;
use crate::runtime::latency::{CycleTiming, SLOW_CYCLE_THRESHOLD};
use crate::runtime::synchronized_writer::{FrameHold, SynchronizedWriter};
use crate::runtime::{FRAME_INTERVAL, PresentationState};
use crate::test_support::telemetry::{TRACER_PROVIDER_LOCK, capture_events};
use crate::test_support::{FixedClock, SessionFixtureBuilder};

/// Starts a synchronized update.
const SYNCHRONIZED_UPDATE_BEGIN: &[u8] = b"\x1b[?2026h";
/// Ends a synchronized update.
const SYNCHRONIZED_UPDATE_END: &[u8] = b"\x1b[?2026l";

/// Environment marker used to distinguish the nested PTY test process.
const PRODUCTION_RUN_CHILD_ENV: &str = "AGENTTY_TEST_PRODUCTION_RUN_CHILD";
/// Fully-qualified libtest name invoked inside the nested PTY.
const PRODUCTION_RUN_CHILD_TEST: &str =
    "runtime::core::tests::production_run_exits_cleanly_in_pty_child";

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

/// Drives the concrete terminal entrypoint in a PTY so its setup, event
/// reader, and ordered cleanup path remain covered by source tests.
#[test]
fn production_run_exits_cleanly_in_pty() {
    // Arrange
    let test_binary = std::env::current_exe().expect("test binary path should be available");
    let mut session = PtySessionBuilder::new(test_binary)
        .args(["--exact", PRODUCTION_RUN_CHILD_TEST, "--nocapture"])
        .env(PRODUCTION_RUN_CHILD_ENV, "1")
        .spawn()
        .expect("failed to spawn production runtime test in a PTY");
    session
        .wait_for_text("Sessions", Duration::from_secs(10))
        .expect("production runtime should render its initial list view");

    // Act
    session
        .press_key("q")
        .expect("failed to open quit confirmation");
    session
        .wait_for_text("Confirm Quit", Duration::from_secs(5))
        .expect("quit confirmation should be rendered");
    session
        .press_key("y")
        .expect("failed to confirm runtime exit");
    let exited_successfully = session
        .wait_for_exit(Duration::from_secs(5))
        .expect("production runtime should exit before the timeout");

    // Assert
    assert!(exited_successfully);
}

/// Runs only when re-invoked by `production_run_exits_cleanly_in_pty`.
#[tokio::test]
async fn production_run_exits_cleanly_in_pty_child() {
    // Arrange
    if std::env::var_os(PRODUCTION_RUN_CHILD_ENV).is_none() {
        return;
    }
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    let result = tokio::time::timeout(Duration::from_secs(10), run(&mut app))
        .await
        .expect("production runtime should not hang");

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn event_reader_task_spawn_starts_a_shutdown_capable_reader() {
    // Arrange
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    // Act
    let result = EventReaderTask::spawn(event_tx).shutdown().await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn event_reader_task_shutdown_signals_and_joins_reader() {
    // Arrange
    let shutdown = Arc::new(AtomicBool::new(false));
    let reader_shutdown = Arc::clone(&shutdown);
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let join_handle = std::thread::spawn(move || {
        started_tx
            .send(())
            .expect("reader should signal that it started");
        while !reader_shutdown.load(Ordering::Relaxed) {
            std::thread::yield_now();
        }
    });
    started_rx
        .recv()
        .expect("reader should start before shutdown is requested");
    let task = EventReaderTask {
        join_handle,
        shutdown,
    };

    // Act
    let result = task.shutdown().await;

    // Assert
    assert!(result.is_ok());
}

#[tokio::test]
async fn event_reader_task_shutdown_reports_reader_panic() {
    // Arrange
    let shutdown = Arc::new(AtomicBool::new(false));
    let join_handle = std::thread::spawn(|| {
        std::panic::resume_unwind(Box::new("reader panic"));
    });
    let task = EventReaderTask {
        join_handle,
        shutdown,
    };

    // Act
    let result = task.shutdown().await;

    // Assert
    let error = result.expect_err("reader panic should be reported");
    assert_eq!(error.to_string(), "terminal event reader panicked");
}

#[tokio::test]
async fn event_reader_task_maps_blocking_join_failure() {
    // Arrange
    let join_error = tokio::spawn(async {
        std::panic::resume_unwind(Box::new("blocking join panic"));
    })
    .await
    .expect_err("panicked task should return a join error");

    // Act
    let result = EventReaderTask::map_join_result(Err(join_error));

    // Assert
    let error = result.expect_err("blocking join failure should be reported");
    assert!(
        error
            .to_string()
            .starts_with("failed to join event reader task:")
    );
}

/// Flattens a test terminal buffer into one searchable string.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

/// Test backend wrapper that counts terminal clears and draw flushes.
struct CountingBackend {
    clear_count: Arc<AtomicUsize>,
    draw_count: Arc<AtomicUsize>,
    inner: TestBackend,
}

impl CountingBackend {
    /// Creates a counting wrapper around one `TestBackend`.
    fn new(width: u16, height: u16) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let clear_count = Arc::new(AtomicUsize::new(0));
        let draw_count = Arc::new(AtomicUsize::new(0));

        (
            Self {
                clear_count: Arc::clone(&clear_count),
                draw_count: Arc::clone(&draw_count),
                inner: TestBackend::new(width, height),
            },
            clear_count,
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

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.clear_count.fetch_add(1, Ordering::Relaxed);
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

/// Terminal output shared between a test and the writer that fills it.
#[derive(Clone, Default)]
struct SharedOutput(Rc<RefCell<Vec<u8>>>);

impl SharedOutput {
    /// Returns and forgets everything written so far.
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

impl io::Write for SharedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts the occurrences of `needle` in `output`.
fn occurrences(output: &[u8], needle: &[u8]) -> usize {
    output
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Verifies a page change reaches the terminal as one synchronized update:
/// the clear and the repaint after it are presented together, although
/// clearing flushes the backend.
#[tokio::test]
async fn render_presents_a_page_clear_and_its_repaint_as_one_update() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let output = SharedOutput::default();
    let frame_hold = FrameHold::default();
    let backend =
        CrosstermBackend::new(SynchronizedWriter::new(output.clone(), frame_hold.clone()));
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 80, 24)),
        },
    )
    .expect("failed to create terminal");
    let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::unix_epoch());
    let last_draw = DrawStamp::now(clock.as_ref());
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    let mut main_loop_state = MainLoopState {
        app: &mut app,
        clock,
        event_rx: &mut event_rx,
        frame_hold,
        last_draw,
        presentation: Rc::new(PresentationState::default()),
        terminal: &mut terminal,
        tick: &mut tick,
        timing: CycleTiming::default(),
    };
    main_loop_state.render().expect("list frame should render");
    main_loop_state.app.mode = AppMode::View {
        session_id: "missing-session".into(),
        scroll_offset: None,
    };
    main_loop_state.app.mark_dirty();
    output.take();

    // Act
    main_loop_state
        .render()
        .expect("session frame should render");

    // Assert
    let written = output.take();
    let first_update_end = written
        .windows(SYNCHRONIZED_UPDATE_END.len())
        .position(|window| window == SYNCHRONIZED_UPDATE_END)
        .expect("the frame update ends");
    let frame_update = &written[..first_update_end];
    assert!(
        frame_update.starts_with(SYNCHRONIZED_UPDATE_BEGIN),
        "{written:?}"
    );
    assert_eq!(
        occurrences(frame_update, SYNCHRONIZED_UPDATE_BEGIN),
        1,
        "{written:?}"
    );
    assert_eq!(occurrences(frame_update, b"\x1b[2J"), 1, "{written:?}");
    assert!(
        occurrences(frame_update, b"Agentty") > 0,
        "the repaint follows the clear inside the update: {written:?}"
    );
    assert_eq!(
        occurrences(&written, SYNCHRONIZED_UPDATE_END),
        1,
        "{written:?}"
    );
}

/// Verifies base-page transitions clear stale terminal cells while
/// redraws within one page keep Ratatui's normal diff rendering.
#[tokio::test]
async fn render_frame_clears_terminal_only_when_base_page_changes() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let (backend, clear_count, _draw_count) = CountingBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let clock = crate::infra::clock::RealClock;
    let mut last_draw = DrawStamp::now(&clock);
    let presentation = PresentationState::default();

    // Act
    render_frame(
        &mut app,
        &mut terminal,
        &clock,
        &mut last_draw,
        &presentation,
    )
    .expect("initial list frame should render");
    app.mode = AppMode::View {
        session_id: "missing-session".into(),
        scroll_offset: None,
    };
    app.mark_dirty();
    let repeated_clear_decisions = {
        let snapshot = app.view_snapshot();

        [
            presentation.terminal_clear_needed(&snapshot),
            presentation.terminal_clear_needed(&snapshot),
        ]
    };
    render_frame(
        &mut app,
        &mut terminal,
        &clock,
        &mut last_draw,
        &presentation,
    )
    .expect("session frame should render");
    app.mark_dirty();
    render_frame(
        &mut app,
        &mut terminal,
        &clock,
        &mut last_draw,
        &presentation,
    )
    .expect("same session page should redraw");

    // Assert
    assert_eq!(repeated_clear_decisions, [true, true]);
    assert_eq!(clear_count.load(Ordering::Relaxed), 1);
}

/// Wall clock whose Unix second count tests set directly.
struct SettableWallClock {
    unix_seconds: AtomicI64,
}

impl Clock for SettableWallClock {
    fn now_instant(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    fn now_system_time(&self) -> std::time::SystemTime {
        let unix_seconds = u64::try_from(self.unix_seconds.load(Ordering::Relaxed))
            .expect("test wall clock should stay after the Unix epoch");

        std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(unix_seconds)
    }
}

/// Verifies idle frames without tick-driven UI still redraw once the wall
/// clock enters a new minute, so minute-granular labels such as session ages
/// do not go stale.
#[tokio::test]
async fn render_frame_redraws_idle_ui_when_wall_clock_minute_changes() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let (backend, _clear_count, draw_count) = CountingBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let clock = SettableWallClock {
        unix_seconds: AtomicI64::new(1_782_864_000),
    };
    let mut last_draw = DrawStamp::now(&clock);
    let presentation = PresentationState::default();
    app.mark_dirty();
    render_frame(
        &mut app,
        &mut terminal,
        &clock,
        &mut last_draw,
        &presentation,
    )
    .expect("initial frame should render");
    let initial_draw_count = draw_count.load(Ordering::Relaxed);

    // Act
    clock.unix_seconds.store(1_782_864_059, Ordering::Relaxed);
    render_frame(
        &mut app,
        &mut terminal,
        &clock,
        &mut last_draw,
        &presentation,
    )
    .expect("same-minute idle frame should be skipped");
    let same_minute_draw_count = draw_count.load(Ordering::Relaxed);
    clock.unix_seconds.store(1_782_864_060, Ordering::Relaxed);
    render_frame(
        &mut app,
        &mut terminal,
        &clock,
        &mut last_draw,
        &presentation,
    )
    .expect("next-minute idle frame should render");
    let next_minute_draw_count = draw_count.load(Ordering::Relaxed);

    // Assert
    assert!(!app.has_visible_tick_driven_ui());
    assert_eq!(same_minute_draw_count, initial_draw_count);
    assert_eq!(next_minute_draw_count, initial_draw_count + 1);
}

/// Wall time, in milliseconds after the Unix epoch, inside the nerpa
/// greeting's tail wag.
const GREETING_WAG_UNIX_MILLIS: u64 = 5_000;

/// Wall time, in milliseconds after the Unix epoch, just before the nerpa
/// greeting's tail wag ends and the next cycle's rest begins.
const GREETING_WAG_END_UNIX_MILLIS: u64 = 5_990;

/// Verifies a visible nerpa greeting redraws on every frame tick while its
/// tail wags, while a spinner-only session keeps the slower forced redraw
/// cadence.
#[tokio::test]
async fn render_frame_redraws_frame_animation_on_every_frame_tick() {
    // Arrange
    let (mut app, _base_dir) = new_draft_greeting_app().await;
    let (backend, _clear_count, draw_count) = CountingBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let clock_at_frame = greeting_clock_at_frame(GREETING_WAG_UNIX_MILLIS);
    let mut last_draw = DrawStamp::now(&clock_at_frame(0));
    let presentation = PresentationState::default();
    app.mark_dirty();
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(0),
        &mut last_draw,
        &presentation,
    )
    .expect("initial frame should render");
    let initial_draw_count = draw_count.load(Ordering::Relaxed);

    // Act
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(1),
        &mut last_draw,
        &presentation,
    )
    .expect("greeting frame should render");
    let greeting_draw_count = draw_count.load(Ordering::Relaxed);
    let session = app
        .sessions
        .sessions_mut()
        .first_mut()
        .expect("session should remain available");
    session.prompt = "Ship it".to_string();
    session.status = Status::InProgress;
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(2),
        &mut last_draw,
        &presentation,
    )
    .expect("spinner frame should be skipped");
    let spinner_draw_count = draw_count.load(Ordering::Relaxed);

    // Assert
    assert!(!app.has_visible_frame_animation());
    assert!(app.has_visible_tick_driven_ui());
    assert_eq!(greeting_draw_count, initial_draw_count + 1);
    assert_eq!(spinner_draw_count, greeting_draw_count);
}

/// Verifies a visible nerpa greeting draws once more after its tail wag
/// ends, then keeps the slower forced redraw cadence while the art rests.
#[tokio::test]
async fn render_frame_skips_frame_ticks_while_greeting_rests() {
    // Arrange
    let (mut app, _base_dir) = new_draft_greeting_app().await;
    let (backend, _clear_count, draw_count) = CountingBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let clock_at_frame = greeting_clock_at_frame(GREETING_WAG_END_UNIX_MILLIS);
    let mut last_draw = DrawStamp::now(&clock_at_frame(0));
    let presentation = PresentationState::default();
    app.mark_dirty();
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(0),
        &mut last_draw,
        &presentation,
    )
    .expect("last wag frame should render");
    let wag_draw_count = draw_count.load(Ordering::Relaxed);

    // Act
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(1),
        &mut last_draw,
        &presentation,
    )
    .expect("first rest frame should render");
    let first_rest_draw_count = draw_count.load(Ordering::Relaxed);
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(2),
        &mut last_draw,
        &presentation,
    )
    .expect("second rest frame should be skipped");
    let second_rest_draw_count = draw_count.load(Ordering::Relaxed);

    // Assert
    assert!(app.has_visible_frame_animation());
    assert_eq!(first_rest_draw_count, wag_draw_count + 1);
    assert_eq!(second_rest_draw_count, first_rest_draw_count);
}

/// Verifies a draft session whose terminal is too small for the nerpa
/// greeting keeps the slower forced redraw cadence during a tail wag,
/// because no painted art can animate.
#[tokio::test]
async fn render_frame_skips_frame_ticks_when_greeting_is_hidden() {
    // Arrange
    let (mut app, _base_dir) = new_draft_greeting_app().await;
    let (backend, _clear_count, draw_count) = CountingBackend::new(80, 12);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let clock_at_frame = greeting_clock_at_frame(GREETING_WAG_UNIX_MILLIS);
    let mut last_draw = DrawStamp::now(&clock_at_frame(0));
    let presentation = PresentationState::default();
    app.mark_dirty();
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(0),
        &mut last_draw,
        &presentation,
    )
    .expect("initial frame should render");
    let initial_draw_count = draw_count.load(Ordering::Relaxed);

    // Act
    render_frame(
        &mut app,
        &mut terminal,
        &clock_at_frame(1),
        &mut last_draw,
        &presentation,
    )
    .expect("hidden greeting frame should be skipped");
    let hidden_draw_count = draw_count.load(Ordering::Relaxed);

    // Assert
    assert!(app.has_visible_frame_animation());
    assert!(!presentation.greeting_animated());
    assert_eq!(hidden_draw_count, initial_draw_count);
}

/// Verifies a frame highlights a mouse selection, and the frame after the
/// release opens a clicked link or copies a drag without waiting for the
/// clipboard.
#[tokio::test]
async fn render_frame_resolves_mouse_selection_gestures() {
    // Arrange
    let link = "https://example.com/docs";
    let opened = Arc::new(Mutex::new(Vec::new()));
    let (frames_done_tx, frames_done_rx) = std::sync::mpsc::channel::<()>();
    let (copied_tx, mut copied_rx) = mpsc::unbounded_channel();
    let mut clipboard = MockTerminalClipboard::new();
    // The clipboard stays busy until every frame is rendered, so a frame
    // that waited for it would time out here and report `false`.
    clipboard
        .expect_copy_through_tmux()
        .times(1)
        .returning(move |text| {
            let is_after_frames = frames_done_rx.recv_timeout(Duration::from_secs(5)).is_ok();
            let _ = copied_tx.send((text.to_string(), is_after_frames));

            true
        });
    let mut browser = MockBrowserClient::new();
    browser.expect_open().returning({
        let opened = Arc::clone(&opened);

        move |url| opened.lock().expect("opened lock").push(url.to_string())
    });
    let clients = crate::test_support::test_app_clients()
        .with_browser_client(Arc::new(browser))
        .with_terminal_clipboard(Arc::new(clipboard));
    let (mut app, _base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("session-id")
            .status(Status::Review)
            .transcript(format!("see {link} now"))
            .build(),
    );
    app.mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };
    let mut terminal =
        Terminal::new(TestBackend::new(80, 24)).expect("failed to create test terminal");
    let clock = crate::infra::clock::RealClock;
    let mut last_draw = DrawStamp::now(&clock);
    let presentation = PresentationState::default();
    let mut render = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
        app.mark_dirty();
        render_frame(app, terminal, &clock, &mut last_draw, &presentation)
            .expect("frame should render");
    };
    render(&mut app, &mut terminal);
    let (column, row) = position_of(terminal.backend().buffer(), link);
    let report = |kind, column| MouseEvent {
        column,
        kind,
        modifiers: KeyModifiers::NONE,
        row,
    };
    let selection = presentation.screen_selection();

    // Act
    selection.handle_mouse(report(MouseEventKind::Down(MouseButton::Left), column));
    selection.handle_mouse(report(MouseEventKind::Drag(MouseButton::Left), column + 4));
    render(&mut app, &mut terminal);
    let is_highlighted = (column..=column + 4).all(|highlighted_column| {
        terminal.backend().buffer()[(highlighted_column, row)]
            .modifier
            .contains(Modifier::REVERSED)
    });
    selection.handle_mouse(report(MouseEventKind::Up(MouseButton::Left), column + 4));
    render(&mut app, &mut terminal);
    selection.handle_mouse(report(MouseEventKind::Down(MouseButton::Left), column));
    selection.handle_mouse(report(MouseEventKind::Up(MouseButton::Left), column));
    render(&mut app, &mut terminal);
    render(&mut app, &mut terminal);
    frames_done_tx
        .send(())
        .expect("the clipboard mock should still be alive");
    let copied = copied_rx.recv().await;

    // Assert
    assert!(is_highlighted);
    assert_eq!(copied, Some(("https".to_string(), true)));
    assert_eq!(*opened.lock().expect("opened lock"), vec![link]);
}

/// Returns the cell where `needle` starts on the rendered screen.
fn position_of(buffer: &ratatui::buffer::Buffer, needle: &str) -> (u16, u16) {
    let area = buffer.area;

    (area.top()..area.bottom())
        .find_map(|row| {
            let row_text: String = (area.left()..area.right())
                .map(|column| buffer[(column, row)].symbol())
                .collect();
            let column = row_text.find(needle)?;
            let column = u16::try_from(row_text[..column].chars().count()).ok()?;

            Some((column, row))
        })
        .expect("needle should be rendered")
}

/// Builds a test app viewing one draft session that shows the nerpa
/// greeting.
async fn new_draft_greeting_app() -> (App, tempfile::TempDir) {
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    app.sessions.push_session(
        SessionFixtureBuilder::new()
            .id("session-id")
            .status(Status::Draft)
            .build(),
    );
    app.mode = AppMode::View {
        session_id: "session-id".into(),
        scroll_offset: None,
    };

    (app, base_dir)
}

/// Returns a clock factory whose monotonic and wall times both advance by
/// one [`FRAME_INTERVAL`] per frame, starting at `start_unix_millis`.
fn greeting_clock_at_frame(start_unix_millis: u64) -> impl Fn(u32) -> FixedClock {
    let started_at = std::time::Instant::now();
    let start_system_time = SystemTime::UNIX_EPOCH + Duration::from_millis(start_unix_millis);

    move |frame: u32| {
        FixedClock::new(
            started_at + FRAME_INTERVAL * frame,
            start_system_time + FRAME_INTERVAL * frame,
        )
    }
}

/// Verifies that `run_with_backend` drives the main loop with a
/// `TestBackend` and exits cleanly when quit key events are injected.
#[tokio::test]
async fn run_with_backend_exits_on_quit_key() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
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

/// Verifies a dropped injected event sender exits the full runtime loop
/// instead of repeatedly producing empty input cycles.
#[tokio::test]
async fn run_with_backend_returns_error_when_event_channel_closes() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    drop(event_tx);

    // Act
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_with_backend(&mut app, &mut terminal, &mut event_rx),
    )
    .await
    .expect("runtime should exit when its event channel closes");

    // Assert
    let error = result.expect_err("closed event channel should fail the runtime");
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(error.to_string(), "terminal event reader stopped");
}

#[tokio::test]
async fn run_with_backend_waits_for_cleanup_tasks_after_quit() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
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
async fn run_with_backend_flushes_turn_telemetry_emitted_during_quit_cleanup() {
    for (outcome, expected_event) in [
        (TurnOutcome::Finished, "agentty_turn_finish"),
        (TurnOutcome::Interrupted, "agentty_turn_interrupt"),
    ] {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let (analytics, receiver) = capture_events(2);
        app.set_analytics(Some(analytics));
        let agent = AgentSelection::new(AgentKind::Codex, AgentKind::Codex.default_model());
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let services = app.services.clone();
        app.services.track_cleanup_task(tokio::spawn(async move {
            release_rx.await.expect("release turn settlement");
            services.emit_app_event(AppEvent::RefreshSessions);
            let turn = TurnSelection { agent };
            services.emit_app_event(AppEvent::SessionTurnStarted {
                session_id: "settled-turn".into(),
                turn,
            });
            services.emit_app_event(AppEvent::SessionTurnEnded { outcome, turn });
        }));
        for key in ['q', 'y'] {
            event_tx
                .send(Event::Key(KeyEvent::new(
                    KeyCode::Char(key),
                    KeyModifiers::NONE,
                )))
                .expect("quit key");
        }
        let run_future = run_with_backend(&mut app, &mut terminal, &mut event_rx);
        tokio::pin!(run_future);

        // Act / Assert
        tokio::select! {
            result = &mut run_future => {
                unreachable!("runtime exited before turn settlement: {result:?}");
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        release_tx.send(()).expect("settle turn during shutdown");
        let result = run_future.await;
        let events = receiver.join().expect("telemetry receiver");

        // Assert
        assert!(result.is_ok());
        assert_eq!(events.len(), 2);
        for name in ["agentty_turn_start", expected_event] {
            let event = events
                .iter()
                .find(|event| event["event"] == name)
                .expect("turn event sent before shutdown returns");
            assert_eq!(event["properties"]["agent"], agent.kind().name());
            assert_eq!(event["properties"]["model"], agent.model().as_str());
        }
    }
}

#[tokio::test]
/// Verifies each cycle tells remote clients about a change to what the
/// session API reports, here a setting whose event publishes nothing, and
/// stays quiet while nothing changes.
async fn run_cycle_advances_the_remote_revision_once_for_a_reported_change() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.sessions
        .push_session(crate::test_support::session_fixture(
            "session-1",
            Status::Review,
        ));
    let mut revisions = crate::app::test_support::start_fake_listener(&mut app).await;
    let initial_revision = *revisions.borrow_and_update();
    app.services
        .emit_app_event(AppEvent::SessionReasoningLevelUpdated {
            reasoning_level: ReasoningLevel::Low,
            session_id: "session-1".into(),
        });
    let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("failed to create terminal");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::unix_epoch());
    let last_draw = DrawStamp::now(clock.as_ref());
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    let mut main_loop_state = MainLoopState {
        app: &mut app,
        clock,
        event_rx: &mut event_rx,
        frame_hold: FrameHold::default(),
        last_draw,
        presentation: Rc::new(PresentationState::default()),
        terminal: &mut terminal,
        tick: &mut tick,
        timing: CycleTiming::default(),
    };

    // Act
    let first_cycle = main_loop_state.run_cycle().await;
    let after_change = *revisions.borrow_and_update();
    let second_cycle = main_loop_state.run_cycle().await;
    let is_changed_again = revisions
        .has_changed()
        .expect("revision sender should live");

    // Assert
    assert!(matches!(first_cycle, Ok(EventResult::Continue)));
    assert!(matches!(second_cycle, Ok(EventResult::Continue)));
    assert_eq!(after_change, initial_revision + 1);
    assert!(!is_changed_again);
}

#[tokio::test]
/// Verifies the cycle after a key tells remote clients what the key
/// changed, here a project sync the key starts, although no app event
/// follows the key.
async fn run_cycle_advances_the_remote_revision_once_for_a_change_made_by_a_key() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_git_test_app().await;
    let mut sync_main_runner = crate::app::test_support::MockSyncMainRunner::new();
    sync_main_runner
        .expect_start_sync_main()
        .times(1)
        .returning(|_, _, _, _| {});
    app.sync_main_runner = Arc::new(sync_main_runner);
    // The sync orchestrator reports Git status once at startup and then
    // waits for its next periodic pass; apply that report now, one event at a
    // time so a batch cannot reduce it before this loop sees it.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = app.next_app_event().await.expect("startup event");
            let is_git_status = matches!(event, AppEvent::GitStatusUpdated { .. });
            app.apply_single_app_event(event).await;
            if is_git_status {
                break;
            }
        }
    })
    .await
    .expect("startup Git status should arrive");
    let mut revisions = crate::app::test_support::start_fake_listener(&mut app).await;
    let initial_revision = *revisions.borrow_and_update();
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("failed to create terminal");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::unix_epoch());
    let last_draw = DrawStamp::now(clock.as_ref());
    // The first tick fires at once; consuming it leaves only terminal input
    // to wake the cycles below.
    let mut tick = tokio::time::interval(Duration::from_mins(1));
    tick.tick().await;
    let mut main_loop_state = MainLoopState {
        app: &mut app,
        clock,
        event_rx: &mut event_rx,
        frame_hold: FrameHold::default(),
        last_draw,
        presentation: Rc::new(PresentationState::default()),
        terminal: &mut terminal,
        tick: &mut tick,
        timing: CycleTiming::default(),
    };
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));

    // Act: a late startup event may take a cycle before the key, which
    // stays queued until a cycle handles it.
    event_tx
        .send(key(KeyCode::Char('s')))
        .expect("failed to send sync key");
    // A cycle starts only while the key is queued, so none waits on the
    // tick, and the last one is the cycle that handled the key.
    while !main_loop_state.event_rx.is_empty() {
        assert!(matches!(
            main_loop_state.run_cycle().await,
            Ok(EventResult::Continue)
        ));
    }
    let is_sync_started = main_loop_state.app.active_project_sync_status().is_some();
    let after_key_cycle = *revisions.borrow_and_update();
    event_tx
        .send(Event::FocusGained)
        .expect("failed to send focus event");
    let next_cycle = main_loop_state.run_cycle().await;
    let after_next_cycle = *revisions.borrow_and_update();
    main_loop_state.app.publish_reported_state_changes();
    let is_changed_again = revisions
        .has_changed()
        .expect("revision sender should live");

    // Assert
    assert!(is_sync_started);
    assert!(matches!(next_cycle, Ok(EventResult::Continue)));
    // The key runs after its cycle's check, so the next cycle reports it.
    assert_eq!(after_key_cycle, initial_revision);
    assert_eq!(after_next_cycle, initial_revision + 1);
    assert!(!is_changed_again);
}

#[tokio::test]
/// Verifies idle runtime ticks do not redraw when no visible spinner or
/// timer is active and the wall-clock minute is unchanged.
async fn run_cycle_skips_idle_redraws_without_tick_driven_ui() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let (backend, _clear_count, draw_count) = CountingBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::unix_epoch());
    let last_draw = DrawStamp::now(clock.as_ref());
    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    let mut main_loop_state = MainLoopState {
        app: &mut app,
        clock,
        event_rx: &mut event_rx,
        frame_hold: FrameHold::default(),
        last_draw,
        presentation: Rc::new(PresentationState::default()),
        terminal: &mut terminal,
        tick: &mut tick,
        timing: CycleTiming::default(),
    };

    // Act
    for _ in 0..60 {
        // Exercise elapsed redraw deadlines without crossing a wall-clock
        // minute.
        main_loop_state.clock = Arc::new(FixedClock::new(
            main_loop_state.clock.now_instant() + FRAME_INTERVAL,
            SystemTime::UNIX_EPOCH,
        ));
        main_loop_state.tick.reset_immediately();
        assert!(matches!(
            main_loop_state.run_cycle().await,
            Ok(EventResult::Continue)
        ));
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
    let result = main_loop_state.run_cycle().await;

    // Assert
    assert!(matches!(result, Ok(EventResult::Quit)));
    assert!(!main_loop_state.app.has_visible_tick_driven_ui());
    assert!(
        (1..=2).contains(&idle_draw_count),
        "expected only initial idle draws across 60 ticks, observed {idle_draw_count}"
    );
}

#[tokio::test]
/// Verifies one queued `SessionUpdated` event syncs the touched session
/// before the next render without scanning all session handles.
async fn run_cycle_renders_pending_session_update_before_waiting_for_events() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let session_id = "session-1".to_string();
    let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
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
        SessionHandles::new(Status::InProgress),
    );

    app.mode = AppMode::View {
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
        let transcript = SessionTranscript::new(vec![SessionMessage::conversation(
            0,
            SessionMessageKind::AssistantAnswer,
            "synced output",
        )]);
        if let Ok(mut handle_transcript) = handles.transcript.lock() {
            *handle_transcript = transcript;
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
    let last_draw = DrawStamp::now(clock.as_ref());
    let mut main_loop_state = MainLoopState {
        app: &mut app,
        clock,
        event_rx: &mut event_rx,
        frame_hold: FrameHold::default(),
        last_draw,
        presentation: Rc::new(PresentationState::default()),
        terminal: &mut terminal,
        tick: &mut tick,
        timing: CycleTiming::default(),
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

/// Clock whose monotonic time advances by a fixed step on every reading, so
/// each measured foreground phase takes a known, nonzero duration.
struct SteppingClock {
    origin: std::time::Instant,
    readings: AtomicU32,
    step: Duration,
}

impl Clock for SteppingClock {
    fn now_instant(&self) -> std::time::Instant {
        self.origin + self.step * self.readings.fetch_add(1, Ordering::Relaxed)
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_000)
    }
}

#[tokio::test]
/// Verifies a cycle exports one `ui.slow_cycle` root only when its measured
/// work reaches the threshold on the injected clock.
async fn run_cycle_traces_only_cycles_at_or_above_the_slow_threshold() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let (_event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
    let mut tick = tokio::time::interval(Duration::from_millis(1));
    let fast_clock: Arc<dyn Clock> = Arc::new(FixedClock::unix_epoch());
    let mut main_loop_state = MainLoopState {
        app: &mut app,
        last_draw: DrawStamp::now(fast_clock.as_ref()),
        clock: fast_clock,
        event_rx: &mut event_rx,
        frame_hold: FrameHold::default(),
        presentation: Rc::new(PresentationState::default()),
        terminal: &mut terminal,
        tick: &mut tick,
        timing: CycleTiming::default(),
    };

    // Act
    let fast_result = main_loop_state.run_cycle().await;
    let fast_spans = exporter.get_finished_spans().expect("finished spans");
    main_loop_state.clock = Arc::new(SteppingClock {
        origin: std::time::Instant::now(),
        readings: AtomicU32::new(0),
        step: SLOW_CYCLE_THRESHOLD,
    });
    let slow_result = main_loop_state.run_cycle().await;

    // Assert
    assert!(matches!(fast_result, Ok(EventResult::Continue)));
    assert!(matches!(slow_result, Ok(EventResult::Continue)));
    assert!(
        !fast_spans.iter().any(|span| span.name == "ui.slow_cycle"),
        "an unmeasurably fast cycle must not be traced"
    );
    let slow_spans = exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .filter(|span| span.name == "ui.slow_cycle")
        .collect::<Vec<_>>();
    assert_eq!(slow_spans.len(), 1);
    assert!(slow_spans[0].attributes.iter().any(|attribute| {
        attribute.key.as_str() == "agentty.ui.reduce_ms"
            && matches!(attribute.value, opentelemetry::Value::I64(elapsed) if elapsed >= 50)
    }));
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
    let elapsed = forced_redraw_elapsed(&frozen, anchor, FORCED_REDRAW_INTERVAL);

    // Assert
    assert!(
        !elapsed,
        "forced-redraw cadence must read elapsed time through the injected clock, not the host \
         wall clock"
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
    let elapsed = forced_redraw_elapsed(&frozen, last_draw_at, FORCED_REDRAW_INTERVAL);

    // Assert
    assert!(
        elapsed,
        "forced-redraw cadence should trigger once the injected clock has advanced past \
         FORCED_REDRAW_INTERVAL"
    );
}

/// Verifies backend failures surface as `io::Error` values that keep the
/// backend message.
#[test]
fn backend_err_wraps_backend_failures_as_io_errors() {
    // Arrange
    let backend_error = io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed");

    // Act
    let error = backend_err(backend_error);

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.to_string(), "terminal closed");
}
