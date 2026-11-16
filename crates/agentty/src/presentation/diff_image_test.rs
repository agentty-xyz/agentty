//! Image diff decoding, downscaling, and read-classification tests.

use image::codecs::gif::GifEncoder;
use image::codecs::png::PngEncoder;
use image::{Delay, ExtendedColorType, Frame, ImageEncoder, Rgba, RgbaImage};

use super::{DiffImage, DiffImageSide, is_image_path};

/// Encodes one solid-color RGBA PNG.
fn solid_png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
    let raster = RgbaImage::from_pixel(width, height, Rgba(color));
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(raster.as_raw(), width, height, ExtendedColorType::Rgba8)
        .expect("PNG fixture should encode");

    bytes
}

/// Encodes a two-frame GIF whose frames use different solid colors.
fn two_frame_gif(first_color: [u8; 4], second_color: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        let frames = [first_color, second_color].map(|color| {
            Frame::from_parts(
                RgbaImage::from_pixel(4, 2, Rgba(color)),
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            )
        });
        encoder
            .encode_frames(frames)
            .expect("GIF fixture should encode");
    }

    bytes
}

#[test]
fn is_image_path_accepts_png_and_gif_case_insensitively() {
    // Arrange
    let paths = [
        "docs/demo.gif",
        "assets/LOGO.PNG",
        "README.md",
        "Makefile",
        "notes.png.txt",
    ];

    // Act
    let results = paths.map(is_image_path);

    // Assert
    assert_eq!(results, [true, true, false, false, false]);
}

#[test]
fn decode_keeps_small_images_at_native_size() {
    // Arrange
    let bytes = solid_png(3, 2, [10, 20, 30, 255]);

    // Act
    let image = DiffImage::decode(&bytes).expect("PNG should decode");

    // Assert
    assert_eq!((image.width, image.height), (3, 2));
    assert_eq!((image.original_width, image.original_height), (3, 2));
    assert_eq!(image.pixel(2, 1), [10, 20, 30, 255]);
    assert_eq!(image.pixel(3, 2), [0, 0, 0, 0]);
}

#[test]
fn decode_downscales_large_images_preserving_aspect_ratio() {
    // Arrange
    let wide_bytes = solid_png(640, 160, [200, 100, 50, 255]);
    let thin_bytes = solid_png(1000, 1, [200, 100, 50, 255]);

    // Act
    let wide = DiffImage::decode(&wide_bytes).expect("wide PNG should decode");
    let thin = DiffImage::decode(&thin_bytes).expect("thin PNG should decode");

    // Assert
    assert_eq!((wide.width, wide.height), (320, 80));
    assert_eq!((wide.original_width, wide.original_height), (640, 160));
    assert_eq!(wide.pixels.len(), 320 * 80);
    assert_eq!(wide.pixel(100, 40), [200, 100, 50, 255]);
    assert_eq!((thin.width, thin.height), (320, 1));
}

#[test]
fn decode_uses_the_first_gif_frame() {
    // Arrange
    let bytes = two_frame_gif([255, 0, 0, 255], [0, 0, 255, 255]);

    // Act
    let image = DiffImage::decode(&bytes).expect("GIF should decode");

    // Assert
    assert_eq!((image.width, image.height), (4, 2));
    assert_eq!(image.pixel(0, 0), [255, 0, 0, 255]);
}

#[test]
fn decode_rejects_unrecognized_bytes() {
    // Arrange
    let bytes = b"not an image";

    // Act
    let result = DiffImage::decode(bytes);

    // Assert
    assert!(matches!(result, Err(message) if message.starts_with("Cannot decode image")));
}

#[test]
fn from_read_classifies_each_read_outcome() {
    // Arrange
    let png = solid_png(1, 1, [1, 2, 3, 255]);

    // Act
    let decoded = DiffImageSide::from_read(Ok(ag_git::FileBytes::Bytes(png)));
    let corrupt = DiffImageSide::from_read(Ok(ag_git::FileBytes::Bytes(vec![0, 1, 2])));
    let missing = DiffImageSide::from_read(Ok(ag_git::FileBytes::Missing));
    let too_large = DiffImageSide::from_read(Ok(ag_git::FileBytes::TooLarge));
    let failed = DiffImageSide::from_read(Err("git failed".to_string()));

    // Assert
    assert!(matches!(decoded, DiffImageSide::Image(image) if image.pixel(0, 0) == [1, 2, 3, 255]));
    assert!(
        matches!(corrupt, DiffImageSide::Unavailable(message) if message.starts_with("Cannot decode image"))
    );
    assert_eq!(missing, DiffImageSide::Absent);
    assert_eq!(
        too_large,
        DiffImageSide::Unavailable("Image too large to preview".to_string())
    );
    assert_eq!(failed, DiffImageSide::Unavailable("git failed".to_string()));
}
