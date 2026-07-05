#[cfg(target_os = "linux")]
use std::io;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

use image::ImageFormat;

use super::ClipboardBackend;
use crate::{ClipboardError, RgbaImageData, format, uri};

const IMAGE_PNG_MIME: &str = "image/png";
const TEXT_URI_LIST_MIME: &str = "text/uri-list";
#[cfg(target_os = "linux")]
const WL_PASTE_COMMAND: &str = "wl-paste";
const WL_PASTE_LIST_TYPES_ARGS: &[&str] = &["--list-types"];
#[cfg(target_os = "linux")]
const WL_PASTE_VERSION_ARGS: &[&str] = &["--version"];
const TEXT_MIME_CANDIDATES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "text/plain",
    "UTF8_STRING",
    "TEXT",
    "STRING",
];

pub(crate) struct WaylandClipboard {
    runner: Box<dyn WaylandCommandRunner>,
}

impl WaylandClipboard {
    #[cfg(target_os = "linux")]
    pub(crate) fn new() -> Result<Self, ClipboardError> {
        let clipboard = Self::with_runner(Box::new(SystemWaylandCommandRunner));
        clipboard.ensure_wl_paste_available()?;

        Ok(clipboard)
    }

    fn with_runner(runner: Box<dyn WaylandCommandRunner>) -> Self {
        Self { runner }
    }

    #[cfg(target_os = "linux")]
    fn ensure_wl_paste_available(&self) -> Result<(), ClipboardError> {
        let output = self.run_wl_paste(WL_PASTE_VERSION_ARGS)?;
        if !output.status_success {
            return Err(ClipboardError::Unavailable {
                reason: "`wl-paste --version` failed; install the `wl-clipboard` package"
                    .to_string(),
            });
        }

        Ok(())
    }

    fn read_clipboard_bytes_for_mime(&self, mime_type: &str) -> Result<Vec<u8>, ClipboardError> {
        let args = ["--no-newline", "--type", mime_type];

        self.run_successful(&args, "failed to read Wayland clipboard payload")
    }

    fn available_mime_types(&self) -> Result<Vec<String>, ClipboardError> {
        let stdout = self.run_successful(
            WL_PASTE_LIST_TYPES_ARGS,
            "failed to list Wayland clipboard types",
        )?;
        let mime_types = parse_mime_types(&stdout);
        if mime_types.is_empty() {
            return Err(ClipboardError::ContentUnavailable);
        }

        Ok(mime_types)
    }

    fn run_successful(
        &self,
        args: &[&str],
        context: &'static str,
    ) -> Result<Vec<u8>, ClipboardError> {
        let output = self.run_wl_paste(args)?;
        if !output.status_success {
            return Err(ClipboardError::Backend {
                reason: wl_paste_failure_reason(context, &output.stderr),
            });
        }

        Ok(output.stdout)
    }

    fn run_wl_paste(&self, args: &[&str]) -> Result<WaylandCommandOutput, ClipboardError> {
        let owned_args = args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();

        self.runner.run(&owned_args)
    }
}

impl ClipboardBackend for WaylandClipboard {
    fn read_text(&mut self) -> Result<String, ClipboardError> {
        let mime_types = self.available_mime_types()?;
        let mime_type =
            select_text_mime_type(&mime_types).ok_or(ClipboardError::ContentUnavailable)?;
        let bytes = self.read_clipboard_bytes_for_mime(mime_type)?;

        String::from_utf8(bytes).map_err(|error| {
            ClipboardError::image_conversion(
                "failed to decode Wayland clipboard text as UTF-8",
                error,
            )
        })
    }

    fn read_file_list(&mut self) -> Result<Vec<PathBuf>, ClipboardError> {
        let mime_types = self.available_mime_types()?;
        if !mime_types
            .iter()
            .any(|mime_type| mime_type == TEXT_URI_LIST_MIME)
        {
            return Err(ClipboardError::ContentUnavailable);
        }
        let bytes = self.read_clipboard_bytes_for_mime(TEXT_URI_LIST_MIME)?;
        let paths = uri::paths_from_uri_list(&bytes);
        if paths.is_empty() {
            return Err(ClipboardError::ContentUnavailable);
        }

        Ok(paths)
    }

    fn read_image_rgba(&mut self) -> Result<RgbaImageData, ClipboardError> {
        let mime_types = self.available_mime_types()?;
        if !mime_types
            .iter()
            .any(|mime_type| mime_type == IMAGE_PNG_MIME)
        {
            return Err(ClipboardError::ContentUnavailable);
        }
        let bytes = self.read_clipboard_bytes_for_mime(IMAGE_PNG_MIME)?;

        format::decode_image_rgba(&bytes, ImageFormat::Png)
    }
}

struct WaylandCommandOutput {
    status_success: bool,
    stderr: Vec<u8>,
    stdout: Vec<u8>,
}

#[cfg_attr(test, mockall::automock)]
trait WaylandCommandRunner {
    fn run(&self, args: &[String]) -> Result<WaylandCommandOutput, ClipboardError>;
}

#[cfg(target_os = "linux")]
struct SystemWaylandCommandRunner;

#[cfg(target_os = "linux")]
impl WaylandCommandRunner for SystemWaylandCommandRunner {
    fn run(&self, args: &[String]) -> Result<WaylandCommandOutput, ClipboardError> {
        let output = Command::new(WL_PASTE_COMMAND)
            .args(args)
            .output()
            .map_err(map_wl_paste_spawn_error)?;

        Ok(WaylandCommandOutput {
            status_success: output.status.success(),
            stderr: output.stderr,
            stdout: output.stdout,
        })
    }
}

#[cfg(target_os = "linux")]
fn map_wl_paste_spawn_error(error: io::Error) -> ClipboardError {
    if error.kind() == io::ErrorKind::NotFound {
        return ClipboardError::Unavailable {
            reason: "Wayland clipboard image paste requires `wl-paste`; install the \
                     `wl-clipboard` package"
                .to_string(),
        };
    }

    ClipboardError::backend("failed to run `wl-paste`", error)
}

fn parse_mime_types(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn select_text_mime_type(mime_types: &[String]) -> Option<&str> {
    TEXT_MIME_CANDIDATES
        .iter()
        .find(|candidate| mime_types.iter().any(|mime_type| mime_type == **candidate))
        .copied()
}

fn wl_paste_failure_reason(context: &str, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    if stderr.is_empty() {
        return format!("{context}: `wl-paste` exited unsuccessfully");
    }

    format!("{context}: {stderr}")
}

#[cfg(test)]
mod tests {
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    use mockall::predicate;

    use super::*;

    #[test]
    fn test_read_image_rgba_reads_wayland_png_payload() {
        // Arrange
        let mut runner = MockWaylandCommandRunner::new();
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(args, &["--list-types"])
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: b"image/png\ntext/plain;charset=utf-8\n".to_vec(),
                })
            });
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(args, &["--no-newline", "--type", "image/png"])
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: test_png_bytes(),
                })
            });
        let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

        // Act
        let image_data = clipboard
            .read_image_rgba()
            .expect("mocked PNG payload should decode");

        // Assert
        assert_eq!(image_data.width, 1);
        assert_eq!(image_data.height, 1);
        assert_eq!(image_data.rgba_bytes, vec![255, 0, 0, 255]);
    }

    #[test]
    fn test_read_file_list_reads_wayland_uri_list_payload() {
        // Arrange
        let mut runner = MockWaylandCommandRunner::new();
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(args, &["--list-types"])
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: b"text/uri-list\ntext/plain;charset=utf-8\n".to_vec(),
                })
            });
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(args, &["--no-newline", "--type", "text/uri-list"])
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: b"file:///tmp/image%201.png\r\n".to_vec(),
                })
            });
        let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

        // Act
        let paths = clipboard
            .read_file_list()
            .expect("mocked URI list should parse");

        // Assert
        assert_eq!(paths, vec![PathBuf::from("/tmp/image 1.png")]);
    }

    #[test]
    fn test_read_text_prefers_utf8_plain_text_payload() {
        // Arrange
        let mut runner = MockWaylandCommandRunner::new();
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(args, &["--list-types"])
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: b"text/html\ntext/plain;charset=utf-8\n".to_vec(),
                })
            });
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(
                    args,
                    &["--no-newline", "--type", "text/plain;charset=utf-8"],
                )
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: b"/tmp/image.png".to_vec(),
                })
            });
        let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

        // Act
        let text = clipboard
            .read_text()
            .expect("mocked text payload should decode");

        // Assert
        assert_eq!(text, "/tmp/image.png");
    }

    #[test]
    fn test_read_image_rgba_reports_content_unavailable_without_png_mime() {
        // Arrange
        let mut runner = MockWaylandCommandRunner::new();
        runner
            .expect_run()
            .once()
            .with(predicate::function(|args: &[String]| {
                args_match(args, &["--list-types"])
            }))
            .returning(|_| {
                Ok(WaylandCommandOutput {
                    status_success: true,
                    stderr: Vec::new(),
                    stdout: b"text/plain;charset=utf-8\n".to_vec(),
                })
            });
        let mut clipboard = WaylandClipboard::with_runner(Box::new(runner));

        // Act
        let result = clipboard.read_image_rgba();

        // Assert
        assert!(matches!(result, Err(ClipboardError::ContentUnavailable)));
    }

    #[test]
    fn test_parse_mime_types_trims_blank_lines() {
        // Arrange
        let stdout = b"\n image/png \n\ntext/plain;charset=utf-8\n";

        // Act
        let mime_types = parse_mime_types(stdout);

        // Assert
        assert_eq!(
            mime_types,
            vec![
                "image/png".to_string(),
                "text/plain;charset=utf-8".to_string()
            ]
        );
    }

    #[test]
    fn test_wl_paste_failure_reason_uses_stderr_when_present() {
        // Arrange
        let stderr = b"compositor does not support data-control\n";

        // Act
        let reason = wl_paste_failure_reason("failed to list Wayland clipboard types", stderr);

        // Assert
        assert_eq!(
            reason,
            "failed to list Wayland clipboard types: compositor does not support data-control"
        );
    }

    fn test_png_bytes() -> Vec<u8> {
        let mut png_bytes = Vec::new();
        PngEncoder::new(&mut png_bytes)
            .write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
            .expect("test PNG should encode");

        png_bytes
    }

    fn args_match(args: &[String], expected: &[&str]) -> bool {
        args.iter().map(String::as_str).eq(expected.iter().copied())
    }
}
