use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tachyonfx::{Duration, Effect, Interpolation, fx};

use crate::icon::TACHYON_LOADER_WIDTH;
use crate::ui::style;

const TACHYON_LOADER_FRAME_COUNT: usize = 9;
const TACHYON_LOADER_PERIOD_MS: u32 = 900;
const TACHYON_LOADER_STEP_MS: u32 = 100;

/// Stateful Tachyonfx loader effect for the shared `▌▌▌` loader glyph.
///
/// The effect stores its previous frame so callers with persistent UI state can
/// advance the Tachyonfx phase by frame deltas instead of rebuilding the effect
/// for each render tick.
pub(crate) struct TachyonLoaderEffect {
    effect: Effect,
    spinner_frame: Option<usize>,
}

impl TachyonLoaderEffect {
    /// Builds a reusable loader effect ready for the first frame.
    pub(crate) fn new() -> Self {
        Self {
            effect: Self::build_effect(),
            spinner_frame: None,
        }
    }

    /// Applies the loader pulse to `area`, advancing from the previous frame
    /// offset when available.
    pub(crate) fn apply(&mut self, buffer: &mut Buffer, area: Rect, spinner_frame: usize) {
        let elapsed_steps = self.elapsed_steps(spinner_frame);
        let phase_ms = u32::try_from(elapsed_steps).unwrap_or_default() * TACHYON_LOADER_STEP_MS;

        self.effect
            .process(Duration::from_millis(phase_ms), buffer, area);
        self.spinner_frame = Some(spinner_frame);
    }

    /// Applies one deterministic Tachyon loader frame without preserving
    /// animation state between renders.
    pub(crate) fn apply_stateless(buffer: &mut Buffer, area: Rect, spinner_frame: usize) {
        let mut effect = Self::new();
        effect.apply(buffer, area, spinner_frame);
    }

    /// Finds the bottom-most `▌▌▌` glyph in `area` and applies a deterministic
    /// Tachyon loader frame to it.
    pub(crate) fn apply_to_last_glyph(
        buffer: &mut Buffer,
        area: Rect,
        spinner_frame: usize,
    ) -> Option<Rect> {
        let loader_area = find_last_loader_glyph_area(buffer, area)?;
        Self::apply_stateless(buffer, loader_area, spinner_frame);

        Some(loader_area)
    }

    /// Returns elapsed animation steps and resets the finite Tachyonfx effect
    /// when the spinner frame crosses into a new loader cycle.
    fn elapsed_steps(&mut self, spinner_frame: usize) -> usize {
        let frame_offset = spinner_frame % TACHYON_LOADER_FRAME_COUNT;
        let Some(previous_spinner_frame) = self.spinner_frame else {
            return frame_offset;
        };
        if spinner_frame <= previous_spinner_frame {
            return 0;
        }

        let previous_frame_offset = previous_spinner_frame % TACHYON_LOADER_FRAME_COUNT;
        let elapsed_steps = spinner_frame - previous_spinner_frame;
        if previous_frame_offset + elapsed_steps >= TACHYON_LOADER_FRAME_COUNT {
            self.effect = Self::build_effect();

            return frame_offset;
        }

        elapsed_steps
    }

    /// Builds the Tachyonfx effect that sweeps emphasis across the loader
    /// glyph cells.
    fn build_effect() -> Effect {
        fx::effect_fn_buf(
            (),
            (TACHYON_LOADER_PERIOD_MS, Interpolation::Linear),
            move |_state, context, buffer| {
                let active_color = style::palette::warning();
                let base_color = style::palette::warning_soft();
                let muted_color = style::palette::text_subtle();
                let alpha = context.alpha();
                let active_index = if alpha < (1.0 / 3.0) {
                    0
                } else if alpha < (2.0 / 3.0) {
                    1
                } else {
                    2
                };
                let trailing_index = (active_index + 2) % 3;

                for (cell_index, position) in context.area.positions().enumerate() {
                    let cell = &mut buffer[position];
                    if cell.symbol() == " " {
                        continue;
                    }

                    let loader_index = cell_index % usize::from(TACHYON_LOADER_WIDTH);
                    let color = if loader_index == active_index {
                        active_color
                    } else if loader_index == trailing_index {
                        base_color
                    } else {
                        muted_color
                    };

                    cell.set_fg(color);
                }
            },
        )
    }
}

/// Finds the bottom-most `▌▌▌` glyph area inside `area`.
fn find_last_loader_glyph_area(buffer: &Buffer, area: Rect) -> Option<Rect> {
    if area.width < TACHYON_LOADER_WIDTH {
        return None;
    }

    let end_y = area.y.saturating_add(area.height);
    let max_x = area
        .x
        .saturating_add(area.width)
        .saturating_sub(TACHYON_LOADER_WIDTH);

    for y in (area.y..end_y).rev() {
        for x in area.x..=max_x {
            let matches_loader = (0..TACHYON_LOADER_WIDTH)
                .all(|offset| buffer[(x.saturating_add(offset), y)].symbol() == "▌");
            if matches_loader {
                return Some(Rect::new(x, y, TACHYON_LOADER_WIDTH, 1));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_stateless_emphasizes_loader_cells() {
        // Arrange
        let area = Rect::new(0, 0, TACHYON_LOADER_WIDTH, 1);
        let mut buffer = Buffer::empty(area);
        for column in 0..TACHYON_LOADER_WIDTH {
            buffer[(column, 0)].set_symbol("▌");
        }

        // Act
        TachyonLoaderEffect::apply_stateless(&mut buffer, area, 4);

        // Assert
        let foreground_colors = (0..TACHYON_LOADER_WIDTH)
            .map(|column| buffer[(column, 0)].fg)
            .collect::<Vec<_>>();
        assert!(foreground_colors.contains(&style::palette::warning()));
        assert!(foreground_colors.contains(&style::palette::warning_soft()));
    }

    #[test]
    fn test_apply_to_last_glyph_targets_bottom_most_loader() {
        // Arrange
        let area = Rect::new(0, 0, 8, 3);
        let mut buffer = Buffer::empty(area);
        for column in 0..TACHYON_LOADER_WIDTH {
            buffer[(column, 0)].set_symbol("▌");
            buffer[(column + 3, 2)].set_symbol("▌");
        }

        // Act
        let loader_area = TachyonLoaderEffect::apply_to_last_glyph(&mut buffer, area, 0);

        // Assert
        assert_eq!(loader_area, Some(Rect::new(3, 2, TACHYON_LOADER_WIDTH, 1)));
        assert_eq!(buffer[(0, 0)].fg, ratatui::style::Color::Reset);
        assert_eq!(buffer[(3, 2)].fg, style::palette::warning());
    }
}
