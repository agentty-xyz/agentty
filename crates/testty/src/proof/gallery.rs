//! Proof gallery aggregator.
//!
//! [`write_gallery()`] scans a directory for proof artifacts produced by the
//! proof backends (`.txt`, `.png`, `.gif`, `.html`) and writes a single
//! `index.html` that links or embeds each artifact ordered by file name. The
//! gallery provides a one-click entry point for browsing every proof a run
//! produced.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use super::html::escape_html;
use super::report::ProofError;

/// File name of the gallery index written into the scanned directory.
const INDEX_FILE_NAME: &str = "index.html";

/// A recognized proof artifact discovered while scanning a directory.
struct Artifact {
    /// Kind of artifact, used to decide how it is embedded or linked.
    kind: ArtifactKind,
    /// File name of the artifact, relative to the scanned directory.
    name: String,
}

/// Kind of recognized proof artifact, derived from its file extension.
enum ArtifactKind {
    /// An animated capture (`.gif`), embedded as an `<img>` tag.
    Animation,
    /// A self-contained nested report (`.html`), linked via an `<a>` tag.
    Html,
    /// A still frame image (`.png`), embedded as an `<img>` tag.
    Image,
    /// A textual capture (`.txt`), embedded inline as escaped `<pre>` content.
    Text,
}

impl ArtifactKind {
    /// Map a lowercase file extension to its [`ArtifactKind`], if recognized.
    fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "gif" => Some(Self::Animation),
            "html" => Some(Self::Html),
            "png" => Some(Self::Image),
            "txt" => Some(Self::Text),
            _ => None,
        }
    }
}

/// Scan `dir` for proof artifacts and write an aggregated `index.html`.
///
/// Recognized artifacts are `.txt`, `.png`, `.gif`, and `.html` files in
/// `dir`. They are sorted lexicographically by file name, so prefixing each
/// artifact with a zero-padded step number (for example, `01_launched.txt`)
/// makes the gallery reflect run order. Text artifacts are embedded inline,
/// images and animations are embedded as `<img>` tags, and nested HTML reports
/// are linked. The previously written `index.html`, if any, is skipped so the
/// gallery never references itself; re-running regenerates it in place.
///
/// Returns the path to the written `index.html`.
///
/// # Errors
///
/// Returns a [`ProofError`] if the directory cannot be read or the index
/// cannot be written.
pub fn write_gallery(dir: &Path) -> Result<PathBuf, ProofError> {
    let mut artifacts = collect_artifacts(dir)?;
    artifacts.sort_by(|left, right| left.name.cmp(&right.name));

    let html = render_gallery(dir, &artifacts)?;

    let index_path = dir.join(INDEX_FILE_NAME);
    std::fs::write(&index_path, html)?;

    Ok(index_path)
}

/// Collect every recognized proof artifact directly inside `dir`.
///
/// Non-file entries, the gallery's own `index.html`, and files whose extension
/// is not a recognized artifact type are skipped.
fn collect_artifacts(dir: &Path) -> Result<Vec<Artifact>, ProofError> {
    let mut artifacts = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }

        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) if name != INDEX_FILE_NAME => name.to_string(),
            _ => continue,
        };

        let kind = path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .and_then(|extension| ArtifactKind::from_extension(&extension));

        if let Some(kind) = kind {
            artifacts.push(Artifact { kind, name });
        }
    }

    Ok(artifacts)
}

/// Render the gallery document for `artifacts` found in `dir`.
fn render_gallery(dir: &Path, artifacts: &[Artifact]) -> Result<String, ProofError> {
    let mut html = String::with_capacity(1024);

    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str("<title>Proof Gallery</title>\n</head>\n<body>\n");
    html.push_str("<h1>Proof Gallery</h1>\n");

    for artifact in artifacts {
        write_artifact(&mut html, dir, artifact)?;
    }

    html.push_str("</body>\n</html>\n");

    Ok(html)
}

/// Append the gallery section for a single `artifact` to `html`.
///
/// Each section carries a heading with the artifact's file name and embeds or
/// links the artifact based on its [`ArtifactKind`].
fn write_artifact(html: &mut String, dir: &Path, artifact: &Artifact) -> Result<(), ProofError> {
    let escaped_name = escape_html(&artifact.name);

    write!(html, "<section>\n<h2>{escaped_name}</h2>\n")
        .map_err(|error| ProofError::Format(error.to_string()))?;

    match artifact.kind {
        ArtifactKind::Text => {
            let content = std::fs::read_to_string(dir.join(&artifact.name))?;
            writeln!(html, "<pre>{}</pre>", escape_html(&content))
                .map_err(|error| ProofError::Format(error.to_string()))?;
        }
        ArtifactKind::Image | ArtifactKind::Animation => {
            let url = relative_url(&artifact.name);
            writeln!(html, "<img src=\"{url}\" alt=\"{escaped_name}\">")
                .map_err(|error| ProofError::Format(error.to_string()))?;
        }
        ArtifactKind::Html => {
            let url = relative_url(&artifact.name);
            writeln!(html, "<a href=\"{url}\">{escaped_name}</a>")
                .map_err(|error| ProofError::Format(error.to_string()))?;
        }
    }

    html.push_str("</section>\n");

    Ok(())
}

/// Build a safe relative URL for an artifact file `name`.
///
/// HTML escaping alone is not enough for `src`/`href` values: a bare name can
/// be misread as a URL scheme, fragment, or query, so a name such as
/// `javascript:run.html` would resolve to a scheme rather than a sibling file.
/// The name is emitted as an explicit relative path (prefixed with `./`) with
/// every path segment percent-encoded so characters like `:`, `#`, `?`, and
/// spaces stay part of the file reference.
fn relative_url(name: &str) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789ABCDEF";

    let mut url = String::with_capacity(name.len() + 2);
    url.push_str("./");

    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                url.push(byte as char);
            }
            _ => {
                url.push('%');
                url.push(HEX_DIGITS[(byte >> 4) as usize] as char);
                url.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
            }
        }
    }

    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_references_each_artifact() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("01_start.txt"), "first frame text").expect("write txt");
        std::fs::write(dir.join("02_step.png"), b"fake png bytes").expect("write png");
        std::fs::write(dir.join("03_anim.gif"), b"fake gif bytes").expect("write gif");
        std::fs::write(dir.join("04_report.html"), "<html></html>").expect("write html");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        assert_eq!(index_path, dir.join("index.html"));
        assert!(html.contains("01_start.txt"), "missing txt reference");
        assert!(html.contains("02_step.png"), "missing png reference");
        assert!(html.contains("03_anim.gif"), "missing gif reference");
        assert!(html.contains("04_report.html"), "missing html reference");
    }

    #[test]
    fn artifacts_appear_in_run_order() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("02_second.txt"), "second").expect("write second");
        std::fs::write(dir.join("01_first.txt"), "first").expect("write first");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        let first_pos = html.find("01_first.txt").expect("first present");
        let second_pos = html.find("02_second.txt").expect("second present");
        assert!(first_pos < second_pos, "artifacts not in run order");
    }

    #[test]
    fn text_artifact_content_is_embedded_and_escaped() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("01_log.txt"), "value <b> & \"quote\"").expect("write txt");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        assert!(html.contains("value &lt;b&gt; &amp; &quot;quote&quot;"));
    }

    #[test]
    fn existing_index_is_not_referenced() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("index.html"), "<html>stale</html>").expect("write stale index");
        std::fs::write(dir.join("01_start.txt"), "content").expect("write txt");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        assert!(html.contains("01_start.txt"));
        assert!(
            !html.contains("href=\"index.html\""),
            "gallery references itself"
        );
    }

    #[test]
    fn non_artifact_files_are_ignored() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("notes.md"), "ignore me").expect("write md");
        std::fs::write(dir.join("01_start.txt"), "content").expect("write txt");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        assert!(html.contains("01_start.txt"));
        assert!(!html.contains("notes.md"), "non-artifact referenced");
    }

    #[test]
    fn image_links_are_safe_relative_urls() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("01 step#a.png"), b"bytes").expect("write png");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        assert!(
            html.contains("src=\"./01%20step%23a.png\""),
            "src not percent-encoded as a relative url"
        );
    }

    #[test]
    fn html_link_cannot_be_a_javascript_scheme() {
        // Arrange
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
        let dir = temp_dir.path();
        std::fs::write(dir.join("javascript:run.html"), "<html></html>").expect("write html");

        // Act
        let index_path = write_gallery(dir).expect("gallery should be written");
        let html = std::fs::read_to_string(&index_path).expect("read index");

        // Assert
        assert!(
            html.contains("href=\"./javascript%3Arun.html\""),
            "href is not a scheme-safe relative url"
        );
        assert!(
            !html.contains("href=\"javascript:"),
            "href resolves to a javascript scheme"
        );
    }
}
