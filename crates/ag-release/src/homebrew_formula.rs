//! The Homebrew formula published as `<app>.rb`.
//!
//! The formula is rendered from `src/template/homebrew_formula.rb` with the
//! release's archive URLs and SHA-256 digests baked in, so publishing it to
//! a tap's `Formula/` directory makes `brew install <tap>/<app>` install the
//! release. Linux installs the glibc archive of an architecture when one is
//! configured and the statically linked musl archive otherwise.

use std::collections::BTreeMap;

use askama::Template;

use crate::plan::{AppPlan, ArchivePlan, ReleasePlan};
use crate::target::Libc;

/// Values of one app's Homebrew formula.
#[derive(Template)]
#[template(path = "homebrew_formula.rb", escape = "none")]
pub(crate) struct HomebrewFormula<'plan> {
    /// Version of `ag-release`, recorded in the header.
    ag_release_version: &'static str,
    /// App name as written in comments.
    app: &'plan str,
    /// Ruby string words of the binaries to install.
    binaries: String,
    /// Ruby string of each binary's name, which the `test` block runs with
    /// `--version`.
    binary_names: Vec<String>,
    /// Formula class name that Homebrew derives from the formula name.
    class_name: String,
    /// Ruby string of the package description.
    description: Option<String>,
    /// Ruby string of the version.
    formula_version: String,
    /// Ruby string of the repository URL.
    homepage: String,
    /// Ruby arguments of the `license` call.
    license: Option<String>,
    /// `on_macos` and `on_linux` blocks, each with at least one archive.
    platforms: Vec<FormulaPlatform>,
    repository_url: &'plan str,
    /// Version as written in comments.
    version: &'plan str,
}

impl<'plan> HomebrewFormula<'plan> {
    /// Renders `app`'s formula for the release `tag` of `plan`, embedding
    /// each archive's SHA-256 from `sha256`.
    ///
    /// # Errors
    /// Returns an error when `sha256` has no digest for an archive the
    /// formula installs.
    pub(crate) fn render(
        plan: &'plan ReleasePlan,
        app: &'plan AppPlan,
        tag: &str,
        sha256: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let download_url = plan.download_url(tag);
        let mut platforms = Vec::new();
        for (name, libcs) in [
            ("macos", &[Libc::None][..]),
            ("linux", &[Libc::Glibc, Libc::Musl]),
        ] {
            let mut archives = Vec::new();
            for (arch, aarch64) in [("arm", true), ("intel", false)] {
                let archive = libcs.iter().find_map(|libc| {
                    app.archives.iter().find(|archive| {
                        archive.target.libc() == *libc && archive.target.is_aarch64() == aarch64
                    })
                });
                if let Some(archive) = archive {
                    archives.push(Self::archive(archive, arch, &download_url, sha256)?);
                }
            }
            if !archives.is_empty() {
                platforms.push(FormulaPlatform { archives, name });
            }
        }
        let binary_names = app
            .binaries
            .iter()
            .map(|binary| Self::quote(binary))
            .collect::<Vec<_>>();
        let formula = Self {
            ag_release_version: env!("CARGO_PKG_VERSION"),
            app: &app.name,
            binaries: binary_names.join(", "),
            binary_names,
            class_name: Self::class_name(&app.name),
            // Homebrew descriptions end without a full stop.
            description: app
                .homebrew_description
                .as_deref()
                .map(|description| Self::quote(description.trim_end().trim_end_matches('.'))),
            formula_version: Self::quote(&plan.version),
            homepage: Self::quote(&plan.repository.url),
            license: app.license.as_deref().and_then(Self::license),
            platforms,
            repository_url: &plan.repository.url,
            version: &plan.version,
        };

        Ok(format!("{formula}\n"))
    }

    /// Returns the `on_arm` or `on_intel` block of `archive`.
    fn archive(
        archive: &ArchivePlan,
        arch: &'static str,
        download_url: &str,
        sha256: &BTreeMap<String, String>,
    ) -> Result<FormulaArchive, String> {
        let digest = sha256
            .get(&archive.name)
            .ok_or_else(|| format!("no SHA-256 is recorded for `{}`", archive.name))?;

        Ok(FormulaArchive {
            arch,
            sha256: Self::quote(digest),
            url: Self::quote(&format!("{download_url}/{}", archive.name)),
        })
    }

    /// Returns the class name Homebrew expects for the formula `name`: the
    /// name capitalized, with each `-`, `_`, or `.` before a letter or digit
    /// removed and that character upper-cased, as `Formulary.class_s` does.
    fn class_name(name: &str) -> String {
        let mut class_name = String::with_capacity(name.len());
        let lowercase = name.to_ascii_lowercase();
        let mut characters = lowercase.chars().peekable();
        if let Some(first) = characters.next() {
            class_name.push(first.to_ascii_uppercase());
        }
        while let Some(character) = characters.next() {
            match characters.peek() {
                Some(next) if "-_.".contains(character) && next.is_ascii_alphanumeric() => {
                    class_name.push(next.to_ascii_uppercase());
                    characters.next();
                }
                _ => class_name.push(character),
            }
        }

        class_name
    }

    /// Returns the `license` arguments for the SPDX expression `license`: one
    /// identifier, or `any_of:` or `all_of:` a list of identifiers joined
    /// only by `OR` (or Cargo's legacy `/`) or only by `AND`. Other
    /// expressions give `None`, which leaves the license out.
    fn license(license: &str) -> Option<String> {
        let identifier = |text: &str| {
            !text.is_empty()
                && text
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || ".-+".contains(character))
        };
        let list = |parts: Vec<&str>| {
            parts.iter().all(|part| identifier(part)).then(|| {
                parts
                    .iter()
                    .map(|part| Self::quote(part))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
        };
        let license = license.trim();
        if identifier(license) {
            return Some(Self::quote(license));
        }
        let any = license
            .split(" OR ")
            .flat_map(|part| part.split('/'))
            .map(str::trim)
            .collect::<Vec<_>>();
        if any.len() > 1 {
            return list(any).map(|identifiers| format!("any_of: [{identifiers}]"));
        }
        let all = license.split(" AND ").map(str::trim).collect::<Vec<_>>();
        if all.len() > 1 {
            return list(all).map(|identifiers| format!("all_of: [{identifiers}]"));
        }

        None
    }

    /// Returns `value` as a double-quoted Ruby string without interpolation.
    fn quote(value: &str) -> String {
        let escaped = value
            .replace('\\', r"\\")
            .replace('"', "\\\"")
            .replace('#', r"\#");

        format!("\"{escaped}\"")
    }
}

/// The `on_macos` or `on_linux` block of a formula.
struct FormulaPlatform {
    /// The `on_arm` and `on_intel` blocks the platform has archives for.
    archives: Vec<FormulaArchive>,
    /// `macos` or `linux`.
    name: &'static str,
}

/// The `on_arm` or `on_intel` block of one archive.
struct FormulaArchive {
    /// `arm` or `intel`.
    arch: &'static str,
    /// Ruby string of the archive's SHA-256.
    sha256: String,
    /// Ruby string of the archive's download URL.
    url: String,
}

#[cfg(test)]
#[path = "homebrew_formula_test.rs"]
mod tests;
