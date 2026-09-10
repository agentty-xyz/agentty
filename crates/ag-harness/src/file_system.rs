use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
#[cfg(target_vendor = "apple")]
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::fd::OwnedFd;
#[cfg(any(target_os = "android", target_os = "linux"))]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
#[cfg(target_vendor = "apple")]
use rustix::fs::CopyfileFlags;
use rustix::fs::{AtFlags, FileType, FlockOperation, Mode, OFlags, RenameFlags};
use tokio::io::AsyncRead;

const DIRECTORY_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW);
const FILE_OPEN_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK);
const TEMPORARY_OPEN_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CLOEXEC)
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW);
const DIRECTORY_MODE: Mode = Mode::RWXU
    .union(Mode::RGRP)
    .union(Mode::XGRP)
    .union(Mode::ROTH)
    .union(Mode::XOTH);
const TEMPORARY_MODE: Mode = Mode::RUSR.union(Mode::WUSR);
const UPDATE_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const UPDATE_LOCK_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(target_vendor = "apple")]
const COPYFILE_PACK: CopyfileFlags = CopyfileFlags::from_bits_retain(1 << 22);
#[cfg(any(target_os = "android", target_os = "linux"))]
const XATTR_BUFFER_SIZE: usize = 64 * 1024;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Asynchronous filesystem boundary used by harness tools.
///
/// The harness uses this boundary for diagnostic path resolution,
/// descriptor-relative opening, and stale-safe replacement, keeping repository
/// containment inside the filesystem operation.
#[cfg_attr(test, mockall::automock)]
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Resolves a path to its canonical absolute representation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the path cannot be resolved.
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

    /// Opens a repository-relative file without following symlinks beneath
    /// `root`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when `path` is invalid, traverses a symlink, does
    /// not name a regular file, or cannot be opened for reading.
    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>>;

    /// Safely creates or replaces one repository-relative regular file.
    ///
    /// `expected` is `None` for a create-only operation. For replacement it
    /// contains the exact bytes that must still be present when the prepared
    /// file is atomically exchanged with the target, preventing stale model
    /// context from overwriting newer content.
    /// Replacements preserve the target's ownership and access-control
    /// metadata or fail and restore the original file.
    /// Missing parent directories are created without following symlinks.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when containment cannot be enforced, the target
    /// changed, a create target already exists, or the replacement
    /// cannot be completed.
    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()>;
}

struct ParentDirectory {
    descriptor: OwnedFd,
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct AccessMetadata {
    attributes: Vec<ExtendedAttribute>,
    group: rustix::fs::Gid,
    mode: Mode,
    owner: rustix::fs::Uid,
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct ExtendedAttribute {
    name: OsString,
    value: Vec<u8>,
}

/// Tokio-backed filesystem implementation for local repositories.
pub struct LocalFileSystem;

impl LocalFileSystem {
    fn open_beneath(root: &Path, relative_path: &Path) -> io::Result<std::fs::File> {
        let mut directory =
            rustix::fs::open(root, DIRECTORY_OPEN_FLAGS, Mode::empty()).map_err(io::Error::from)?;
        let components = relative_path
            .components()
            .map(|component| match component {
                Component::Normal(component) => Ok(component),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "read path must be repository-relative",
                )),
            })
            .collect::<io::Result<Vec<&OsStr>>>()?;
        let (file_name, ancestor_components) = components.split_last().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "read path must not be empty")
        })?;

        for component in ancestor_components {
            directory =
                rustix::fs::openat(&directory, *component, DIRECTORY_OPEN_FLAGS, Mode::empty())
                    .map_err(io::Error::from)?;
        }
        let descriptor = rustix::fs::openat(&directory, *file_name, FILE_OPEN_FLAGS, Mode::empty())
            .map_err(io::Error::from)?;
        let metadata = rustix::fs::fstat(&descriptor).map_err(io::Error::from)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "read path must name a regular file",
            ));
        }

        Ok(std::fs::File::from(descriptor))
    }

    fn replace_beneath(
        root: &Path,
        relative_path: &Path,
        expected: Option<&[u8]>,
        content: &[u8],
    ) -> io::Result<()> {
        let (parent, file_name) = Self::open_parent_beneath(root, relative_path)?;
        let temporary_name = Self::temporary_name();
        let descriptor = rustix::fs::openat(
            &parent.descriptor,
            &temporary_name,
            TEMPORARY_OPEN_FLAGS,
            TEMPORARY_MODE,
        )
        .map_err(io::Error::from)?;
        let mut temporary_file = std::fs::File::from(descriptor);
        let mut remove_temporary = true;
        let operation = (|| {
            temporary_file.write_all(content)?;
            temporary_file.sync_all()?;
            if let Some(expected) = expected {
                let update = Self::install_update(
                    &parent.descriptor,
                    &temporary_file,
                    &file_name,
                    &temporary_name,
                    expected,
                    &mut remove_temporary,
                );
                update?;
            } else {
                rustix::fs::renameat_with(
                    &parent.descriptor,
                    &temporary_name,
                    &parent.descriptor,
                    &file_name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(io::Error::from)?;
                remove_temporary = false;
                let _ = rustix::fs::fsync(&parent.descriptor);
            }

            Ok(())
        })();
        if remove_temporary {
            let _ = rustix::fs::unlinkat(&parent.descriptor, &temporary_name, AtFlags::empty());
        }

        operation
    }

    fn install_update(
        parent: &OwnedFd,
        temporary_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        expected: &[u8],
        remove_temporary: &mut bool,
    ) -> io::Result<()> {
        Self::install_update_with_metadata(
            parent,
            temporary_file,
            file_name,
            temporary_name,
            expected,
            remove_temporary,
            Self::copy_metadata,
        )
    }

    fn install_update_with_metadata(
        parent: &OwnedFd,
        temporary_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        expected: &[u8],
        remove_temporary: &mut bool,
        copy_metadata: impl FnOnce(&std::fs::File, &std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        Self::acquire_update_lock(parent, UPDATE_LOCK_TIMEOUT)?;
        let original_file = match Self::verify_target(parent, file_name, expected) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "write target changed since it was read",
                ));
            }
            Err(error) => return Err(error),
        };
        rustix::fs::renameat_with(
            parent,
            temporary_name,
            parent,
            file_name,
            RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from)?;
        *remove_temporary = false;
        let metadata_result =
            copy_metadata(&original_file, temporary_file).and_then(|()| temporary_file.sync_all());
        if let Err(error) = metadata_result {
            Self::roll_back_exchange(
                parent,
                temporary_file,
                file_name,
                temporary_name,
                remove_temporary,
            )?;

            return Err(error);
        }
        Self::validate_exchange(
            parent,
            temporary_file,
            file_name,
            temporary_name,
            expected,
            remove_temporary,
        )
    }

    fn acquire_update_lock(parent: &OwnedFd, timeout: Duration) -> io::Result<()> {
        Self::acquire_update_lock_with(timeout, || {
            rustix::fs::flock(parent, FlockOperation::NonBlockingLockExclusive)
                .map_err(io::Error::from)
        })
    }

    fn acquire_update_lock_with(
        timeout: Duration,
        mut try_lock: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let started_at = Instant::now();
        loop {
            match try_lock() {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if error.kind() != io::ErrorKind::WouldBlock {
                        return Err(error);
                    }
                }
            }

            let remaining = timeout.saturating_sub(started_at.elapsed());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for the write coordination lock",
                ));
            }
            thread::sleep(UPDATE_LOCK_RETRY_INTERVAL.min(remaining));
        }
    }

    #[cfg(target_vendor = "apple")]
    fn copy_metadata(source: &std::fs::File, destination: &std::fs::File) -> io::Result<()> {
        let mut expected = Self::apple_metadata_snapshot(source)?;
        Self::apple_copyfile(source, destination, CopyfileFlags::METADATA)?;

        Self::verify_apple_metadata(&mut expected, source, destination)
    }

    #[cfg(target_vendor = "apple")]
    fn verify_apple_metadata(
        expected: &mut std::fs::File,
        source: &std::fs::File,
        destination: &std::fs::File,
    ) -> io::Result<()> {
        let mut current_source = Self::apple_metadata_snapshot(source)?;
        let mut current_destination = Self::apple_metadata_snapshot(destination)?;
        if !Self::files_match(expected, &mut current_source)?
            || !Self::files_match(expected, &mut current_destination)?
        {
            return Err(Self::metadata_changed_error());
        }

        Ok(())
    }

    #[cfg(target_vendor = "apple")]
    fn apple_metadata_snapshot(source: &std::fs::File) -> io::Result<std::fs::File> {
        let snapshot = tempfile::tempfile()?;
        let flags = CopyfileFlags::METADATA.union(COPYFILE_PACK);
        Self::apple_copyfile(source, &snapshot, flags)?;

        Ok(snapshot)
    }

    #[cfg(target_vendor = "apple")]
    #[expect(
        unsafe_code,
        reason = "rustix exposes Apple's stateful fcopyfile metadata API as unsafe"
    )]
    fn apple_copyfile(
        source: &std::fs::File,
        destination: &std::fs::File,
        flags: CopyfileFlags,
    ) -> io::Result<()> {
        let state = rustix::fs::copyfile_state_alloc().map_err(io::Error::from)?;
        // SAFETY: `state` was allocated immediately above and remains live
        // until the matching `copyfile_state_free` call below.
        let copy_result = unsafe { rustix::fs::fcopyfile(source, destination, state, flags) }
            .map_err(io::Error::from);
        // SAFETY: This is the one matching free for the live state allocated
        // above.
        let free_result =
            unsafe { rustix::fs::copyfile_state_free(state) }.map_err(io::Error::from);

        copy_result.and(free_result)
    }

    #[cfg(target_vendor = "apple")]
    fn files_match(left: &mut std::fs::File, right: &mut std::fs::File) -> io::Result<bool> {
        let left_length = left.metadata()?.len();
        if left_length != right.metadata()?.len() {
            return Ok(false);
        }
        let mut remaining = left_length;
        left.seek(SeekFrom::Start(0))?;
        right.seek(SeekFrom::Start(0))?;
        let mut left_buffer = [0_u8; 8 * 1024];
        let mut right_buffer = [0_u8; 8 * 1024];
        while remaining > 0 {
            let buffer_length = u64::try_from(left_buffer.len()).unwrap_or(u64::MAX);
            let chunk_length =
                usize::try_from(remaining.min(buffer_length)).unwrap_or(left_buffer.len());
            left.read_exact(&mut left_buffer[..chunk_length])?;
            right.read_exact(&mut right_buffer[..chunk_length])?;
            if left_buffer[..chunk_length] != right_buffer[..chunk_length] {
                return Ok(false);
            }
            remaining -= u64::try_from(chunk_length).unwrap_or(remaining);
        }

        Ok(true)
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn copy_metadata(source: &std::fs::File, destination: &std::fs::File) -> io::Result<()> {
        let expected = Self::access_metadata(source)?;
        let destination_names = Self::extended_attribute_names(destination)?;

        rustix::fs::fchown(destination, Some(expected.owner), Some(expected.group))
            .map_err(io::Error::from)?;
        for name in destination_names.iter().filter(|name| {
            !expected
                .attributes
                .iter()
                .any(|attribute| &attribute.name == *name)
        }) {
            rustix::fs::fremovexattr(destination, name).map_err(io::Error::from)?;
        }
        for attribute in &expected.attributes {
            rustix::fs::fsetxattr(
                destination,
                &attribute.name,
                &attribute.value,
                rustix::fs::XattrFlags::empty(),
            )
            .map_err(io::Error::from)?;
        }
        rustix::fs::fchmod(destination, expected.mode).map_err(io::Error::from)?;

        Self::verify_copied_metadata(source, destination, &expected)
    }

    #[cfg(not(any(target_vendor = "apple", target_os = "android", target_os = "linux")))]
    fn copy_metadata(_source: &std::fs::File, _destination: &std::fs::File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe write metadata preservation is unsupported on this platform",
        ))
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn extended_attribute_names(file: &std::fs::File) -> io::Result<Vec<OsString>> {
        let mut buffer = vec![0_u8; XATTR_BUFFER_SIZE];
        let length = rustix::fs::flistxattr(file, &mut buffer).map_err(io::Error::from)?;
        buffer.truncate(length);

        let mut names = buffer
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(|name| OsString::from_vec(name.to_vec()))
            .collect::<Vec<_>>();
        names.sort();

        Ok(names)
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn extended_attribute(file: &std::fs::File, name: &OsStr) -> io::Result<Vec<u8>> {
        let mut value = vec![0_u8; XATTR_BUFFER_SIZE];
        let length = rustix::fs::fgetxattr(file, name, &mut value).map_err(io::Error::from)?;
        value.truncate(length);

        Ok(value)
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn access_metadata(file: &std::fs::File) -> io::Result<AccessMetadata> {
        let metadata = rustix::fs::fstat(file).map_err(io::Error::from)?;
        let attributes = Self::extended_attribute_names(file)?
            .into_iter()
            .map(|name| {
                let value = Self::extended_attribute(file, &name)?;

                Ok(ExtendedAttribute { name, value })
            })
            .collect::<io::Result<Vec<_>>>()?;

        Ok(AccessMetadata {
            attributes,
            group: rustix::fs::Gid::from_raw(metadata.st_gid),
            mode: Mode::from_bits_truncate(metadata.st_mode),
            owner: rustix::fs::Uid::from_raw(metadata.st_uid),
        })
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    fn verify_copied_metadata(
        source: &std::fs::File,
        destination: &std::fs::File,
        expected: &AccessMetadata,
    ) -> io::Result<()> {
        if Self::access_metadata(source)? != *expected
            || Self::access_metadata(destination)? != *expected
        {
            return Err(Self::metadata_changed_error());
        }

        Ok(())
    }

    fn metadata_changed_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "write target metadata changed during replacement",
        )
    }

    fn roll_back_exchange(
        parent: &OwnedFd,
        prepared_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        remove_temporary: &mut bool,
    ) -> io::Result<()> {
        rustix::fs::renameat_with(
            parent,
            temporary_name,
            parent,
            file_name,
            RenameFlags::EXCHANGE,
        )
        .map_err(io::Error::from)
        .map_err(Self::map_exchange_error)?;
        *remove_temporary = false;
        if !Self::path_matches_file(parent, temporary_name, prepared_file)? {
            rustix::fs::renameat_with(
                parent,
                temporary_name,
                parent,
                file_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(io::Error::from)?;

            return Err(Self::concurrent_target_error());
        }
        *remove_temporary = true;

        rustix::fs::fsync(parent).map_err(io::Error::from)
    }

    fn validate_exchange(
        parent: &OwnedFd,
        prepared_file: &std::fs::File,
        file_name: &OsStr,
        temporary_name: &OsStr,
        expected: &[u8],
        remove_temporary: &mut bool,
    ) -> io::Result<()> {
        if let Err(error) = Self::verify_target(parent, temporary_name, expected) {
            Self::roll_back_exchange(
                parent,
                prepared_file,
                file_name,
                temporary_name,
                remove_temporary,
            )?;

            return Err(error);
        }
        if !Self::path_matches_file(parent, file_name, prepared_file)? {
            return Err(Self::concurrent_target_error());
        }
        let exchange_is_durable = rustix::fs::fsync(parent).is_ok();
        if exchange_is_durable
            && rustix::fs::unlinkat(parent, temporary_name, AtFlags::empty()).is_ok()
        {
            let _ = rustix::fs::fsync(parent);
        }

        Ok(())
    }

    fn path_matches_file(
        parent: &OwnedFd,
        file_name: &OsStr,
        expected_file: &std::fs::File,
    ) -> io::Result<bool> {
        let expected = rustix::fs::fstat(expected_file).map_err(io::Error::from)?;
        let current = match rustix::fs::statat(parent, file_name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(current) => current,
            Err(error) if io::Error::from(error).kind() == io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(error) => return Err(io::Error::from(error)),
        };

        Ok(expected.st_dev == current.st_dev && expected.st_ino == current.st_ino)
    }

    fn concurrent_target_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "write target changed during atomic replacement",
        )
    }

    fn map_exchange_error(error: io::Error) -> io::Error {
        if error.kind() == io::ErrorKind::NotFound {
            return Self::concurrent_target_error();
        }

        error
    }

    fn open_parent_beneath(
        root: &Path,
        relative_path: &Path,
    ) -> io::Result<(ParentDirectory, OsString)> {
        let mut directory =
            rustix::fs::open(root, DIRECTORY_OPEN_FLAGS, Mode::empty()).map_err(io::Error::from)?;
        let mut components = relative_path
            .components()
            .map(|component| match component {
                Component::Normal(component) => Ok(component.to_os_string()),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write path must be repository-relative",
                )),
            })
            .collect::<io::Result<Vec<OsString>>>()?;
        let file_name = components.pop().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write path must not be empty")
        })?;

        for component in components {
            match rustix::fs::openat(&directory, &component, DIRECTORY_OPEN_FLAGS, Mode::empty()) {
                Ok(descriptor) => directory = descriptor,
                Err(error) if io::Error::from(error).kind() == io::ErrorKind::NotFound => {
                    Self::create_directory(&directory, &component)?;
                    directory = rustix::fs::openat(
                        &directory,
                        &component,
                        DIRECTORY_OPEN_FLAGS,
                        Mode::empty(),
                    )
                    .map_err(io::Error::from)?;
                }
                Err(error) => return Err(io::Error::from(error)),
            }
        }

        Ok((
            ParentDirectory {
                descriptor: directory,
            },
            file_name,
        ))
    }

    fn create_directory(parent: &OwnedFd, name: &OsStr) -> io::Result<()> {
        match rustix::fs::mkdirat(parent, name, DIRECTORY_MODE) {
            Ok(()) => Ok(()),
            Err(error) if io::Error::from(error).kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(io::Error::from(error)),
        }
    }

    fn verify_target(
        parent: &OwnedFd,
        file_name: &OsStr,
        expected: &[u8],
    ) -> io::Result<std::fs::File> {
        let descriptor = rustix::fs::openat(parent, file_name, FILE_OPEN_FLAGS, Mode::empty())
            .map_err(io::Error::from)?;
        let metadata = rustix::fs::fstat(&descriptor).map_err(io::Error::from)?;
        if !FileType::from_raw_mode(metadata.st_mode).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write path must name a regular file",
            ));
        }
        let mut target = std::fs::File::from(descriptor);
        if !Self::target_matches(&mut target, expected)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "write target changed since it was read",
            ));
        }

        Ok(target)
    }

    fn target_matches(target: &mut impl io::Read, expected: &[u8]) -> io::Result<bool> {
        let mut buffer = [0_u8; 8 * 1024];
        for expected_chunk in expected.chunks(buffer.len()) {
            match target.read_exact(&mut buffer[..expected_chunk.len()]) {
                Ok(()) if &buffer[..expected_chunk.len()] == expected_chunk => {}
                Ok(()) => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        let mut extra = [0_u8; 1];
        match target.read_exact(&mut extra) {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(true),
            Err(error) => Err(error),
        }
    }

    fn temporary_name() -> OsString {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);

        OsString::from(format!(
            ".ag-harness-write-{}-{sequence}",
            std::process::id()
        ))
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    async fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        tokio::fs::canonicalize(path).await
    }

    async fn open_beneath(
        &self,
        root: &Path,
        path: &Path,
    ) -> io::Result<Box<dyn AsyncRead + Send + Unpin>> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        let file = tokio::task::spawn_blocking(move || Self::open_beneath(&root, &path))
            .await
            .map_err(io::Error::other)??;

        Ok(Box::new(tokio::fs::File::from_std(file)))
    }

    async fn replace_beneath(
        &self,
        root: &Path,
        path: &Path,
        expected: Option<Vec<u8>>,
        content: Vec<u8>,
    ) -> io::Result<()> {
        let root = root.to_path_buf();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || {
            Self::replace_beneath(&root, &path, expected.as_deref(), &content)
        })
        .await
        .map_err(io::Error::other)?
    }
}

#[cfg(test)]
#[path = "file_system_test.rs"]
mod tests;
