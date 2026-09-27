//! Bounded, ordered content supplied to a model.

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use thiserror::Error;

/// One content part in a user message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputBlock {
    /// Text content.
    Text(String),
    /// An encoded image.
    Image(ImageContent),
}

/// Image media type accepted by the current chat adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageMediaType {
    /// JPEG image.
    Jpeg,
    /// PNG image.
    Png,
}

impl ImageMediaType {
    fn mime_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    fn signature(self) -> &'static [u8] {
        match self {
            Self::Jpeg => &[0xFF, 0xD8, 0xFF],
            Self::Png => &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        }
    }
}

/// Image bytes sent as a data URL to compatible providers.
#[derive(Clone, Eq, PartialEq)]
pub struct ImageContent {
    media_type: ImageMediaType,
    bytes: Arc<[u8]>,
}

impl fmt::Debug for ImageContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImageContent")
            .field("bytes", &self.bytes.len())
            .field("media_type", &self.media_type)
            .finish()
    }
}

impl ImageContent {
    /// Maximum raw bytes for one image.
    pub const MAX_BYTES: usize = 10 * 1024 * 1024;

    /// Validates an image's size and container signature.
    ///
    /// # Errors
    /// Returns an error when the bytes are empty, oversized, or have a
    /// mismatched signature.
    pub fn new(media_type: ImageMediaType, bytes: Vec<u8>) -> Result<Self, InputError> {
        Self::validate(media_type, &bytes)?;

        Ok(Self {
            media_type,
            bytes: bytes.into(),
        })
    }

    /// Validates and shares image bytes already retained by the caller.
    ///
    /// # Errors
    /// Returns an error when the bytes are empty, oversized, or have a
    /// mismatched signature.
    pub fn from_shared(media_type: ImageMediaType, bytes: Arc<[u8]>) -> Result<Self, InputError> {
        Self::validate(media_type, &bytes)?;

        Ok(Self { media_type, bytes })
    }

    /// Returns the raw image bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn validate(media_type: ImageMediaType, bytes: &[u8]) -> Result<(), InputError> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(InputError::ImageTooLarge);
        }
        if bytes.is_empty() || !bytes.starts_with(media_type.signature()) {
            return Err(InputError::InvalidImage);
        }

        Ok(())
    }

    pub(crate) fn to_data_url(&self) -> String {
        format!(
            "data:{};base64,{}",
            self.media_type.mime_type(),
            STANDARD.encode(&self.bytes)
        )
    }
}

/// Ordered user content for one message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnInput {
    blocks: Vec<InputBlock>,
}

impl TurnInput {
    /// Maximum encoded content bytes.
    pub const MAX_ENCODED_BYTES: usize = 48 * 1024 * 1024;
    /// Maximum image blocks in one message.
    pub const MAX_IMAGES: usize = 8;
    /// Maximum aggregate raw image bytes.
    pub const MAX_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;

    /// Validates and creates ordered user content.
    ///
    /// # Errors
    /// Returns an error when image count, total image bytes, or encoded content
    /// exceeds its limit.
    pub fn new(blocks: Vec<InputBlock>) -> Result<Self, InputError> {
        let mut images = 0_usize;
        let mut image_bytes = 0_usize;
        let mut encoded_bytes = 0_usize;
        for block in &blocks {
            match block {
                InputBlock::Text(text) => encoded_bytes = encoded_bytes.saturating_add(text.len()),
                InputBlock::Image(image) => {
                    images += 1;
                    image_bytes = image_bytes.saturating_add(image.bytes.len());
                    encoded_bytes = encoded_bytes.saturating_add(
                        5 + image.media_type.mime_type().len()
                            + 8
                            + image.bytes.len().div_ceil(3).saturating_mul(4),
                    );
                }
            }
        }
        if images > Self::MAX_IMAGES {
            return Err(InputError::TooManyImages);
        }
        if image_bytes > Self::MAX_TOTAL_IMAGE_BYTES {
            return Err(InputError::ImagesTooLarge);
        }
        if encoded_bytes > Self::MAX_ENCODED_BYTES {
            return Err(InputError::EncodedInputTooLarge);
        }

        Ok(Self { blocks })
    }

    /// Returns content parts in request order.
    pub fn blocks(&self) -> &[InputBlock] {
        &self.blocks
    }

    /// Returns whether the content contains an image.
    pub fn has_images(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, InputBlock::Image(_)))
    }
}

/// Invalid or oversized user content.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InputError {
    /// Image bytes are empty or do not match the declared media type.
    #[error("image bytes do not match the declared media type")]
    InvalidImage,
    /// One image exceeds the byte limit.
    #[error("image exceeds the byte limit")]
    ImageTooLarge,
    /// Too many image blocks are present.
    #[error("input exceeds the image count limit")]
    TooManyImages,
    /// Total raw image bytes exceed their limit.
    #[error("input images exceed the aggregate byte limit")]
    ImagesTooLarge,
    /// Encoded content exceeds its limit.
    #[error("encoded input exceeds the byte limit")]
    EncodedInputTooLarge,
}

#[cfg(test)]
#[path = "input_test.rs"]
mod tests;
