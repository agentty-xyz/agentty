//! SHA-256 checksums in the `sha256sum` text format.

use sha2::{Digest, Sha256};

/// Returns the lowercase hexadecimal SHA-256 digest of `contents`.
pub(crate) fn sha256(contents: &[u8]) -> String {
    hex::encode(Sha256::digest(contents))
}

/// Returns the `sha256sum` line for `file` with hexadecimal `digest`,
/// `<digest>  <file>` and a newline.
pub(crate) fn line(digest: &str, file: &str) -> String {
    format!("{digest}  {file}\n")
}

/// Returns whether `checksum_file`, the contents of a `sha256sum`-compatible
/// checksum file, records `digest` as its first field.
pub(crate) fn records(checksum_file: &[u8], digest: &str) -> bool {
    String::from_utf8_lossy(checksum_file)
        .split_whitespace()
        .next()
        .is_some_and(|recorded| recorded.eq_ignore_ascii_case(digest))
}

#[cfg(test)]
#[path = "checksum_test.rs"]
mod tests;
