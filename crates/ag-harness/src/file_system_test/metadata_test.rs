use std::ffi::OsStr;
#[cfg(any(target_os = "android", target_os = "linux"))]
use std::ffi::OsString;
use std::io;
#[cfg(target_vendor = "apple")]
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use crate::file_system::{FileSystem as _, LocalFileSystem};

#[tokio::test]
async fn local_file_system_replaces_expected_content_and_preserves_access_metadata() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let nested = repository.path().join("nested");
    std::fs::create_dir(&nested).expect("nested directory should be created");
    let path = nested.join("script.sh");
    std::fs::write(&path, b"old\n").expect("fixture should be written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750))
        .expect("fixture mode should be set");
    let original_file = std::fs::File::open(&path).expect("fixture should open");
    rustix::fs::fsetxattr(
        &original_file,
        "user.ag-harness-test",
        b"preserved",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("fixture xattr should be set");
    let original_metadata = original_file
        .metadata()
        .expect("fixture metadata should read");
    let file_system = LocalFileSystem;

    // Act
    file_system
        .replace_beneath(
            repository.path(),
            Path::new("nested/script.sh"),
            Some(b"old\n".to_vec()),
            b"new\n".to_vec(),
        )
        .await
        .expect("existing file should be replaced");

    // Assert
    assert_eq!(
        std::fs::read(&path).expect("file should be readable"),
        b"new\n"
    );
    let updated_file = std::fs::File::open(&path).expect("updated file should open");
    let updated_metadata = updated_file
        .metadata()
        .expect("updated metadata should read");
    assert_eq!(updated_metadata.permissions().mode() & 0o777, 0o750);
    assert_eq!(updated_metadata.uid(), original_metadata.uid());
    assert_eq!(updated_metadata.gid(), original_metadata.gid());
    let mut xattr = [0_u8; 16];
    let xattr_length = rustix::fs::fgetxattr(&updated_file, "user.ag-harness-test", &mut xattr)
        .expect("updated xattr should read");
    assert_eq!(&xattr[..xattr_length], b"preserved");
}

#[cfg(target_vendor = "apple")]
#[test]
fn local_file_system_detects_apple_metadata_added_after_snapshot() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let path = directory.path().join("source");
    std::fs::write(&path, b"source").expect("source should be written");
    let source = std::fs::File::open(path).expect("source should open");
    let mut expected =
        LocalFileSystem::apple_metadata_snapshot(&source).expect("metadata should snapshot");
    rustix::fs::fsetxattr(
        &source,
        "user.ag-harness-added",
        b"added",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("source xattr should be added");

    // Act
    let error = LocalFileSystem::verify_apple_metadata(&mut expected, &source, &source)
        .expect_err("added source metadata should reject the snapshot");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[cfg(target_vendor = "apple")]
#[test]
fn local_file_system_compares_complete_metadata_archives() {
    // Arrange
    let mut left = tempfile::tempfile().expect("left archive should open");
    let mut different = tempfile::tempfile().expect("different archive should open");
    let mut longer = tempfile::tempfile().expect("longer archive should open");
    left.write_all(b"left").expect("left archive should write");
    different
        .write_all(b"diff")
        .expect("different archive should write");
    longer
        .write_all(b"longer")
        .expect("longer archive should write");

    // Act
    let content_matches = LocalFileSystem::files_match(&mut left, &mut different)
        .expect("equal-length archives should compare");
    let length_matches = LocalFileSystem::files_match(&mut left, &mut longer)
        .expect("different-length archives should compare");

    // Assert
    assert!(!content_matches);
    assert!(!length_matches);
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[test]
fn local_file_system_removes_destination_only_extended_attributes() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let source_path = directory.path().join("source");
    let destination_path = directory.path().join("destination");
    std::fs::write(&source_path, b"source").expect("source should be written");
    std::fs::write(&destination_path, b"destination").expect("destination should be written");
    let source = std::fs::File::open(source_path).expect("source should open");
    let destination = std::fs::File::open(destination_path).expect("destination should open");
    rustix::fs::fsetxattr(
        &destination,
        "user.ag-harness-extra",
        b"remove",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("destination xattr should be set");

    // Act
    LocalFileSystem::copy_metadata(&source, &destination)
        .expect("source metadata should be copied");

    // Assert
    assert_eq!(
        LocalFileSystem::extended_attribute_names(&destination)
            .expect("destination xattrs should list"),
        Vec::<OsString>::new()
    );
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[test]
fn local_file_system_rejects_mismatched_copied_extended_attributes() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let source_path = directory.path().join("source");
    let destination_path = directory.path().join("destination");
    std::fs::write(&source_path, b"source").expect("source should be written");
    std::fs::write(&destination_path, b"destination").expect("destination should be written");
    let source = std::fs::File::open(source_path).expect("source should open");
    let destination = std::fs::File::open(destination_path).expect("destination should open");
    rustix::fs::fsetxattr(
        &source,
        "user.ag-harness-test",
        b"source",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("source xattr should be set");
    rustix::fs::fsetxattr(
        &destination,
        "user.ag-harness-test",
        b"destination",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("destination xattr should be set");
    let expected =
        LocalFileSystem::access_metadata(&source).expect("source metadata should snapshot");

    // Act
    let error = LocalFileSystem::verify_copied_metadata(&source, &destination, &expected)
        .expect_err("different xattr values should fail verification");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[test]
fn local_file_system_detects_extended_attributes_added_after_snapshot() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory should be created");
    let source_path = directory.path().join("source");
    let destination_path = directory.path().join("destination");
    std::fs::write(&source_path, b"source").expect("source should be written");
    std::fs::write(&destination_path, b"destination").expect("destination should be written");
    let source = std::fs::File::open(source_path).expect("source should open");
    let destination = std::fs::File::open(destination_path).expect("destination should open");
    let expected =
        LocalFileSystem::access_metadata(&source).expect("source metadata should snapshot");
    rustix::fs::fsetxattr(
        &source,
        "user.ag-harness-added",
        b"added",
        rustix::fs::XattrFlags::empty(),
    )
    .expect("source xattr should be added");

    // Act
    let error = LocalFileSystem::verify_copied_metadata(&source, &destination, &expected)
        .expect_err("added source xattr should fail verification");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn local_file_system_refreshes_target_mode_before_exchange() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let prepared_path = repository.path().join(".prepared");
    std::fs::write(&target_path, b"expected").expect("target should be written");
    std::fs::set_permissions(&target_path, std::fs::Permissions::from_mode(0o750))
        .expect("target mode should be set");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    std::fs::set_permissions(&prepared_path, std::fs::Permissions::from_mode(0o600))
        .expect("prepared mode should be set");
    let prepared_file = std::fs::OpenOptions::new()
        .write(true)
        .open(&prepared_path)
        .expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_prepared = true;

    // Act
    LocalFileSystem::install_update(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".prepared"),
        b"expected",
        &mut remove_prepared,
    )
    .expect("prepared file should be installed");

    // Assert
    assert!(!remove_prepared);
    assert_eq!(
        std::fs::read(&target_path).expect("target should read"),
        b"model change"
    );
    assert_eq!(
        std::fs::metadata(&target_path)
            .expect("target metadata should read")
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(!prepared_path.exists());
}

#[test]
fn local_file_system_rolls_back_failed_mode_installation() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let prepared_path = repository.path().join(".prepared");
    std::fs::write(&target_path, b"expected").expect("target should be written");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    let prepared_file = std::fs::OpenOptions::new()
        .write(true)
        .open(&prepared_path)
        .expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_prepared = true;

    // Act
    let error = LocalFileSystem::install_update_with_metadata(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".prepared"),
        b"expected",
        &mut remove_prepared,
        |_, _| Err(io::Error::other("injected metadata failure")),
    )
    .expect_err("failed metadata installation should roll back");

    // Assert
    assert_ne!(error.kind(), io::ErrorKind::NotFound);
    assert!(remove_prepared);
    assert_eq!(
        std::fs::read(&target_path).expect("target should be readable"),
        b"expected"
    );
    assert_eq!(
        std::fs::read(&prepared_path).expect("prepared file should be readable"),
        b"model change"
    );
}

#[test]
fn local_file_system_preserves_recovery_when_metadata_rollback_fails() {
    // Arrange
    let repository = tempfile::tempdir().expect("repository should be created");
    let target_path = repository.path().join("output.txt");
    let prepared_path = repository.path().join(".prepared");
    std::fs::write(&target_path, b"expected").expect("target should be written");
    std::fs::write(&prepared_path, b"model change").expect("prepared file should be written");
    let prepared_file = std::fs::OpenOptions::new()
        .write(true)
        .open(&prepared_path)
        .expect("prepared file should open");
    let (parent, file_name) =
        LocalFileSystem::open_parent_beneath(repository.path(), Path::new("output.txt"))
            .expect("target parent should open");
    let mut remove_prepared = true;

    // Act
    let error = LocalFileSystem::install_update_with_metadata(
        &parent.descriptor,
        &prepared_file,
        &file_name,
        OsStr::new(".prepared"),
        b"expected",
        &mut remove_prepared,
        |_, _| {
            std::fs::remove_file(&target_path)?;

            Err(io::Error::other("injected metadata failure"))
        },
    )
    .expect_err("failed rollback should preserve the recovery file");

    // Assert
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(!remove_prepared);
    assert!(!target_path.exists());
    assert_eq!(
        std::fs::read(prepared_path).expect("original target should remain recoverable"),
        b"expected"
    );
}
