use image::ImageFormat;

use crate::{ClipboardError, RgbaImageData};

pub(crate) fn decode_image_rgba(
    image_bytes: &[u8],
    image_format: ImageFormat,
) -> Result<RgbaImageData, ClipboardError> {
    let decoded_image = image::load_from_memory_with_format(image_bytes, image_format)
        .map_err(|error| {
            ClipboardError::image_conversion("failed to decode clipboard image", error)
        })?
        .into_rgba8();
    let (width, height) = decoded_image.dimensions();

    Ok(RgbaImageData {
        height,
        rgba_bytes: decoded_image.into_raw(),
        width,
    })
}

#[cfg(test)]
mod tests {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};

    use super::*;

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
        let image_data =
            decode_image_rgba(&png_bytes, ImageFormat::Png).expect("PNG should decode");

        // Assert
        assert_eq!(image_data.width, 2);
        assert_eq!(image_data.height, 1);
        assert_eq!(image_data.rgba_bytes, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }
}
