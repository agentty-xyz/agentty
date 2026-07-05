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
