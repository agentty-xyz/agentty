//! Raw colour detection in page scripts unit tests.

use super::raw_colors;
use crate::design_token::stylesheet::RawColor;

fn found(source: &str, first_line: usize) -> Vec<(usize, String)> {
    raw_colors(source, first_line)
        .into_iter()
        .map(|RawColor { line, text }| (line, text))
        .collect()
}

#[test]
fn literal_colours_in_strings_and_templates_are_reported_with_their_line() {
    // Arrange
    let source = r#"const bg = '#070B08';
const shadow = "0 0 4px rgba(0, 0, 0, 0.2)";
const glow = `hsl( .5turn 10% 10%) ${size}px
  #12345678`;
const wide = 'color(display-p3 1 0 0)';
const escaped = 'it\'s #abc';
const label = `${theme === "green" ? '#fff' : `nested #000 ${`deep #111`}`} tail #222`;
const map = `${ { a: 1 }.a } #333`;
"#;

    // Act
    let colors = found(source, 10);

    // Assert
    assert_eq!(
        colors,
        [
            (10, "#070B08".to_owned()),
            (11, "rgba()".to_owned()),
            (12, "hsl()".to_owned()),
            (13, "#12345678".to_owned()),
            (14, "color()".to_owned()),
            (15, "#abc".to_owned()),
            (16, "#fff".to_owned()),
            (16, "#000".to_owned()),
            (16, "#111".to_owned()),
            (16, "#222".to_owned()),
            (17, "#333".to_owned()),
        ]
    );
}

#[test]
fn comments_selectors_regexes_and_computed_colours_are_not_raw_colours() {
    // Arrange
    let source = r##"// A comment names #fff and rgb(0, 0, 0).
/* So does this one: #000000. */
class Box { #add = 1; }
document.querySelector('#add').closest("#fade");
const pattern = /['"#fff]/g;
const quoted = a.split(/"/)[0] + "x";
const ratio = total / 2 / 4 + {}.size / 2;
function source() { return /'#bad'/.source; }
const channel = 'rgb(' + level + ',' + level + ',' + level + ')';
const relative = 'rgb(from var(--color-bg) r g b / 50%)';
const tokens = 'var(--color-bg)';
const words = 'green dark white #12 #12345 #ggg &#9472; a#fff';
const open = 'unterminated #fff
const shadowed = color + '(';
"##;

    // Act
    let colors = found(source, 1);

    // Assert
    assert_eq!(colors, [(13, "#fff".to_owned())]);
    assert_eq!(found("", 1), []);
}

#[test]
fn unterminated_literals_end_at_the_end_of_the_source() {
    // Arrange
    for (source, expected) in [
        ("const a = `#fff", vec![(1, "#fff".to_owned())]),
        ("const a = `${`", Vec::new()),
        ("const a = /[/", Vec::new()),
        ("/* #fff", Vec::new()),
        ("const a = 'rgb(", Vec::new()),
        ("const a = 'x\\", Vec::new()),
        ("const a = `x\\", Vec::new()),
        ("x = /a\\/b/i + '#fff'", vec![(1, "#fff".to_owned())]),
        ("/'#fff'/.test(x)", Vec::new()),
    ] {
        // Act
        let colors = found(source, 1);

        // Assert
        assert_eq!(colors, expected, "{source}");
    }
}
