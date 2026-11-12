use std::io::Cursor;
use std::mem;
use std::path::PathBuf;

use image::codecs::png::PngEncoder;
use image::codecs::tiff::TiffEncoder;
use image::{ExtendedColorType, ImageEncoder};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::backend::contract::ClipboardBackend;
use crate::backend::macos::{
    MacosClipboard, PASTEBOARD_TYPE_FILE_URL, PASTEBOARD_TYPE_PNG, PASTEBOARD_TYPE_STRING,
    PASTEBOARD_TYPE_TIFF, new_backend,
};
use crate::error::ClipboardError;

/// Value of `NSPasteboardNameGeneral`, the system clipboard's pasteboard.
const GENERAL_PASTEBOARD_NAME: &str = "Apple CFPasteboard general";
const RED_PIXEL_RGBA: [u8; 4] = [255, 0, 0, 255];

/// One fixture value stored under a pasteboard type.
enum PasteboardValue {
    Data(Vec<u8>),
    Text(&'static str),
}

/// Uniquely named private pasteboard that never touches the user's general
/// pasteboard and is emptied when the test finishes.
struct TestPasteboard {
    pasteboard: Retained<NSPasteboard>,
}

impl TestPasteboard {
    /// Creates a private pasteboard holding one item per entry in `items`.
    fn with_items(items: &[&[(&str, PasteboardValue)]]) -> Self {
        let pasteboard = NSPasteboard::pasteboardWithUniqueName();
        let pasteboard_items = items
            .iter()
            .map(|values| Self::pasteboard_item(values))
            .collect::<Vec<_>>();
        pasteboard.clearContents();
        if !pasteboard_items.is_empty() {
            assert!(
                pasteboard.writeObjects(&NSArray::from_retained_slice(&pasteboard_items)),
                "private test pasteboard should accept fixture items"
            );
        }

        Self { pasteboard }
    }

    fn pasteboard_item(
        values: &[(&str, PasteboardValue)],
    ) -> Retained<ProtocolObject<dyn NSPasteboardWriting>> {
        let pasteboard_item = NSPasteboardItem::new();
        for (pasteboard_type, value) in values {
            let pasteboard_type = NSString::from_str(pasteboard_type);
            let is_stored =
                match value {
                    PasteboardValue::Data(bytes) => pasteboard_item
                        .setData_forType(&NSData::with_bytes(bytes), &pasteboard_type),
                    PasteboardValue::Text(text) => pasteboard_item
                        .setString_forType(&NSString::from_str(text), &pasteboard_type),
                };
            assert!(is_stored, "pasteboard item should store fixture value");
        }

        ProtocolObject::from_retained(pasteboard_item)
    }

    fn clipboard(&self) -> MacosClipboard {
        MacosClipboard::new(self.pasteboard.clone())
    }
}

impl Drop for TestPasteboard {
    fn drop(&mut self) {
        self.pasteboard.clearContents();
    }
}

#[test]
fn test_new_backend_wraps_general_pasteboard_without_reading_it() {
    // Arrange
    let general_pasteboard = NSPasteboard::generalPasteboard();

    // Act
    let backend = new_backend();

    // Assert
    assert_eq!(
        mem::size_of_val(backend.as_ref()),
        mem::size_of::<MacosClipboard>()
    );
    assert_eq!(
        general_pasteboard.name().to_string(),
        GENERAL_PASTEBOARD_NAME
    );
}

#[test]
fn test_read_text_returns_first_plain_text_item() {
    // Arrange
    let test_pasteboard = TestPasteboard::with_items(&[
        &[(PASTEBOARD_TYPE_PNG, PasteboardValue::Data(png_bytes()))],
        &[(
            PASTEBOARD_TYPE_STRING,
            PasteboardValue::Text("clipboard text"),
        )],
    ]);
    let mut clipboard = test_pasteboard.clipboard();

    // Act
    let text = clipboard
        .read_text()
        .expect("private pasteboard text should be read");

    // Assert
    assert_eq!(text, "clipboard text");
}

#[test]
fn test_read_text_reports_content_unavailable_without_text_item() {
    // Arrange
    let test_pasteboard =
        TestPasteboard::with_items(&[&[(PASTEBOARD_TYPE_PNG, PasteboardValue::Data(png_bytes()))]]);
    let mut clipboard = test_pasteboard.clipboard();

    // Act
    let result = clipboard.read_text();

    // Assert
    assert!(matches!(result, Err(ClipboardError::ContentUnavailable)));
}

#[test]
fn test_read_file_list_returns_local_file_urls_from_all_items() {
    // Arrange
    let test_pasteboard = TestPasteboard::with_items(&[
        &[(
            PASTEBOARD_TYPE_FILE_URL,
            PasteboardValue::Text("file:///tmp/image%201.png"),
        )],
        &[(PASTEBOARD_TYPE_STRING, PasteboardValue::Text("not a file"))],
        &[(
            PASTEBOARD_TYPE_FILE_URL,
            PasteboardValue::Text("https://example.com/image.png"),
        )],
        &[(
            PASTEBOARD_TYPE_FILE_URL,
            PasteboardValue::Text("file:///tmp/notes.txt"),
        )],
    ]);
    let mut clipboard = test_pasteboard.clipboard();

    // Act
    let paths = clipboard
        .read_file_list()
        .expect("private pasteboard file URLs should be read");

    // Assert
    assert_eq!(
        paths,
        vec![
            PathBuf::from("/tmp/image 1.png"),
            PathBuf::from("/tmp/notes.txt")
        ]
    );
}

#[test]
fn test_read_file_list_reports_content_unavailable_without_file_urls() {
    // Arrange
    let test_pasteboard = TestPasteboard::with_items(&[&[(
        PASTEBOARD_TYPE_STRING,
        PasteboardValue::Text("/tmp/image.png"),
    )]]);
    let mut clipboard = test_pasteboard.clipboard();

    // Act
    let result = clipboard.read_file_list();

    // Assert
    assert!(matches!(result, Err(ClipboardError::ContentUnavailable)));
}

#[test]
fn test_read_image_rgba_decodes_png_and_tiff_payloads() {
    // Arrange
    let png_pasteboard =
        TestPasteboard::with_items(&[&[(PASTEBOARD_TYPE_PNG, PasteboardValue::Data(png_bytes()))]]);
    let tiff_pasteboard = TestPasteboard::with_items(&[&[(
        PASTEBOARD_TYPE_TIFF,
        PasteboardValue::Data(tiff_bytes()),
    )]]);

    // Act
    let png_image = png_pasteboard
        .clipboard()
        .read_image_rgba()
        .expect("private pasteboard PNG should decode");
    let tiff_image = tiff_pasteboard
        .clipboard()
        .read_image_rgba()
        .expect("private pasteboard TIFF should decode");

    // Assert
    for image_data in [png_image, tiff_image] {
        assert_eq!(image_data.width, 1);
        assert_eq!(image_data.height, 1);
        assert_eq!(image_data.rgba_bytes, RED_PIXEL_RGBA.to_vec());
    }
}

#[test]
fn test_read_image_rgba_reports_content_unavailable_without_image_data() {
    // Arrange
    let test_pasteboard =
        TestPasteboard::with_items(&[&[(PASTEBOARD_TYPE_STRING, PasteboardValue::Text("text"))]]);
    let mut clipboard = test_pasteboard.clipboard();

    // Act
    let result = clipboard.read_image_rgba();

    // Assert
    assert!(matches!(result, Err(ClipboardError::ContentUnavailable)));
}

fn png_bytes() -> Vec<u8> {
    let mut png_bytes = Vec::new();
    PngEncoder::new(&mut png_bytes)
        .write_image(&RED_PIXEL_RGBA, 1, 1, ExtendedColorType::Rgba8)
        .expect("test PNG should encode");

    png_bytes
}

fn tiff_bytes() -> Vec<u8> {
    let mut tiff_bytes = Cursor::new(Vec::new());
    TiffEncoder::new(&mut tiff_bytes)
        .write_image(&RED_PIXEL_RGBA, 1, 1, ExtendedColorType::Rgba8)
        .expect("test TIFF should encode");

    tiff_bytes.into_inner()
}
