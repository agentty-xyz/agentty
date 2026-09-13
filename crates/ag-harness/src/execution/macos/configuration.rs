//! Seatbelt construction for a deliberately narrow, single-process native
//! stage.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::{fs, io};

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};
use tempfile::TempDir;

use crate::execution::contract::{Access, Command, Policy};

const GIT_METADATA_BYTES: u64 = 4096;

/// Owns the explicitly supplied scratch tree, including on validation failure.
/// No executor implements the public lifecycle contract yet.
pub(super) struct Configuration {
    policy: Policy,
    scratch: TempDir,
}

impl Configuration {
    pub(super) fn new(policy: Policy, scratch: TempDir) -> io::Result<Self> {
        let configuration = Self { policy, scratch };
        configuration.validate()?;

        Ok(configuration)
    }

    pub(super) fn scratch(&self) -> &Path {
        self.scratch.path()
    }

    /// Called only before launch or after reaping; host processes are trusted.
    pub(super) fn cleanup(&self) -> io::Result<()> {
        if self.scratch().try_exists()? {
            remove_directory_contents(self.scratch())?;
            fs::remove_dir(self.scratch())?;
        }

        Ok(())
    }

    pub(super) fn profile(&self, command: &Command) -> io::Result<String> {
        // Revalidate immediately before launch; other host processes are
        // trusted.
        let protected = self.validate()?;
        let directory = self.policy.workspace().join(command.directory());
        let directory = directory.canonicalize()?;
        if !directory.starts_with(self.policy.workspace()) || !directory.is_dir() {
            return Err(unsupported("working directory escapes the workspace"));
        }
        for executable in [command.executable(), Path::new("/usr/bin/env")] {
            validate_entry(executable)?;
            if executable.as_os_str().as_encoded_bytes().contains(&b'=') {
                return Err(unsupported(
                    "env cannot launch executable paths containing '='",
                ));
            }
            if !executable.is_file()
                || self.policy.requested_access(executable) == Ok(Access::Denied)
            {
                return Err(unsupported("executable requires an explicit read grant"));
            }
        }
        let mut profile = String::from(
            "(version 1)\n(deny default)\n(allow process-exec)\n(allow sysctl-read)\n",
        );
        rule(
            &mut profile,
            "allow",
            "file-read*",
            "subpath",
            self.policy.workspace(),
        );
        for path in self.policy.external_reads() {
            rule(&mut profile, "allow", "file-read*", "subpath", path);
        }
        for path in self.policy.external_entries() {
            rule(&mut profile, "allow", "file-read*", "literal", path);
        }
        for path in self.policy.workspace_writes() {
            rule(
                &mut profile,
                "allow",
                "file-write*",
                "subpath",
                &self.policy.workspace().join(path),
            );
        }
        rule(
            &mut profile,
            "allow",
            "file-read* file-write*",
            "subpath",
            self.scratch(),
        );
        // Seatbelt rule ordering matters, especially for file creation.
        // Invariant denials must follow every capability grant.
        profile.push_str(
            "(deny file-link)\n(deny file-write-unlink (vnode-type DIRECTORY))\n(deny \
             file-write-create (require-not (require-any (vnode-type REGULAR-FILE) (vnode-type \
             DIRECTORY))))\n(deny file-write* (regex #\"/[.][gG][iI][tT](/|$)\"))\n",
        );
        rule(
            &mut profile,
            "deny",
            "file-write-mode file-write-flags file-write-owner file-write-acl file-write-xattr",
            "subpath",
            self.scratch(),
        );
        for path in protected {
            rule(&mut profile, "deny", "file-write*", "subpath", &path);
        }

        Ok(profile)
    }

    pub(super) fn environment(
        &self,
    ) -> impl Iterator<Item = (&std::ffi::OsString, &std::ffi::OsString)> {
        self.policy.environment().iter()
    }

    pub(super) fn directory(&self, command: &Command) -> PathBuf {
        self.policy.workspace().join(command.directory())
    }

    fn validate(&self) -> io::Result<Vec<PathBuf>> {
        if !self.policy.exposes_host_information() {
            return Err(unsupported(
                "native execution requires an explicit host-information grant",
            ));
        }
        validate_entry(self.policy.workspace())?;
        validate_entry(self.scratch())?;
        if !self.policy.workspace().is_dir() || !self.scratch().is_dir() {
            return Err(unsupported("workspace and scratch must be directories"));
        }
        for path in std::iter::once(self.policy.workspace())
            .chain(self.policy.external_reads().iter().map(PathBuf::as_path))
            .chain(self.policy.git_metadata().iter().map(PathBuf::as_path))
        {
            if trees_overlap(path, self.scratch())? {
                return Err(unsupported("scratch overlaps a policy tree"));
            }
        }
        let mut protected = self.policy.git_metadata().to_vec();
        scan_trees_with_budget(
            std::iter::once(self.policy.workspace())
                .chain(std::iter::once(self.scratch()))
                .chain(self.policy.external_reads().iter().map(PathBuf::as_path)),
            &mut protected,
            100_000,
        )?;
        for path in self.policy.external_entries() {
            validate_entry(path)?;
        }
        for path in &protected {
            validate_entry(path)?;
            if path.is_dir() && path.join("objects/info/alternates").try_exists()? {
                return Err(unsupported("Git object alternates are unsupported"));
            }
            if tree_contains(path, self.policy.workspace())? || trees_overlap(path, self.scratch())?
            {
                return Err(unsupported(
                    "protected Git metadata contains the workspace or overlaps scratch",
                ));
            }
        }
        for path in &protected {
            if tree_contains(self.policy.workspace(), path)?
                && !path.starts_with(self.policy.workspace())
            {
                return Err(unsupported(
                    "Git metadata uses an alternate workspace mount path",
                ));
            }
        }
        for path in self.policy.workspace_writes() {
            let target = self.policy.workspace().join(path);
            validate_entry(&target)?;
            if !target.is_dir() {
                return Err(unsupported("write grants require existing directories"));
            }
        }

        Ok(protected)
    }
}

impl Drop for Configuration {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn remove_directory_contents(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = Mode::RWXU;
        rustix::fs::chmodat(CWD, path, mode, AtFlags::SYMLINK_NOFOLLOW)?;
        let mut directory = Dir::new(rustix::fs::open(path, flags, Mode::empty())?)?;
        let mut ancestors = Vec::new();
        loop {
            if let Some(entry) = directory.read() {
                let entry = entry?;
                let name = entry.file_name();
                if name == c"." || name == c".." {
                    continue;
                }
                let metadata =
                    rustix::fs::statat(directory.fd()?, name, AtFlags::SYMLINK_NOFOLLOW)?;
                if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
                    // mkdir can request mode 000. Repair access without
                    // following links; relative operations
                    // also work beyond the absolute path-length limit.
                    rustix::fs::chmodat(directory.fd()?, name, mode, AtFlags::SYMLINK_NOFOLLOW)?;
                    let child = rustix::fs::openat(directory.fd()?, name, flags, Mode::empty())?;
                    ancestors.push(name.to_owned());
                    directory = Dir::new(child)?;
                } else {
                    rustix::fs::unlinkat(directory.fd()?, name, AtFlags::empty())?;
                }
            } else if let Some(name) = ancestors.pop() {
                // Reopen the parent instead of retaining one descriptor per
                // depth.
                let parent = rustix::fs::openat(directory.fd()?, c"..", flags, Mode::empty())?;
                rustix::fs::unlinkat(&parent, name, AtFlags::REMOVEDIR)?;
                // fdopendir may eagerly buffer entries, so open the stream
                // only after removing the child.
                directory = Dir::new(parent)?;
            } else {
                break;
            }
        }
    }

    Ok(())
}

fn validate_entry(path: &Path) -> io::Result<()> {
    validate_node(path)?;
    validate_filesystem(&rustix::fs::statfs(path)?.f_fstypename)?;

    Ok(())
}

fn validate_filesystem(name: &[libc::c_char]) -> io::Result<()> {
    if !name.starts_with(&b"apfs\0".map(u8::cast_signed)) {
        return Err(unsupported("native isolation supports only APFS entries"));
    }

    Ok(())
}

fn validate_node(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || !path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .all(|byte| (32..127).contains(byte))
        || path.canonicalize()? != path
    {
        return Err(unsupported(
            "noncanonical or non-ASCII paths are unsupported",
        ));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !(metadata.is_dir() || metadata.is_file()) || metadata.is_file() && metadata.nlink() != 1 {
        return Err(unsupported(
            "aliases, hard links, IPC and device nodes are unsupported",
        ));
    }

    Ok(())
}

fn scan_trees_with_budget<'a>(
    roots: impl IntoIterator<Item = &'a Path>,
    protected: &mut Vec<PathBuf>,
    mut remaining: usize,
) -> io::Result<()> {
    let mut visited = HashSet::new();
    for root in roots {
        scan_tree(root, protected, &mut remaining, &mut visited)?;
    }
    let mut checked_git = HashSet::new();
    let mut index = 0;
    while index < protected.len() {
        let root = protected[index].clone();
        index += 1;
        if !checked_git.insert(root.clone()) {
            continue;
        }
        scan_tree(&root, protected, &mut remaining, &mut visited)?;
        if root.is_dir() {
            let common = root.join("commondir");
            if common.try_exists()? {
                validate_entry(&common)?;
                protected.push(
                    root.join(git_path(&read_git_metadata(&common)?)?)
                        .canonicalize()?,
                );
            }
        }
    }
    protected.sort_unstable();
    protected.dedup();

    Ok(())
}

fn scan_tree(
    root: &Path,
    protected: &mut Vec<PathBuf>,
    remaining: &mut usize,
    visited: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    if visited.contains(root) {
        return Ok(());
    }
    validate_entry(root)?;
    let mut pending = Vec::new();
    enqueue_entries([Ok(root.to_path_buf())], &mut pending, remaining)?;
    let device = fs::symlink_metadata(root)?.dev();
    while let Some(path) = pending.pop() {
        validate_node(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        validate_device(metadata.dev(), device)?;
        if !visited.insert(path.clone()) {
            continue;
        }
        if path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().eq_ignore_ascii_case(b".git"))
        {
            protected.push(path.clone());
            if metadata.is_file() {
                let content = read_git_metadata(&path)?;
                let target = git_path(
                    content
                        .strip_prefix("gitdir: ")
                        .ok_or_else(|| unsupported("invalid Git indirection"))?,
                )?;
                let target = path.with_file_name("").join(target).canonicalize()?;
                validate_entry(&target)?;
                protected.push(target);
            }
        }
        if metadata.is_dir() {
            let entries = fs::read_dir(path)?.map(|entry| entry.map(|entry| entry.path()));
            enqueue_entries(entries, &mut pending, remaining)?;
        }
    }

    Ok(())
}

fn enqueue_entries(
    entries: impl IntoIterator<Item = io::Result<PathBuf>>,
    pending: &mut Vec<PathBuf>,
    remaining: &mut usize,
) -> io::Result<()> {
    for entry in entries {
        *remaining = remaining
            .checked_sub(1)
            .ok_or_else(|| unsupported("tree validation limit exceeded"))?;
        pending.push(entry?);
    }

    Ok(())
}

fn read_git_metadata(path: &Path) -> io::Result<String> {
    let mut content = String::new();
    fs::File::open(path)?
        .take(GIT_METADATA_BYTES + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > GIT_METADATA_BYTES {
        return Err(unsupported("Git indirection exceeds 4096 bytes"));
    }

    Ok(content)
}

fn git_path(content: &str) -> io::Result<&str> {
    let path = content.strip_suffix('\n').unwrap_or(content);
    if path.is_empty() || path.trim() != path || path.contains(['\n', '\r']) {
        return Err(unsupported("ambiguous Git indirection is unsupported"));
    }

    Ok(path)
}

fn trees_overlap(left: &Path, right: &Path) -> io::Result<bool> {
    Ok(tree_contains(left, right)? || tree_contains(right, left)?)
}

fn tree_contains(root: &Path, path: &Path) -> io::Result<bool> {
    let expected = identity(root)?;
    for ancestor in path.ancestors() {
        if identity(ancestor)? == expected {
            return Ok(true);
        }
    }

    Ok(false)
}

fn identity(path: &Path) -> io::Result<(u64, u64)> {
    let metadata = fs::metadata(path)?;

    Ok((metadata.dev(), metadata.ino()))
}

fn validate_device(actual: u64, expected: u64) -> io::Result<()> {
    if actual != expected {
        return Err(unsupported("nested mounts are unsupported"));
    }

    Ok(())
}

fn rule(profile: &mut String, disposition: &str, operation: &str, filter: &str, path: &Path) {
    let path = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let _ = writeln!(profile, "({disposition} {operation} ({filter} \"{path}\"))");
}

pub(super) fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
#[path = "configuration_test.rs"]
mod tests;
