//! Conservative mount policy: existing files can be written in place; directory
//! mutation is unsupported because mounts cannot protect newly created Git
//! names.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::execution::contract::{Access, Command, Policy};

/// Validated host input. The trusted owner must keep all source trees quiescent
/// until asynchronous launch returns; the backend waits for every source bind
/// to be pinned before releasing that requirement.
pub(super) struct Configuration {
    command: Command,
    policy: Policy,
    scratch: PathBuf,
}

impl Configuration {
    pub(super) fn new(command: Command, policy: Policy, scratch: PathBuf) -> io::Result<Self> {
        let configuration = Self {
            command,
            policy,
            scratch,
        };
        configuration.validate()?;

        Ok(configuration)
    }

    pub(super) fn command(&self) -> &Command {
        &self.command
    }

    pub(super) fn policy(&self) -> &Policy {
        &self.policy
    }

    pub(super) fn scratch(&self) -> &Path {
        &self.scratch
    }

    pub(super) fn writable_paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.policy
            .workspace_writes()
            .iter()
            .map(|path| self.policy.workspace().join(path))
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if !self.policy.exposes_host_information() {
            return Err(unsupported(
                "native ELF auxiliary vectors and vDSO require a host-information grant",
            ));
        }
        let workspace = self.policy.workspace();
        canonical(workspace)?;
        canonical(&self.scratch)?;
        if !workspace.is_dir() || !self.scratch.is_dir() || overlaps(workspace, &self.scratch) {
            return Err(unsupported(
                "workspace and scratch must be disjoint directories",
            ));
        }
        let mut roots = vec![workspace.to_path_buf(), self.scratch.clone()];
        for root in self.policy.external_reads() {
            canonical(root)?;
            if roots.iter().any(|other| overlaps(root, other)) {
                return Err(unsupported("overlapping read grants are unsupported"));
            }
            roots.push(root.clone());
        }
        let mut metadata = Vec::new();
        let mut identities = BTreeMap::new();
        for root in self.policy.git_metadata() {
            canonical(root)?;
            metadata.push(root.clone());
            if root.is_dir() {
                inspect_common_directory(root, &mut metadata)?;
            }
        }
        for root in roots.iter().filter(|root| **root != self.scratch) {
            if ["/tmp", "/proc", "/sys", "/dev", "/.ag-isolation"]
                .iter()
                .any(|reserved| overlaps(root, Path::new(reserved)))
            {
                return Err(unsupported("grants overlap reserved sandbox paths"));
            }
            inspect_tree(root, &mut metadata, &mut identities)?;
        }
        // Other launches own the contents below this shared parent. Only its
        // identity participates in source/Git alias validation.
        inspect_entry(&self.scratch, &mut identities)?;
        inspect_git(&self.scratch, true, &mut metadata)?;
        let mut index = 0;
        while index < metadata.len() {
            let root = metadata[index].clone();
            if overlaps(&root, &self.scratch) {
                return Err(unsupported("scratch overlaps protected Git metadata"));
            }
            inspect_tree(&root, &mut metadata, &mut identities)?;
            index += 1;
        }
        for relative in self.policy.workspace_writes() {
            let path = workspace.join(relative);
            canonical(&path)?;
            if !fs::symlink_metadata(&path)?.is_file()
                || metadata.iter().any(|root| path.starts_with(root))
            {
                return Err(unsupported("writes require existing non-Git regular files"));
            }
        }
        let executable = self.command.executable();
        canonical(executable)?;
        if !fs::metadata(executable)?.is_file()
            || self.policy.requested_access(executable) != Ok(Access::ReadOnly)
        {
            return Err(unsupported(
                "executable requires an explicit read-only grant",
            ));
        }
        let directory = workspace.join(self.command.directory());
        canonical(&directory)?;
        if !directory.is_dir() {
            return Err(unsupported("working directory must exist inside workspace"));
        }

        Ok(())
    }

    pub(super) fn validate_launch_directory(&self, path: &Path) -> io::Result<()> {
        canonical(path)?;
        if path.parent() != Some(self.scratch())
            || !path.is_dir()
            || fs::read_dir(path)?.next().is_some()
        {
            return Err(unsupported(
                "launch scratch must be a fresh empty child directory",
            ));
        }
        inspect_entry(path, &mut BTreeMap::new())?;

        Ok(())
    }
}

fn canonical(path: &Path) -> io::Result<()> {
    if !path.is_absolute() || fs::canonicalize(path)? != path {
        return Err(unsupported(
            "paths must be canonical and contain no symlink aliases",
        ));
    }

    Ok(())
}

fn overlaps(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn inspect_tree(
    path: &Path,
    git: &mut Vec<PathBuf>,
    identities: &mut BTreeMap<(u64, u64), PathBuf>,
) -> io::Result<()> {
    let Some(metadata) = inspect_entry(path, identities)? else {
        return Ok(());
    };
    inspect_git(path, metadata.is_dir(), git)?;
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            inspect_tree(&entry?.path(), git, identities)?;
        }
    }

    Ok(())
}

fn inspect_git(path: &Path, directory: bool, git: &mut Vec<PathBuf>) -> io::Result<()> {
    let git_name = path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(".git"));
    let bare_repository = directory
        && path.join("HEAD").is_file()
        && (path.join("commondir").exists()
            || path.join("objects").is_dir() && path.join("refs").is_dir());
    if git_name || bare_repository {
        git.push(path.to_path_buf());
        if directory {
            inspect_common_directory(path, git)?;
        } else {
            let content = read_reference(path)?;
            let target = content
                .strip_prefix("gitdir: ")
                .ok_or_else(|| unsupported("invalid Git indirection"))?;
            let directory =
                fs::canonicalize(path.with_file_name(target.trim_end_matches(['\r', '\n'])))?;
            if !directory.is_dir() {
                return Err(unsupported("Git directory is not a directory"));
            }
            git.push(directory.clone());
            inspect_common_directory(&directory, git)?;
        }
    }

    Ok(())
}

fn inspect_entry(
    path: &Path,
    identities: &mut BTreeMap<(u64, u64), PathBuf>,
) -> io::Result<Option<fs::Metadata>> {
    #[cfg(target_os = "linux")]
    if !matches!(
        rustix::fs::statfs(path)?.f_type,
        // ext, XFS, Btrfs, tmpfs, overlayfs and FUSE (including virtiofs).
        0xef53 | 0x5846_5342 | 0x9123_683e | 0x0102_1994 | 0x794c_7630 | 0x6573_5546
    ) {
        return Err(unsupported("filesystem type is unsupported"));
    }
    let metadata = fs::symlink_metadata(path)?;
    if !(metadata.is_dir() || metadata.is_file()) || metadata.is_file() && metadata.nlink() != 1 {
        return Err(unsupported(
            "symlinks, hard links, IPC and device nodes are unsupported",
        ));
    }
    if let Some(previous) = identities.insert((metadata.dev(), metadata.ino()), path.to_path_buf())
    {
        if previous != path {
            return Err(unsupported(
                "distinct paths alias the same filesystem object",
            ));
        }

        return Ok(None);
    }

    Ok(Some(metadata))
}

fn inspect_common_directory(directory: &Path, git: &mut Vec<PathBuf>) -> io::Result<()> {
    reject_alternates(directory)?;
    match read_reference(&directory.join("commondir")) {
        Ok(content) => {
            let common = fs::canonicalize(directory.join(content.trim_end_matches(['\r', '\n'])))?;
            if !common.is_dir() {
                return Err(unsupported("Git common directory is not a directory"));
            }
            reject_alternates(&common)?;
            git.push(common);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    Ok(())
}

fn reject_alternates(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory.join("objects/info/alternates")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(unsupported("Git alternate object stores are unsupported")),
    }
}

fn read_reference(path: &Path) -> io::Result<String> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(unsupported("Git metadata references must be regular files"));
    }
    let mut content = String::new();
    fs::File::open(path)?
        .take(1_048_577)
        .read_to_string(&mut content)?;
    if content.len() > 1_048_576 {
        return Err(unsupported("Git metadata reference exceeds 1 MiB"));
    }

    Ok(content)
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
#[path = "configuration_test.rs"]
mod tests;
