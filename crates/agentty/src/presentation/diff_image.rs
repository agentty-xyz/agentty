//! Decoded first-frame rasters for before/after image diff previews.

use std::ffi::OsStr;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use image::{DynamicImage, ImageReader, ImageResult, Limits, imageops};

/// Repository file extensions previewed as images in the diff view.
const IMAGE_EXTENSIONS: [&str; 2] = ["gif", "png"];

/// Largest retained raster edge in pixels. Larger images are downscaled once
/// after decoding so per-frame half-block sampling stays cheap.
const MAX_RETAINED_EDGE: u32 = 320;

/// Largest decoded image edge in pixels accepted before allocation.
const MAX_DECODED_EDGE: u32 = 16_384;

/// Largest decoder allocation in bytes accepted for one preview image.
const MAX_DECODE_ALLOCATION: u64 = 256 * 1024 * 1024;

/// Returns whether `path` names a file type previewed as an image.
#[must_use]
pub fn is_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|image_extension| extension.eq_ignore_ascii_case(image_extension))
        })
}

/// Decoded, size-bounded RGBA raster for one side of an image diff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffImage {
    /// Retained raster height in pixels.
    pub height: u32,
    /// Source image height in pixels before downscaling.
    pub original_height: u32,
    /// Source image width in pixels before downscaling.
    pub original_width: u32,
    /// Row-major RGBA pixels of the retained raster.
    pub pixels: Arc<[[u8; 4]]>,
    /// Retained raster width in pixels.
    pub width: u32,
}

impl DiffImage {
    /// Decodes PNG or GIF bytes, keeping only the first animation frame and
    /// downscaling it to at most [`MAX_RETAINED_EDGE`] pixels per edge.
    ///
    /// # Errors
    /// Returns a user-facing message when the format is unsupported, the
    /// image exceeds decoder limits, or the data is corrupt.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let decoded = Self::decode_first_frame(bytes)
            .map_err(|error| format!("Cannot decode image: {error}"))?;
        let original_width = decoded.width();
        let original_height = decoded.height();
        let (width, height) = Self::retained_size(original_width, original_height);
        let mut raster = decoded.into_rgba8();
        if (width, height) != (original_width, original_height) {
            raster = imageops::thumbnail(&raster, width, height);
        }

        Ok(Self {
            height,
            original_height,
            original_width,
            pixels: raster.pixels().map(|pixel| pixel.0).collect(),
            width,
        })
    }

    /// Returns the RGBA pixel at one retained-raster coordinate.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let index = u64::from(y) * u64::from(self.width) + u64::from(x);

        usize::try_from(index)
            .ok()
            .and_then(|index| self.pixels.get(index))
            .copied()
            .unwrap_or_default()
    }

    /// Decodes the first frame with bounded dimensions and allocation.
    fn decode_first_frame(bytes: &[u8]) -> ImageResult<DynamicImage> {
        let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
        let mut limits = Limits::default();
        limits.max_alloc = Some(MAX_DECODE_ALLOCATION);
        limits.max_image_height = Some(MAX_DECODED_EDGE);
        limits.max_image_width = Some(MAX_DECODED_EDGE);
        reader.limits(limits);

        reader.decode()
    }

    /// Returns the aspect-preserving raster size within
    /// [`MAX_RETAINED_EDGE`], keeping each edge at least one pixel.
    fn retained_size(width: u32, height: u32) -> (u32, u32) {
        let longest_edge = width.max(height);
        if longest_edge <= MAX_RETAINED_EDGE {
            return (width, height);
        }

        let scale = |edge: u32| {
            let scaled = u64::from(edge) * u64::from(MAX_RETAINED_EDGE) / u64::from(longest_edge);

            u32::try_from(scaled).unwrap_or(MAX_RETAINED_EDGE).max(1)
        };

        (scale(width), scale(height))
    }
}

/// One side of an image diff preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffImageSide {
    /// The side does not exist, such as the old side of an added file.
    Absent,
    /// The decoded first frame.
    Image(DiffImage),
    /// The side exists but cannot be shown, with a user-facing reason.
    Unavailable(String),
}

impl DiffImageSide {
    /// Classifies one bounded file or blob read and decodes present bytes.
    #[must_use]
    pub fn from_read(result: Result<ag_git::FileBytes, String>) -> Self {
        match result {
            Ok(ag_git::FileBytes::Bytes(bytes)) => match DiffImage::decode(&bytes) {
                Ok(image) => Self::Image(image),
                Err(error) => Self::Unavailable(error),
            },
            Ok(ag_git::FileBytes::Missing) => Self::Absent,
            Ok(ag_git::FileBytes::TooLarge) => {
                Self::Unavailable("Image too large to preview".to_string())
            }
            Err(error) => Self::Unavailable(error),
        }
    }
}

/// Old and new sides of one changed image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffImagePair {
    /// Post-change worktree image.
    pub after: DiffImageSide,
    /// Pre-change image from the diff's old blob.
    pub before: DiffImageSide,
}

/// Source of the pre-change side of a changed image, read from its diff
/// header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffImageBefore {
    /// The image did not exist before the change.
    Absent,
    /// The pre-change image is stored in this git blob.
    Blob(String),
    /// The change kept the content, such as a pure rename or a mode-only
    /// change, so the pre-change image equals the post-change one.
    Unchanged,
}

#[cfg(test)]
#[path = "diff_image_test.rs"]
mod tests;
