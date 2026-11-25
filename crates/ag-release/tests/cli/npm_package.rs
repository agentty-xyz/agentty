//! The npm package that `ag-release assemble` writes, installed for real with
//! `npm install --global --prefix <temporary directory>` and run.
//!
//! Every npm and Node run happens in a [`Sandbox`]: a cleared environment
//! with a temporary `HOME`, npm cache, npm configuration, and global prefix,
//! offline, and a `PATH` of symbolic links to the host tools the package
//! needs, preceded by fake executables. Archives download from `file://`
//! URLs or from a loopback HTTP server. A `--require` preload can make the
//! package's own scripts see another platform, while npm sees the host.

use std::error::Error;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::{env, fs, io, thread};

use tempfile::{TempDir, tempdir};

use crate::support::{
    HOST_TARGET, Release, SUPPORTED, link_tools, mixed_libc_manifest, result, sha256,
};

/// Root manifest of a demo workspace released with the npm installer for
/// every supported target.
const MANIFEST: &str = r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
installers = ["npm"]
min-glibc = "2.17"

[workspace.metadata.release.targets]
aarch64-apple-darwin = "demo-runner"
aarch64-unknown-linux-gnu = "demo-runner"
x86_64-apple-darwin = "demo-runner"
x86_64-unknown-linux-gnu = "demo-runner"

[workspace.metadata.release.apps.demo-app]
"#;

/// The npm package tarball `assemble` writes.
const PACKAGE: &str = "demo-app-npm-package.tar.gz";

/// Host tools that npm, the package, and the demo binary run.
const TOOLS: [&str; 8] = ["cat", "curl", "gzip", "node", "npm", "sh", "sysctl", "tar"];

/// `sysctl -n hw.optional.arm64` of a Mac without Apple silicon.
const UNKNOWN_OID: &str = "echo 'sysctl: unknown oid' >&2; exit 1";

/// Assembles a release whose `demo-app` binary prints its target, its path,
/// and its arguments, then acts on them: `exit <status>`, `signal <name>` to
/// end by that signal, `stdin` to copy standard input, or `trap <name>` to
/// exit with status 7 on that signal while waiting for it.
///
/// `trap` waits with the `wait` built-in on a background `cat` of standard
/// input, which the trap stops. A trapped signal interrupts `wait` even when
/// it arrives just before `wait` starts; `read` can miss one arriving just
/// before it blocks, as in `dash`, and then wait for input forever.
fn release() -> Result<Release, Box<dyn Error>> {
    Release::assemble(MANIFEST, |target| {
        [
            &format!(r#"printf '%s\n' "demo-app {target}" "$0" "$@""#),
            r#"case "$1" in"#,
            r#"exit) exit "$2" ;;"#,
            r#"signal) kill -"$2" $$ ;;"#,
            "stdin) cat ;;",
            r#"trap) trap 'echo "got $2"; kill $!; exit 7' "$2"; exec 3<&0"#,
            "cat <&3 >/dev/null & echo ready; wait ;;",
            "esac",
        ]
        .join("\n")
    })
}

/// Returns the archive the host platform installs.
fn host_archive() -> String {
    format!("demo-app-{HOST_TARGET}.tar.gz")
}

/// Returns the `file://` URL of `directory`.
fn file_url(directory: &Path) -> String {
    format!("file://{}", directory.display())
}

/// An isolated environment for npm, Node, and the installed command.
struct Sandbox {
    /// Download base URL of the release archives.
    download_url: String,
    root: TempDir,
}

impl Sandbox {
    /// Creates empty home, cache, and prefix directories and a `PATH` with
    /// every available host tool in [`TOOLS`], downloading from `release`.
    fn new(release: &Release) -> io::Result<Self> {
        let sandbox = Self {
            download_url: file_url(release.artifacts.path()),
            root: tempdir()?,
        };
        for directory in ["cache", "fake", "home", "prefix", "tmp", "tools"] {
            fs::create_dir(sandbox.path(directory))?;
        }
        link_tools(&sandbox.path("tools"), &TOOLS)?;

        Ok(sandbox)
    }

    /// Returns `relative` inside the sandbox.
    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    /// Returns the installed package directory, as Agentty finds it:
    /// `npm root --global` joined with the package name.
    fn package_dir(&self) -> Result<PathBuf, String> {
        let root = result(
            &self
                .npm(&["root", "--global"])
                .map_err(|error| error.to_string())?,
        )?;

        Ok(Path::new(root.trim()).join("demo-app"))
    }

    /// Returns where the package keeps the `demo-app` binary.
    fn installed_binary(&self) -> Result<PathBuf, String> {
        Ok(self.package_dir()?.join("node_modules/.bin_real/demo-app"))
    }

    /// Makes the package's scripts see `platform`, `arch`, and the glibc
    /// `glibc`, or no glibc.
    fn platform(&self, platform: &str, arch: &str, glibc: Option<&str>) -> io::Result<()> {
        let glibc = glibc.map_or_else(|| "undefined".to_string(), |version| format!("{version:?}"));
        let preload = [
            r#"const { basename } = require("path");"#,
            r#"const { realpathSync } = require("fs");"#,
            r#"const script = process.argv[1] ? basename(realpathSync(process.argv[1])) : "";"#,
            r#"if (script === "install.js" || script.startsWith("run-")) {"#,
            &format!(r#"  Object.defineProperty(process, "platform", {{ value: "{platform}" }});"#),
            &format!(r#"  Object.defineProperty(process, "arch", {{ value: "{arch}" }});"#),
            &format!(
                "  process.report.getReport = () => ({{ header: {{ glibcVersionRuntime: {glibc} \
                 }} }});"
            ),
            "}",
            "",
        ];

        fs::write(self.path("platform.js"), preload.join("\n"))
    }

    /// Puts an executable `name` running `command` first on `PATH`.
    fn fake(&self, name: &str, command: &str) -> io::Result<()> {
        let path = self.path("fake").join(name);
        fs::write(&path, format!("#!/bin/sh\n{command}\n"))?;

        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
    }

    /// Returns `program` with only the sandbox's variables set.
    fn command(&self, program: impl AsRef<OsStr>) -> io::Result<Command> {
        let search_path =
            env::join_paths([self.path("fake"), self.path("tools")]).map_err(io::Error::other)?;
        let mut command = Command::new(program);
        command
            .current_dir(self.root.path())
            .env_clear()
            .env("DEMO_APP_DOWNLOAD_URL", &self.download_url)
            .env("HOME", self.path("home"))
            .env("PATH", search_path)
            .env("TMPDIR", self.path("tmp"))
            .env("npm_config_audit", "false")
            .env("npm_config_cache", self.path("cache"))
            .env("npm_config_fund", "false")
            .env("npm_config_globalconfig", self.path("npmrc"))
            .env("npm_config_offline", "true")
            .env("npm_config_prefix", self.path("prefix"))
            .env("npm_config_update_notifier", "false")
            .env("npm_config_userconfig", self.path("home/.npmrc"));
        let preload = self.path("platform.js");
        if preload.is_file() {
            command.env("NODE_OPTIONS", format!("--require {}", preload.display()));
        }

        Ok(command)
    }

    /// Runs `npm` with `args`.
    fn npm(&self, args: &[&str]) -> io::Result<Output> {
        self.command(self.path("tools/npm"))?.args(args).output()
    }

    /// Installs the release's package globally into the sandbox prefix.
    fn install(&self, release: &Release, args: &[&str]) -> io::Result<Output> {
        let prefix = self.path("prefix");
        let tarball = release.artifact(PACKAGE);
        let mut command = self.command(self.path("tools/npm"))?;

        command
            .args(["install", "--global", "--prefix"])
            .arg(prefix)
            .args(args)
            .arg(tarball)
            .output()
    }

    /// Returns the installed `demo-app` command with `args`.
    fn demo_app(&self, args: &[&str]) -> io::Result<Command> {
        let mut command = self.command(self.path("prefix/bin/demo-app"))?;
        command.args(args);

        Ok(command)
    }

    /// Runs the installed `demo-app` with `args`.
    fn run(&self, args: &[&str]) -> io::Result<Output> {
        self.demo_app(args)?.output()
    }
}

/// Serves the files of `directory` over HTTP on a loopback port until the
/// test process exits, and returns the base URL.
fn serve(directory: &Path) -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let directory = directory.to_path_buf();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            // A failed response fails the download the test checks.
            drop(respond(stream, &directory));
        }
    });

    Ok(url)
}

/// Answers one HTTP request with the requested file, or 404.
fn respond(mut stream: TcpStream, directory: &Path) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    let name = request.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match fs::read(directory.join(name.trim_start_matches('/'))) {
        Ok(body) => ("200 OK", body),
        Err(_) => ("404 Not Found", Vec::new()),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;

    stream.write_all(&body)
}

#[test]
fn installed_command_runs_the_binary_from_the_bin_real_directory() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let sandbox = Sandbox::new(&release)?;

    // Act
    let installed = sandbox.install(&release, &[])?;
    let arguments = result(&sandbox.run(&["two words", "--flag"])?);
    let status = sandbox.run(&["exit", "3"])?.status;
    let terminated = sandbox.run(&["signal", "TERM"])?.status;
    let interrupted = sandbox.run(&["signal", "INT"])?.status;
    let mut child = sandbox
        .demo_app(&["stdin"])?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("piped standard input")?
        .write_all(b"typed input\n")?;
    let echoed = result(&child.wait_with_output()?);

    // Assert
    assert!(result(&installed).is_ok(), "{installed:?}");
    let binary = sandbox.installed_binary()?;
    assert!(binary.is_file(), "{}", binary.display());
    let arguments = arguments?;
    let lines = arguments.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 4, "{arguments}");
    assert_eq!(lines[0], format!("demo-app {HOST_TARGET}"));
    // Agentty recognizes npm installs by the running executable's path.
    assert_eq!(fs::canonicalize(lines[1])?, fs::canonicalize(&binary)?);
    assert_eq!(lines[2..], ["two words", "--flag"]);
    assert_eq!(status.code(), Some(3));
    assert_eq!(terminated.signal(), Some(15));
    assert_eq!(interrupted.signal(), Some(2));
    assert!(echoed?.ends_with("stdin\ntyped input\n"));

    Ok(())
}

#[test]
fn termination_requests_to_the_command_reach_the_binary() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let sandbox = Sandbox::new(&release)?;
    let installed = sandbox.install(&release, &[])?;
    let mut child = sandbox
        .demo_app(&["trap", "TERM"])?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("piped standard output")?);
    let mut started = String::new();
    while !started.ends_with("ready\n") && stdout.read_line(&mut started)? > 0 {}

    // Act
    let killed = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()?;
    let mut rest = String::new();
    stdout.read_to_string(&mut rest)?;
    let status = child.wait()?;

    // Assert
    assert!(result(&installed).is_ok(), "{installed:?}");
    assert!(started.ends_with("\ntrap\nTERM\nready\n"), "{started}");
    assert!(killed.success());
    assert_eq!(rest, "got TERM\n");
    assert_eq!(status.code(), Some(7));

    Ok(())
}

#[test]
fn first_run_downloads_the_binary_when_install_scripts_are_skipped() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let mut sandbox = Sandbox::new(&release)?;
    let installed = sandbox.install(&release, &["--ignore-scripts"])?;
    let binary = sandbox.installed_binary()?;
    let skipped = binary.exists();
    sandbox.download_url = serve(release.artifacts.path())?;

    // Act
    let first = sandbox.run(&[])?;
    let installed_binary = binary.is_file();
    sandbox.download_url = file_url(&sandbox.path("empty"));
    let second = sandbox.run(&[])?;

    // Assert
    assert!(result(&installed).is_ok(), "{installed:?}");
    assert!(!skipped);
    assert!(result(&first).is_ok(), "{first:?}");
    let binary_dir = fs::canonicalize(binary.parent().ok_or("binary directory")?)?;
    let binary_dir = binary_dir.display();
    assert_eq!(
        String::from_utf8_lossy(&first.stderr),
        format!(
            "downloading demo-app 0.1.0 for {HOST_TARGET}\ninstalled demo-app 0.1.0 to \
             {binary_dir}\n"
        )
    );
    assert!(installed_binary);
    assert!(result(&second).is_ok(), "{second:?}");
    assert_eq!(String::from_utf8_lossy(&second.stderr), "");

    Ok(())
}

#[test]
fn failed_downloads_and_checksum_mismatches_install_nothing() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let original = fs::read(release.artifact(&host_archive()))?;
    let tampered = Release::archive(HOST_TARGET, "echo tampered")?;
    let mirror = tempdir()?;
    fs::write(mirror.path().join(host_archive()), &tampered)?;
    let empty = tempdir()?;
    let mut postinstall = Sandbox::new(&release)?;
    postinstall.download_url = file_url(mirror.path());
    let mut first_run = Sandbox::new(&release)?;
    let skipped = first_run.install(&release, &["--ignore-scripts"])?;
    let mismatch = format!(
        "checksum mismatch for {}: expected {}, found {}",
        host_archive(),
        sha256(&original),
        sha256(&tampered)
    );
    let missing = format!(
        "cannot download {}/{}: ",
        file_url(empty.path()),
        host_archive()
    );

    // Act
    let rejected = postinstall.install(&release, &[])?;
    first_run.download_url = file_url(mirror.path());
    let mismatched_run = first_run.run(&[])?;
    first_run.download_url = file_url(empty.path());
    let missing_run = first_run.run(&[])?;

    // Assert
    let rejected_stderr = result(&rejected).expect_err("rejected install");
    assert!(
        rejected_stderr.contains(&format!("error: {mismatch}\n")),
        "{rejected_stderr}"
    );
    assert!(!postinstall.package_dir()?.exists());
    assert!(!postinstall.path("prefix/bin/demo-app").exists());
    assert!(result(&skipped).is_ok(), "{skipped:?}");
    assert_eq!(mismatched_run.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&mismatched_run.stderr),
        format!("downloading demo-app 0.1.0 for {HOST_TARGET}\nerror: {mismatch}\n")
    );
    let missing_stderr = result(&missing_run).expect_err("missing archive");
    assert!(
        missing_stderr.contains(&format!("\nerror: {missing}")),
        "{missing_stderr}"
    );
    assert!(!first_run.installed_binary()?.exists());

    Ok(())
}

#[test]
fn proxy_variables_route_downloads_through_curl() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let sandbox = Sandbox::new(&release)?;
    let installed = sandbox.install(&release, &["--ignore-scripts"])?;
    let real_curl = fs::read_link(sandbox.path("tools/curl"))?;
    fs::remove_file(sandbox.path("tools/curl"))?;
    let logged_curl = format!(
        "printf '%s\\n' \"$@\" >\"{}\"\nexec \"{}\" \"$@\"",
        sandbox.path("curl.log").display(),
        real_curl.display()
    );
    let missing = format!(
        "cannot download {}/{}: curl failed: cannot run curl: ",
        sandbox.download_url,
        host_archive()
    );

    // Act
    let mut without_curl = Vec::new();
    for variable in [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "npm_config_https_proxy",
        "npm_config_proxy",
    ] {
        let output = sandbox
            .demo_app(&[])?
            .env(variable, "http://127.0.0.1:9")
            .output()?;
        without_curl.push((variable, output));
    }
    sandbox.fake("curl", &logged_curl)?;
    let with_curl = sandbox
        .demo_app(&[])?
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .output()?;
    let standard_arguments = fs::read_to_string(sandbox.path("curl.log"))?;
    let standard_binary = sandbox.installed_binary()?.is_file();
    fs::remove_file(sandbox.installed_binary()?)?;
    // npm's settings take precedence over the standard variables.
    let with_npm_config = sandbox
        .demo_app(&[])?
        .env("npm_config_https_proxy", "http://127.0.0.1:8")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("npm_config_noproxy", "demo.invalid")
        .output()?;
    let npm_config_arguments = fs::read_to_string(sandbox.path("curl.log"))?;

    // Assert
    assert!(result(&installed).is_ok(), "{installed:?}");
    for (variable, output) in without_curl {
        let stderr = result(&output).expect_err("download without curl");
        assert!(
            stderr.contains(&format!("\nerror: {missing}")),
            "{variable}: {stderr}"
        );
    }
    let source = format!("{}/{}\n", sandbox.download_url, host_archive());
    assert!(result(&with_curl).is_ok(), "{with_curl:?}");
    assert!(
        standard_arguments.starts_with("--proxy\nhttp://127.0.0.1:9\n--fail\n"),
        "{standard_arguments}"
    );
    assert!(
        standard_arguments.ends_with(&source),
        "{standard_arguments}"
    );
    assert!(standard_binary);
    assert!(result(&with_npm_config).is_ok(), "{with_npm_config:?}");
    assert!(
        npm_config_arguments
            .starts_with("--proxy\nhttp://127.0.0.1:8\n--noproxy\ndemo.invalid\n--fail\n"),
        "{npm_config_arguments}"
    );
    assert!(
        npm_config_arguments.ends_with(&source),
        "{npm_config_arguments}"
    );
    assert!(sandbox.installed_binary()?.is_file());

    Ok(())
}

/// A simulated `"<process.platform> <process.arch>"`, glibc version, and
/// `sysctl` command, with the target whose binary runs or the error.
type PlatformCase<'case> = (
    &'case str,
    Option<&'case str>,
    &'case str,
    Result<String, String>,
);

/// Installs `release`'s package without install scripts, then for each case
/// runs the command on that simulated platform and checks that it installs
/// and runs the expected target's binary or fails with the expected error.
fn assert_platform_detection(
    release: &Release,
    cases: Vec<PlatformCase<'_>>,
) -> Result<(), Box<dyn Error>> {
    let sandbox = Sandbox::new(release)?;
    // `--force` installs a package whose `os` field excludes the host.
    let installed = sandbox.install(release, &["--ignore-scripts", "--force"])?;
    let binary = sandbox.installed_binary()?;
    assert!(result(&installed).is_ok(), "{installed:?}");

    for (simulated_platform, glibc, sysctl, expected) in cases {
        let simulated = format!("{simulated_platform} {glibc:?} ({sysctl})");
        let (platform, arch) = simulated_platform
            .split_once(' ')
            .ok_or("platform and arch")?;
        sandbox.platform(platform, arch, glibc)?;
        sandbox.fake("sysctl", sysctl)?;
        if let Some(directory) = binary.parent().filter(|directory| directory.exists()) {
            fs::remove_dir_all(directory)?;
        }

        let output = result(&sandbox.run(&[])?);

        let output = output
            .map(|stdout| stdout.lines().next().unwrap_or_default().to_string())
            .map_err(|stderr| {
                stderr
                    .strip_prefix("error: ")
                    .unwrap_or(&stderr)
                    .trim_end()
                    .to_string()
            });
        assert_eq!(output, expected, "{simulated}");
        assert_eq!(binary.is_file(), expected.is_ok(), "{simulated}");
    }

    Ok(())
}

/// Returns the first output line of the demo binary for `target`.
fn selected(target: &str) -> String {
    format!("demo-app {target}")
}

#[test]
fn platform_detection_selects_the_archive_or_fails_clearly() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let unsupported = |platform: &str| Err(format!("unsupported platform {platform}; {SUPPORTED}"));
    let old_glibc = "demo-app needs glibc 2.17 or newer, but this system has glibc 2.12";
    let unknown_glibc = "cannot determine the glibc version; demo-app needs glibc 2.17 or newer";
    // Simulated `process.platform` and `process.arch`, glibc, and `sysctl`.
    let cases = [
        (
            "darwin arm64",
            None,
            UNKNOWN_OID,
            Ok(selected("aarch64-apple-darwin")),
        ),
        // Rosetta 2 runs Node for x64 on Apple silicon.
        (
            "darwin x64",
            None,
            "echo 1",
            Ok(selected("aarch64-apple-darwin")),
        ),
        (
            "darwin x64",
            None,
            UNKNOWN_OID,
            Ok(selected("x86_64-apple-darwin")),
        ),
        (
            "linux arm64",
            Some("2.17"),
            UNKNOWN_OID,
            Ok(selected("aarch64-unknown-linux-gnu")),
        ),
        (
            "linux x64",
            Some("2.39"),
            UNKNOWN_OID,
            Ok(selected("x86_64-unknown-linux-gnu")),
        ),
        (
            "linux x64",
            None,
            UNKNOWN_OID,
            unsupported("linux x64 without glibc"),
        ),
        (
            "linux x64",
            Some("2.12"),
            UNKNOWN_OID,
            Err(old_glibc.to_string()),
        ),
        (
            "linux x64",
            Some("unknown"),
            UNKNOWN_OID,
            Err(unknown_glibc.to_string()),
        ),
        (
            "linux riscv64",
            Some("2.39"),
            UNKNOWN_OID,
            unsupported("linux riscv64"),
        ),
        ("freebsd x64", None, UNKNOWN_OID, unsupported("freebsd x64")),
    ];

    // Act and Assert
    assert_platform_detection(&release, cases.into())
}

#[test]
fn platform_detection_prefers_glibc_and_falls_back_to_static_musl() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = Release::assemble(&mixed_libc_manifest(r#""npm""#), |target| {
        format!("echo demo-app {target}")
    })?;
    let cases = [
        (
            "linux x64",
            Some("2.39"),
            UNKNOWN_OID,
            Ok(selected("x86_64-unknown-linux-gnu")),
        ),
        (
            "linux x64",
            Some("2.12"),
            UNKNOWN_OID,
            Ok(selected("x86_64-unknown-linux-musl")),
        ),
        (
            "linux x64",
            Some("unknown"),
            UNKNOWN_OID,
            Ok(selected("x86_64-unknown-linux-musl")),
        ),
        (
            "linux x64",
            None,
            UNKNOWN_OID,
            Ok(selected("x86_64-unknown-linux-musl")),
        ),
        (
            "linux arm64",
            Some("2.39"),
            UNKNOWN_OID,
            Ok(selected("aarch64-unknown-linux-musl")),
        ),
        (
            "linux arm64",
            None,
            UNKNOWN_OID,
            Ok(selected("aarch64-unknown-linux-musl")),
        ),
    ];

    // Act and Assert
    assert_platform_detection(&release, cases.into())
}

#[test]
fn npm_rejects_platforms_without_a_target() -> Result<(), Box<dyn Error>> {
    // Arrange
    let release = release()?;
    let sandbox = Sandbox::new(&release)?;
    fs::write(
        sandbox.path("platform.js"),
        r#"Object.defineProperty(process, "platform", { value: "freebsd" });"#,
    )?;

    // Act
    let rejected = sandbox.install(&release, &[])?;

    // Assert
    let stderr = result(&rejected).expect_err("rejected install");
    assert!(stderr.contains("npm error code EBADPLATFORM\n"), "{stderr}");
    assert!(stderr.contains("Valid os:   darwin,linux\n"), "{stderr}");
    assert!(!sandbox.path("prefix/bin/demo-app").exists());

    Ok(())
}
