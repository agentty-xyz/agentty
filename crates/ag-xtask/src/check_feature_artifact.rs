use std::collections::BTreeSet;
use std::fs;
use std::io::BufReader;
use std::path::Path;

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;

const FEATURE_HEIGHT: u32 = 800;
const FEATURE_WIDTH: u32 = 1600;

/// Validate the complete published feature-artifact inventory.
///
/// # Errors
///
/// Returns every discovered invariant violation in one actionable report.
pub(crate) fn run(content_dir: &Path, static_dir: &Path) -> Result<(), String> {
    let mut problems = Vec::new();
    let pages = collect_stems(content_dir, "md", false, &mut problems)?;
    let gifs = collect_stems(static_dir, "gif", false, &mut problems)?;
    let posters = collect_stems(static_dir, "png", false, &mut problems)?;
    let hashes = collect_stems(static_dir, "hash", true, &mut problems)?;

    compare_artifact_set("GIF", &pages, &gifs, &mut problems);
    compare_artifact_set("PNG poster", &pages, &posters, &mut problems);
    compare_artifact_set("hash sidecar", &pages, &hashes, &mut problems);

    for name in &pages {
        validate_page(content_dir, name, &mut problems);
        validate_image(static_dir, name, "gif", &mut problems);
        validate_image(static_dir, name, "png", &mut problems);
        validate_hash(static_dir, name, &mut problems);
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "feature artifact validation failed:\n- {}",
            problems.join("\n- ")
        ))
    }
}

/// Collect published artifact stems and report transaction backups separately.
fn collect_stems(
    dir: &Path,
    extension: &str,
    hidden: bool,
    problems: &mut Vec<String>,
) -> Result<BTreeSet<String>, String> {
    let entries =
        fs::read_dir(dir).map_err(|err| format!("failed to read {}: {err}", dir.display()))?;
    let mut stems = BTreeSet::new();

    for entry in entries {
        let path = entry
            .map_err(|err| format!("failed to read an entry in {}: {err}", dir.display()))?
            .path();
        if path.extension().is_none_or(|value| value != extension) {
            continue;
        }

        let raw_stem = utf8_file_stem(&path)?;
        let stem = if hidden {
            raw_stem
                .strip_prefix('.')
                .ok_or_else(|| format!("hash sidecar must start with a dot: {}", path.display()))?
        } else {
            raw_stem
        };
        if extension != "md"
            && let Some(name) = stem
                .strip_suffix(".previous")
                .filter(|name| !name.is_empty())
        {
            problems.push(format!(
                "lingering transaction backup for `{name}`: {}",
                path.display()
            ));

            continue;
        }
        if stem != "_index" {
            stems.insert(stem.to_string());
        }
    }

    Ok(stems)
}

/// Return one artifact stem only when its filename is valid UTF-8.
fn utf8_file_stem(path: &Path) -> Result<&str, String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("non-UTF-8 artifact name: {}", path.display()))
}

/// Report missing and orphaned artifacts relative to feature pages.
fn compare_artifact_set(
    label: &str,
    pages: &BTreeSet<String>,
    artifacts: &BTreeSet<String>,
    problems: &mut Vec<String>,
) {
    for name in pages.difference(artifacts) {
        problems.push(format!("missing {label} for `{name}`"));
    }
    for name in artifacts.difference(pages) {
        problems.push(format!("orphaned {label} for `{name}`"));
    }
}

/// Validate a page's GIF metadata points to its same-named artifact.
fn validate_page(content_dir: &Path, name: &str, problems: &mut Vec<String>) {
    let path = content_dir.join(format!("{name}.md"));
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            problems.push(format!("failed to read {}: {err}", path.display()));

            return;
        }
    };
    let expected = format!("gif = \"{name}.gif\"");
    if !content.lines().any(|line| line.trim() == expected) {
        problems.push(format!("{} must declare `{expected}`", path.display()));
    }
}

/// Fully decode one browser image and validate its dimensions.
fn validate_image(static_dir: &Path, name: &str, extension: &str, problems: &mut Vec<String>) {
    let path = static_dir.join(format!("{name}.{extension}"));
    let dimensions = match decode_image(&path, extension) {
        Ok(dimensions) => dimensions,
        Err(image::ImageError::IoError(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return;
        }
        Err(err) => {
            problems.push(format!("failed to decode {}: {err}", path.display()));

            return;
        }
    };

    validate_dimensions(&path, dimensions, problems);
}

/// Fully decode a GIF animation or one non-animated browser image.
fn decode_image(path: &Path, extension: &str) -> image::ImageResult<Option<(u32, u32)>> {
    if extension != "gif" {
        let image = image::open(path)?;

        return Ok(Some((image.width(), image.height())));
    }

    let file = fs::File::open(path).map_err(image::ImageError::IoError)?;
    let decoder = GifDecoder::new(BufReader::new(file))?;
    let mut dimensions = None;
    for frame in decoder.into_frames() {
        let frame = frame?;
        if dimensions.is_none() {
            dimensions = Some(frame.buffer().dimensions());
        }
    }

    Ok(dimensions)
}

/// Validate one image has the canonical feature-demo dimensions.
fn validate_dimensions(path: &Path, dimensions: Option<(u32, u32)>, problems: &mut Vec<String>) {
    match dimensions {
        Some((FEATURE_WIDTH, FEATURE_HEIGHT)) => {}
        Some((width, height)) => problems.push(format!(
            "{} is {width}x{height}; expected {FEATURE_WIDTH}x{FEATURE_HEIGHT}",
            path.display()
        )),
        None => problems.push(format!(
            "{} is empty or has an invalid image header",
            path.display()
        )),
    }
}

/// Validate a sidecar contains one parseable freshness hash.
fn validate_hash(static_dir: &Path, name: &str, problems: &mut Vec<String>) {
    let path = static_dir.join(format!(".{name}.hash"));
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            problems.push(format!("failed to read {}: {err}", path.display()));

            return;
        }
    };
    if raw.trim().parse::<u64>().is_err() {
        problems.push(format!(
            "{} must contain one unsigned decimal hash",
            path.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::ffi::OsStringExt;

    use super::*;

    fn write_gif_with_corrupt_later_frame(path: &Path) {
        let first = image::Frame::from_parts(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 0, 255])),
            0,
            0,
            image::Delay::from_numer_denom_ms(100, 1),
        );
        let second = image::Frame::from_parts(
            image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 255, 255, 255])),
            0,
            0,
            image::Delay::from_numer_denom_ms(100, 1),
        );
        let mut encoded = Vec::new();
        image::codecs::gif::GifEncoder::new(&mut encoded)
            .encode_frames([first, second])
            .expect("encode two-frame GIF");
        let second_control_extension = encoded
            .windows(3)
            .enumerate()
            .filter_map(|(index, bytes)| (bytes == [0x21, 0xf9, 0x04]).then_some(index))
            .nth(1)
            .expect("find second frame control extension");
        encoded.truncate(second_control_extension + 5);
        fs::write(path, encoded).expect("write GIF with corrupt later frame");
    }

    fn write_valid_feature(content_dir: &Path, static_dir: &Path, name: &str) {
        fs::write(
            content_dir.join(format!("{name}.md")),
            format!("+++\n[extra]\ngif = \"{name}.gif\"\n+++\n"),
        )
        .expect("write page");

        let image = image::RgbaImage::from_pixel(
            FEATURE_WIDTH,
            FEATURE_HEIGHT,
            image::Rgba([0, 0, 0, 255]),
        );
        image
            .save_with_format(
                static_dir.join(format!("{name}.gif")),
                image::ImageFormat::Gif,
            )
            .expect("write GIF");
        image
            .save_with_format(
                static_dir.join(format!("{name}.png")),
                image::ImageFormat::Png,
            )
            .expect("write PNG");
        fs::write(static_dir.join(format!(".{name}.hash")), "42\n").expect("write hash");
    }

    #[test]
    fn accepts_one_complete_feature_artifact_set() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");

        // Act
        let result = run(&content_dir, &static_dir);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn reports_missing_orphaned_and_invalid_artifacts_together() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        fs::write(content_dir.join("broken.md"), "gif = \"wrong.gif\"\n").expect("write page");
        fs::write(static_dir.join("orphan.gif"), b"not a gif").expect("write orphan");

        // Act
        let error = run(&content_dir, &static_dir).expect_err("invalid inventory should fail");

        // Assert
        assert!(error.contains("missing GIF for `broken`"));
        assert!(error.contains("missing PNG poster for `broken`"));
        assert!(error.contains("missing hash sidecar for `broken`"));
        assert!(error.contains("orphaned GIF for `orphan`"));
        assert!(error.contains("must declare `gif = \"broken.gif\"`"));
    }

    #[test]
    fn reports_invalid_dimensions_and_hash() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");
        fs::write(static_dir.join(".demo.hash"), "invalid\n").expect("write invalid hash");
        let oversized_gif = image::RgbaImage::from_pixel(3200, 1600, image::Rgba([0, 0, 0, 255]));
        oversized_gif
            .save_with_format(static_dir.join("demo.gif"), image::ImageFormat::Gif)
            .expect("write oversized GIF");

        // Act
        let error = run(&content_dir, &static_dir).expect_err("invalid artifacts should fail");

        // Assert
        assert!(error.contains("is 3200x1600; expected 1600x800"));
        assert!(error.contains("must contain one unsigned decimal hash"));
    }

    #[test]
    fn reports_lingering_transaction_backups_without_orphan_noise() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");
        fs::write(static_dir.join("demo.previous.gif"), b"previous gif").expect("write GIF backup");
        fs::write(static_dir.join("demo.previous.png"), b"previous poster")
            .expect("write PNG backup");
        fs::write(static_dir.join(".demo.previous.hash"), b"previous hash")
            .expect("write hash backup");

        // Act
        let error =
            run(&content_dir, &static_dir).expect_err("transaction backups should fail validation");

        // Assert
        assert!(error.contains("lingering transaction backup for `demo`"));
        assert!(error.contains("demo.previous.gif"));
        assert!(error.contains("demo.previous.png"));
        assert!(error.contains(".demo.previous.hash"));
        assert!(!error.contains("orphaned GIF for `demo.previous`"));
        assert!(!error.contains("orphaned PNG poster for `demo.previous`"));
        assert!(!error.contains("orphaned hash sidecar for `demo.previous`"));
    }

    #[test]
    fn reports_truncated_images_with_valid_headers() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");
        let mut gif_header = b"GIF89a".to_vec();
        gif_header.extend_from_slice(&1600_u16.to_le_bytes());
        gif_header.extend_from_slice(&800_u16.to_le_bytes());
        fs::write(static_dir.join("demo.gif"), gif_header).expect("write truncated GIF");
        let mut png_header = b"\x89PNG\r\n\x1a\n".to_vec();
        png_header.extend_from_slice(&[0; 8]);
        png_header.extend_from_slice(&FEATURE_WIDTH.to_be_bytes());
        png_header.extend_from_slice(&FEATURE_HEIGHT.to_be_bytes());
        fs::write(static_dir.join("demo.png"), png_header).expect("write truncated PNG");

        // Act
        let error = run(&content_dir, &static_dir).expect_err("truncated images should fail");

        // Assert
        assert!(error.contains("failed to decode"));
        assert!(error.contains("demo.gif"));
        assert!(error.contains("demo.png"));
    }

    #[test]
    fn reports_gif_corruption_after_a_decodable_first_frame() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");
        let gif_path = static_dir.join("demo.gif");
        write_gif_with_corrupt_later_frame(&gif_path);
        assert!(
            image::open(&gif_path).is_ok(),
            "first-frame decoding must accept the regression fixture"
        );

        // Act
        let error = run(&content_dir, &static_dir)
            .expect_err("later-frame corruption should fail validation");

        // Assert
        assert!(error.contains("failed to decode"));
        assert!(error.contains("demo.gif"));
    }

    #[test]
    fn reports_non_file_artifacts_instead_of_treating_them_as_present() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");
        fs::remove_file(static_dir.join("demo.gif")).expect("remove valid GIF");
        fs::create_dir(static_dir.join("demo.gif")).expect("create invalid GIF directory");

        // Act
        let error = run(&content_dir, &static_dir).expect_err("non-file artifact should fail");

        // Assert
        assert!(error.contains("failed to decode"));
        assert!(error.contains("demo.gif"));
    }

    #[cfg(unix)]
    #[test]
    fn reports_non_utf8_artifact_names() {
        // Arrange
        let invalid_name = std::ffi::OsString::from_vec(b"demo\xff.gif".to_vec());
        let path = std::path::PathBuf::from(invalid_name);

        // Act
        let error = utf8_file_stem(&path).expect_err("non-UTF-8 name should fail");

        // Assert
        assert!(error.contains("non-UTF-8 artifact name"));
    }

    #[test]
    fn reports_unreadable_feature_page() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(content_dir.join("demo.md")).expect("create page directory");
        fs::create_dir_all(&static_dir).expect("create static dir");

        // Act
        let error = run(&content_dir, &static_dir).expect_err("unreadable page should fail");

        // Assert
        assert!(error.contains("failed to read"));
        assert!(error.contains("demo.md"));
    }

    #[test]
    fn reports_an_image_without_decoded_dimensions() {
        // Arrange
        let path = Path::new("demo.gif");
        let mut problems = Vec::new();

        // Act
        validate_dimensions(path, None, &mut problems);

        // Assert
        assert_eq!(
            problems,
            ["demo.gif is empty or has an invalid image header"]
        );
    }

    #[test]
    fn reports_unreadable_hash_sidecar() {
        // Arrange
        let temp = tempfile::tempdir().expect("temp dir");
        let content_dir = temp.path().join("content");
        let static_dir = temp.path().join("static");
        fs::create_dir_all(&content_dir).expect("create content dir");
        fs::create_dir_all(&static_dir).expect("create static dir");
        write_valid_feature(&content_dir, &static_dir, "demo");
        let hash_path = static_dir.join(".demo.hash");
        fs::remove_file(&hash_path).expect("remove hash file");
        fs::create_dir(&hash_path).expect("create unreadable hash directory");

        // Act
        let error = run(&content_dir, &static_dir).expect_err("unreadable hash should fail");

        // Assert
        assert!(error.contains("failed to read"));
        assert!(error.contains(".demo.hash"));
    }
}
