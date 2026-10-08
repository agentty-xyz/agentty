use std::time::{Duration, Instant, SystemTime};

use super::{read_frame_time, visible_review_session_id};
use crate::app::Tab;
use crate::infra::clock::Clock;
use crate::presentation::app_mode::{AppMode, DiffFocus, DiffLineComments};

/// Unix timestamp where the test clock's local offset switches from summer to
/// winter time.
const DST_END_UNIX_SECONDS: i64 = 1_792_890_000;

/// Pinned clock whose local offset changes at one daylight-saving boundary.
struct DaylightSavingClock {
    unix_seconds: u64,
}

impl Clock for DaylightSavingClock {
    fn local_utc_offset_seconds(&self, timestamp_seconds: i64) -> i64 {
        if timestamp_seconds < DST_END_UNIX_SECONDS {
            return 7_200;
        }

        3_600
    }

    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(self.unix_seconds)
    }
}

#[test]
fn read_frame_time_resolves_visible_session_offset_at_its_start() {
    // Arrange
    let clock = DaylightSavingClock {
        unix_seconds: 1_793_000_000,
    };
    let created_at = DST_END_UNIX_SECONDS - 86_400;

    // Act
    let with_session = read_frame_time(&clock, Some(created_at));
    let without_session = read_frame_time(&clock, None);

    // Assert
    assert_eq!(with_session.unix_seconds(), 1_793_000_000);
    assert_eq!(with_session.unix_millis(), 1_793_000_000_000);
    assert_eq!(with_session.local_utc_offset_seconds(), 3_600);
    assert_eq!(with_session.local_utc_offset_seconds_at(created_at), 7_200);
    assert_eq!(
        without_session.local_utc_offset_seconds_at(created_at),
        3_600
    );
}

#[test]
fn visible_review_session_id_includes_diff_comments() {
    // Arrange
    let mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: crate::presentation::app_mode::DiffPreview::default(),
        review_comments: Some(crate::presentation::app_mode::DiffReviewComments::loading(
            1,
        )),
        restore: None,
        scroll_cache: None,
        session_id: "session-id".into(),
        scroll_offset: 0,
    };

    // Act
    let session_id = visible_review_session_id(&mode);

    // Assert
    assert_eq!(session_id, Some("session-id"));
}

#[test]
fn visible_review_session_id_includes_loading_diff() {
    // Arrange
    let mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: None,
        request_id: 1,
        restore: None,
        session_id: "loading-session".into(),
        sidebar_focus: crate::presentation::app_mode::DiffSidebarFocus::Files,
    };

    // Act
    let session_id = visible_review_session_id(&mode);

    // Assert
    assert_eq!(session_id, Some("loading-session"));
}

#[tokio::test]
async fn view_snapshot_builds_settings_screen_only_for_settings_tab() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;

    // Act
    app.tabs.set(Tab::Sessions);
    let sessions_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();
    app.tabs.set(Tab::Settings);
    let settings_tab_has_settings_screen = app.view_snapshot().settings_screen.is_some();

    // Assert
    assert!(!sessions_tab_has_settings_screen);
    assert!(settings_tab_has_settings_screen);
}
