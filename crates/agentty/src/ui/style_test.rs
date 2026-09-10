use ratatui::style::Color;

use super::super::icon::Icon;
use super::{
    DARK_HORIZON_PALETTE, GREEN_PALETTE, border_style, forge_indicator_color, palette,
    scoped_active_theme, status_color, status_icon,
};
use crate::domain::session::{ReviewRequestState, Status};
use crate::domain::theme::ColorTheme;

#[test]
fn status_color_returns_muted_text_for_new() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = status_color(Status::Draft);

    // Assert
    assert_eq!(color, palette::text_muted());
}

#[test]
fn status_color_returns_success_for_merged_and_done() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let merged_color = status_color(Status::Merged);
    let done_color = status_color(Status::Done);

    // Assert
    assert_eq!(merged_color, palette::success());
    assert_eq!(done_color, palette::success());
}

#[test]
fn status_color_returns_danger_for_canceled() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = status_color(Status::Canceled);

    // Assert
    assert_eq!(color, palette::danger());
}

#[test]
fn status_color_returns_warning_for_in_progress() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = status_color(Status::InProgress);

    // Assert
    assert_eq!(color, palette::warning());
}

#[test]
fn status_color_returns_info_for_review() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = status_color(Status::Review);

    // Assert
    assert_eq!(color, palette::info());
}

#[test]
fn status_color_returns_accent_for_merging() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = status_color(Status::Merging);

    // Assert
    assert_eq!(color, palette::accent());
}

#[test]
fn status_color_uses_green_palette_when_active() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Green);

    // Act
    let color = status_color(Status::Merging);

    // Assert
    assert_eq!(color, GREEN_PALETTE.accent);
}

#[test]
fn active_palette_returns_current_table_contrast_tones() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let palette = palette::active();

    // Assert
    assert_eq!(palette.surface, Color::DarkGray);
    assert_eq!(palette.surface_elevated, Color::Black);
    assert_eq!(palette.surface_prompt, Color::Rgb(35, 42, 55));
    assert_eq!(palette.border, Color::Gray);
    assert_eq!(palette.text, Color::White);
}

#[test]
fn active_palette_returns_green_terminal_tones() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Green);

    // Act
    let palette = palette::active();

    // Assert
    assert_eq!(palette.surface, Color::Rgb(6, 18, 8));
    assert_eq!(palette.surface_elevated, Color::Rgb(10, 32, 12));
    assert_eq!(palette.border, Color::Rgb(74, 132, 78));
    assert_eq!(palette.text, Color::Rgb(205, 245, 211));
    assert_eq!(palette.text_muted, Color::Rgb(126, 159, 120));
    assert_eq!(palette.accent, Color::Rgb(86, 184, 105));
}

#[test]
fn active_palette_returns_muted_green_status_tones() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Green);

    // Act
    let palette = palette::active();

    // Assert
    assert_eq!(palette.success, Color::Rgb(86, 184, 105));
    assert_eq!(palette.warning, Color::Rgb(181, 169, 99));
    assert_eq!(palette.danger, Color::Rgb(190, 88, 78));
    assert_eq!(palette.question, Color::Rgb(132, 189, 142));
}

#[test]
fn status_color_uses_dark_horizon_palette_when_active() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::DarkHorizon);

    // Act
    let color = status_color(Status::Merging);

    // Assert
    assert_eq!(color, DARK_HORIZON_PALETTE.accent);
}

#[test]
fn active_palette_returns_dark_horizon_navy_tones() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::DarkHorizon);

    // Act
    let palette = palette::active();

    // Assert
    assert_eq!(palette.surface, Color::Rgb(22, 24, 31));
    assert_eq!(palette.surface_elevated, Color::Rgb(33, 36, 48));
    assert_eq!(palette.surface_selection, Color::Rgb(45, 50, 68));
    assert_eq!(palette.border, Color::Rgb(52, 60, 82));
    assert_eq!(palette.text, Color::Rgb(214, 217, 232));
    assert_eq!(palette.text_muted, Color::Rgb(132, 136, 168));
    assert_eq!(palette.accent, Color::Rgb(89, 225, 227));
}

#[test]
fn active_palette_returns_dark_horizon_status_tones() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::DarkHorizon);

    // Act
    let palette = palette::active();

    // Assert
    assert_eq!(palette.success, Color::Rgb(41, 211, 152));
    assert_eq!(palette.warning, Color::Rgb(250, 194, 154));
    assert_eq!(palette.danger, Color::Rgb(209, 67, 76));
    assert_eq!(palette.question, Color::Rgb(184, 119, 219));
}

#[test]
fn dark_horizon_selection_surface_is_lighter_than_base_surface() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::DarkHorizon);

    // Act
    let selection = palette::surface_selection();
    let surface = palette::surface();

    // Assert
    let (selection_red, selection_green, selection_blue) =
        rgb_components(selection).expect("dark horizon selection surface must be RGB");
    let (surface_red, surface_green, surface_blue) =
        rgb_components(surface).expect("dark horizon base surface must be RGB");
    assert!(
        selection_red > surface_red
            && selection_green > surface_green
            && selection_blue > surface_blue,
        "selected rows must stay visually distinct from the base surface",
    );
}

/// Returns the RGB components of a palette color, or `None` for
/// non-RGB terminal colors.
fn rgb_components(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        _ => None,
    }
}

#[test]
fn dark_horizon_accent_and_danger_are_distinct() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::DarkHorizon);

    // Act
    let accent = palette::accent();
    let danger = palette::danger();

    // Assert
    assert_ne!(
        accent, danger,
        "accent and danger must be distinct so canceled states do not blend into focused chrome",
    );
}

#[test]
fn dark_horizon_accent_soft_and_question_are_distinct() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::DarkHorizon);

    // Act
    let accent_soft = palette::accent_soft();
    let question = palette::question();

    // Assert
    assert_ne!(
        accent_soft, question,
        "accent_soft and question must be distinct so queued and question states are visually \
         separable",
    );
}

#[test]
fn border_style_uses_active_palette_border_color() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Green);

    // Act
    let style = border_style();

    // Assert
    assert_eq!(style.fg, Some(GREEN_PALETTE.border));
}

#[test]
fn status_icon_returns_check_for_merged_and_done() {
    // Arrange, Act
    let merged_icon = status_icon(Status::Merged);
    let done_icon = status_icon(Status::Done);

    // Assert
    assert!(matches!(merged_icon, Icon::Check));
    assert!(matches!(done_icon, Icon::Check));
}

#[test]
fn status_icon_returns_cross_for_canceled() {
    // Arrange / Act
    let icon = status_icon(Status::Canceled);

    // Assert
    assert!(matches!(icon, Icon::Cross));
}

#[test]
fn status_icon_returns_pending_for_new() {
    // Arrange / Act
    let icon = status_icon(Status::Draft);

    // Assert
    assert!(matches!(icon, Icon::Pending));
}

#[test]
fn forge_indicator_color_returns_warning_for_open() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = forge_indicator_color(Some(ReviewRequestState::Open));

    // Assert
    assert_eq!(color, palette::warning());
}

#[test]
fn forge_indicator_color_returns_success_for_merged() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = forge_indicator_color(Some(ReviewRequestState::Merged));

    // Assert
    assert_eq!(color, palette::success());
}

#[test]
fn forge_indicator_color_returns_danger_for_closed() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = forge_indicator_color(Some(ReviewRequestState::Closed));

    // Assert
    assert_eq!(color, palette::danger());
}

#[test]
fn forge_indicator_color_returns_accent_soft_for_published_only() {
    // Arrange
    let _theme_scope = scoped_active_theme(ColorTheme::Current);

    // Act
    let color = forge_indicator_color(None);

    // Assert
    assert_eq!(color, palette::accent_soft());
}
