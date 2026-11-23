//! WCAG 2 contrast ratios for the colour pairs the token file declares.
//!
//! Every pair must reach its group's minimum in every theme, unless the group
//! records it as a shortfall with the measured ratio and a reason. A recorded
//! ratio must stay exact, so a colour change that moves it, or that fixes the
//! pair, fails until the record is updated or removed.

use crate::design_token::token::{ContrastGroup, THEMES, TokenSet, Value};

/// Reports every pair that misses its minimum without a matching shortfall,
/// every shortfall whose ratio moved or that no longer falls short, and every
/// reference that names no opaque colour.
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
    let mut matched = vec![false; group.shortfalls.len()];
    let foregrounds = resolve(set, usage, &group.foregrounds, errors);
    let backgrounds = resolve(set, usage, &group.backgrounds, errors);
    for (foreground, foreground_colors) in &foregrounds {
        for (background, background_colors) in &backgrounds {
            for (theme_index, theme) in THEMES.iter().enumerate() {
                let measured = ratio(
                    foreground_colors[theme_index],
                    background_colors[theme_index],
                );
                let shortfall = group.shortfalls.iter().position(|shortfall| {
                    shortfall.theme == *theme
                        && shortfall.foreground == foreground.as_str()
                        && shortfall.background == background.as_str()
                });
                let pair = format!("{theme}: {foreground} on {background} ({usage})");
                match shortfall {
                    None if measured < group.minimum => errors.push(format!(
                        "{pair} is {measured:.2}:1, below {}:1; change a colour or record a \
                         shortfall with its reason",
                        group.minimum
                    )),
                    None => {}
                    Some(index) => {
                        matched[index] = true;
                        let recorded = group.shortfalls[index].ratio;
                        if measured >= group.minimum {
                            errors.push(format!(
                                "{pair} now reaches {measured:.2}:1; remove its shortfall"
                            ));
                        } else if format!("{measured:.2}") != format!("{recorded:.2}") {
                            errors.push(format!(
                                "{pair} is {measured:.2}:1, but its shortfall records \
                                 {recorded:.2}; update the record"
                            ));
                        }
                    }
                }
            }
        }
    }
    for (shortfall, matched) in group.shortfalls.iter().zip(matched) {
        if !matched {
            errors.push(format!(
                "{}: shortfall {} on {} ({usage}) matches no pair of the group",
                shortfall.theme, shortfall.foreground, shortfall.background
            ));
        }
        if shortfall.reason.trim().is_empty() {
            errors.push(format!(
                "{}: shortfall {} on {} ({usage}) needs a reason",
                shortfall.theme, shortfall.foreground, shortfall.background
            ));
        }
    }
}

/// Resolves `{path}` references to their opaque colour in each theme,
/// reporting and skipping any that cannot be compared.
fn resolve<'reference>(
    set: &TokenSet,
    usage: &str,
    references: &'reference [String],
    errors: &mut Vec<String>,
) -> Vec<(&'reference String, [[u8; 3]; THEMES.len()])> {
    let mut resolved = Vec::new();
    for reference in references {
        let token = reference
            .strip_prefix('{')
            .and_then(|path| path.strip_suffix('}'))
            .and_then(|path| set.find(path));
        let colors = token.and_then(|token| {
            let colors: Option<Vec<[u8; 3]>> = THEMES
                .iter()
                .map(|theme| match token.value_in(theme) {
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
