use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::QueuePulseEffect;
use crate::ui::style;

#[test]
fn test_apply_stateless_breathes_from_subtle_toward_normal_text() {
    // Arrange
    let area = Rect::new(0, 0, 1, 1);
    let mut buffer = Buffer::empty(area);
    buffer[(0, 0)]
        .set_symbol("≡")
        .set_fg(style::palette::text_subtle());

    // Act
    QueuePulseEffect::apply_stateless(&mut buffer, area, 5);

    // Assert
    assert_ne!(buffer[(0, 0)].fg, style::palette::text_subtle());
    assert_ne!(buffer[(0, 0)].fg, style::palette::warning());
}

#[test]
fn test_apply_stateless_wraps_after_full_period() {
    // Arrange
    let area = Rect::new(0, 0, 1, 1);
    let mut first_cycle = Buffer::empty(area);
    first_cycle[(0, 0)]
        .set_symbol("≡")
        .set_fg(style::palette::text_subtle());
    let mut second_cycle = first_cycle.clone();

    // Act
    QueuePulseEffect::apply_stateless(&mut first_cycle, area, 5);
    QueuePulseEffect::apply_stateless(&mut second_cycle, area, 25);

    // Assert
    assert_eq!(first_cycle[(0, 0)].fg, second_cycle[(0, 0)].fg);
}
