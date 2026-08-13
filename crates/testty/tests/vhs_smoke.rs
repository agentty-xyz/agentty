//! Native recorder-image smoke test.
//!
//! The publish workflow runs this ignored test inside each candidate E2E
//! image. Normal source-test hooks skip it because it intentionally launches
//! VHS, Chromium, `ttyd`, and `FFmpeg`.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::{AnimationDecoder, DynamicImage};
use testty::feature::{FeatureDemo, GifMode, GifStatus};
use testty::scenario::Scenario;
use testty::session::PtySessionBuilder;

#[test]
#[ignore = "requires the complete native VHS recorder image"]
fn recorder_image_generates_feature_artifacts() {
    const CANONICAL_DIMENSIONS: (u32, u32) = (1600, 800);

    // Arrange
    let output_dir = tempfile::tempdir().expect("create recorder smoke output");
    let scenario = Scenario::new("recorder_smoke")
        .sleep_ms(200)
        .write_text("stty size")
        .press_key("Enter")
        .wait_for_text("24 80", 5_000)
        .capture_labeled("geometry", "Canonical 80 by 24 recorder geometry");
    let shell = Path::new("/bin/sh");
    let builder = PtySessionBuilder::new(shell).size(80, 24);

    // Act
    let result = FeatureDemo::new("recorder_smoke")
        .gif_output_dir(output_dir.path())
        .gif_mode(GifMode::AlwaysGenerate)
        .run(&scenario, builder, shell, &[])
        .expect("run native recorder smoke");

    // Assert
    assert!(
        matches!(&result.gif_status, GifStatus::Generated(_)),
        "recorder did not generate a GIF: {:?}",
        result.gif_status
    );
    let gif_path = result
        .gif_status
        .gif_path()
        .expect("generated status has a GIF path");
    let poster_path = output_dir.path().join("recorder_smoke.png");
    let hash_path = output_dir.path().join(".recorder_smoke.hash");
    assert!(std::fs::metadata(hash_path).is_ok_and(|metadata| metadata.len() > 0));

    let gif = File::open(gif_path).expect("open generated GIF");
    let gif_decoder = GifDecoder::new(BufReader::new(gif)).expect("decode generated GIF header");
    let mut gif_frame_count = 0;
    for frame in gif_decoder.into_frames() {
        let frame = frame.expect("fully decode generated GIF frame");
        assert_eq!(frame.buffer().dimensions(), CANONICAL_DIMENSIONS);
        gif_frame_count += 1;
    }
    assert!(gif_frame_count > 0, "generated GIF must contain a frame");

    let poster = File::open(&poster_path).expect("open generated PNG poster");
    let poster_decoder =
        PngDecoder::new(BufReader::new(poster)).expect("decode generated PNG header");
    let poster = DynamicImage::from_decoder(poster_decoder).expect("fully decode generated PNG");
    assert_eq!((poster.width(), poster.height()), CANONICAL_DIMENSIONS);
}
