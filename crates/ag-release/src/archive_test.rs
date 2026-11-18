//! Deterministic archive unit tests.

use std::ffi::OsStr;
use std::io::Read;

use flate2::read::GzDecoder;
use tar::EntryType;

use super::write_gzip;
use crate::archive::{self, Archive, FileMode};
use crate::workspace::test_support::BrokenPipe;

/// One decoded archive entry: path, type, mode, uid, gid, mtime, user name,
/// group name, and contents.
type DecodedEntry = (
    String,
    EntryType,
    u32,
    u64,
    u64,
    u64,
    String,
    String,
    Vec<u8>,
);

/// Returns the demo archive with files added in `order`.
fn demo_archive(order: &[usize]) -> Archive {
    let files = [
        ("demo-app", FileMode::Executable, b"binary".to_vec()),
        ("README.md", FileMode::Regular, b"# demo\n".to_vec()),
        ("docs//guide.md", FileMode::Regular, b"guide\n".to_vec()),
    ];
    let mut archive = Archive::new("demo-app-x86_64-unknown-linux-gnu".to_string());
    for index in order {
        let (path, mode, contents) = files[*index].clone();
        archive.add_file(path, mode, contents).expect("add file");
    }

    archive
}

/// Decodes every entry of a `.tar.gz` archive.
fn decode(archive: &[u8]) -> Vec<DecodedEntry> {
    let mut tar = tar::Archive::new(GzDecoder::new(archive));

    tar.entries()
        .expect("entries")
        .map(|entry| {
            let mut entry = entry.expect("entry");
            let header = entry.header().clone();
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).expect("contents");

            (
                String::from_utf8_lossy(&header.path_bytes()).into_owned(),
                header.entry_type(),
                header.mode().expect("mode"),
                header.uid().expect("uid"),
                header.gid().expect("gid"),
                header.mtime().expect("mtime"),
                String::from_utf8_lossy(header.username_bytes().expect("user name")).into_owned(),
                String::from_utf8_lossy(header.groupname_bytes().expect("group name")).into_owned(),
                contents,
            )
        })
        .collect()
}

#[test]
fn archives_are_byte_identical_for_the_same_inputs_in_any_order() {
    // Arrange
    let first = demo_archive(&[0, 1, 2]);
    let second = demo_archive(&[2, 0, 1]);

    // Act
    let first = first.to_tar_gz(1_700_000_000).expect("first archive");
    let second = second.to_tar_gz(1_700_000_000).expect("second archive");

    // Assert
    assert_eq!(first, second);
    // gzip header: no flags, no timestamp, unknown operating system.
    assert_eq!(first[3..10], [0, 0, 0, 0, 0, 2, 255]);
    let entry = |path: &str, kind, mode, contents: &[u8]| {
        (
            path.to_string(),
            kind,
            mode,
            0,
            0,
            1_700_000_000,
            String::new(),
            String::new(),
            contents.to_vec(),
        )
    };
    let directory = EntryType::Directory;
    let file = EntryType::Regular;
    assert_eq!(
        decode(&first),
        vec![
            entry("demo-app-x86_64-unknown-linux-gnu/", directory, 0o755, b""),
            entry(
                "demo-app-x86_64-unknown-linux-gnu/README.md",
                file,
                0o644,
                b"# demo\n"
            ),
            entry(
                "demo-app-x86_64-unknown-linux-gnu/demo-app",
                file,
                0o755,
                b"binary"
            ),
            entry(
                "demo-app-x86_64-unknown-linux-gnu/docs/",
                directory,
                0o755,
                b""
            ),
            entry(
                "demo-app-x86_64-unknown-linux-gnu/docs/guide.md",
                file,
                0o644,
                b"guide\n"
            ),
        ]
    );
}

#[test]
fn files_only_archives_record_no_directory_entries() {
    // Arrange
    let mut archive = Archive::files_only("package".to_string());
    archive
        .add_file("lib/index.js", FileMode::Regular, b"index\n".to_vec())
        .expect("add file");
    archive
        .add_file("run.js", FileMode::Executable, b"run\n".to_vec())
        .expect("add file");

    // Act
    let tarball = archive.to_tar_gz(0).expect("files-only archive");

    // Assert
    let layout = decode(&tarball)
        .into_iter()
        .map(|entry| (entry.0, entry.1, entry.2))
        .collect::<Vec<_>>();
    assert_eq!(
        layout,
        [
            (
                "package/lib/index.js".to_string(),
                EntryType::Regular,
                0o644
            ),
            ("package/run.js".to_string(), EntryType::Regular, 0o755),
        ]
    );
}

#[test]
fn archives_stamp_every_entry_with_the_given_time() {
    // Arrange
    let archive = demo_archive(&[0]);

    // Act
    let epoch = archive.to_tar_gz(0).expect("epoch archive");
    let later = archive.to_tar_gz(1).expect("later archive");

    // Assert
    assert_ne!(epoch, later);
    assert!(decode(&epoch).iter().all(|entry| entry.5 == 0));
    assert!(decode(&later).iter().all(|entry| entry.5 == 1));
}

#[test]
fn add_file_rejects_paths_that_conflict_with_other_entries() {
    // Arrange
    let paths = [
        "demo-app",
        "/demo-app",
        "demo-app/nested",
        "docs",
        "docs/guide.md/x",
    ];

    for path in paths {
        let mut archive = demo_archive(&[0, 2]);

        // Act
        let added = archive.add_file(path, FileMode::Regular, Vec::new());

        // Assert
        let normalized = path.trim_start_matches('/');
        assert_eq!(
            added,
            Err(format!(
                "archive `demo-app-x86_64-unknown-linux-gnu` cannot hold `{normalized}` because \
                 it conflicts with another entry"
            ))
        );
    }
}

#[test]
fn modification_time_reads_source_date_epoch_or_defaults_to_zero() {
    // Arrange
    let invalid = ["", "-1", "+1", "1.5", "soon", "99999999999999999999"];

    // Act
    let unset = archive::modification_time(None);
    let set = archive::modification_time(Some(OsStr::new("1700000000")));
    let rejected = invalid.map(|value| archive::modification_time(Some(OsStr::new(value))));

    // Assert
    assert_eq!(unset, Ok(0));
    assert_eq!(set, Ok(1_700_000_000));
    for (value, result) in invalid.iter().zip(rejected) {
        assert_eq!(
            result,
            Err(format!(
                "`SOURCE_DATE_EPOCH` must be a nonnegative integer number of seconds, found \
                 `{value}`"
            ))
        );
    }
}

#[test]
fn gzip_round_trips_and_is_deterministic() {
    // Arrange
    let contents = b"source tarball".repeat(64);

    // Act
    let first = archive::gzip(&contents).expect("first gzip");
    let second = archive::gzip(&contents).expect("second gzip");

    // Assert
    assert_eq!(first, second);
    let mut decoded = Vec::new();
    GzDecoder::new(first.as_slice())
        .read_to_end(&mut decoded)
        .expect("decode");
    assert_eq!(decoded, contents);
}

#[test]
fn write_failures_are_reported() {
    // Arrange
    let archive = demo_archive(&[0]);

    // Act
    let tar_gz = archive.write_tar_gz(BrokenPipe, 0).map(|_| ());
    let gzip = write_gzip(BrokenPipe, b"contents").map(|_| ());

    // Assert
    let expected = Err("failed to write an archive: broken pipe".to_string());
    assert_eq!(tar_gz, expected);
    assert_eq!(gzip, expected);
}
