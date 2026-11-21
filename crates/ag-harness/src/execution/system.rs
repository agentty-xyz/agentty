//! The platform's system directories that `BashConfig::with_system_reads`
//! grants: what Bash, Git, and developer toolchains need to start.

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

/// System directories granted on macOS. `/usr` holds the shared libraries
/// and Apple's tool shims, `/System` the frameworks and dyld cache,
/// `/Library/Developer` the Command Line Tools those shims run, and
/// `/private/var/select` the shell `/bin/sh` starts.
#[cfg(target_os = "macos")]
const SYSTEM_READS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/System",
    "/Library/Developer",
    // System frameworks installed apart from the sealed system volume, such
    // as `MobileDevice`, which Xcode's tools load through links in
    // `/System/Library`.
    "/Library/Apple",
    // Where a selected Xcode records its accepted license, which its tool
    // shims check before running.
    "/Library/Preferences/com.apple.dt.Xcode.plist",
    "/private/etc",
    "/private/var/db/timezone",
    "/private/var/db/xcode_select_link",
    "/private/var/select",
    "/dev/random",
    "/dev/urandom",
    "/dev/zero",
];

/// System directories granted on Linux; the sandbox adds a private `/proc`
/// and a minimal `/dev` beside them.
#[cfg(target_os = "linux")]
const SYSTEM_READS: &[&str] = &[
    "/bin", "/sbin", "/usr", "/lib", "/lib32", "/lib64", "/libx32", "/etc",
];

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const SYSTEM_READS: &[&str] = &[];

/// Returns the system directories present on this host, including on macOS
/// the Xcode that `xcode-select` chose, when it is not the Command Line
/// Tools.
pub(super) fn reads() -> Vec<PathBuf> {
    let reads = SYSTEM_READS.iter().map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let reads = reads.chain(selected_xcode(Path::new(
        "/private/var/db/xcode_select_link",
    )));

    reads.filter(|path| path.exists()).collect()
}

/// Returns the application bundle of the developer directory `link` selects
/// when it lies in one, such as `/Applications/Xcode.app` for
/// `/Applications/Xcode.app/Contents/Developer`.
#[cfg(target_os = "macos")]
fn selected_xcode(link: &Path) -> Option<PathBuf> {
    let developer = std::fs::canonicalize(link).ok()?;
    let bundle = developer.parent()?.parent()?;

    bundle
        .extension()
        .is_some_and(|extension| extension == "app")
        .then(|| bundle.to_path_buf())
}

#[cfg(test)]
#[path = "system_test.rs"]
mod tests;
