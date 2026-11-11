use std::convert::Infallible;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

use crate::app::App;
use crate::domain::session_message::SessionTranscript;

/// Test backend whose size queries fail once a test requests it.
///
/// Terminal construction still succeeds, so handlers observe the failure on
/// their own layout queries.
pub(super) struct FailingSizeBackend {
    inner: TestBackend,
    size_fails: AtomicBool,
}

impl FailingSizeBackend {
    /// Creates a backend that answers size queries until asked to fail.
    pub(super) fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            size_fails: AtomicBool::new(false),
        }
    }

    /// Makes every later size query fail.
    pub(super) fn fail_size_queries(&self) {
        self.size_fails.store(true, Ordering::Relaxed);
    }
}

impl Backend for FailingSizeBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        infallible(self.inner.draw(content))
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        infallible(self.inner.hide_cursor())
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        infallible(self.inner.show_cursor())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        infallible(self.inner.get_cursor_position())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        infallible(self.inner.set_cursor_position(position))
    }

    fn clear(&mut self) -> io::Result<()> {
        infallible(self.inner.clear())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        infallible(self.inner.clear_region(clear_type))
    }

    fn append_lines(&mut self, line_count: u16) -> io::Result<()> {
        infallible(self.inner.append_lines(line_count))
    }

    fn size(&self) -> io::Result<Size> {
        if self.size_fails.load(Ordering::Relaxed) {
            return Err(io::Error::other("terminal size unavailable"));
        }

        infallible(self.inner.size())
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        infallible(self.inner.window_size())
    }

    fn flush(&mut self) -> io::Result<()> {
        infallible(self.inner.flush())
    }
}

/// Converts one `TestBackend` result, which cannot fail, into `io::Result`.
fn infallible<Value>(result: Result<Value, Infallible>) -> io::Result<Value> {
    result.map_err(|never| match never {})
}

pub(super) fn session_replay_text(session: &crate::domain::session::Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

pub(super) async fn appendable_stack_test_app() -> (App, tempfile::TempDir, String, String) {
    let (mut app, base_dir) = crate::test_support::new_git_test_app_with_mock_tmux_client().await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let source_session_id = app.create_session().await.expect("failed to create source");
    crate::test_support::set_session_status_for_test(
        &mut app,
        &parent_session_id,
        crate::domain::session::Status::Review,
    );
    crate::test_support::set_session_status_for_test(
        &mut app,
        &source_session_id,
        crate::domain::session::Status::Review,
    );

    (app, base_dir, parent_session_id, source_session_id)
}

/// Builds one in-memory project row for project switcher handler tests.
pub(super) fn switcher_project_item(
    project_id: i64,
    name: &str,
    path: std::path::PathBuf,
    last_opened_at: Option<i64>,
) -> crate::domain::project::ProjectListItem {
    crate::domain::project::ProjectListItem {
        active_session_count: 0,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: crate::domain::project::Project {
            created_at: 0,
            display_name: Some(name.to_string()),
            git_branch: None,
            id: project_id,
            is_favorite: false,
            last_opened_at,
            path,
            updated_at: 0,
        },
        session_count: 0,
    }
}
