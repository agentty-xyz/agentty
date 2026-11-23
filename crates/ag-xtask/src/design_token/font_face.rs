//! The font stylesheet: one `@font-face` rule per self-hosted face, with the
//! WOFF2 file embedded as a `data:` URI and its copyright and licence notice
//! in a comment, so the stylesheet loads nothing from a path or the network
//! and names its licence wherever it is copied.

use std::path::{Component, Path};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::design_token::token::FontFace;

/// Signature that starts every WOFF2 file.
const WOFF2_SIGNATURE: &[u8] = b"wOF2";

/// A face whose files were read and checked.
pub(super) struct Embedded<'face> {
    face: &'face FontFace,
    font: Vec<u8>,
}

impl<'face> Embedded<'face> {
    /// Checks a face, its WOFF2 file, and the licence text it names.
    ///
    /// # Errors
    /// Returns every reason the face cannot be embedded.
    pub(super) fn new(
        face: &'face FontFace,
        font: Vec<u8>,
        license: &str,
    ) -> Result<Self, Vec<String>> {
        let name = &face.family;
        let mut errors = Vec::new();
        if name.is_empty()
            || name.contains("*/")
            || name
                .chars()
                .any(|char| matches!(char, '"' | '\\') || char.is_control())
        {
            errors.push(format!(
                "font face {name:?}: the family must be a name without quotes, backslashes, \
                 control characters, or the end of a CSS comment"
            ));
        }
        if !(1..=1000).contains(&face.weight) {
            errors.push(format!(
                "font face {name}: weight {} is outside 1 to 1000",
                face.weight
            ));
        }
        if !font.starts_with(WOFF2_SIGNATURE) {
            errors.push(format!(
                "font face {name}: {} is not a WOFF2 file",
                face.source
            ));
        }
        if license.trim().is_empty() {
            errors.push(format!(
                "font face {name}: {} holds no licence text",
                face.license
            ));
        }
        if face.notice.trim().is_empty() || face.notice.contains("*/") || face.notice.contains('\n')
        {
            errors.push(format!(
                "font face {name}: the notice must be one line that cannot end a CSS comment"
            ));
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        Ok(Self { face, font })
    }
}

/// Returns whether `path` stays inside the directory it is relative to.
pub(super) fn is_relative_inside(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Renders `faces` after `header`, each notice before its rule. The notice
/// comment starts with `/*!`, which minifiers and compressed Sass output
/// keep.
pub(super) fn stylesheet(header: &str, faces: &[Embedded<'_>]) -> String {
    let rules: Vec<String> = faces
        .iter()
        .map(|embedded| {
            let face = embedded.face;
            format!(
                "\n/*! {}: {} */\n@font-face {{\n  font-family: \"{}\";\n  font-style: {};\n  \
                 font-weight: {};\n  font-display: swap;\n  src: \
                 url(\"data:font/woff2;base64,{}\") format(\"woff2\");\n}}\n",
                face.family,
                face.notice.trim(),
                face.family,
                face.style.css(),
                face.weight,
                STANDARD.encode(&embedded.font),
            )
        })
        .collect();

    format!("{header}\n{}", rules.concat())
}

#[cfg(test)]
#[path = "font_face_test.rs"]
mod tests;
