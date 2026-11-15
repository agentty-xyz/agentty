/// Image payload read from the system clipboard in one backend request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardImage {
    /// Original encoded PNG bytes, header-checked but not decoded.
    Png(Vec<u8>),
    /// Decoded pixels from a non-PNG source, such as macOS TIFF data.
    Rgba(RgbaImageData),
}

/// Owned RGBA image pixels read from the system clipboard.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaImageData {
    /// Image height in pixels.
    pub height: u32,
    /// Pixel data in RGBA byte order.
    pub rgba_bytes: Vec<u8>,
    /// Image width in pixels.
    pub width: u32,
}
