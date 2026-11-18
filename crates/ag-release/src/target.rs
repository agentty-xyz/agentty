//! Supported release target triples.

use std::fmt;

use serde::{Serialize, Serializer};

/// A target triple that release archives and installers support.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Target {
    /// macOS on Apple Silicon.
    Aarch64AppleDarwin,
    /// Linux with glibc on 64-bit ARM.
    Aarch64UnknownLinuxGnu,
    /// macOS on Intel.
    X8664AppleDarwin,
    /// Linux with glibc on x86-64.
    X8664UnknownLinuxGnu,
}

impl Target {
    /// Every supported target, in triple order.
    pub(crate) const ALL: [Self; 4] = [
        Self::Aarch64AppleDarwin,
        Self::Aarch64UnknownLinuxGnu,
        Self::X8664AppleDarwin,
        Self::X8664UnknownLinuxGnu,
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
            Self::X8664AppleDarwin => "x86_64-apple-darwin",
            Self::X8664UnknownLinuxGnu => "x86_64-unknown-linux-gnu",
        }
    }

    /// Returns the platform name that release notes show for this target.
    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin => "Apple Silicon macOS",
            Self::Aarch64UnknownLinuxGnu => "ARM64 Linux",
            Self::X8664AppleDarwin => "Intel macOS",
            Self::X8664UnknownLinuxGnu => "x64 Linux",
        }
    }

    /// Returns the operating system name `uname -s` prints on this target.
    pub(crate) fn uname_system(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin | Self::X8664AppleDarwin => "Darwin",
            Self::Aarch64UnknownLinuxGnu | Self::X8664UnknownLinuxGnu => "Linux",
        }
    }

    /// Returns every machine name `uname -m` may print on this target.
    pub(crate) fn uname_machines(self) -> &'static [&'static str] {
        match self {
            Self::Aarch64AppleDarwin | Self::Aarch64UnknownLinuxGnu => &["arm64", "aarch64"],
            Self::X8664AppleDarwin | Self::X8664UnknownLinuxGnu => &["x86_64", "amd64"],
        }
    }

    /// Returns Node's `process.platform` on this target, which npm's `os`
    /// field also uses.
    pub(crate) fn node_platform(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin | Self::X8664AppleDarwin => "darwin",
            Self::Aarch64UnknownLinuxGnu | Self::X8664UnknownLinuxGnu => "linux",
        }
    }

    /// Returns Node's `process.arch` on this target, which npm's `cpu` field
    /// also uses.
    pub(crate) fn node_arch(self) -> &'static str {
        match self {
            Self::Aarch64AppleDarwin | Self::Aarch64UnknownLinuxGnu => "arm64",
            Self::X8664AppleDarwin | Self::X8664UnknownLinuxGnu => "x64",
        }
    }

    /// Returns whether the target links against glibc, whose minimum version
    /// installers enforce.
    pub(crate) fn is_glibc(self) -> bool {
        self.uname_system() == "Linux"
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

#[cfg(test)]
#[path = "target_test.rs"]
mod tests;
