//! Terminal event reading and dispatch of terminal, app, and tick events into
//! runtime processing cycles.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use tracing::debug;

use crate::app::{App, AppRuntimeEvent};
use crate::domain::input::InputCommand;
use crate::infra::clock::Clock;
use crate::presentation::app_mode::AppMode;
use crate::runtime::latency::CycleTiming;
use crate::runtime::mode::chat_scroll::ChatScrollBatch;
use crate::runtime::screen_selection::ScreenSelection;
use crate::runtime::{EventResult, FRAME_INTERVAL, PresentationState, key_handler, mode};
use crate::ui::RenderCacheStore;

/// Maximum terminal input events processed in one foreground cycle.
///
/// Additional queued events remain buffered for the next runtime cycle so the
/// render loop can redraw between large paste/key-repeat bursts.
const TERMINAL_EVENT_DRAIN_BUDGET: usize = 64;
/// Yield to painting after eight milliseconds even when keys remain queued.
const TERMINAL_EVENT_DRAIN_DURATION: Duration = Duration::from_millis(8);

/// Reads terminal events from an underlying event backend.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait EventSource: Send + Sync + 'static {
    /// Polls for an available event.
    fn poll(&self, timeout: Duration) -> io::Result<bool>;

    /// Reads the next available event.
    fn read(&self) -> io::Result<Event>;
}

struct CrosstermEventSource;

impl EventSource for CrosstermEventSource {
    fn poll(&self, timeout: Duration) -> io::Result<bool> {
        crossterm::event::poll(timeout)
    }

    fn read(&self) -> io::Result<Event> {
        crossterm::event::read()
    }
}

/// Represents the next runtime wake-up source while awaiting input or redraw.
enum LoopSignal {
    /// One terminal input event from the foreground reader thread.
    Event(Option<io::Result<Event>>),
    /// One foreground-owned app event or session actor command.
    Runtime(AppRuntimeEvent),
    /// One redraw tick with no immediate input payload.
    Tick,
}

/// Converts injected terminal messages into the fallible production event
/// transport used by the runtime loop.
pub(crate) trait TerminalEventMessage {
    fn into_event_result(self) -> io::Result<Event>;
}

impl TerminalEventMessage for Event {
    fn into_event_result(self) -> io::Result<Event> {
        Ok(self)
    }
}

impl TerminalEventMessage for io::Result<Event> {
    fn into_event_result(self) -> io::Result<Event> {
        self
    }
}

/// Returns the terminal-reader failure used when any event sender disappears.
fn terminal_event_channel_closed_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "terminal event reader stopped",
    )
}

/// Spawns the terminal event reader thread with production dependencies.
pub(crate) fn spawn_event_reader(
    event_tx: mpsc::UnboundedSender<io::Result<Event>>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let event_source: Arc<dyn EventSource> = Arc::new(CrosstermEventSource);

    spawn_event_reader_with_source(event_source, event_tx, shutdown)
}

/// Spawns the terminal event reader with injected dependencies.
fn spawn_event_reader_with_source(
    event_source: Arc<dyn EventSource>,
    event_tx: mpsc::UnboundedSender<io::Result<Event>>,
    shutdown: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while !shutdown.load(Ordering::Relaxed)
            && forward_next_terminal_event(event_source.as_ref(), &event_tx)
        {}
    })
}

/// Polls for and forwards one terminal event, returning whether reading should
/// continue.
fn forward_next_terminal_event(
    event_source: &dyn EventSource,
    event_tx: &mpsc::UnboundedSender<io::Result<Event>>,
) -> bool {
    let event = match event_source.poll(FRAME_INTERVAL) {
        Ok(true) => event_source.read(),
        Ok(false) => return true,
        Err(error) if error.kind() == io::ErrorKind::Interrupted => return true,
        Err(error) => Err(error),
    };

    match event {
        Ok(event) => match translate_mouse_event(event) {
            Some(event) => event_tx.send(Ok(event)).is_ok(),
            None => true,
        },
        Err(error) if error.kind() == io::ErrorKind::Interrupted => true,
        Err(error) => {
            let _ = event_tx.send(Err(error));

            false
        }
    }
}

/// Converts mouse-wheel reports into arrow keys, keeps left-button reports
/// for screen selection, and drops other mouse reports.
///
/// Direct terminals already turn the wheel into `Up`/`Down` on the alternate
/// screen. Mouse reporting is requested only inside `tmux`, which otherwise
/// swallows the wheel or leaves it to the outer terminal, so this mapping
/// keeps scrolling identical across both transports.
fn translate_mouse_event(event: Event) -> Option<Event> {
    let Event::Mouse(mouse) = event else {
        return Some(event);
    };
    let key_code = match mouse.kind {
        MouseEventKind::ScrollUp => KeyCode::Up,
        MouseEventKind::ScrollDown => KeyCode::Down,
        MouseEventKind::Down(MouseButton::Left)
        | MouseEventKind::Drag(MouseButton::Left)
        | MouseEventKind::Up(MouseButton::Left) => return Some(event),
        _ => return None,
    };

    Some(Event::Key(KeyEvent::new(key_code, KeyModifiers::NONE)))
}

/// Waits for the next terminal/app event or tick and dispatches one runtime
/// processing cycle.
pub(crate) async fn process_events<B: Backend, Message: TerminalEventMessage>(
    app: &mut App,
    presentation: Rc<PresentationState>,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<Message>,
    tick: &mut tokio::time::Interval,
    timing: &mut CycleTiming,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    process_events_with_scroll_handler(
        app,
        presentation,
        terminal,
        event_rx,
        tick,
        timing,
        ChatScrollBatch::handle,
    )
    .await
}

/// Runs the production dispatcher with an injectable scroll handler so
/// measurement failures can be tested without a failing physical terminal.
async fn process_events_with_scroll_handler<B: Backend, Message: TerminalEventMessage>(
    app: &mut App,
    presentation: Rc<PresentationState>,
    terminal: &mut Terminal<B>,
    event_rx: &mut mpsc::UnboundedReceiver<Message>,
    tick: &mut tokio::time::Interval,
    timing: &mut CycleTiming,
    mut handle_scroll: impl FnMut(
        &mut ChatScrollBatch,
        &mut App,
        &RenderCacheStore,
        &Terminal<B>,
        KeyEvent,
    ) -> io::Result<bool>,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let clock = app.services.clock();
    let mut scroll_batch = ChatScrollBatch::default();
    process_events_with_handler(
        clock.as_ref(),
        app,
        terminal,
        event_rx,
        tick,
        timing,
        |app, terminal, event| {
            mode::diff::apply_refreshed_diff(app, presentation.render_cache_store());
            update_screen_selection(app, presentation.screen_selection(), event.as_ref());
            if let Some(Event::Key(key)) = event.as_ref()
                && is_press_key_event(*key)
            {
                match handle_scroll(
                    &mut scroll_batch,
                    app,
                    presentation.render_cache_store(),
                    terminal,
                    *key,
                ) {
                    Ok(true) => return Box::pin(std::future::ready(Ok(EventResult::Continue))),
                    Err(error) => return Box::pin(std::future::ready(Err(error))),
                    Ok(false) => {}
                }
            }
            scroll_batch = ChatScrollBatch::default();
            let presentation = Rc::clone(&presentation);

            Box::pin(process_event(app, presentation, terminal, event))
        },
    )
    .await
}

/// Processes one event/tick cycle with an injected event handler so loop exit
/// branches can be tested without a real terminal.
///
/// Work after the wait is added to `timing`, with tick-driven session
/// refreshes kept apart from event handling, when the cycle continues.
async fn process_events_with_handler<Terminal, Message, EventHandler>(
    clock: &dyn Clock,
    app: &mut App,
    terminal: &mut Terminal,
    event_rx: &mut mpsc::UnboundedReceiver<Message>,
    tick: &mut tokio::time::Interval,
    timing: &mut CycleTiming,
    mut handle_event: EventHandler,
) -> io::Result<EventResult>
where
    Message: TerminalEventMessage,
    EventHandler: for<'handler> FnMut(
        &'handler mut App,
        &'handler mut Terminal,
        Option<Event>,
    ) -> Pin<
        Box<dyn Future<Output = io::Result<EventResult>> + 'handler>,
    >,
{
    // Wait for either a terminal event or the next tick (for redraws).
    // This yields to tokio so spawned tasks (agent output, git status) can
    // make progress on this worker thread.
    let signal = tokio::select! {
        event = event_rx.recv() => {
            LoopSignal::Event(event.map(TerminalEventMessage::into_event_result))
        },
        runtime_event = app.next_runtime_event() => LoopSignal::Runtime(runtime_event),
        _ = tick.tick() => LoopSignal::Tick,
    };
    let batch_started_at = clock.now_instant();
    let mut handled_terminal_events = 0;
    let mut reload_elapsed = Duration::ZERO;
    let maybe_event = match signal {
        LoopSignal::Runtime(runtime_event) => {
            match runtime_event {
                AppRuntimeEvent::App(event) => {
                    app.apply_app_events(*event).await;
                }
                AppRuntimeEvent::Session(command) => {
                    app.apply_session_runtime_command(*command).await;
                }
            }

            None
        }
        LoopSignal::Event(Some(Ok(event))) => {
            handled_terminal_events += 1;

            Some(event)
        }
        LoopSignal::Event(Some(Err(error))) => return Err(error),
        LoopSignal::Event(None) => return Err(terminal_event_channel_closed_error()),
        LoopSignal::Tick => {
            let reload_started_at = clock.now_instant();
            if app.refresh_sessions_if_needed().await {
                app.mark_dirty();
            }
            reload_elapsed = CycleTiming::elapsed_since(clock, reload_started_at);

            None
        }
    };

    if matches!(
        handle_event(app, terminal, maybe_event).await?,
        EventResult::Quit
    ) {
        return Ok(EventResult::Quit);
    }

    // Drain a bounded number of remaining queued events before re-rendering so
    // rapid key presses stay responsive without starving the next frame.
    let remaining_terminal_event_budget =
        TERMINAL_EVENT_DRAIN_BUDGET.saturating_sub(handled_terminal_events);
    for _ in 0..remaining_terminal_event_budget {
        if clock
            .now_instant()
            .saturating_duration_since(batch_started_at)
            >= TERMINAL_EVENT_DRAIN_DURATION
        {
            break;
        }
        let event = match event_rx.try_recv() {
            Ok(message) => message.into_event_result()?,
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                return Err(terminal_event_channel_closed_error());
            }
        };

        handled_terminal_events += 1;
        if matches!(
            handle_event(app, terminal, Some(event)).await?,
            EventResult::Quit
        ) {
            return Ok(EventResult::Quit);
        }
    }

    // Terminal input changes live session details, such as clearing a
    // focused review, that no app event follows.
    if handled_terminal_events > 0 {
        app.publish_live_session_changes();
    }

    let remaining_events = event_rx.len();
    if remaining_events > 0 {
        debug!(
            budget = TERMINAL_EVENT_DRAIN_BUDGET,
            handled_terminal_events,
            remaining_events,
            "terminal event drain budget exhausted with queued events remaining"
        );
    }
    timing.record_reload(reload_elapsed);
    timing.record_events(
        handled_terminal_events,
        CycleTiming::elapsed_since(clock, batch_started_at).saturating_sub(reload_elapsed),
    );

    Ok(EventResult::Continue)
}

/// Routes a single terminal event to the active mode handler.
///
/// `Event::Paste` is handled in text-input modes so multiline clipboard
/// content is inserted as text instead of interpreted as navigation keys.
async fn process_event<B: Backend>(
    app: &mut App,
    presentation: Rc<PresentationState>,
    terminal: &mut Terminal<B>,
    event: Option<Event>,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    process_event_with_key_handler(app, terminal, event, |app, terminal, key| {
        let presentation = Rc::clone(&presentation);

        Box::pin(async move {
            key_handler::handle_key_event(app, presentation.as_ref(), terminal, key).await
        })
    })
    .await
}

/// Feeds one terminal event to the mouse selection and requests a redraw
/// when the selection changed.
///
/// Left-button reports start, extend, and finish a selection. A key press
/// drops a selection whose button is still down, so a release that never
/// arrives cannot leave a stale highlight. That includes the arrow key a
/// wheel tick becomes: a selection is fixed to screen cells, so it cannot
/// follow text that scrolls. The dispatcher calls this before any handler
/// can consume the event, so every mode cancels alike.
fn update_screen_selection(
    app: &mut App,
    screen_selection: &ScreenSelection,
    event: Option<&Event>,
) {
    let is_changed = match event {
        Some(Event::Mouse(mouse)) => screen_selection.handle_mouse(*mouse),
        Some(Event::Key(key)) if is_press_key_event(*key) => screen_selection.cancel(),
        _ => false,
    };
    if is_changed {
        app.mark_dirty();
    }
}

/// Routes one terminal event with an injected key handler for deterministic
/// branch tests.
async fn process_event_with_key_handler<Terminal, KeyHandler>(
    app: &mut App,
    terminal: &mut Terminal,
    event: Option<Event>,
    mut handle_key_event: KeyHandler,
) -> io::Result<EventResult>
where
    KeyHandler: for<'handler> FnMut(
        &'handler mut App,
        &'handler mut Terminal,
        KeyEvent,
    ) -> Pin<
        Box<dyn Future<Output = io::Result<EventResult>> + 'handler>,
    >,
{
    if let Some(event) = event {
        match event {
            Event::Key(key) if is_press_key_event(key) => {
                return handle_key_event(app, terminal, key).await;
            }
            Event::Paste(pasted_text) => {
                process_paste_event(app, &pasted_text).await;
                app.mark_dirty();
            }
            _ => {}
        }
    }

    Ok(EventResult::Continue)
}

/// Returns whether the runtime should treat the key event as actionable input.
///
/// Keyboard enhancement protocols can emit release and repeat events. The TUI
/// only reacts to the initial press so higher-level mode handlers retain their
/// existing semantics.
fn is_press_key_event(key: KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

/// Applies one pasted-text event to the active editable input.
async fn process_paste_event(app: &mut App, pasted_text: &str) {
    if let AppMode::ExecutionGraph(view) = &mut app.mode
        && let Some(editor) = &mut view.editor
    {
        editor.input.apply(InputCommand::InsertText(
            mode::input_key::normalize_pasted_text(pasted_text),
        ));
    }
    if matches!(&app.mode, AppMode::Prompt { .. }) {
        mode::prompt::handle_paste(app, pasted_text).await;
    }

    if matches!(&app.mode, AppMode::Question { .. }) {
        mode::question::handle_paste(app, pasted_text);
    }

    if matches!(&app.mode, AppMode::Diff { .. }) {
        mode::diff::handle_paste(app, pasted_text);
    }

    if let AppMode::ProjectCatalog {
        dialog:
            crate::presentation::project_catalog::ProjectCatalogDialog::PathInput {
                input, error, ..
            },
    } = &mut app.mode
    {
        input.apply(InputCommand::InsertText(
            mode::input_key::normalize_single_line_pasted_text(pasted_text),
        ));
        *error = None;
    }

    if let AppMode::PublishBranchInput {
        input,
        locked_upstream_ref: None,
        ..
    } = &mut app.mode
    {
        let text = mode::input_key::normalize_single_line_pasted_text(pasted_text);
        input.apply(InputCommand::InsertText(text));
    }

    if matches!(&app.mode, AppMode::List)
        && let Some(action) = app.settings_presentation.action_for_paste(pasted_text)
    {
        let view = app.settings.view();
        let _ = app.settings_presentation.apply(&view, action);
    }
}

#[cfg(test)]
#[path = "event_test.rs"]
mod tests;
