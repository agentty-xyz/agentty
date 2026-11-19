//! The shell installer that `ag-release assemble` writes, run for real under
//! `/bin/sh` against a `file://` download URL.
//!
//! Every run happens in a [`Sandbox`]: a cleared environment with a
//! temporary `HOME` and a `PATH` of symbolic links to the host tools the
//! installer needs, preceded by fake `uname`, `ldd`, `sysctl`, and `wget`
//! executables that simulate other platforms, and by fake `head` and `curl`
//! executables where a test needs them. Runs never see the real home
//! directory or shell startup files.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fs, io};

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};

use crate::support::{HOST_TARGET, ag_release, committed_demo_workspace, isolate_git};

/// Root manifest of a demo workspace released with the shell installer for
/// every supported target.
const MANIFEST: &str = r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
installers = ["shell"]
min-glibc = "2.17"

[workspace.metadata.release.targets]
aarch64-apple-darwin = "demo-runner"
aarch64-unknown-linux-gnu = "demo-runner"
x86_64-apple-darwin = "demo-runner"
x86_64-unknown-linux-gnu = "demo-runner"

[workspace.metadata.release.apps.demo-app]
"#;

/// Every configured target, as the installer lists them.
const TARGETS: [&str; 4] = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
];

/// The error suffix naming every supported target.
const SUPPORTED: &str = "demo-app supports aarch64-apple-darwin, aarch64-unknown-linux-gnu, \
                         x86_64-apple-darwin, x86_64-unknown-linux-gnu";

/// Host tools the installer and the fake executables run.
const TOOLS: [&str; 19] = [
    "awk",
    "cat",
    "chmod",
    "cp",
    "curl",
    "grep",
    "gzip",
    "head",
    "ldd",
    "mkdir",
    "mktemp",
    "mv",
    "rm",
    "sed",
    "sha256sum",
    "shasum",
    "sysctl",
    "tar",
    "uname",
];

/// `ldd --version` of a supported glibc.
const GLIBC: &str = "echo 'ldd (GNU libc) 2.39'";

/// `sysctl -n hw.optional.arm64` of a Mac without Apple silicon.
const UNKNOWN_OID: &str = "echo 'sysctl: unknown oid' >&2; exit 1";

/// The archive the simulated Linux x86-64 platform installs.
const LINUX_ARCHIVE: &str = "demo-app-x86_64-unknown-linux-gnu.tar.gz";

/// A release of the demo app for every target, assembled by `ag-release`.
struct Release {
    _workspace: TempDir,
    /// Artifacts directory holding the archives and the installer.
    artifacts: TempDir,
}

impl Release {
    /// Writes an archive and checksum file for every target, whose `demo-app`
    /// binary prints its target, and assembles the release.
    fn assemble() -> Result<Self, Box<dyn Error>> {
        let workspace = committed_demo_workspace(MANIFEST)?;
        let artifacts = tempdir()?;
        for target in TARGETS {
            let archive = Self::archive(target, &format!("echo demo-app {target}"))?;
            let name = format!("demo-app-{target}.tar.gz");
            fs::write(
                artifacts.path().join(format!("{name}.sha256")),
                format!("{}  {name}\n", sha256(&archive)),
            )?;
            fs::write(artifacts.path().join(name), archive)?;
        }
        let directory = artifacts.path().to_str().ok_or("UTF-8 artifacts path")?;
        let assemble = ["assemble", "--tag", "v0.1.0", "--artifacts-dir", directory];
        let output = isolate_git(&mut ag_release(workspace.path(), &assemble)).output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into());
        }

        Ok(Self {
            _workspace: workspace,
            artifacts,
        })
    }

    /// Returns a `.tar.gz` holding `demo-app-<target>/demo-app`, a script
    /// that runs `command`.
    fn archive(target: &str, command: &str) -> io::Result<Vec<u8>> {
        let script = format!("#!/bin/sh\n{command}\n");
        let mut header = tar::Header::new_gnu();
        header.set_size(script.len() as u64);
        header.set_mode(0o755);
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        builder.append_data(
            &mut header,
            format!("demo-app-{target}/demo-app"),
            script.as_bytes(),
        )?;

        builder.into_inner()?.finish()
    }

    fn installer(&self) -> PathBuf {
        self.artifacts.path().join("demo-app-installer.sh")
    }
}

/// An isolated environment to run the installer in.
struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    /// Creates an empty home directory and a `PATH` with every available
    /// host tool in [`TOOLS`].
    fn new() -> io::Result<Self> {
        let sandbox = Self { root: tempdir()? };
        for directory in ["fake", "home", "tmp", "tools"] {
            fs::create_dir(sandbox.path(directory))?;
        }
        let host_path = env::var_os("PATH").unwrap_or_default();
        for tool in TOOLS {
            let found = env::split_paths(&host_path)
                .map(|directory| directory.join(tool))
                .find(|path| path.is_file());
            if let Some(path) = found {
                symlink(path, sandbox.path("tools").join(tool))?;
            }
        }

        Ok(sandbox)
    }

    /// Returns `relative` inside the sandbox.
    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn home(&self) -> PathBuf {
        self.path("home")
    }

    /// Simulates the platform `uname -s` and `uname -m` report, with
    /// `ldd --version` running `ldd`.
    fn platform(&self, system: &str, machine: &str, ldd: &str) -> io::Result<()> {
        self.fake(
            "uname",
            &format!("case \"$1\" in -m) echo {machine} ;; *) echo {system} ;; esac"),
        )?;

        self.fake("ldd", ldd)
    }

    /// Puts an executable `name` running `command` first on `PATH`.
    fn fake(&self, name: &str, command: &str) -> io::Result<()> {
        write_executable(&self.path("fake").join(name), command)
    }

    /// Removes the host tool `name` from `PATH`.
    fn remove_tool(&self, name: &str) -> io::Result<()> {
        match fs::remove_file(self.path("tools").join(name)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }

    /// Returns the sandbox `PATH`, with `first`, when given, before the fake
    /// and host tools.
    fn search_path(&self, first: Option<&Path>) -> io::Result<OsString> {
        let directories = first
            .map(Path::to_path_buf)
            .into_iter()
            .chain([self.path("fake"), self.path("tools")]);

        env::join_paths(directories).map_err(io::Error::other)
    }

    /// Runs `installer` with `<APP>_UNMANAGED_INSTALL` naming a `flat`
    /// directory, and returns the run's output with that directory.
    fn install_flat(&self, installer: &Path) -> io::Result<(Output, PathBuf)> {
        let flat = self.path("flat");
        let output = self.run(
            installer,
            &[("DEMO_APP_UNMANAGED_INSTALL", flat.clone().into_os_string())],
        )?;

        Ok((output, flat))
    }

    /// Runs `installer` with only `HOME`, `PATH`, `TMPDIR`, a download URL
    /// naming the installer's directory, and `variables` set.
    fn run(&self, installer: &Path, variables: &[(&str, OsString)]) -> io::Result<Output> {
        self.run_with_arguments(installer, &[], variables)
    }

    /// Runs `installer` with `arguments` in the environment of [`Self::run`].
    fn run_with_arguments(
        &self,
        installer: &Path,
        arguments: &[&str],
        variables: &[(&str, OsString)],
    ) -> io::Result<Output> {
        let artifacts = installer.parent().unwrap_or(installer);
        let search_path = self.search_path(None)?;

        Command::new("/bin/sh")
            .arg(installer)
            .args(arguments)
            .current_dir(self.root.path())
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", search_path)
            .env("TMPDIR", self.path("tmp"))
            .env(
                "DEMO_APP_DOWNLOAD_URL",
                format!("file://{}", artifacts.display()),
            )
            .envs(variables.iter().cloned())
            .output()
    }
}

/// Writes an executable `sh` script running `command` to `path`.
fn write_executable(path: &Path, command: &str) -> io::Result<()> {
    fs::write(path, format!("#!/bin/sh\n{command}\n"))?;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

/// Returns the lowercase hexadecimal SHA-256 of `contents`.
fn sha256(contents: &[u8]) -> String {
    hex::encode(Sha256::digest(contents))
}

/// Returns standard output of a successful run, or standard error.
fn result(output: &Output) -> Result<String, String> {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    if output.status.success() {
        return Ok(text(&output.stdout));
    }

    Err(text(&output.stderr))
}

/// Returns what the installed binary at `path` prints.
fn run_binary(path: &Path) -> Result<String, String> {
    Command::new(path)
        .output()
        .map_err(|error| format!("cannot run `{}`: {error}", path.display()))
        .and_then(|output| result(&output))
}

/// Returns every file below `directory` with its contents, by relative path.
fn files(directory: &Path) -> io::Result<BTreeMap<String, String>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(directory)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .into_owned();
            files.insert(relative, fs::read_to_string(&path)?);
        }
    }

    Ok(files)
}

/// Returns the receipt the installer writes for a managed install.
fn receipt(prefix: &Path, modify_path: bool) -> String {
    let prefix = prefix.display();
    let version = env!("CARGO_PKG_VERSION");

    [
        r#"{"binaries":["demo-app"],"install_layout":"cargo-home","#,
        &format!(r#""install_prefix":"{prefix}","modify_path":{modify_path},"#),
        &format!(r#""provider":{{"source":"ag-release","version":"{version}"}},"#),
        r#""source":{"app_name":"demo-app","name":"demo-app","owner":"demo-org","#,
        r#""release_type":"github"},"version":"0.1.0"}"#,
        "\n",
    ]
    .concat()
}

/// Returns the `env` script that puts `bin` on `PATH`.
fn env_script(bin: &str) -> String {
    format!(
        "#!/bin/sh\n# add binaries to PATH if they aren't added yet\n# affix colons on either \
         side of $PATH to simplify matching\ncase \":${{PATH}}:\" in\n    *:\"{bin}\":*)\n        \
         ;;\n    *)\n        # Prepending path in case a system-installed binary needs to be \
         overridden\n        export PATH=\"{bin}:$PATH\"\n        ;;\nesac\n"
    )
}

/// Returns the `env.fish` script that puts `bin` on `PATH`.
fn fish_env_script(bin: &str) -> String {
    format!(
        "if not contains \"{bin}\" $PATH\n    # Prepending path in case a system-installed binary \
         needs to be overridden\n    set -x PATH \"{bin}\" $PATH\nend\n"
    )
}

#[test]
fn installer_installs_replaces_and_puts_binaries_on_path_idempotently() -> Result<(), Box<dyn Error>>
{
    // Arrange
    let release = Release::assemble()?;
    let sandbox = Sandbox::new()?;
    let home = sandbox.home();
    let bin = home.join(".cargo/bin");
    fs::create_dir_all(&bin)?;
    fs::write(bin.join("demo-app"), "previous release")?;
    fs::hard_link(bin.join("demo-app"), sandbox.path("previous-demo-app"))?;
    fs::write(home.join(".bashrc"), "# bash settings\n")?;
    let github_path = sandbox.path("github-path");
    let variables = [("GITHUB_PATH", github_path.clone().into_os_string())];

    // Act
    let first = result(&sandbox.run(&release.installer(), &variables)?);
    let first_binary = run_binary(&bin.join("demo-app"));
    let first_files = files(&home)?;
    sandbox.platform("Linux", "x86_64", GLIBC)?;
    let second = result(&sandbox.run(&release.installer(), &variables)?);

    // Assert
    let bin = bin.display().to_string();
    let first = first?;
    assert!(
        first.starts_with(&format!(
            "downloading demo-app 0.1.0 for {HOST_TARGET}\n  {bin}/demo-app\ninstalled demo-app \
             0.1.0 to {bin}\n"
        )),
        "{first}"
    );
    assert!(first.contains("\n    source \"$HOME/.cargo/env\" (sh, bash, zsh)\n"));
    assert_eq!(first_binary, Ok(format!("demo-app {HOST_TARGET}\n")));
    assert_eq!(
        fs::read_to_string(sandbox.path("previous-demo-app"))?,
        "previous release"
    );
    let source_line = "\n. \"$HOME/.cargo/env\"\n";
    let expected_files = [
        (".bashrc", format!("# bash settings\n{source_line}")),
        (
            ".cargo/bin/demo-app",
            format!("#!/bin/sh\necho demo-app {HOST_TARGET}\n"),
        ),
        (".cargo/env", env_script("$HOME/.cargo/bin")),
        (".cargo/env.fish", fish_env_script("$HOME/.cargo/bin")),
        (
            ".config/demo-app/demo-app-receipt.json",
            receipt(&home.join(".cargo"), true),
        ),
        (
            ".config/fish/conf.d/demo-app.env.fish",
            "\nsource \"$HOME/.cargo/env.fish\"\n".to_string(),
        ),
        (".profile", source_line.to_string()),
        (".zshrc", source_line.to_string()),
    ]
    .map(|(path, contents)| (path.to_string(), contents))
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    assert_eq!(first_files, expected_files);
    let second = second?;
    assert!(!second.contains("updated"), "{second}");
    assert!(!second.contains("created"), "{second}");
    assert_eq!(
        run_binary(&home.join(".cargo/bin/demo-app")),
        Ok("demo-app x86_64-unknown-linux-gnu\n".to_string())
    );
    let mut second_files = files(&home)?;
    assert_eq!(
        second_files.remove(".cargo/bin/demo-app"),
        Some("#!/bin/sh\necho demo-app x86_64-unknown-linux-gnu\n".to_string())
    );
    let mut unchanged = first_files;
    unchanged.remove(".cargo/bin/demo-app");
    assert_eq!(second_files, unchanged);
    assert_eq!(fs::read_to_string(github_path)?, format!("{bin}\n{bin}\n"));

    Ok(())
}

#[test]
fn installer_honors_install_locations_and_path_opt_outs() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let opted_out = Sandbox::new()?;
    let custom = Sandbox::new()?;
    let unmanaged = Sandbox::new()?;
    let on_path = Sandbox::new()?;
    for sandbox in [&opted_out, &custom, &unmanaged, &on_path] {
        sandbox.platform("Linux", "x86_64", GLIBC)?;
    }
    let cargo_home = |sandbox: &Sandbox| ("CARGO_HOME", sandbox.path("cargo").into_os_string());
    let on_path_bin = on_path.home().join(".cargo/bin");

    // Act
    let opted_out_run = opted_out.run(
        &release.installer(),
        &[
            cargo_home(&opted_out),
            ("XDG_CONFIG_HOME", opted_out.path("config").into_os_string()),
            ("DEMO_APP_NO_MODIFY_PATH", "1".into()),
        ],
    )?;
    let custom_run = custom.run(
        &release.installer(),
        &[
            cargo_home(&custom),
            (
                "DEMO_APP_INSTALL_DIR",
                custom.path("custom").into_os_string(),
            ),
        ],
    )?;
    let unmanaged_run = unmanaged.run(
        &release.installer(),
        &[
            (
                "DEMO_APP_INSTALL_DIR",
                unmanaged.path("custom").into_os_string(),
            ),
            (
                "DEMO_APP_UNMANAGED_INSTALL",
                unmanaged.path("flat").into_os_string(),
            ),
        ],
    )?;
    let on_path_run = on_path.run(
        &release.installer(),
        &[("PATH", on_path.search_path(Some(&on_path_bin))?)],
    )?;

    // Assert
    let linux_binary = "#!/bin/sh\necho demo-app x86_64-unknown-linux-gnu\n".to_string();
    let only = |entries: &[(&str, &String)]| {
        entries
            .iter()
            .map(|(path, contents)| ((*path).to_string(), (*contents).clone()))
            .collect::<BTreeMap<_, _>>()
    };
    assert!(result(&opted_out_run).is_ok(), "{opted_out_run:?}");
    assert_eq!(files(&opted_out.home())?, BTreeMap::new());
    assert_eq!(
        files(&opted_out.path("cargo"))?,
        only(&[("bin/demo-app", &linux_binary)])
    );
    assert_eq!(
        files(&opted_out.path("config"))?,
        only(&[(
            "demo-app/demo-app-receipt.json",
            &receipt(&opted_out.path("cargo"), false)
        )])
    );
    assert!(result(&custom_run).is_ok(), "{custom_run:?}");
    assert!(!custom.path("cargo").exists());
    let custom_dir = custom.path("custom");
    let custom_bin = custom_dir.join("bin").display().to_string();
    let custom_line = format!("\n. \"{}\"\n", custom_dir.join("env").display());
    let custom_files = files(&custom_dir)?;
    assert_eq!(custom_files["env"], env_script(&custom_bin));
    assert_eq!(custom_files["bin/demo-app"], linux_binary);
    let custom_home = files(&custom.home())?;
    assert_eq!(
        custom_home[".config/demo-app/demo-app-receipt.json"],
        receipt(&custom_dir, true)
    );
    assert_eq!(custom_home[".profile"], custom_line);
    assert!(result(&unmanaged_run).is_ok(), "{unmanaged_run:?}");
    assert_eq!(files(&unmanaged.home())?, BTreeMap::new());
    assert!(!unmanaged.path("custom").exists());
    assert_eq!(
        files(&unmanaged.path("flat"))?,
        only(&[("demo-app", &linux_binary)])
    );
    assert!(result(&on_path_run).is_ok(), "{on_path_run:?}");
    assert_eq!(
        files(&on_path.home())?.into_keys().collect::<Vec<_>>(),
        [
            ".cargo/bin/demo-app",
            ".config/demo-app/demo-app-receipt.json"
        ]
    );

    Ok(())
}

#[test]
fn installer_selects_the_archive_for_the_platform() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let glibc_minimum = "echo 'ldd (Demo GLIBC 2.17-1) 2.17'";
    let cases = [
        (
            "Darwin",
            "arm64",
            GLIBC,
            UNKNOWN_OID,
            "aarch64-apple-darwin",
        ),
        // Rosetta 2 reports x86_64 on Apple silicon.
        ("Darwin", "x86_64", GLIBC, "echo 1", "aarch64-apple-darwin"),
        (
            "Darwin",
            "x86_64",
            GLIBC,
            UNKNOWN_OID,
            "x86_64-apple-darwin",
        ),
        (
            "Linux",
            "aarch64",
            glibc_minimum,
            UNKNOWN_OID,
            "aarch64-unknown-linux-gnu",
        ),
        (
            "Linux",
            "amd64",
            GLIBC,
            UNKNOWN_OID,
            "x86_64-unknown-linux-gnu",
        ),
    ];

    for (system, machine, ldd, sysctl, target) in cases {
        let sandbox = Sandbox::new()?;
        sandbox.platform(system, machine, ldd)?;
        sandbox.fake("sysctl", sysctl)?;

        // Act
        let (output, flat) = sandbox.install_flat(&release.installer())?;

        // Assert
        let platform = format!("{system} {machine} ({sysctl})");
        assert!(result(&output).is_ok(), "{platform}: {output:?}");
        assert_eq!(
            run_binary(&flat.join("demo-app")),
            Ok(format!("demo-app {target}\n")),
            "{platform}"
        );
    }

    Ok(())
}

#[test]
fn installer_rejects_unsupported_platforms() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let musl = "echo 'musl libc (x86_64)' >&2; echo 'Version 1.2.5' >&2; exit 1";
    let cases = [
        (
            "Linux",
            "x86_64",
            musl,
            format!("unsupported platform Linux x86_64 with musl libc; {SUPPORTED}"),
        ),
        (
            "Linux",
            "x86_64",
            "echo 'ldd (GNU libc) 2.12'",
            "demo-app needs glibc 2.17 or newer, but this system has glibc 2.12".to_string(),
        ),
        (
            "Linux",
            "x86_64",
            "echo 'ldd: unknown option'",
            "cannot determine the glibc version; demo-app needs glibc 2.17 or newer".to_string(),
        ),
        (
            "Linux",
            "riscv64",
            GLIBC,
            format!("unsupported platform Linux riscv64; {SUPPORTED}"),
        ),
        (
            "FreeBSD",
            "amd64",
            GLIBC,
            format!("unsupported platform FreeBSD amd64; {SUPPORTED}"),
        ),
    ];

    for (system, machine, ldd, message) in cases {
        let sandbox = Sandbox::new()?;
        sandbox.platform(system, machine, ldd)?;

        // Act
        let (output, flat) = sandbox.install_flat(&release.installer())?;

        // Assert
        assert_eq!(result(&output), Err(format!("error: {message}\n")));
        assert!(!flat.exists(), "{message}");
    }

    Ok(())
}

#[test]
fn installer_rejects_a_32_bit_userland_on_a_64_bit_kernel() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let elf32 = r"printf '\177ELF\001'";
    let elf64 = r"printf '\177ELF\002'";
    let unreadable = "exit 1";
    let rejected = Err(format!(
        "error: unsupported platform Linux aarch64 with a 32-bit userland; {SUPPORTED}\n"
    ));
    // The ELF header of `/proc/self/exe`, then of `/bin/sh`, and whether the
    // installer rejects the platform.
    let cases = [
        (elf32, elf64, true),
        (unreadable, elf32, true),
        (elf64, elf32, false),
        (unreadable, unreadable, false),
    ];

    for (current_exe, shell, is_rejected) in cases {
        let sandbox = Sandbox::new()?;
        sandbox.platform("Linux", "aarch64", GLIBC)?;
        // `head -c 5 <file>` prints the magic number and class of an ELF file.
        sandbox.fake(
            "head",
            &format!(
                "case \"$3\" in /proc/self/exe) {current_exe} ;; /bin/sh) {shell} ;; *) exit 1 ;; \
                 esac"
            ),
        )?;

        // Act
        let (output, flat) = sandbox.install_flat(&release.installer())?;

        // Assert
        let headers = format!("{current_exe} / {shell}");
        if is_rejected {
            assert_eq!(result(&output), rejected, "{headers}");
            assert!(!flat.exists(), "{headers}");
        } else {
            assert!(result(&output).is_ok(), "{headers}: {output:?}");
            assert_eq!(
                run_binary(&flat.join("demo-app")),
                Ok("demo-app aarch64-unknown-linux-gnu\n".to_string()),
                "{headers}"
            );
        }
    }

    Ok(())
}

#[test]
fn installer_downloads_with_curl_or_wget_and_fails_closed() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let original = fs::read(release.artifacts.path().join(LINUX_ARCHIVE))?;
    let tampered = Release::archive("x86_64-unknown-linux-gnu", "echo tampered")?;
    let mirror = tempdir()?;
    let tampered_installer = mirror.path().join("demo-app-installer.sh");
    fs::copy(release.installer(), &tampered_installer)?;
    fs::write(mirror.path().join(LINUX_ARCHIVE), &tampered)?;
    let empty = tempdir()?;
    let empty_installer = empty.path().join("demo-app-installer.sh");
    fs::copy(release.installer(), &empty_installer)?;
    let mismatch = format!(
        "checksum mismatch for {LINUX_ARCHIVE}: expected {}, found {}",
        sha256(&original),
        sha256(&tampered)
    );
    let unverifiable =
        format!("cannot verify {LINUX_ARCHIVE}: neither sha256sum nor shasum is installed");
    let missing = format!(
        "cannot download file://{}/{LINUX_ARCHIVE}",
        empty.path().display()
    );
    let no_downloader = "installing demo-app needs curl or wget".to_string();
    let installer = release.installer();
    let cases = [
        (tampered_installer.as_path(), &[][..], mismatch),
        (&installer, &["sha256sum", "shasum"], unverifiable),
        (&empty_installer, &[], missing),
        (&installer, &["curl"], no_downloader),
    ];

    for (installer, removed_tools, message) in cases {
        let sandbox = Sandbox::new()?;
        sandbox.platform("Linux", "x86_64", GLIBC)?;
        for tool in removed_tools {
            sandbox.remove_tool(tool)?;
        }

        // Act
        let (output, flat) = sandbox.install_flat(installer)?;

        // Assert
        let stderr = result(&output).expect_err("installer failure");
        assert!(stderr.ends_with(&format!("error: {message}\n")), "{stderr}");
        assert!(!flat.exists(), "{message}");
    }

    // Arrange
    let sandbox = Sandbox::new()?;
    sandbox.platform("Linux", "x86_64", GLIBC)?;
    sandbox.remove_tool("curl")?;
    sandbox.fake("wget", "cp \"${4#file://}\" \"$3\"")?;

    // Act
    let (output, flat) = sandbox.install_flat(&installer)?;

    // Assert
    assert!(result(&output).is_ok(), "{output:?}");
    assert_eq!(
        run_binary(&flat.join("demo-app")),
        Ok("demo-app x86_64-unknown-linux-gnu\n".to_string())
    );

    Ok(())
}

#[test]
fn installer_prints_usage_and_rejects_other_arguments() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let usage = [
        "Installs demo-app 0.1.0 from https://github.com/demo-org/demo-app.",
        "",
        "Usage: sh demo-app-installer.sh [--help]",
        "",
        "Environment variables:",
        "  DEMO_APP_DOWNLOAD_URL: base URL of the release archives.",
        "  DEMO_APP_INSTALL_DIR: install prefix; binaries go to its bin directory.",
        "  DEMO_APP_UNMANAGED_INSTALL: directory to install the binaries into",
        "    directly, without a receipt or PATH changes.",
        "  DEMO_APP_NO_MODIFY_PATH: 1 leaves PATH and shell startup files unchanged.",
        "",
    ]
    .join("\n");
    let cases = [
        ("--help", Ok(usage.clone())),
        ("-h", Ok(usage)),
        (
            "--no-modify-path",
            Err(
                "error: unknown argument --no-modify-path; set DEMO_APP_NO_MODIFY_PATH=1 instead\n"
                    .to_string(),
            ),
        ),
        (
            "--verbose",
            Err("error: unknown argument --verbose; the only argument is --help\n".to_string()),
        ),
    ];

    for (argument, expected) in cases {
        let sandbox = Sandbox::new()?;
        sandbox.platform("Linux", "x86_64", GLIBC)?;

        // Act
        let output = sandbox.run_with_arguments(&release.installer(), &[argument], &[])?;

        // Assert
        assert_eq!(result(&output), expected, "{argument}");
        assert_eq!(files(&sandbox.home())?, BTreeMap::new(), "{argument}");
        assert_eq!(fs::read_dir(sandbox.path("tmp"))?.count(), 0, "{argument}");
    }

    Ok(())
}

#[test]
fn installer_downloads_with_wget_when_curl_is_a_snap() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble()?;
    let with_wget = Sandbox::new()?;
    let without_wget = Sandbox::new()?;
    for sandbox in [&with_wget, &without_wget] {
        sandbox.platform("Linux", "x86_64", GLIBC)?;
        fs::create_dir_all(sandbox.path("snap/bin"))?;
        // A strictly confined snap cannot write into the installer's work
        // directory.
        write_executable(
            &sandbox.path("snap/bin/curl"),
            "echo 'curl: cannot write' >&2; exit 23",
        )?;
    }
    with_wget.fake("wget", "cp \"${4#file://}\" \"$3\"")?;
    let variables = |sandbox: &Sandbox| -> io::Result<[(&str, OsString); 2]> {
        Ok([
            (
                "PATH",
                sandbox.search_path(Some(&sandbox.path("snap/bin")))?,
            ),
            (
                "DEMO_APP_UNMANAGED_INSTALL",
                sandbox.path("flat").into_os_string(),
            ),
        ])
    };

    // Act
    let with_wget_run = with_wget.run(&release.installer(), &variables(&with_wget)?)?;
    let without_wget_run = without_wget.run(&release.installer(), &variables(&without_wget)?)?;

    // Assert
    assert!(result(&with_wget_run).is_ok(), "{with_wget_run:?}");
    assert_eq!(
        run_binary(&with_wget.path("flat/demo-app")),
        Ok("demo-app x86_64-unknown-linux-gnu\n".to_string())
    );
    let snap_curl = without_wget.path("snap/bin/curl");
    assert_eq!(
        result(&without_wget_run),
        Err(format!(
            "error: {} is a snap, which cannot write the installer's temporary files; install \
             wget or a curl that is not a snap\n",
            snap_curl.display()
        ))
    );
    assert!(!without_wget.path("flat").exists());

    Ok(())
}
