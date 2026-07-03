use std::path::PathBuf;

use crate::{ClipboardError, RgbaImageData};

pub(crate) trait ClipboardBackend {
    fn read_text(&mut self) -> Result<String, ClipboardError>;

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError>;

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError>;
}
