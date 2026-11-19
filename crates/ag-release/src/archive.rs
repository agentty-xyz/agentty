//! Deterministic `.tar.gz` archives built in process.
//!
//! The same inputs always produce byte-identical archives: entries are sorted
//! by path, owned by uid and gid 0 with no user or group names, stamped with
//! one modification time, and use fixed modes. The gzip header carries no
//! file name or timestamp.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::{self, Write};

use flate2::write::GzEncoder;
use flate2::{Compression, GzBuilder};
use tar::{EntryType, Header};

/// Environment variable that fixes archive modification times.
pub(crate) const SOURCE_DATE_EPOCH: &str = "SOURCE_DATE_EPOCH";

/// gzip header operating-system value meaning "unknown".
const UNKNOWN_OS: u8 = 255;

/// Mode of archived directories and executables.
const EXECUTABLE_MODE: u32 = 0o755;

/// Mode of other archived files.
const REGULAR_MODE: u32 = 0o644;

/// Returns the archive modification time: `SOURCE_DATE_EPOCH` seconds when
/// set, otherwise 0.
///
/// # Errors
/// Returns an error when `value` is not a nonnegative integer.
pub(crate) fn modification_time(value: Option<&OsStr>) -> Result<u64, String> {
    let Some(value) = value else {
        return Ok(0);
    };

    value
        .to_str()
        .filter(|text| text.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| {
            format!(
                "`{SOURCE_DATE_EPOCH}` must be a nonnegative integer number of seconds, found `{}`",
                value.to_string_lossy()
            )
        })
}

/// Returns `contents` gzip-compressed with a deterministic header.
///
/// # Errors
/// Returns an error when compression fails.
pub(crate) fn gzip(contents: &[u8]) -> Result<Vec<u8>, String> {
    write_gzip(Vec::new(), contents)
}

/// How an archived file may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileMode {
    /// A program, mode 0755.
    Executable,
    /// Any other file, mode 0644.
    Regular,
}

impl FileMode {
    fn bits(self) -> u32 {
        match self {
            Self::Executable => EXECUTABLE_MODE,
            Self::Regular => REGULAR_MODE,
        }
    }
}

/// A `.tar.gz` archive whose files all live below one top-level directory.
pub(crate) struct Archive {
    /// Top-level directory name.
    directory: String,
    /// Whether the archive records directory entries.
    directory_entries: bool,
    /// Files by `/`-separated path relative to `directory`.
    files: BTreeMap<String, ArchiveFile>,
}

impl Archive {
    /// Returns an empty archive with the top-level `directory`.
    pub(crate) fn new(directory: String) -> Self {
        Self {
            directory,
            directory_entries: true,
            files: BTreeMap::new(),
        }
    }

    /// Returns an empty archive with the top-level `directory` that, like
    /// the tarballs `npm pack` writes, records no directory entries.
    pub(crate) fn files_only(directory: String) -> Self {
        Self {
            directory_entries: false,
            ..Self::new(directory)
        }
    }

    /// Adds a file at the `/`-separated `path` below the top-level directory.
    ///
    /// # Errors
    /// Returns an error when `path` is already a file or directory of the
    /// archive, or lies below one of its files.
    pub(crate) fn add_file(
        &mut self,
        path: &str,
        mode: FileMode,
        contents: Vec<u8>,
    ) -> Result<(), String> {
        let path = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("/");
        let conflicts = self.files.keys().any(|existing| {
            *existing == path || Self::is_below(existing, &path) || Self::is_below(&path, existing)
        });
        if conflicts {
            return Err(format!(
                "archive `{}` cannot hold `{path}` because it conflicts with another entry",
                self.directory
            ));
        }
        self.files.insert(path, ArchiveFile { contents, mode });

        Ok(())
    }

    /// Returns the deterministic `.tar.gz` bytes, stamping every entry with
    /// `mtime` seconds since the Unix epoch.
    ///
    /// # Errors
    /// Returns an error when the archive cannot be encoded.
    pub(crate) fn to_tar_gz(&self, mtime: u64) -> Result<Vec<u8>, String> {
        self.write_tar_gz(Vec::new(), mtime)
    }

    /// Returns whether `path` lies below the directory `ancestor`.
    fn is_below(path: &str, ancestor: &str) -> bool {
        path.strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
    }

    /// Writes the archive into `writer`.
    fn write_tar_gz<W: Write>(&self, writer: W, mtime: u64) -> Result<W, String> {
        self.encode_tar_gz(writer, mtime)
            .map_err(|error| write_error(&error))
    }

    /// Encodes the sorted entries through a deterministic gzip encoder.
    fn encode_tar_gz<W: Write>(&self, writer: W, mtime: u64) -> io::Result<W> {
        let mut builder = tar::Builder::new(gzip_encoder(writer));
        for (path, entry) in self.entries() {
            let mut header = Header::new_gnu();
            header.set_entry_type(entry.kind);
            header.set_mode(entry.mode);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(mtime);
            header.set_size(entry.contents.len() as u64);
            builder.append_data(&mut header, path, entry.contents)?;
        }

        builder.into_inner()?.finish()
    }

    /// Returns every entry by archive path: the top-level directory, each
    /// directory holding a file, and the files, or only the files without
    /// directory entries. Directory paths end in `/`, so each directory sorts
    /// before its contents.
    fn entries(&self) -> BTreeMap<String, Entry<'_>> {
        let mut directories = BTreeSet::new();
        if self.directory_entries {
            directories.insert(String::new());
            for path in self.files.keys() {
                for (index, _) in path.match_indices('/') {
                    directories.insert(format!("{}/", &path[..index]));
                }
            }
        }
        let directories = directories.into_iter().map(|directory| {
            let entry = Entry {
                contents: &[],
                kind: EntryType::Directory,
                mode: EXECUTABLE_MODE,
            };

            (format!("{}/{directory}", self.directory), entry)
        });
        let files = self.files.iter().map(|(path, file)| {
            let entry = Entry {
                contents: &file.contents,
                kind: EntryType::Regular,
                mode: file.mode.bits(),
            };

            (format!("{}/{path}", self.directory), entry)
        });

        directories.chain(files).collect()
    }
}

/// Compresses `contents` into `writer`.
fn write_gzip<W: Write>(writer: W, contents: &[u8]) -> Result<W, String> {
    let mut encoder = gzip_encoder(writer);

    encoder
        .write_all(contents)
        .and_then(|()| encoder.finish())
        .map_err(|error| write_error(&error))
}

/// Returns a maximum-compression gzip encoder with no file name, timestamp,
/// or host operating system in its header.
fn gzip_encoder<W: Write>(writer: W) -> GzEncoder<W> {
    GzBuilder::new()
        .mtime(0)
        .operating_system(UNKNOWN_OS)
        .write(writer, Compression::best())
}

fn write_error(error: &io::Error) -> String {
    format!("failed to write an archive: {error}")
}

/// One file's contents and mode.
struct ArchiveFile {
    contents: Vec<u8>,
    mode: FileMode,
}

/// One archive entry ready to write.
struct Entry<'archive> {
    contents: &'archive [u8],
    kind: EntryType,
    mode: u32,
}

#[cfg(test)]
#[path = "archive_test.rs"]
mod tests;
