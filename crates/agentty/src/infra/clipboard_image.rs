//! Clipboard image boundary for prompt-mode pasted attachments.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use ag_clipboard::{Clipboard, ClipboardImage};
use ag_session::MessageImage;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

use crate::infra::clock::Clock;
use crate::infra::fs::{self, FsClient};
use crate::infra::home;

/// Process-wide capture sequence that keeps pasted image paths unique when
/// two captures share an attachment number and clock millisecond.
static NEXT_CLIPBOARD_IMAGE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Boxed async result used by [`ClipboardImageClient`] trait methods.
pub(crate) type ClipboardImageFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Opens one clipboard reader on the blocking capture thread.
type ClipboardOpener =
    Arc<dyn Fn() -> Result<Clipboard, ag_clipboard::ClipboardError> + Send + Sync>;

/// Typed error returned by clipboard image capture and persistence operations.
///
/// Wraps clipboard access, filesystem, image encoding, and validation failures
/// so callers can distinguish error categories without parsing opaque strings.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ClipboardError {
    /// Clipboard access is not available on this system.
    #[error("Clipboard is unavailable: {reason}")]
    Unavailable {
        /// Human-readable reason from the clipboard backend.
        reason: String,
    },

    /// Clipboard does not contain image data, a copied PNG file, or a
    /// recognizable PNG path.
    #[error("Clipboard does not contain an image")]
    NoImage,

    /// A referenced PNG path from clipboard text does not exist on disk.
    #[error("Clipboard PNG path does not exist")]
    PngPathNotFound,

    /// A parent directory for the clipboard image is missing from the path.
    #[error("Missing clipboard image directory")]
    MissingDirectory,

    /// A filesystem operation during image persistence failed.
    #[error("{context}: {source}")]
    Persist {
        /// Human-readable operation label.
        context: &'static str,
        /// Underlying filesystem-boundary error.
        source: fs::FsError,
    },

    /// The image encoding or buffer-save operation failed.
    #[error("Failed to write pasted image PNG: {0}")]
    ImageEncode(image::ImageError),

    /// The persisted image path could not be resolved to an absolute path.
    #[error("Failed to resolve pasted image path: {0}")]
    PathResolve(fs::FsError),

    /// The session identifier is empty.
    #[error("Session id is missing for clipboard image temp storage")]
    EmptySessionId,

    /// The system clock returned a pre-Unix-epoch timestamp.
    #[error("System clock is before the Unix epoch: {0}")]
    SystemClock(std::time::SystemTimeError),

    /// The background image capture task panicked or was cancelled.
    #[error("Clipboard image task failed: {0}")]
    TaskJoin(tokio::task::JoinError),
}

/// Persisted clipboard image metadata used by prompt-mode attachment flows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PersistedClipboardImage {
    /// PNG file written under `AGENTTY_ROOT/tmp/<session-id>/images/`.
    pub(crate) local_image_path: PathBuf,
}

/// Async boundary for capturing and persisting one pasted clipboard image.
///
/// Production uses [`RealClipboardImageClient`], while tests can inject
/// `MockClipboardImageClient` through [`crate::app::AppServices`] to avoid
/// touching the host clipboard or filesystem.
#[cfg_attr(test, mockall::automock)]
pub(crate) trait ClipboardImageClient: Send + Sync {
    /// Captures one clipboard image and persists it under the session temp
    /// image directory.
    ///
    /// # Errors
    /// Returns a [`ClipboardError`] when clipboard access fails, the clipboard
    /// does not expose an image payload, or the PNG cannot be persisted.
    fn persist_clipboard_image(
        &self,
        session_id: String,
        attachment_number: usize,
    ) -> ClipboardImageFuture<Result<PersistedClipboardImage, ClipboardError>>;
}

/// Production [`ClipboardImageClient`] backed by `ag-clipboard`, the injected
/// filesystem boundary, and the injected wall clock.
pub(crate) struct RealClipboardImageClient {
    clock: Arc<dyn Clock>,
    fs_client: Arc<dyn FsClient>,
    open_clipboard: ClipboardOpener,
}

impl RealClipboardImageClient {
    /// Creates a clipboard-image adapter from shared infrastructure
    /// dependencies that reads the system clipboard.
    pub(crate) fn new(clock: Arc<dyn Clock>, fs_client: Arc<dyn FsClient>) -> Self {
        Self::with_clipboard_opener(clock, fs_client, Arc::new(Clipboard::new))
    }

    /// Creates a clipboard-image adapter that reads clipboards produced by
    /// `open_clipboard`.
    fn with_clipboard_opener(
        clock: Arc<dyn Clock>,
        fs_client: Arc<dyn FsClient>,
        open_clipboard: ClipboardOpener,
    ) -> Self {
        Self {
            clock,
            fs_client,
            open_clipboard,
        }
    }
}

impl ClipboardImageClient for RealClipboardImageClient {
    fn persist_clipboard_image(
        &self,
        session_id: String,
        attachment_number: usize,
    ) -> ClipboardImageFuture<Result<PersistedClipboardImage, ClipboardError>> {
        let clock = Arc::clone(&self.clock);
        let fs_client = Arc::clone(&self.fs_client);
        let open_clipboard = Arc::clone(&self.open_clipboard);

        Box::pin(async move {
            persist_clipboard_image(
                &session_id,
                attachment_number,
                fs_client.as_ref(),
                clock.as_ref(),
                open_clipboard,
            )
            .await
        })
    }
}

/// Reads one clipboard image and persists it as a PNG under the session temp
/// image directory.
///
/// # Errors
/// Returns a [`ClipboardError`] when clipboard access fails, the clipboard
/// does not expose an image payload, or the PNG cannot be persisted through
/// the filesystem boundary.
async fn persist_clipboard_image(
    session_id: &str,
    attachment_number: usize,
    fs_client: &dyn FsClient,
    clock: &dyn Clock,
    open_clipboard: ClipboardOpener,
) -> Result<PersistedClipboardImage, ClipboardError> {
    let image_output_path =
        build_clipboard_image_path(session_id, attachment_number, "png", clock)?;
    let clipboard_payload = read_clipboard_payload(open_clipboard).await?;

    persist_clipboard_payload(fs_client, &image_output_path, clipboard_payload).await?;

    Ok(PersistedClipboardImage {
        local_image_path: canonicalize_persisted_image_path(fs_client, &image_output_path).await?,
    })
}

/// Persists one image a remote device sent with a message under the
/// session temp image directory, where pasted images live, and returns its
/// absolute path.
///
/// # Errors
/// Returns a [`ClipboardError`] when the image cannot be written or its path
/// resolved through the filesystem boundary.
pub(crate) async fn persist_message_image(
    fs_client: &dyn FsClient,
    clock: &dyn Clock,
    session_id: &str,
    attachment_number: usize,
    image: MessageImage,
) -> Result<PathBuf, ClipboardError> {
    let image_output_path = build_clipboard_image_path(
        session_id,
        attachment_number,
        image.format().extension(),
        clock,
    )?;
    persist_clipboard_payload(
        fs_client,
        &image_output_path,
        ClipboardPayload::Encoded(image.into_data()),
    )
    .await?;

    canonicalize_persisted_image_path(fs_client, &image_output_path).await
}

/// Normalizes one [`ClipboardError`] into short prompt-mode status text.
#[must_use]
pub(crate) fn normalize_clipboard_image_error(error: &ClipboardError) -> String {
    match error {
        ClipboardError::Unavailable { reason } if reason.contains("wl-paste") => {
            "Wayland clipboard image paste requires wl-paste. Install the wl-clipboard package."
                .to_string()
        }
        ClipboardError::Unavailable { .. } => {
            "Clipboard is unavailable. Try again after granting clipboard access.".to_string()
        }
        ClipboardError::NoImage => "Clipboard does not contain an image.".to_string(),
        ClipboardError::PngPathNotFound => "Clipboard PNG path does not exist.".to_string(),
        ClipboardError::Persist { .. }
        | ClipboardError::ImageEncode(_)
        | ClipboardError::PathResolve(_)
        | ClipboardError::MissingDirectory => {
            "Failed to persist pasted image from the clipboard.".to_string()
        }
        ClipboardError::TaskJoin(_) => "Clipboard image capture failed.".to_string(),
        ClipboardError::EmptySessionId | ClipboardError::SystemClock(_) => error.to_string(),
    }
}

/// Returns the temp directory used for pasted prompt images for one session
/// identifier.
///
/// # Errors
/// Returns [`ClipboardError::EmptySessionId`] when `session_id` is empty.
pub(crate) fn clipboard_image_directory(session_id: &str) -> Result<PathBuf, ClipboardError> {
    let session_id = session_temp_directory_name(session_id)?;
    let agentty_root = home::agentty_home();

    Ok(agentty_root.join("tmp").join(session_id).join("images"))
}

/// Builds a stable unique path, ending in `extension`, for one pasted or
/// received image.
///
/// # Errors
/// Returns an error when the session id cannot be used as a temp directory
/// name.
fn build_clipboard_image_path(
    session_id: &str,
    attachment_number: usize,
    extension: &str,
    clock: &dyn Clock,
) -> Result<PathBuf, ClipboardError> {
    let timestamp_millis = clock
        .now_system_time()
        .duration_since(UNIX_EPOCH)
        .map_err(ClipboardError::SystemClock)?
        .as_millis();
    let sequence = NEXT_CLIPBOARD_IMAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name =
        format!("image-{attachment_number:03}-{timestamp_millis}-{sequence}.{extension}");

    Ok(clipboard_image_directory(session_id)?.join(file_name))
}

/// Returns the directory-name fragment used for one session image temp root.
///
/// # Errors
/// Returns [`ClipboardError::EmptySessionId`] when the session id is empty.
fn session_temp_directory_name(session_id: &str) -> Result<&str, ClipboardError> {
    if session_id.is_empty() {
        return Err(ClipboardError::EmptySessionId);
    }

    Ok(session_id)
}

/// Copies a PNG file path exposed in the clipboard file list into the target
/// image path.
///
/// # Errors
/// Returns an error when clipboard file-list access fails or no copied PNG
/// file is present.
fn clipboard_png_path_from_file_list(clipboard: &mut Clipboard) -> Result<PathBuf, ClipboardError> {
    clipboard
        .read_file_list()
        .map_err(|_| ClipboardError::NoImage)?
        .into_iter()
        .find(|path| is_png_path(path))
        .ok_or(ClipboardError::NoImage)
}

/// Copies a PNG file path exposed as clipboard text into the target image
/// path.
///
/// # Errors
/// Returns an error when clipboard text is unavailable or is not a PNG path.
fn clipboard_png_path_from_text(clipboard: &mut Clipboard) -> Result<PathBuf, ClipboardError> {
    let clipboard_text = clipboard.read_text().map_err(|_| ClipboardError::NoImage)?;
    let source_image_path = PathBuf::from(clipboard_text.trim());

    if !is_png_path(&source_image_path) {
        return Err(ClipboardError::NoImage);
    }

    Ok(source_image_path)
}

/// Returns whether a filesystem path names a PNG image.
fn is_png_path(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
}

/// Reads one clipboard image payload on a blocking thread.
///
/// Sources are tried in order: a copied PNG file, one clipboard image read,
/// and finally a PNG path in clipboard text. An encoded PNG image is kept
/// byte-for-byte, avoiding a decode and re-encode of large screenshots; other
/// image data is re-encoded as PNG.
///
/// # Errors
/// Returns an error when clipboard access fails, image encoding fails, or the
/// clipboard does not contain a copied PNG file, image data, or a PNG file
/// path.
async fn read_clipboard_payload(
    open_clipboard: ClipboardOpener,
) -> Result<ClipboardPayload, ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut clipboard = open_clipboard().map_err(|error| ClipboardError::Unavailable {
            reason: error.to_string(),
        })?;

        if let Ok(source_image_path) = clipboard_png_path_from_file_list(&mut clipboard) {
            Ok(ClipboardPayload::ExistingPngPath(source_image_path))
        } else if let Ok(clipboard_image) = clipboard.read_image() {
            let encoded_png = match clipboard_image {
                ClipboardImage::Png(png_bytes) => png_bytes,
                ClipboardImage::Rgba(image_data) => encode_clipboard_image_to_png(
                    &image_data.rgba_bytes,
                    image_data.width,
                    image_data.height,
                )?,
            };

            Ok(ClipboardPayload::Encoded(encoded_png))
        } else {
            Ok(ClipboardPayload::ExistingPngPath(
                clipboard_png_path_from_text(&mut clipboard)?,
            ))
        }
    })
    .await
    .map_err(ClipboardError::TaskJoin)?
}

/// Resolves one persisted image path to the exact absolute filesystem path
/// that downstream transports should reference.
///
/// # Errors
/// Returns [`ClipboardError::PathResolve`] when the persisted file cannot be
/// resolved from disk.
async fn canonicalize_persisted_image_path(
    fs_client: &dyn FsClient,
    image_output_path: &Path,
) -> Result<PathBuf, ClipboardError> {
    fs_client
        .canonicalize(image_output_path.to_path_buf())
        .await
        .map_err(ClipboardError::PathResolve)
}

/// Image payload to persist: one extracted on the blocking clipboard
/// thread, or one a remote device sent.
enum ClipboardPayload {
    /// Encoded image bytes: a PNG copied from the clipboard or re-encoded
    /// from decoded pixels, or a PNG or JPEG a remote device sent.
    Encoded(Vec<u8>),
    /// Existing PNG file path referenced by clipboard text.
    ExistingPngPath(PathBuf),
}

/// Encodes one clipboard image buffer into PNG bytes.
///
/// # Errors
/// Returns an error when the PNG encoder fails.
fn encode_clipboard_image_to_png(
    image_bytes: &[u8],
    image_width: u32,
    image_height: u32,
) -> Result<Vec<u8>, ClipboardError> {
    let mut encoded_png = Vec::new();

    PngEncoder::new(&mut encoded_png)
        .write_image(
            image_bytes,
            image_width,
            image_height,
            ExtendedColorType::Rgba8,
        )
        .map_err(ClipboardError::ImageEncode)?;

    Ok(encoded_png)
}

/// Persists one clipboard payload through the injected filesystem boundary.
///
/// # Errors
/// Returns an error when directory creation, file reads or writes, or PNG-path
/// validation fails.
async fn persist_clipboard_payload(
    fs_client: &dyn FsClient,
    image_output_path: &Path,
    clipboard_payload: ClipboardPayload,
) -> Result<(), ClipboardError> {
    let image_directory = image_output_path
        .parent()
        .ok_or(ClipboardError::MissingDirectory)?
        .to_path_buf();

    fs_client
        .create_dir_all(image_directory)
        .await
        .map_err(|source| ClipboardError::Persist {
            context: "Failed to create clipboard image directory",
            source,
        })?;

    let image_bytes = match clipboard_payload {
        ClipboardPayload::Encoded(encoded_png) => encoded_png,
        ClipboardPayload::ExistingPngPath(source_image_path) => {
            if !fs_client.is_file(source_image_path.clone()) {
                return Err(ClipboardError::PngPathNotFound);
            }

            fs_client
                .read_file(source_image_path)
                .await
                .map_err(|source| ClipboardError::Persist {
                    context: "Failed to read clipboard PNG file",
                    source,
                })?
        }
    };

    fs_client
        .write_file(image_output_path.to_path_buf(), image_bytes)
        .await
        .map_err(|source| ClipboardError::Persist {
            context: "Failed to write pasted image PNG",
            source,
        })
}

#[cfg(test)]
#[path = "clipboard_image_test.rs"]
mod tests;
