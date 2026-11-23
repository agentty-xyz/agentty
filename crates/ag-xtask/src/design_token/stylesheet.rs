//! Raw colour detection in hand-written stylesheets, whose colours must come
//! from design token custom properties.
//!
//! Only declaration values are searched, so ID selectors such as `#add` and
//! property names such as `white-space` never match. Comments and strings
//! are skipped, as are `//` line comments in SCSS.

/// Colour functions that take literal channels. `color-mix()` is not one:
/// it mixes colours that are themselves checked.
const COLOR_FUNCTIONS: [&str; 10] = [
    "color", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "rgb", "rgba",
];

/// CSS named colours. `transparent` and `currentcolor` name no design value,
/// so they stay allowed.
const NAMED_COLORS: [&str; 148] = [
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgreen",
    "darkgrey",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkslategrey",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dimgrey",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "green",
    "greenyellow",
    "grey",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgray",
    "lightgreen",
    "lightgrey",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightslategrey",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "rebeccapurple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "slategrey",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
];

/// A raw colour and the line it starts on.
#[derive(Debug, PartialEq)]
pub(super) struct RawColor {
    pub(super) line: usize,
    pub(super) text: String,
}

/// Returns every raw colour in the declaration values of `source`. `scss`
/// also treats `//` as the start of a line comment.
pub(super) fn raw_colors(source: &str, scss: bool) -> Vec<RawColor> {
    let mut found = Vec::new();
    for (lines, value) in declaration_values(source, scss) {
        scan_value(&lines, &value, &mut found);
    }

    found
}

/// Splits `source` into statements, with comments and string contents
/// removed and every character tagged with its line, and returns the value
/// of each one that is a declaration. A statement that ends at `{` is a
/// selector or an at-rule prelude.
fn declaration_values(source: &str, scss: bool) -> Vec<(Vec<usize>, String)> {
    let mut values = Vec::new();
    let mut statement: Vec<(usize, char)> = Vec::new();
    let mut line = 1;
    let mut chars = source.chars().peekable();
    while let Some(current) = chars.next() {
        match current {
            '/' if chars.peek() == Some(&'*') => {
                skip(&mut chars, &mut line, |previous, char| {
                    previous == '*' && char == '/'
                });
            }
            '/' if scss && chars.peek() == Some(&'/') => {
                skip(&mut chars, &mut line, |_, char| char == '\n');
            }
            '"' | '\'' => {
                statement.push((line, '"'));
                let mut escaped = false;
                skip(&mut chars, &mut line, |_, char| {
                    let ends = !escaped && char == current;
                    escaped = !escaped && char == '\\';

                    ends
                });
                statement.push((line, '"'));
            }
            '{' => statement.clear(),
            ';' | '}' => {
                push_value(&statement, &mut values);
                statement.clear();
            }
            _ => {
                statement.push((line, current));
                line += usize::from(current == '\n');
            }
        }
    }

    values
}

/// Consumes characters through the first one that `ends` accepts, given it
/// and the character before it, counting the lines it passes.
fn skip(
    chars: &mut impl Iterator<Item = char>,
    line: &mut usize,
    mut ends: impl FnMut(char, char) -> bool,
) {
    let mut previous = ' ';
    for skipped in chars {
        *line += usize::from(skipped == '\n');
        if ends(previous, skipped) {
            break;
        }
        previous = skipped;
    }
}

/// Records the value of `statement` when it is a declaration, which a colon
/// separates from its property name.
fn push_value(statement: &[(usize, char)], values: &mut Vec<(Vec<usize>, String)>) {
    if let Some(colon) = statement.iter().position(|(_, char)| *char == ':') {
        let value = &statement[colon + 1..];
        values.push((
            value.iter().map(|(line, _)| *line).collect(),
            value.iter().map(|(_, char)| *char).collect(),
        ));
    }
}

/// Reports hex colours, colour functions, and named colours in one value,
/// whose characters start on the lines in `lines`.
fn scan_value(lines: &[usize], value: &str, found: &mut Vec<RawColor>) {
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let start = index;
        let is_word = |char: char| char.is_ascii_alphanumeric() || char == '-' || char == '_';
        if chars[index] == '#' {
            index += 1;
            while index < chars.len() && is_word(chars[index]) {
                index += 1;
            }
            let digits = &chars[start + 1..index];
            if matches!(digits.len(), 3 | 4 | 6 | 8) && digits.iter().all(char::is_ascii_hexdigit) {
                found.push(RawColor {
                    line: lines[start],
                    text: chars[start..index].iter().collect(),
                });
            }
        } else if is_word(chars[index]) {
            while index < chars.len() && is_word(chars[index]) {
                index += 1;
            }
            let word = chars[start..index]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            let is_function = chars.get(index) == Some(&'(');
            if (is_function && COLOR_FUNCTIONS.contains(&word.as_str()))
                || (!is_function && NAMED_COLORS.contains(&word.as_str()))
            {
                found.push(RawColor {
                    line: lines[start],
                    text: if is_function {
                        format!("{word}()")
                    } else {
                        word
                    },
                });
            }
        } else {
            index += 1;
        }
    }
}

#[cfg(test)]
#[path = "stylesheet_test.rs"]
mod tests;
