//! Collect instrumented launcher profiles without changing production
//! isolation.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

pub(super) struct Coverage {
    destination: PathBuf,
    directory: TempDir,
    profiles: PathBuf,
}

impl Coverage {
    pub(super) fn new(workspace: &Path) -> Option<Self> {
        let profile = std::env::var_os("LLVM_PROFILE_FILE")?;
        let destination = Path::new(&profile)
            .parent()
            .expect("profile directory")
            .to_path_buf();
        let directory = tempfile::tempdir().expect("coverage wrapper");
        let profiles = workspace.join("output/coverage");
        std::fs::create_dir(&profiles).expect("profile output");
        let coverage = Self {
            destination,
            directory,
            profiles,
        };
        // Read-only sandbox policies leave the profile directory unwritable
        // inside the namespace; those inner profiles are discarded on tmpfs
        // instead of emitting profile warnings into captured output.
        let script = format!(
            "#!/bin/bash\nphase=outer\ncase \"${{1-}}\" in --seatbelt-child|--namespace-init) \
             phase=inner;; esac\nprefix={}\nif [ \"$phase\" = inner ] && [ ! -w \"$prefix\" ]; \
             then prefix=; fi\nexport \
             LLVM_PROFILE_FILE=\"$prefix\"/\"$phase-$$-$RANDOM-$RANDOM-%p%c.profraw\"\nexec {} \
             \"$@\"\n",
            quote(&coverage.profiles),
            quote(&launcher()),
        );
        std::fs::write(coverage.launcher(), script).expect("coverage wrapper source");
        std::fs::set_permissions(coverage.launcher(), std::fs::Permissions::from_mode(0o700))
            .expect("executable wrapper");

        Some(coverage)
    }

    pub(super) fn launcher(&self) -> PathBuf {
        self.directory
            .path()
            .canonicalize()
            .expect("wrapper directory")
            .join("launcher")
    }

    pub(super) fn profiles(&self) -> Vec<PathBuf> {
        std::fs::read_dir(&self.profiles)
            .expect("profiles")
            .map(|entry| entry.expect("profile").path())
            .collect()
    }
}

impl Drop for Coverage {
    fn drop(&mut self) {
        for profile in self.profiles() {
            let name = format!(
                "sandbox-{}-{}",
                self.directory
                    .path()
                    .file_name()
                    .expect("unique wrapper")
                    .to_string_lossy(),
                profile.file_name().expect("profile name").to_string_lossy()
            );
            std::fs::copy(&profile, self.destination.join(name)).expect("retain launcher coverage");
        }
    }
}

pub(super) fn launcher() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_ag-harness-sandbox"))
        .canonicalize()
        .expect("built launcher")
}

fn quote(path: &Path) -> String {
    format!(
        "'{}'",
        path.to_str()
            .expect("UTF8 fixture path")
            .replace('\'', "'\\''")
    )
}
