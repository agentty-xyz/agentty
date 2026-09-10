use super::{
    build_activity_heatmap_grid, build_heatmap_month_row, build_recent_activity_stats,
    build_visible_heatmap_month_row, heatmap_intensity_level, heatmap_max_count,
    heatmap_month_markers, visible_heatmap_week_count,
};
use crate::domain::session::DailyActivity;

#[test]
fn test_build_activity_heatmap_grid_places_values_in_expected_cells() {
    // Arrange
    let end_day_key = 0_i64;
    let activity = vec![
        DailyActivity {
            day_key: 0,
            session_count: 2,
        },
        DailyActivity {
            day_key: -3,
            session_count: 1,
        },
    ];

    // Act
    let grid = build_activity_heatmap_grid(&activity, end_day_key);

    // Assert
    assert_eq!(grid[3][52], 2);
    assert_eq!(grid[0][52], 1);
}

#[test]
fn test_build_recent_activity_stats_counts_trailing_windows_and_streaks() {
    // Arrange
    let end_day_key = 100_i64;
    let activity = vec![
        DailyActivity {
            day_key: 71,
            session_count: 5,
        },
        DailyActivity {
            day_key: 97,
            session_count: 1,
        },
        DailyActivity {
            day_key: 98,
            session_count: 2,
        },
        DailyActivity {
            day_key: 99,
            session_count: 3,
        },
        DailyActivity {
            day_key: 100,
            session_count: 4,
        },
    ];

    // Act
    let stats = build_recent_activity_stats(&activity, end_day_key);

    // Assert
    assert_eq!(stats.sessions_last_7_days, 10);
    assert_eq!(stats.sessions_last_30_days, 15);
    assert_eq!(stats.current_streak_days, 4);
    assert_eq!(stats.best_streak_days, 4);
}

#[test]
fn test_build_recent_activity_stats_handles_no_activity() {
    // Arrange
    let activity = Vec::new();
    let end_day_key = 100_i64;

    // Act
    let stats = build_recent_activity_stats(&activity, end_day_key);

    // Assert
    assert_eq!(stats.sessions_last_7_days, 0);
    assert_eq!(stats.sessions_last_30_days, 0);
    assert_eq!(stats.current_streak_days, 0);
    assert_eq!(stats.best_streak_days, 0);
}

#[test]
fn test_heatmap_month_markers_start_on_month_changes() {
    // Arrange
    let end_day_key = 0_i64;

    // Act
    let markers = heatmap_month_markers(end_day_key);

    // Assert
    assert_eq!(markers.first(), Some(&(0, "Dec")));
    assert!(markers.iter().any(|marker| marker.1 == "Jan"));
}

#[test]
fn test_build_heatmap_month_row_places_labels_on_week_columns() {
    // Arrange
    let end_day_key = 0_i64;

    // Act
    let month_row = build_heatmap_month_row(end_day_key, 4, 2);

    // Assert
    assert!(month_row.starts_with("    Dec"));
    assert_eq!(month_row.chars().count(), 110);
}

#[test]
fn test_visible_heatmap_week_count_clamps_to_available_width() {
    // Arrange
    let available_width = 26_usize;

    // Act
    let visible_week_count = visible_heatmap_week_count(available_width, 4, 2);

    // Assert
    assert_eq!(visible_week_count, 11);
}

#[test]
fn test_build_visible_heatmap_month_row_uses_trailing_weeks() {
    // Arrange
    let end_day_key = 0_i64;

    // Act
    let month_row = build_visible_heatmap_month_row(end_day_key, 4, 2, 11);

    // Assert
    assert_eq!(month_row.chars().count(), 26);
    assert!(month_row.contains("Dec"));
    assert_ne!(month_row.trim(), "");
}

#[test]
fn test_heatmap_intensity_level_scales_from_zero_to_max() {
    // Arrange
    let max_count = 8_u32;

    // Act
    let zero = heatmap_intensity_level(0, max_count);
    let low = heatmap_intensity_level(1, max_count);
    let medium = heatmap_intensity_level(4, max_count);
    let max = heatmap_intensity_level(8, max_count);

    // Assert
    assert_eq!(zero, 0);
    assert_eq!(low, 1);
    assert_eq!(medium, 2);
    assert_eq!(max, 4);
}

#[test]
fn test_heatmap_max_count_returns_largest_daily_value() {
    // Arrange
    let grid = vec![vec![0, 2, 1], vec![3, 4, 0]];

    // Act
    let max_count = heatmap_max_count(&grid);

    // Assert
    assert_eq!(max_count, 4);
}
