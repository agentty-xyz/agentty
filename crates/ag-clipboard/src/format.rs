//! Clipboard PNG header validation.

use std::io::Cursor;

use image::codecs::png::PngDecoder;

use crate::ClipboardError;

/// Returns `png_bytes` unchanged after checking the PNG signature and
/// header chunks, without decoding pixel data.
pub(crate) fn validate_png(png_bytes: Vec<u8>) -> Result<Vec<u8>, ClipboardError> {
    PngDecoder::new(Cursor::new(png_bytes.as_slice()))
        .map_err(|error| ClipboardError::image_conversion("invalid clipboard PNG header", error))?;

    Ok(png_bytes)
}

#[cfg(test)]
#[path = "format_test.rs"]
mod tests;
