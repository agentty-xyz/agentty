//! WCAG 2 contrast ratios for the colour pairs the token file declares.
//!
//! Every pair must reach its group's minimum in every theme, at standard
//! contrast and when the system asks for more, unless the group names the
//! one contrast level it applies to; there is no way to record an exception.

use crate::design_token::token::{
    Contrast, ContrastGroup, DARK_THEME, DEFAULT_THEME, TokenSet, Value,
};

/// Every theme at every contrast level, in the order failures are reported.
const MODES: [(&str, Contrast); 4] = [
    (DEFAULT_THEME, Contrast::Standard),
    (DARK_THEME, Contrast::Standard),
    (DEFAULT_THEME, Contrast::High),
    (DARK_THEME, Contrast::High),
];

/// Reports every pair that misses its minimum and every reference that names
/// no opaque colour.
pub(super) fn check(set: &TokenSet, errors: &mut Vec<String>) {
    for group in &set.contrast {
        check_group(set, group, errors);
    }
}

/// Returns the WCAG 2 contrast ratio of two sRGB colours.
pub(super) fn ratio(first: [u8; 3], second: [u8; 3]) -> f64 {
    let (first, second) = (luminance(first), luminance(second));

    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn check_group(set: &TokenSet, group: &ContrastGroup, errors: &mut Vec<String>) {
    let usage = &group.usage;
    let foregrounds = resolve(set, usage, &group.foregrounds, errors);
    let backgrounds = resolve(set, usage, &group.backgrounds, errors);
    for (foreground, foreground_colors) in &foregrounds {
        for (background, background_colors) in &backgrounds {
            for (index, (theme, contrast)) in MODES.iter().enumerate() {
                if group.contrast.is_some_and(|only| only != *contrast) {
                    continue;
                }
                let pair = (foreground_colors[index], background_colors[index]);
                // A pair that high contrast leaves unchanged was measured at
                // standard contrast already.
                let standard = MODES
                    .iter()
                    .position(|mode| *mode == (*theme, Contrast::Standard))
                    .unwrap_or(index);
                if *contrast == Contrast::High
                    && group.contrast.is_none()
                    && pair == (foreground_colors[standard], background_colors[standard])
                {
                    continue;
                }
                let measured = ratio(pair.0, pair.1);
                if measured < group.minimum {
                    let mode = match contrast {
                        Contrast::High => format!("{theme}, high contrast"),
                        Contrast::Standard => (*theme).to_owned(),
                    };
                    errors.push(format!(
                        "{mode}: {foreground} on {background} ({usage}) is {measured:.2}:1, below \
                         {}:1; change a colour",
                        group.minimum
                    ));
                }
            }
        }
    }
}

/// Resolves `{path}` references to their opaque colour in each mode,
/// reporting and skipping any that cannot be compared.
fn resolve<'reference>(
    set: &TokenSet,
    usage: &str,
    references: &'reference [String],
    errors: &mut Vec<String>,
) -> Vec<(&'reference String, [[u8; 3]; MODES.len()])> {
    let mut resolved = Vec::new();
    for reference in references {
        let token = reference
            .strip_prefix('{')
            .and_then(|path| path.strip_suffix('}'))
            .and_then(|path| set.find(path));
        let colors = token.and_then(|token| {
            let colors: Option<Vec<[u8; 3]>> = MODES
                .iter()
                .map(|(theme, contrast)| match token.value_in(theme, *contrast) {
                    Value::Color(color) => color.opaque(),
                    Value::Css(_) => None,
                })
                .collect();

            colors.and_then(|colors| colors.try_into().ok())
        });
        match colors {
            Some(colors) => resolved.push((reference, colors)),
            None => errors.push(format!(
                "contrast ({usage}): {reference} must reference an opaque colour token, such as \
                 {{color.bg}}"
            )),
        }
    }

    resolved
}

/// Returns the relative luminance of an sRGB colour.
fn luminance(rgb: [u8; 3]) -> f64 {
    let [red, green, blue] = rgb.map(|channel| {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.040_45 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    });

    0.2126 * red + 0.7152 * green + 0.0722 * blue
}

#[cfg(test)]
#[path = "contrast_test.rs"]
mod tests;
