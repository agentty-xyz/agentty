//! Supported release target triples.

use std::fmt;

use serde::{Serialize, Serializer};

/// A target triple that release archives and installers support.
///
/// Variants are declared in triple order, which their derived ordering
/// follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Target {
    /// macOS on Apple Silicon.
    Aarch64AppleDarwin,
    /// Linux with glibc on 64-bit ARM.
    Aarch64UnknownLinuxGnu,
    /// Statically linked Linux with musl on 64-bit ARM.
    Aarch64UnknownLinuxMusl,
    /// macOS on Intel.
    X8664AppleDarwin,
    /// Linux with glibc on x86-64.
    X8664UnknownLinuxGnu,
    /// Statically linked Linux with musl on x86-64.
    X8664UnknownLinuxMusl,
}

impl Target {
    /// Every supported target, in triple order.
    pub(crate) const ALL: [Self; 6] = [
        Self::Aarch64AppleDarwin,
        Self::Aarch64UnknownLinuxGnu,
        Self::Aarch64UnknownLinuxMusl,
        Self::X8664AppleDarwin,
        Self::X8664UnknownLinuxGnu,
        Self::X8664UnknownLinuxMusl,
    ];

    /// Returns the supported target spelled `triple`, if any.
    pub(crate) fn from_triple(triple: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|target| target.triple() == triple)
    }

    /// Returns the Rust target triple.
    pub(crate) fn triple(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin => "aarch64-apple-darwin",
            Self::Aarch64UnknownLinuxGnu => "aarch64-unknown-linux-gnu",
            Self::Aarch64UnknownLinuxMusl => "aarch64-unknown-linux-musl",
            Self::X8664AppleDarwin => "x86_64-apple-darwin",
            Self::X8664UnknownLinuxGnu => "x86_64-unknown-linux-gnu",
            Self::X8664UnknownLinuxMusl => "x86_64-unknown-linux-musl",
        }
    }

    /// Returns the platform name that release notes show for this target.
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin => "Apple Silicon macOS",
            Self::Aarch64UnknownLinuxGnu => "ARM64 Linux",
            Self::Aarch64UnknownLinuxMusl => "ARM64 Linux (static musl)",
            Self::X8664AppleDarwin => "Intel macOS",
            Self::X8664UnknownLinuxGnu => "x64 Linux",
            Self::X8664UnknownLinuxMusl => "x64 Linux (static musl)",
        }
    }

    /// Returns the operating system name `uname -s` prints on this target.
    pub(crate) fn uname_system(self) -> &'static str {
        match self.libc() {
            Libc::None => "Darwin",
            Libc::Glibc | Libc::Musl => "Linux",
        }
    }

    /// Returns every machine name `uname -m` may print on this target.
    pub(crate) fn uname_machines(self) -> &'static [&'static str] {
        if self.is_aarch64() {
            &["arm64", "aarch64"]
        } else {
            &["x86_64", "amd64"]
        }
    }

    /// Returns Node's `process.platform` on this target, which npm's `os`
    /// field also uses.
    pub(crate) fn node_platform(self) -> &'static str {
        match self.libc() {
            Libc::None => "darwin",
            Libc::Glibc | Libc::Musl => "linux",
        }
    }

    /// Returns Node's `process.arch` on this target, which npm's `cpu` field
    /// also uses.
    pub(crate) fn node_arch(self) -> &'static str {
        if self.is_aarch64() { "arm64" } else { "x64" }
    }

    /// Returns the C library the target's binaries use, which installers
    /// match against the system's.
    pub(crate) fn libc(self) -> Libc {
        match self {
            Self::Aarch64AppleDarwin | Self::X8664AppleDarwin => Libc::None,
            Self::Aarch64UnknownLinuxGnu | Self::X8664UnknownLinuxGnu => Libc::Glibc,
            Self::Aarch64UnknownLinuxMusl | Self::X8664UnknownLinuxMusl => Libc::Musl,
        }
    }

    /// Returns whether the target is 64-bit ARM rather than x86-64.
    pub(crate) fn is_aarch64(self) -> bool {
        self.triple().starts_with("aarch64-")
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.triple())
    }
}

/// Serializes as the target triple.
impl Serialize for Target {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.triple())
    }
}

/// The C library of a target's binaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Libc {
    /// Dynamically linked glibc, whose minimum version installers enforce.
    Glibc,
    /// Statically linked musl, which runs on any Linux with the target's
    /// architecture.
    Musl,
    /// Not a Linux target.
    None,
}

impl Libc {
    /// Returns the name installers match against the system's C library:
    /// `gnu`, `musl`, or `none`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Glibc => "gnu",
            Self::Musl => "musl",
            Self::None => "none",
        }
    }
}

#[cfg(test)]
#[path = "target_test.rs"]
mod tests;
