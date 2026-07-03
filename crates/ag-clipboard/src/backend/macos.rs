use std::path::PathBuf;

use image::ImageFormat;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::NSPasteboard;
use objc2_foundation::NSString;

use super::ClipboardBackend;
use crate::{ClipboardError, RgbaImageData, format, uri};

const PASTEBOARD_TYPE_FILE_URL: &str = "public.file-url";
const PASTEBOARD_TYPE_PNG: &str = "public.png";
const PASTEBOARD_TYPE_STRING: &str = "public.utf8-plain-text";
const PASTEBOARD_TYPE_TIFF: &str = "public.tiff";

pub(crate) fn new_backend() -> Box<dyn ClipboardBackend> {
    Box::new(MacosClipboard::new())
}

struct MacosClipboard {
    pasteboard: Retained<NSPasteboard>,
}

impl MacosClipboard {
    fn new() -> Self {
        Self {
            pasteboard: NSPasteboard::generalPasteboard(),
        }
    }

    fn read_first_string_for_type(
        &self,
        pasteboard_type: &NSString,
    ) -> Result<String, ClipboardError> {
        autoreleasepool(|_| {
            let pasteboard_items = self
                .pasteboard
                .pasteboardItems()
                .ok_or(ClipboardError::ContentUnavailable)?;

            for pasteboard_item in &pasteboard_items {
                if let Some(text) = pasteboard_item.stringForType(pasteboard_type) {
                    return Ok(text.to_string());
                }
            }

            Err(ClipboardError::ContentUnavailable)
        })
    }

    fn read_data_for_type(&self, pasteboard_type: &NSString) -> Result<Vec<u8>, ClipboardError> {
        autoreleasepool(|_| {
            self.pasteboard
                .dataForType(pasteboard_type)
                .map(|data| data.to_vec())
                .ok_or(ClipboardError::ContentUnavailable)
        })
    }
}

impl ClipboardBackend for MacosClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        let pasteboard_type = NSString::from_str(PASTEBOARD_TYPE_STRING);

        self.read_first_string_for_type(&pasteboard_type)
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        autoreleasepool(|_| {
            let pasteboard_type = NSString::from_str(PASTEBOARD_TYPE_FILE_URL);
            let pasteboard_items = self
                .pasteboard
                .pasteboardItems()
                .ok_or(ClipboardError::ContentUnavailable)?;
            let paths = pasteboard_items
                .iter()
                .filter_map(|pasteboard_item| pasteboard_item.stringForType(&pasteboard_type))
                .filter_map(|file_url| uri::path_from_file_url_text(&file_url.to_string()))
                .collect::<Vec<_>>();

            if paths.is_empty() {
                return Err(ClipboardError::ContentUnavailable);
            }

            Ok(paths)
        })
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        let png_type = NSString::from_str(PASTEBOARD_TYPE_PNG);
        if let Ok(png_bytes) = self.read_data_for_type(&png_type) {
            return format::decode_image_rgba(&png_bytes, ImageFormat::Png);
        }

        let tiff_type = NSString::from_str(PASTEBOARD_TYPE_TIFF);
        let tiff_bytes = self.read_data_for_type(&tiff_type)?;

        format::decode_image_rgba(&tiff_bytes, ImageFormat::Tiff)
    }
}
