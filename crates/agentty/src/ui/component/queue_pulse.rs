use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tachyonfx::{Duration, Effect, Interpolation, fx};

use crate::ui::style;

const QUEUE_PULSE_FRAME_COUNT: usize = 20;
const QUEUE_PULSE_HALF_PERIOD_MS: u32 = 1_000;
const QUEUE_PULSE_STEP_MS: u32 = 100;

/// Stateless calm breathing effect for one queued-action glyph.
///
/// The effect fades from subtle to normal text and back over two seconds.
/// Applying an absolute frame offset keeps every queued row synchronized and
/// deterministic without retaining per-row animation state.
pub(crate) struct QueuePulseEffect;

impl QueuePulseEffect {
    /// Applies one deterministic pulse frame to `area`.
    pub(crate) fn apply_stateless(buffer: &mut Buffer, area: Rect, spinner_frame: usize) {
        let mut effect = Self::build_effect();
        let frame_offset = spinner_frame % QUEUE_PULSE_FRAME_COUNT;
        let phase_ms = u32::try_from(frame_offset).unwrap_or_default() * QUEUE_PULSE_STEP_MS;

        effect.process(Duration::from_millis(phase_ms), buffer, area);
    }

    /// Builds a repeating fade that is visually slower than active loaders.
    fn build_effect() -> Effect {
        fx::repeating(fx::ping_pong(fx::fade_to_fg(
            style::palette::text(),
            (QUEUE_PULSE_HALF_PERIOD_MS, Interpolation::SineInOut),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
