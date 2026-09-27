use crate::input::{ImageContent, ImageMediaType, InputBlock, InputError, TurnInput};

fn png_image(extra_bytes: usize) -> ImageContent {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.resize(bytes.len() + extra_bytes, 0x11);
    ImageContent::new(ImageMediaType::Png, bytes).expect("valid PNG signature")
}

#[test]
fn rejects_invalid_or_oversized_images() {
    // Arrange
    let invalid = vec![0, 1, 2, 3];
    let oversized = vec![0; ImageContent::MAX_BYTES + 1];

    // Act / Assert
    assert_eq!(
        ImageContent::new(ImageMediaType::Png, vec![]),
        Err(InputError::InvalidImage)
    );
    assert_eq!(
        ImageContent::new(ImageMediaType::Png, invalid),
        Err(InputError::InvalidImage)
    );
    assert_eq!(
        ImageContent::new(ImageMediaType::Png, oversized),
        Err(InputError::ImageTooLarge)
    );
}

#[test]
fn bounds_image_count_and_encoded_input_size() {
    // Arrange
    let image = InputBlock::Image(png_image(1));

    // Act / Assert
    assert_eq!(
        TurnInput::new(vec![image; TurnInput::MAX_IMAGES + 1]),
        Err(InputError::TooManyImages),
    );
    assert_eq!(
        TurnInput::new(vec![InputBlock::Text(
            "x".repeat(TurnInput::MAX_ENCODED_BYTES + 1)
        )]),
        Err(InputError::EncodedInputTooLarge),
    );
    let large_image = png_image(9 * 1024 * 1024);
    assert_eq!(
        TurnInput::new(vec![InputBlock::Image(large_image); 4]),
        Err(InputError::ImagesTooLarge),
    );
}

#[test]
fn serializes_validated_image_as_data_url() {
    // Arrange
    let image = ImageContent::new(
        ImageMediaType::Jpeg,
        vec![0xFF, 0xD8, 0xFF, 0xE0, 0x22, 0x22],
    )
    .expect("valid JPEG signature");

    // Act
    let data_url = image.to_data_url();

    // Assert
    assert_eq!(data_url, "data:image/jpeg;base64,/9j/4CIi");
    assert_eq!(
        format!("{image:?}"),
        "ImageContent { bytes: 6, media_type: Jpeg }"
    );
}
