//! Font face checks and font stylesheet rendering unit tests.

use super::{Embedded, is_relative_inside, stylesheet};
use crate::design_token::token::{FontFace, FontStyle};

/// Returns a regular face of `family` at `weight` with `notice`.
fn face(family: &str, weight: u16, notice: &str) -> FontFace {
    FontFace {
        family: family.to_owned(),
        license: "fonts/OFL.txt".to_owned(),
        notice: notice.to_owned(),
        source: "fonts/Face.woff2".to_owned(),
        style: FontStyle::Normal,
        weight,
    }
}

#[test]
fn stylesheet_embeds_each_face_after_its_notice() {
    // Arrange
    let regular = face("Mono Face", 400, " Copyright A. ");
    let italic = FontFace {
        style: FontStyle::Italic,
        ..face("Mono Face", 700, "Copyright B.")
    };
    let faces = [
        Embedded::new(&regular, b"wOF2a".to_vec(), "Licence A."),
        Embedded::new(&italic, b"wOF2b".to_vec(), "Licence B."),
    ]
    .map(|embedded| embedded.expect("valid face"));

    // Act
    let text = stylesheet("/* Header. */", &faces);

    // Assert
    assert_eq!(
        text,
        "/* Header. */\n\n/*! Mono Face: Copyright A. */\n@font-face {\n  font-family: \"Mono \
         Face\";\n  font-style: normal;\n  font-weight: 400;\n  font-display: swap;\n  src: \
         url(\"data:font/woff2;base64,d09GMmE=\") format(\"woff2\");\n}\n\n/*! Mono Face: \
         Copyright B. */\n@font-face {\n  font-family: \"Mono Face\";\n  font-style: italic;\n  \
         font-weight: 700;\n  font-display: swap;\n  src: \
         url(\"data:font/woff2;base64,d09GMmI=\") format(\"woff2\");\n}\n"
    );
    assert_eq!(stylesheet("/* Header. */", &[]), "/* Header. */\n");
}

#[test]
fn faces_that_cannot_be_embedded_report_every_reason() {
    // Arrange
    let quoted = face("Mono \"Face\"", 0, "Ends a comment */");
    let empty = face("", 1001, "Two\nlines");
    let blank = face("Mono", 400, " ");
    let closing = face("Mono */ body {} /*", 400, "Copyright.");

    // Act
    let quoted = Embedded::new(&quoted, b"OTTO".to_vec(), "Licence.");
    let empty = Embedded::new(&empty, b"wOF2".to_vec(), " \n");
    let blank = Embedded::new(&blank, b"wOF2".to_vec(), "Licence.");
    let closing = Embedded::new(&closing, b"wOF2".to_vec(), "Licence.");

    // Assert
    assert_eq!(
        closing.err(),
        Some(vec![
            "font face \"Mono */ body {} /*\": the family must be a name without quotes, \
             backslashes, control characters, or the end of a CSS comment"
                .to_owned()
        ])
    );
    assert_eq!(
        quoted.err(),
        Some(vec![
            "font face \"Mono \\\"Face\\\"\": the family must be a name without quotes, \
             backslashes, control characters, or the end of a CSS comment"
                .to_owned(),
            "font face Mono \"Face\": weight 0 is outside 1 to 1000".to_owned(),
            "font face Mono \"Face\": fonts/Face.woff2 is not a WOFF2 file".to_owned(),
            "font face Mono \"Face\": the notice must be one line that cannot end a CSS comment"
                .to_owned(),
        ])
    );
    assert_eq!(
        empty.err(),
        Some(vec![
            "font face \"\": the family must be a name without quotes, backslashes, control \
             characters, or the end of a CSS comment"
                .to_owned(),
            "font face : weight 1001 is outside 1 to 1000".to_owned(),
            "font face : fonts/OFL.txt holds no licence text".to_owned(),
            "font face : the notice must be one line that cannot end a CSS comment".to_owned(),
        ])
    );
    assert_eq!(
        blank.err(),
        Some(vec![
            "font face Mono: the notice must be one line that cannot end a CSS comment".to_owned()
        ])
    );
}

#[test]
fn only_relative_paths_inside_the_directory_are_accepted() {
    // Arrange
    let paths = [
        "fonts/Face.woff2",
        "Face.woff2",
        "",
        "/fonts/Face.woff2",
        "../Face.woff2",
        "./Face.woff2",
    ];

    // Act
    let accepted: Vec<bool> = paths.into_iter().map(is_relative_inside).collect();

    // Assert
    assert_eq!(accepted, [true, true, false, false, false, false]);
}
