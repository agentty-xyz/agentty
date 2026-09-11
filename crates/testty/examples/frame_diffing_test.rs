#[path = "support_test.rs"]
mod support;

use std::io;

use super::{main, run};

#[test]
fn reports_identical_changed_and_dashboard_frames() {
    // Arrange
    let mut output = Vec::new();

    // Act
    run(&mut output).expect("showcase output");
    let text = String::from_utf8(output).expect("UTF-8 output");

    // Assert
    assert!(text.contains("Identical: true"));
    assert!(text.contains("Identical: false"));
    assert!(text.contains("Dashboard Refresh"));
    assert!(text.contains(".T.T."));
}

#[test]
fn reports_output_failures() {
    // Arrange
    let mut output = io::Cursor::new([]);

    // Act
    let result = run(&mut output);

    // Assert
    assert_eq!(
        result.expect_err("full output").kind(),
        io::ErrorKind::WriteZero
    );
}

#[test]
fn entry_point_writes_the_showcase() {
    // Arrange / Act
    let result = main();

    // Assert
    assert!(result.is_ok());
}

#[test]
fn propagates_disconnections_at_every_output_write() {
    // Arrange / Act / Assert
    support::assert_output_failures(run);
}
