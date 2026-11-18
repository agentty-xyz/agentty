//! Checksum formatting unit tests.

use crate::checksum;

/// SHA-256 of `hello world`.
const HELLO_WORLD: &str = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

#[test]
fn sha256_lines_use_the_sha256sum_format() {
    // Act
    let digest = checksum::sha256(b"hello world");
    let line = checksum::line(&digest, "demo.tar.gz");

    // Assert
    assert_eq!(digest, HELLO_WORLD);
    assert_eq!(line, format!("{HELLO_WORLD}  demo.tar.gz\n"));
}

#[test]
fn records_compares_the_first_field_of_a_checksum_file() {
    // Arrange
    let cases = [
        (format!("{HELLO_WORLD}  demo.tar.gz\n"), true),
        (format!("{} *demo.tar.gz", HELLO_WORLD.to_uppercase()), true),
        (format!("{HELLO_WORLD}0  demo.tar.gz\n"), false),
        ("0000  demo.tar.gz\n".to_string(), false),
        (String::new(), false),
    ];

    for (contents, expected) in cases {
        // Act
        let recorded = checksum::records(contents.as_bytes(), HELLO_WORLD);

        // Assert
        assert_eq!(recorded, expected, "{contents:?}");
    }
}
