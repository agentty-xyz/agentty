use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder, ImageFormat};

use crate::error::ClipboardError;
use crate::format::decode_image_rgba;

#[test]
fn test_decode_image_rgba_returns_dimensions_and_rgba_bytes() {
    // Arrange
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(
            &[255, 0, 0, 255, 0, 255, 0, 255],
            2,
            1,
            ExtendedColorType::Rgba8,
        )
        .expect("test PNG should encode");

    // Act
    let image_data = decode_image_rgba(&png_bytes, ImageFormat::Png).expect("PNG should decode");

    // Assert
    assert_eq!(image_data.width, 2);
    assert_eq!(image_data.height, 1);
    assert_eq!(image_data.rgba_bytes, vec![255, 0, 0, 255, 0, 255, 0, 255]);
}

#[test]
fn test_decode_image_rgba_reports_image_conversion_failure_for_invalid_bytes() {
    // Arrange
    let invalid_png_bytes = b"not a png".to_vec();

    // Act
    let result = decode_image_rgba(&invalid_png_bytes, ImageFormat::Png);

    // Assert
    assert!(matches!(
        result,
        Err(ClipboardError::ImageConversion { reason })
            if reason.starts_with("failed to decode clipboard image: ")
    ));
}
