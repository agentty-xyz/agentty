//! Raw colour detection unit tests.

use super::{RawColor, raw_colors};

fn found(source: &str, scss: bool) -> Vec<(usize, String)> {
    raw_colors(source, scss)
        .into_iter()
        .map(|RawColor { line, text }| (line, text))
        .collect()
}

#[test]
fn raw_colours_in_declaration_values_are_reported_with_their_line() {
    // Arrange
    let source = "\
#add, a[href='#fff']:hover {
  color: #FFF;
  border: 1px solid #12345678;
  background: RGBA(0, 0, 0, 0.5) url(\"#abc\");
  outline-color: White;
  --tone: hsl(0 0% 0%);
  box-shadow:
    0 0 1px oklch(0.5 0.1 120);
}
";

    // Act
    let colors = found(source, false);

    // Assert
    assert_eq!(
        colors,
        [
            (2, "#FFF".to_owned()),
            (3, "#12345678".to_owned()),
            (4, "rgba()".to_owned()),
            (5, "white".to_owned()),
            (6, "hsl()".to_owned()),
            (8, "oklch()".to_owned()),
        ]
    );
}

#[test]
fn token_references_keywords_and_non_colours_pass() {
    // Arrange
    let source = "\
@import \"tokens\";
@media (min-width: 40rem) {
  .panel {
    white-space: nowrap;
    color: var(--color-red, currentColor);
    background: color-mix(in srgb, var(--color-bg) 50%, transparent);
    grid-template-areas: \"red blue\";
    content: 'a \\' #fff';
    list-style: #12345 #abcdefabc;
  }
}
";

    // Act
    let colors = found(source, true);

    // Assert
    assert!(colors.is_empty(), "{colors:?}");
}

#[test]
fn comments_are_skipped_and_counted_for_lines() {
    // Arrange
    let source = "/* #fff\n   red */\na {\n  // color: #000;\n  color: blue; /* #fff */\n}\n";

    // Act
    let scss = found(source, true);
    let css = found(source, false);

    // Assert
    assert_eq!(scss, [(5, "blue".to_owned())]);
    assert_eq!(css, [(4, "#000".to_owned()), (5, "blue".to_owned())]);
}
