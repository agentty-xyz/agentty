use crate::proof::gallery::write_gallery;

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
