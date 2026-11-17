//! Clipboard PNG header validation tests.

use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::error::ClipboardError;
use crate::format::validate_png;

#[test]
fn test_validate_png_returns_original_bytes_for_valid_png() {
    // Arrange
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG should encode");

    // Act
    let validated_bytes = validate_png(png_bytes.clone()).expect("PNG header should validate");

    // Assert
    assert_eq!(validated_bytes, png_bytes);
}

#[test]
fn test_validate_png_reports_image_conversion_failure_for_invalid_bytes() {
    // Arrange
    let invalid_png_bytes = b"not a png".to_vec();

    // Act
    let result = validate_png(invalid_png_bytes);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::ImageConversion { reason })
            if reason.starts_with("invalid clipboard PNG header: ")
    ));
}
