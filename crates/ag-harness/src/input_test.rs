use crate::input::{
    ImageContent, ImageMediaType, InputBlock, StoredInputError, StoredTurnInput, TurnInput,
    TurnInputError,
};
use crate::model::ModelMessage;

pub(crate) fn png_bytes(payload_len: usize) -> Vec<u8> {
    let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.resize(bytes.len() + payload_len, 0x11);

    bytes
}

pub(crate) fn jpeg_bytes(payload_len: usize) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.resize(bytes.len() + payload_len, 0x22);

    bytes
}

pub(crate) fn png_image(payload_len: usize) -> ImageContent {
    ImageContent::new(ImageMediaType::Png, png_bytes(payload_len)).expect("valid PNG image")
}

#[test]
fn text_conversions_normalize_to_one_user_message() {
    // Arrange
    let from_str = TurnInput::from("hello");
    let from_string = TurnInput::from(String::from("hello"));
    let multi = TurnInput::from_blocks(vec![
        InputBlock::Text("first".to_string()),
        InputBlock::Text("second".to_string()),
    ])
    .expect("text blocks");

    // Act / Assert
    assert_eq!(from_str, from_string);
    assert!(!from_str.has_images());
    assert_eq!(from_str.joined_text(), "hello");
    assert_eq!(
        from_str.into_user_message(),
        ModelMessage::User("hello".to_string())
    );
    assert_eq!(multi.joined_text(), "first\n\nsecond");
    assert_eq!(
        multi.into_user_message(),
        ModelMessage::User("first\n\nsecond".to_string())
    );
}

#[test]
fn image_bearing_input_preserves_block_order() {
    // Arrange
    let image = png_image(4);
    let blocks = vec![
        InputBlock::Text("before".to_string()),
        InputBlock::Image(image.clone()),
        InputBlock::Text("after".to_string()),
    ];

    // Act
    let input = TurnInput::from_blocks(blocks.clone()).expect("mixed blocks");

    // Assert
    assert!(input.has_images());
    assert_eq!(input.blocks(), blocks.as_slice());
    assert_eq!(input.joined_text(), "before\n\nafter");
    assert_eq!(
        input.retained_bytes(),
        "before".len() + image.encoded_data_url_bytes() + "after".len()
    );
    assert_eq!(
        input.clone().into_user_message(),
        ModelMessage::UserInput(input)
    );
}

#[test]
fn image_content_validates_bytes_and_signature() {
    // Arrange / Act / Assert
    assert_eq!(
        ImageContent::new(ImageMediaType::Png, Vec::new()),
        Err(TurnInputError::EmptyImage)
    );
    assert_eq!(
        ImageContent::new(ImageMediaType::Png, vec![0x11; ImageContent::MAX_BYTES + 1]),
        Err(TurnInputError::ImageTooLarge {
            limit: ImageContent::MAX_BYTES,
        })
    );
    assert_eq!(
        ImageContent::new(ImageMediaType::Jpeg, png_bytes(4)),
        Err(TurnInputError::SignatureMismatch {
            media_type: "image/jpeg",
        })
    );
    assert_eq!(
        ImageContent::new(ImageMediaType::Png, jpeg_bytes(4)),
        Err(TurnInputError::SignatureMismatch {
            media_type: "image/png",
        })
    );
    let jpeg = ImageContent::new(ImageMediaType::Jpeg, jpeg_bytes(4)).expect("valid JPEG");
    assert_eq!(jpeg.media_type(), ImageMediaType::Jpeg);
    assert_eq!(jpeg.media_type().as_str(), "image/jpeg");
    assert_eq!(jpeg.bytes(), jpeg_bytes(4).as_slice());
}

#[test]
fn from_blocks_enforces_count_aggregate_and_encoded_limits() {
    // Arrange
    let small = InputBlock::Image(png_image(4));
    let large = InputBlock::Image(png_image(ImageContent::MAX_BYTES - 64));

    // Act / Assert
    assert_eq!(
        TurnInput::from_blocks(vec![small; TurnInput::MAX_IMAGES + 1]),
        Err(TurnInputError::TooManyImages {
            limit: TurnInput::MAX_IMAGES,
        })
    );
    assert_eq!(
        TurnInput::from_blocks(vec![large; 4]),
        Err(TurnInputError::ImagesTooLarge {
            limit: TurnInput::MAX_TOTAL_IMAGE_BYTES,
        })
    );
    assert_eq!(
        TurnInput::from_blocks(vec![InputBlock::Text(
            "a".repeat(TurnInput::MAX_ENCODED_BYTES + 1)
        )]),
        Err(TurnInputError::EncodedInputTooLarge {
            limit: TurnInput::MAX_ENCODED_BYTES,
        })
    );
}

#[test]
fn encoded_accounting_matches_data_url_length() {
    // Arrange
    for image in [png_image(5), png_image(6), png_image(7)] {
        // Act / Assert
        assert_eq!(image.encoded_data_url_bytes(), image.to_data_url().len());
    }
}

#[test]
fn data_url_uses_media_type_and_base64_content() {
    // Arrange
    let image = ImageContent::new(ImageMediaType::Jpeg, jpeg_bytes(2)).expect("valid JPEG");

    // Act
    let url = image.to_data_url();

    // Assert
    assert_eq!(url, "data:image/jpeg;base64,/9j/4CIi");
}

#[test]
fn image_debug_excludes_content() {
    // Arrange
    let image = png_image(16);

    // Act
    let debug = format!("{image:?}");

    // Assert
    assert_eq!(
        debug,
        format!(
            "ImageContent {{ bytes: {}, media_type: Png }}",
            image.bytes().len()
        )
    );
}

#[test]
fn stored_input_round_trips_through_json() {
    // Arrange
    let input = TurnInput::from_blocks(vec![
        InputBlock::Text("caption".to_string()),
        InputBlock::Image(png_image(9)),
        InputBlock::Image(
            ImageContent::new(ImageMediaType::Jpeg, jpeg_bytes(3)).expect("valid JPEG"),
        ),
    ])
    .expect("mixed blocks");

    // Act
    let payload = serde_json::to_string(&StoredTurnInput::from(&input)).expect("encode");
    let decoded = serde_json::from_str::<StoredTurnInput>(&payload)
        .expect("decode")
        .into_input()
        .expect("valid stored input");

    // Assert
    assert_eq!(decoded, input);
}

#[test]
fn stored_input_decodes_beyond_current_input_bounds() {
    // Arrange
    let image = png_image(4);
    let blocks = vec![InputBlock::Image(image); TurnInput::MAX_IMAGES + 1];
    let stored = StoredTurnInput::from(&TurnInput {
        blocks: blocks.clone(),
    });
    let payload = serde_json::to_string(&stored).expect("encode");

    // Act
    let decoded = serde_json::from_str::<StoredTurnInput>(&payload)
        .expect("decode")
        .into_input()
        .expect("stored input beyond current bounds");

    // Assert
    assert_eq!(decoded.blocks(), blocks.as_slice());
}

#[test]
fn stored_input_rejects_invalid_payloads() {
    // Arrange
    let version = r#"{"version":2,"blocks":[]}"#;
    let media_type =
        r#"{"version":1,"blocks":[{"kind":"image","media_type":"image/webp","bytes":"aa"}]}"#;
    let bad_base64 =
        r#"{"version":1,"blocks":[{"kind":"image","media_type":"image/png","bytes":"!!"}]}"#;
    let empty_image =
        r#"{"version":1,"blocks":[{"kind":"image","media_type":"image/png","bytes":""}]}"#;

    // Act / Assert
    for (payload, expected) in [
        (version, "unsupported stored input version 2"),
        (
            media_type,
            "unsupported stored image media type `image/webp`",
        ),
        (empty_image, "image content must not be empty"),
    ] {
        let error = serde_json::from_str::<StoredTurnInput>(payload)
            .expect("decode")
            .into_input()
            .expect_err("invalid stored input");
        assert_eq!(error.to_string(), expected);
    }
    let error = serde_json::from_str::<StoredTurnInput>(bad_base64)
        .expect("decode")
        .into_input()
        .expect_err("invalid base64");
    assert!(matches!(error, StoredInputError::Base64 { .. }), "{error}");
}

#[test]
fn input_errors_render_content_free_diagnostics() {
    // Arrange
    let cases = [
        (
            TurnInputError::EmptyImage,
            "image content must not be empty",
        ),
        (
            TurnInputError::EncodedInputTooLarge { limit: 4 },
            "encoded input exceeds the 4-byte limit",
        ),
        (
            TurnInputError::ImageTooLarge { limit: 3 },
            "image exceeds the 3-byte limit",
        ),
        (
            TurnInputError::ImagesTooLarge { limit: 2 },
            "input images exceed the aggregate 2-byte limit",
        ),
        (
            TurnInputError::SignatureMismatch {
                media_type: "image/png",
            },
            "image bytes do not match the declared image/png signature",
        ),
        (
            TurnInputError::TooManyImages { limit: 1 },
            "input exceeds the 1-image limit",
        ),
    ];

    // Act / Assert
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
    }
}
