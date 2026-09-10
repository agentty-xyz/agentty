use std::path::Path;

use crate::feature::{FeatureDemo, GifMode, GifStatus, Redaction};
use crate::scenario::Scenario;
use crate::session::PtySessionBuilder;
use crate::vhs::VhsTapeSettings;

#[test]
fn feature_demo_builder_sets_metadata() {
    // Arrange / Act
    let demo = FeatureDemo::new("test_feature")
        .title("Test Feature")
        .description("A test description.");

    // Assert
    assert_eq!(demo.meta.name, "test_feature");
    assert_eq!(demo.meta.title, "Test Feature");
    assert_eq!(demo.meta.description, "A test description.");
}

#[test]
fn feature_demo_defaults_title_to_name() {
    // Arrange / Act
    let demo = FeatureDemo::new("my_feature");

    // Assert
    assert_eq!(demo.meta.title, "my_feature");
    assert_eq!(demo.meta.description, "");
}

#[test]
fn feature_demo_defaults_to_feature_demo_gif_settings() {
    // Arrange / Act
    let demo = FeatureDemo::new("settings_check");
    let expected = VhsTapeSettings::feature_demo();

    // Assert
    assert_eq!(demo.gif_settings.width, expected.width);
    assert_eq!(demo.gif_settings.height, expected.height);
    assert_eq!(demo.gif_settings.font_size, expected.font_size);
    assert_eq!(demo.gif_settings.theme, expected.theme);
}

#[test]
fn feature_demo_gif_output_dir_configurable() {
    // Arrange / Act
    let demo = FeatureDemo::new("dir_check").gif_output_dir("/tmp/gifs");

    // Assert
    assert_eq!(demo.gif_output_dir.as_deref(), Some(Path::new("/tmp/gifs")));
}

#[test]
fn feature_demo_run_wires_check_only_vhs_context() {
    // Arrange
    let temp = tempfile::TempDir::new().expect("failed to create temp dir");
    let scenario = Scenario::new("run_context")
        .wait_for_text("ready", 3_000)
        .capture_labeled("ready", "Shell is ready");
    let builder = PtySessionBuilder::new("/bin/sh").args(["-c", "printf 'ready\\n'; sleep 60"]);
    let demo = FeatureDemo::new("run_context")
        .gif_output_dir(temp.path())
        .gif_mode(GifMode::CheckOnly);

    // Act
    let result = demo
        .run(&scenario, builder, Path::new("/bin/true"), &[])
        .expect("feature demo should run");

    // Assert
    assert!(matches!(result.gif_status, GifStatus::Stale { .. }));
    assert!(result.frame.all_text().contains("ready"));
}

#[test]
fn feature_demo_no_gif_dir_means_none() {
    // Arrange / Act
    let demo = FeatureDemo::new("no_gif");

    // Assert
    assert!(demo.gif_output_dir.is_none());
}

#[test]
fn feature_demo_default_mode_is_generate_if_stale() {
    // Arrange / Act
    let demo = FeatureDemo::new("mode_check");

    // Assert
    assert_eq!(demo.gif_mode, GifMode::GenerateIfStale);
}

#[test]
fn feature_demo_gif_mode_configurable() {
    // Arrange / Act
    let demo = FeatureDemo::new("mode_check").gif_mode(GifMode::CheckOnly);

    // Assert
    assert_eq!(demo.gif_mode, GifMode::CheckOnly);
}

#[test]
fn feature_demo_collects_redactions_in_declaration_order() {
    // Arrange / Act
    let demo = FeatureDemo::new("redaction_check")
        .redact(Redaction::hex_after("wt/", 8, "<hash>"))
        .redact(Redaction::hex_after("commit ", 7, "<commit>"));

    // Assert
    let redacted: Vec<String> = demo
        .redactions
        .iter()
        .map(|redaction| redaction.apply("wt/4175e5af commit 9c0b17f"))
        .collect();

    assert_eq!(
        redacted,
        vec![
            "wt/<hash> commit 9c0b17f".to_string(),
            "wt/4175e5af commit <commit>".to_string(),
        ],
    );
}
