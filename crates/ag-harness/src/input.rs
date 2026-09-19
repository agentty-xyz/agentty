//! Ordered, bounded text and image input for one model turn.

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::ModelMessage;
use crate::write_journal::content_hash;

const DATA_URL_PREFIX: &str = "data:";
const DATA_URL_SEPARATOR: &str = ";base64,";
const TEXT_BLOCK_SEPARATOR: &str = "\n\n";

/// Ordered user content for one turn, shared by one-shot and durable entry
/// points.
///
/// Text-only input keeps the existing text contract: it is normalized to one
/// text message, ordered text blocks joined by a blank line, so stored
/// history and host-request fingerprints stay identical to plain-string
/// prompts. Image-bearing input preserves its exact block order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnInput {
    blocks: Vec<InputBlock>,
}

impl TurnInput {
    /// Maximum deterministic encoded size of one input: text bytes plus each
    /// image's base64 data-URL length. This bounds request construction; it
    /// is not an exact provider payload size.
    pub const MAX_ENCODED_BYTES: usize = 48 * 1024 * 1024;
    /// Maximum number of image blocks in one input.
    pub const MAX_IMAGES: usize = 8;
    /// Maximum total raw image bytes across one input.
    pub const MAX_TOTAL_IMAGE_BYTES: usize = 32 * 1024 * 1024;

    /// Creates text-only input from one text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            blocks: vec![InputBlock::Text(text.into())],
        }
    }

    /// Creates input from ordered blocks, validating every image bound.
    ///
    /// # Errors
    /// Returns [`TurnInputError`] when the image count, aggregate image
    /// bytes, or deterministic encoded size exceeds its limit.
    pub fn from_blocks(blocks: Vec<InputBlock>) -> Result<Self, TurnInputError> {
        let images = blocks
            .iter()
            .filter(|block| matches!(block, InputBlock::Image(_)))
            .count();
        if images > Self::MAX_IMAGES {
            return Err(TurnInputError::TooManyImages {
                limit: Self::MAX_IMAGES,
            });
        }
        let mut image_bytes = 0_usize;
        for block in &blocks {
            if let InputBlock::Image(image) = block {
                image_bytes = image_bytes.saturating_add(image.bytes().len());
            }
            if image_bytes > Self::MAX_TOTAL_IMAGE_BYTES {
                return Err(TurnInputError::ImagesTooLarge {
                    limit: Self::MAX_TOTAL_IMAGE_BYTES,
                });
            }
        }
        let input = Self { blocks };
        if input.retained_bytes() > Self::MAX_ENCODED_BYTES {
            return Err(TurnInputError::EncodedInputTooLarge {
                limit: Self::MAX_ENCODED_BYTES,
            });
        }

        Ok(input)
    }

    /// Returns the ordered content blocks.
    pub fn blocks(&self) -> &[InputBlock] {
        &self.blocks
    }

    /// Returns whether any block carries image content.
    pub fn has_images(&self) -> bool {
        self.blocks
            .iter()
            .any(|block| matches!(block, InputBlock::Image(_)))
    }

    /// Returns the text blocks joined by a blank line, without image content.
    pub fn joined_text(&self) -> String {
        let mut joined = String::new();
        for block in &self.blocks {
            let InputBlock::Text(text) = block else {
                continue;
            };
            if !joined.is_empty() {
                joined.push_str(TEXT_BLOCK_SEPARATOR);
            }
            joined.push_str(text);
        }

        joined
    }

    /// Converts this input into its canonical user message.
    ///
    /// Text-only input normalizes to [`ModelMessage::User`]; image-bearing
    /// input keeps its exact blocks in [`ModelMessage::UserInput`]. Stores
    /// persist exactly this message for the current turn.
    pub fn into_user_message(self) -> ModelMessage {
        if self.has_images() {
            ModelMessage::UserInput(self)
        } else {
            ModelMessage::User(self.joined_text())
        }
    }

    /// Returns signature-only input covering every supported media type, for
    /// asking an adapter whether it accepts image history without a stored
    /// payload.
    pub(crate) fn image_probe() -> Self {
        let blocks = [ImageMediaType::Jpeg, ImageMediaType::Png]
            .into_iter()
            .map(|media_type| {
                InputBlock::Image(ImageContent {
                    bytes: media_type.signature().into(),
                    media_type,
                })
            })
            .collect();

        Self { blocks }
    }

    /// Returns the deterministic encoded size: text bytes plus each image's
    /// base64 data-URL length, matching what history replay sends.
    pub(crate) fn retained_bytes(&self) -> usize {
        self.blocks.iter().fold(0, |total, block| {
            total.saturating_add(match block {
                InputBlock::Image(image) => image.encoded_data_url_bytes(),
                InputBlock::Text(text) => text.len(),
            })
        })
    }
}

impl From<&str> for TurnInput {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

impl From<String> for TurnInput {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

/// One ordered content block of a [`TurnInput`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InputBlock {
    /// Bounded, host-supplied image content.
    Image(ImageContent),
    /// One text segment.
    Text(String),
}

/// Validated host-supplied image bytes with a declared media type.
///
/// Validation checks the container signature and byte bounds only; it does
/// not decode the image or verify that the full payload is well formed.
#[derive(Clone, Eq, PartialEq)]
pub struct ImageContent {
    bytes: Arc<[u8]>,
    media_type: ImageMediaType,
}

impl ImageContent {
    /// Maximum raw bytes for one image.
    pub const MAX_BYTES: usize = 10 * 1024 * 1024;

    /// Creates validated image content.
    ///
    /// # Errors
    /// Returns [`TurnInputError`] when the bytes are empty, exceed
    /// [`Self::MAX_BYTES`], or do not start with the declared media type's
    /// container signature.
    pub fn new(media_type: ImageMediaType, bytes: Vec<u8>) -> Result<Self, TurnInputError> {
        if bytes.len() > Self::MAX_BYTES {
            return Err(TurnInputError::ImageTooLarge {
                limit: Self::MAX_BYTES,
            });
        }

        Self::from_persisted(media_type, bytes)
    }

    /// Returns the raw image bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the validated media type.
    pub fn media_type(&self) -> ImageMediaType {
        self.media_type
    }

    /// Returns the deterministic base64 data-URL length for this image.
    pub fn encoded_data_url_bytes(&self) -> usize {
        DATA_URL_PREFIX
            .len()
            .saturating_add(self.media_type.as_str().len())
            .saturating_add(DATA_URL_SEPARATOR.len())
            .saturating_add(base64_len(self.bytes.len()))
    }

    /// Restores image content checking integrity only, so lowering
    /// [`Self::MAX_BYTES`] never makes accepted history unreadable.
    pub(crate) fn from_persisted(
        media_type: ImageMediaType,
        bytes: Vec<u8>,
    ) -> Result<Self, TurnInputError> {
        if bytes.is_empty() {
            return Err(TurnInputError::EmptyImage);
        }
        if !media_type.matches_signature(&bytes) {
            return Err(TurnInputError::SignatureMismatch {
                media_type: media_type.as_str(),
            });
        }

        Ok(Self {
            bytes: bytes.into(),
            media_type,
        })
    }

    pub(crate) fn to_data_url(&self) -> String {
        format!(
            "{DATA_URL_PREFIX}{}{DATA_URL_SEPARATOR}{}",
            self.media_type.as_str(),
            BASE64_STANDARD.encode(&self.bytes)
        )
    }

    pub(crate) fn content_digest(&self) -> String {
        content_hash(&self.bytes)
    }
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

/// Supported image media type for [`ImageContent`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ImageMediaType {
    /// A JPEG image.
    Jpeg,
    /// A PNG image.
    Png,
}

impl ImageMediaType {
    /// Returns the IANA media type identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }

    pub(crate) fn from_media_type(media_type: &str) -> Option<Self> {
        match media_type {
            "image/jpeg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            _ => None,
        }
    }

    fn matches_signature(self, bytes: &[u8]) -> bool {
        bytes.starts_with(self.signature())
    }

    const fn signature(self) -> &'static [u8] {
        match self {
            Self::Jpeg => &[0xFF, 0xD8, 0xFF],
            Self::Png => &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        }
    }
}

/// Invalid or out-of-bounds turn input rejected before any execution.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum TurnInputError {
    /// Image content must not be empty.
    #[error("image content must not be empty")]
    EmptyImage,
    /// The deterministic encoded input size exceeds its limit.
    #[error("encoded input exceeds the {limit}-byte limit")]
    EncodedInputTooLarge {
        /// Configured encoded-size limit in bytes.
        limit: usize,
    },
    /// One image exceeds the per-image byte limit.
    #[error("image exceeds the {limit}-byte limit")]
    ImageTooLarge {
        /// Configured per-image limit in bytes.
        limit: usize,
    },
    /// The aggregate raw image bytes exceed their limit.
    #[error("input images exceed the aggregate {limit}-byte limit")]
    ImagesTooLarge {
        /// Configured aggregate limit in bytes.
        limit: usize,
    },
    /// The image bytes do not match the declared media type's signature.
    #[error("image bytes do not match the declared {media_type} signature")]
    SignatureMismatch {
        /// Declared media type identifier.
        media_type: &'static str,
    },
    /// The input contains more images than the per-input limit.
    #[error("input exceeds the {limit}-image limit")]
    TooManyImages {
        /// Configured image-count limit.
        limit: usize,
    },
}

/// Versioned storage representation of one image-bearing input.
#[derive(Deserialize, Serialize)]
pub(crate) struct StoredTurnInput {
    blocks: Vec<StoredInputBlock>,
    version: u32,
}

impl StoredTurnInput {
    const VERSION: u32 = 1;

    /// Restores accepted input without reapplying new-input bounds, so
    /// lowering a published limit never makes stored history unreadable.
    pub(crate) fn into_input(self) -> Result<TurnInput, StoredInputError> {
        if self.version != Self::VERSION {
            return Err(StoredInputError::Version {
                version: self.version,
            });
        }
        let blocks = self
            .blocks
            .into_iter()
            .map(StoredInputBlock::into_block)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(TurnInput { blocks })
    }
}

impl From<&TurnInput> for StoredTurnInput {
    fn from(input: &TurnInput) -> Self {
        let blocks = input
            .blocks()
            .iter()
            .map(|block| match block {
                InputBlock::Image(image) => StoredInputBlock::Image {
                    bytes: BASE64_STANDARD.encode(image.bytes()),
                    media_type: image.media_type().as_str().to_string(),
                },
                InputBlock::Text(text) => StoredInputBlock::Text { text: text.clone() },
            })
            .collect();

        Self {
            blocks,
            version: Self::VERSION,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredInputBlock {
    Image { bytes: String, media_type: String },
    Text { text: String },
}

impl StoredInputBlock {
    fn into_block(self) -> Result<InputBlock, StoredInputError> {
        match self {
            Self::Image { bytes, media_type } => {
                let media_type = ImageMediaType::from_media_type(&media_type)
                    .ok_or(StoredInputError::MediaType { media_type })?;
                let bytes =
                    BASE64_STANDARD
                        .decode(bytes)
                        .map_err(|error| StoredInputError::Base64 {
                            reason: error.to_string(),
                        })?;

                Ok(InputBlock::Image(ImageContent::from_persisted(
                    media_type, bytes,
                )?))
            }
            Self::Text { text } => Ok(InputBlock::Text(text)),
        }
    }
}

/// Invalid stored user-input payload.
#[derive(Debug, Error)]
pub(crate) enum StoredInputError {
    /// The stored base64 image content cannot be decoded.
    #[error("invalid stored image base64: {reason}")]
    Base64 {
        /// Decoder diagnostic without image content.
        reason: String,
    },
    /// The stored image content fails integrity validation.
    #[error(transparent)]
    Input(#[from] TurnInputError),
    /// The stored media type is not supported.
    #[error("unsupported stored image media type `{media_type}`")]
    MediaType {
        /// Stored media type identifier.
        media_type: String,
    },
    /// The stored payload uses an unknown codec version.
    #[error("unsupported stored input version {version}")]
    Version {
        /// Stored codec version.
        version: u32,
    },
}

fn base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3).saturating_mul(4)
}

#[cfg(test)]
#[path = "input_test.rs"]
mod tests;
