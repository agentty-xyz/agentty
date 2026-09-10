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
#[path = "queue_pulse_test.rs"]
mod tests;
