//! Raw colour and `theme-color` detection in templates unit tests.

use super::scan;
use crate::design_token::stylesheet::RawColor;

fn colors(source: &str) -> Vec<(usize, String)> {
    scan(source)
        .colors
        .into_iter()
        .map(|RawColor { line, text }| (line, text))
        .collect()
}

#[test]
fn styles_scripts_and_colour_attributes_are_searched_and_theme_colors_found() {
    // Arrange
    let source = r##"<!DOCTYPE html>
<!-- <meta name="theme-color" content="#123456"> -->
<html>
<head>
<meta name="Theme-Color" content="#070b08">
<meta name="description" content="#fff is not a colour attribute">
<STYLE>
  a { color: #fff; }
</STYLE>
<script type="module">
  const bg = '#000';
</script>
</head>
<body style="background: red" data-x='#fff'>
<svg><path fill="White" stroke=#abc d="M0 0"/><rect fill="currentColor" /></svg>
<p hidden title = x>Text #fff and {% if a == "b" %}x{% endif %} &lt; 2</p>
</body>
"##;

    // Act
    let scan = scan(source);

    // Assert
    let found: Vec<_> = scan
        .colors
        .iter()
        .map(|color| (color.line, color.text.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (8, "#fff"),
            (11, "#000"),
            (14, "red"),
            (15, "white"),
            (15, "#abc"),
        ]
    );
    assert_eq!(
        scan.theme_colors
            .iter()
            .map(|range| &source[range.clone()])
            .collect::<Vec<_>>(),
        ["#070b08"]
    );
}

#[test]
fn unterminated_markup_ends_at_the_end_of_the_source() {
    // Arrange
    for (source, expected) in [
        ("<p style=\"color: #fff", vec![(1, "#fff".to_owned())]),
        ("<p fill=#fff", vec![(1, "#fff".to_owned())]),
        ("<p", Vec::new()),
        ("<script>const a = '#fff';", vec![(1, "#fff".to_owned())]),
        ("<!-- <p fill=#fff>", Vec::new()),
        ("<a =\"#fff\" fill='#000'>", vec![(1, "#000".to_owned())]),
    ] {
        // Act
        let found = colors(source);

        // Assert
        assert_eq!(found, expected, "{source}");
    }
}
