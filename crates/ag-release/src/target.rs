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
