#[path = "support_test.rs"]
mod support;

use std::path::PathBuf;
use std::process::Command;
use std::{fs, io};

use tempfile::tempdir;

use super::{main, output_dir, print_file_preview, run};

#[test]
fn creates_all_five_proof_formats() {
    // Arrange
    let directory = tempdir().expect("proof directory");
    let mut output = Vec::new();

    // Act
    run(directory.path(), &mut output).expect("proof pipeline");

    // Assert
    for name in [
        "proof.txt",
        "proof_strip.png",
        "proof.gif",
        "proof.html",
        "proof.xml",
    ] {
        assert!(
            fs::metadata(directory.path().join(name))
                .expect("proof artifact")
                .len()
                > 0
        );
    }
    assert!(
        String::from_utf8(output)
            .expect("UTF-8 output")
            .contains("All five proof formats generated")
    );
}

#[test]
fn resolves_explicit_and_default_output_directories() {
    // Arrange / Act
    let explicit = output_dir(["custom".to_string()].into_iter());
    let default = output_dir(std::iter::empty());

    // Assert
    assert_eq!(explicit, PathBuf::from("custom"));
    assert_eq!(default, PathBuf::from("testty_proof_output"));
}

#[test]
fn previews_short_files_and_reports_truncated_lines() {
    // Arrange
    let directory = tempdir().expect("preview directory");
    let path = directory.path().join("preview.txt");
    fs::write(&path, "first\nsecond\nthird\n").expect("preview input");
    let mut full = Vec::new();
    let mut truncated = Vec::new();

    // Act
    print_file_preview(&path, 3, &mut full).expect("full preview");
    print_file_preview(&path, 1, &mut truncated).expect("short preview");

    // Assert
    assert_eq!(
        String::from_utf8(full).expect("UTF-8"),
        "\n  first\n  second\n  third\n"
    );
    assert_eq!(
        String::from_utf8(truncated).expect("UTF-8"),
        "\n  first\n  ... (2 more lines)\n"
    );
}

#[test]
fn reports_filesystem_and_output_failures() {
    // Arrange
    let directory = tempdir().expect("output directory");
    let file = directory.path().join("file");
    fs::write(&file, "content").expect("file fixture");
    let mut output = io::Cursor::new([]);

    // Act
    let invalid_directory = run(&file, &mut Vec::new());
    let failed_output = run(directory.path(), &mut output);
    let missing_preview = print_file_preview(&file.join("missing"), 1, &mut Vec::new());
    let failed_preview = print_file_preview(&file, 1, &mut output);

    // Assert
    assert!(invalid_directory.is_err());
    assert!(failed_output.is_err());
    assert!(missing_preview.is_err());
    assert!(failed_preview.is_err());
}

#[test]
fn entry_point_creates_artifacts_in_an_isolated_process() {
    // Arrange
    const CHILD_MARKER: &str = "TESTTY_PROOF_EXAMPLE_CHILD";
    if std::env::var_os(CHILD_MARKER).is_some() {
        // Act
        let result = main();

        // Assert
        assert!(result.is_ok());
        return;
    }
    let directory = tempdir().expect("isolated working directory");
    let test_name = "tests::entry_point_creates_artifacts_in_an_isolated_process";

    // Act
    let child = Command::new(std::env::current_exe().expect("test executable"))
        .arg(test_name)
        .arg("--exact")
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .current_dir(directory.path())
        .output()
        .expect("example child");

    // Assert
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(directory.path().join(test_name).join("proof.xml").is_file());
}

#[test]
fn propagates_disconnections_at_every_output_write() {
    // Arrange
    let directory = tempdir().expect("proof directory");

    // Act / Assert
    support::assert_output_failures(|output| run(directory.path(), output));
}

#[test]
fn previews_propagate_late_output_disconnections() {
    // Arrange
    let directory = tempdir().expect("preview directory");
    let path = directory.path().join("preview.txt");
    fs::write(&path, "first\nsecond\nthird\n").expect("preview input");

    // Act / Assert
    support::assert_output_failures(|output| print_file_preview(&path, 1, output));
}
