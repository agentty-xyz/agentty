use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use percent_encoding::percent_decode_str;

#[cfg(any(target_os = "linux", test))]
pub(crate) fn paths_from_uri_list(uri_list_bytes: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(uri_list_bytes)
        .lines()
        .filter_map(|line| path_from_file_url_text(line.trim_end_matches('\r')))
        .collect()
}

pub(crate) fn path_from_file_url_text(file_url: &str) -> Option<PathBuf> {
    let file_url = file_url.trim();
    if file_url.is_empty() || file_url.starts_with('#') {
        return None;
    }

    let path_fragment = file_url.strip_prefix("file://")?;
    let path_fragment = path_fragment
        .strip_prefix("localhost")
        .unwrap_or(path_fragment);
    if !path_fragment.starts_with('/') {
        return None;
    }

    let decoded_path = percent_decode_str(path_fragment).collect::<Vec<_>>();

    Some(PathBuf::from(os_string_from_bytes(decoded_path)))
}

#[cfg(unix)]
fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from_vec(bytes)
}

#[cfg(not(unix))]
fn os_string_from_bytes(bytes: Vec<u8>) -> OsString {
    OsString::from(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths_from_uri_list_ignores_comments_and_decodes_file_paths() {
        // Arrange
        let uri_list =
            b"# copied files\r\nfile:///tmp/image%201.png\r\nfile://localhost/tmp/second.png\r\n";

        // Act
        let paths = paths_from_uri_list(uri_list);

        // Assert
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/tmp/image 1.png"),
                PathBuf::from("/tmp/second.png")
            ]
        );
    }

    #[test]
    fn test_path_from_file_url_text_rejects_non_file_and_remote_urls() {
        // Arrange
        let http_url = "https://example.com/image.png";
        let remote_file_url = "file://example.com/tmp/image.png";

        // Act
        let http_path = path_from_file_url_text(http_url);
        let remote_file_path = path_from_file_url_text(remote_file_url);

        // Assert
        assert_eq!(http_path, None);
        assert_eq!(remote_file_path, None);
    }
}
