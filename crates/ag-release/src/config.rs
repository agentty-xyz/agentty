//! `[workspace.metadata.release]` configuration and its validation.
//!
//! Every key is validated strictly: unknown keys, wrong types, and invalid
//! values are errors that name the offending key.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path};

use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::target::Target;

/// Dotted path of the release configuration table.
const TABLE: &str = "workspace.metadata.release";

/// Cargo profile used when `profile` is not set.
const DEFAULT_PROFILE: &str = "release";

/// Keys accepted in `[workspace.metadata.release]`.
const RELEASE_KEYS: [&str; 6] = [
    "apps",
    "include",
    "installers",
    "min-glibc",
    "profile",
    "targets",
];

/// Keys accepted in `[workspace.metadata.release.apps.<app>]`.
const APP_KEYS: [&str; 1] = ["npm-scope"];

/// Validated release configuration of one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleaseConfig {
    /// Released workspace packages by package name.
    pub(crate) apps: BTreeMap<String, AppConfig>,
    /// Relative paths of the files added to every archive, in configured
    /// order. Each app's package directory takes precedence over the
    /// workspace root as the source of each file.
    pub(crate) include: Vec<String>,
    pub(crate) installers: BTreeSet<Installer>,
    /// Minimum glibc version installers require on `*-linux-gnu` targets.
    pub(crate) min_glibc: Option<GlibcVersion>,
    /// Cargo profile used for release builds.
    pub(crate) profile: String,
    /// GitHub Actions runner label for each build target.
    pub(crate) targets: BTreeMap<Target, String>,
}

impl ReleaseConfig {
    /// Validates the `release` table of the workspace metadata that
    /// `cargo metadata` reports.
    ///
    /// # Errors
    /// Returns an error naming the key when the table is missing, has unknown
    /// keys, or holds an invalid value.
    pub(crate) fn parse(workspace_metadata: Option<&Value>) -> Result<Self, String> {
        let release = workspace_metadata
            .and_then(|metadata| metadata.get("release"))
            .ok_or_else(|| format!("missing `[{TABLE}]` in the workspace `Cargo.toml`"))?;
        let table = ConfigTable::new(release, TABLE.to_string())?;
        table.reject_unknown(&RELEASE_KEYS)?;

        Ok(Self {
            apps: Self::parse_apps(&table)?,
            include: Self::parse_include(&table)?,
            installers: Self::parse_installers(&table)?,
            min_glibc: table
                .optional_string("min-glibc")?
                .map(|version| GlibcVersion::parse(version, &table.key("min-glibc")))
                .transpose()?,
            profile: Self::parse_profile(&table)?,
            targets: Self::parse_targets(&table)?,
        })
    }

    /// Returns the configured target spelled `triple`.
    ///
    /// # Errors
    /// Returns an error listing the configured targets when `triple` is not
    /// one of them.
    pub(crate) fn target(&self, triple: &str) -> Result<Target, String> {
        self.targets
            .keys()
            .copied()
            .find(|target| target.triple() == triple)
            .ok_or_else(|| {
                format!(
                    "target `{triple}` is not configured in `{TABLE}.targets`; configured \
                     targets: {}",
                    self.targets
                        .keys()
                        .map(|target| target.triple())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Keeps only the configured targets spelled in `triples`, or every
    /// target when `triples` is empty.
    ///
    /// # Errors
    /// Returns an error when an entry of `triples` is not configured.
    pub(crate) fn select_targets(&mut self, triples: &[String]) -> Result<(), String> {
        if triples.is_empty() {
            return Ok(());
        }
        let selected = triples
            .iter()
            .map(|triple| self.target(triple))
            .collect::<Result<BTreeSet<_>, _>>()?;
        self.targets.retain(|target, _| selected.contains(target));

        Ok(())
    }

    fn parse_apps(table: &ConfigTable<'_>) -> Result<BTreeMap<String, AppConfig>, String> {
        let apps = table.nonempty_table("apps", "app")?;

        apps.entries
            .iter()
            .map(|(name, value)| {
                let app = ConfigTable::new(value, apps.key(name))?;

                Ok((name.clone(), AppConfig::parse(&app)?))
            })
            .collect()
    }

    fn parse_include(table: &ConfigTable<'_>) -> Result<Vec<String>, String> {
        table
            .optional_strings("include")?
            .into_iter()
            .map(|path| {
                if Path::new(path)
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                {
                    Ok(path.to_string())
                } else {
                    Err(format!(
                        "`{}` entry `{path}` must be a relative path inside the workspace root",
                        table.key("include")
                    ))
                }
            })
            .collect()
    }

    fn parse_installers(table: &ConfigTable<'_>) -> Result<BTreeSet<Installer>, String> {
        table
            .optional_strings("installers")?
            .into_iter()
            .map(|name| {
                Installer::from_name(name).ok_or_else(|| {
                    format!(
                        "`{}` entry `{name}` is not a supported installer; supported installers: \
                         {}",
                        table.key("installers"),
                        Installer::ALL.map(Installer::name).join(", ")
                    )
                })
            })
            .collect()
    }

    fn parse_profile(table: &ConfigTable<'_>) -> Result<String, String> {
        let Some(profile) = table.optional_string("profile")? else {
            return Ok(DEFAULT_PROFILE.to_string());
        };
        if !profile
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character))
        {
            return Err(format!(
                "`{}` must be a Cargo profile name, found `{profile}`",
                table.key("profile")
            ));
        }

        Ok(profile.to_string())
    }

    fn parse_targets(table: &ConfigTable<'_>) -> Result<BTreeMap<Target, String>, String> {
        let targets = table.nonempty_table("targets", "target")?;

        targets
            .entries
            .iter()
            .map(|(triple, runner)| {
                let key = targets.key(triple);
                let target = Target::from_triple(triple).ok_or_else(|| {
                    format!(
                        "`{key}` is not a supported target; supported targets: {}",
                        Target::ALL.map(Target::triple).join(", ")
                    )
                })?;
                let runner = runner
                    .as_str()
                    .filter(|runner| !runner.is_empty())
                    .ok_or_else(|| format!("`{key}` must be a nonempty runner label string"))?;

                Ok((target, runner.to_string()))
            })
            .collect()
    }
}

/// Validated `[workspace.metadata.release.apps.<app>]` settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AppConfig {
    /// npm scope, such as `@example`, for the app's npm package name.
    pub(crate) npm_scope: Option<String>,
}

impl AppConfig {
    fn parse(table: &ConfigTable<'_>) -> Result<Self, String> {
        table.reject_unknown(&APP_KEYS)?;
        let npm_scope = table
            .optional_string("npm-scope")?
            .map(|scope| Self::parse_npm_scope(scope, &table.key("npm-scope")))
            .transpose()?;

        Ok(Self { npm_scope })
    }

    /// Accepts `@` followed by a lowercase, URL-safe npm scope name.
    fn parse_npm_scope(scope: &str, key: &str) -> Result<String, String> {
        let valid = scope.strip_prefix('@').is_some_and(|name| {
            name.starts_with(|character: char| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            }) && name.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || "-._".contains(character)
            })
        });
        if !valid {
            return Err(format!(
                "`{key}` must be an npm scope such as `@example`, found `{scope}`"
            ));
        }

        Ok(scope.to_string())
    }
}

/// An installer kind that `assemble` can emit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Installer {
    /// POSIX `sh` installer script.
    Shell,
    /// npm package that downloads the binaries on install.
    Npm,
}

impl Installer {
    /// Every installer kind, in emission order.
    pub(crate) const ALL: [Self; 2] = [Self::Shell, Self::Npm];

    /// Returns the configuration name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Npm => "npm",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|installer| installer.name() == name)
    }
}

/// Serializes as the configuration name.
impl Serialize for Installer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// A `MAJOR.MINOR` glibc version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GlibcVersion {
    pub(crate) major: u32,
    pub(crate) minor: u32,
}

impl GlibcVersion {
    fn parse(version: &str, key: &str) -> Result<Self, String> {
        let number = |part: &str| {
            part.chars()
                .all(|character| character.is_ascii_digit())
                .then(|| part.parse::<u32>().ok())
                .flatten()
        };
        version
            .split_once('.')
            .and_then(|(major, minor)| {
                Some(Self {
                    major: number(major)?,
                    minor: number(minor)?,
                })
            })
            .ok_or_else(|| {
                format!("`{key}` must be a `MAJOR.MINOR` version such as `2.35`, found `{version}`")
            })
    }
}

impl fmt::Display for GlibcVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

/// Serializes as `MAJOR.MINOR`.
impl Serialize for GlibcVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// One configuration table with its dotted path for error messages.
struct ConfigTable<'value> {
    entries: &'value Map<String, Value>,
    path: String,
}

impl<'value> ConfigTable<'value> {
    fn new(value: &'value Value, path: String) -> Result<Self, String> {
        let entries = value
            .as_object()
            .ok_or_else(|| format!("`{path}` must be a table"))?;

        Ok(Self { entries, path })
    }

    fn key(&self, key: &str) -> String {
        format!("{}.{key}", self.path)
    }

    fn reject_unknown(&self, known: &[&str]) -> Result<(), String> {
        match self
            .entries
            .keys()
            .find(|key| !known.contains(&key.as_str()))
        {
            Some(key) => Err(format!(
                "unknown key `{}`; expected one of: {}",
                self.key(key),
                known.join(", ")
            )),
            None => Ok(()),
        }
    }

    fn optional_string(&self, key: &str) -> Result<Option<&'value str>, String> {
        self.entries
            .get(key)
            .map(|value| {
                value
                    .as_str()
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| format!("`{}` must be a nonempty string", self.key(key)))
            })
            .transpose()
    }

    /// Returns a list of unique nonempty strings, empty when the key is absent.
    fn optional_strings(&self, key: &str) -> Result<Vec<&'value str>, String> {
        let Some(value) = self.entries.get(key) else {
            return Ok(Vec::new());
        };
        let invalid = || format!("`{}` must be a list of nonempty strings", self.key(key));
        let items = value.as_array().ok_or_else(invalid)?;
        let mut strings = Vec::with_capacity(items.len());
        for item in items {
            let text = item
                .as_str()
                .filter(|text| !text.is_empty())
                .ok_or_else(invalid)?;
            if strings.contains(&text) {
                return Err(format!("`{}` lists `{text}` more than once", self.key(key)));
            }
            strings.push(text);
        }

        Ok(strings)
    }

    /// Returns a required table that holds at least one `entry_kind` entry.
    fn nonempty_table(&self, key: &str, entry_kind: &str) -> Result<Self, String> {
        let path = self.key(key);
        let value = self
            .entries
            .get(key)
            .ok_or_else(|| format!("missing `[{path}]`; name at least one {entry_kind}"))?;
        let table = Self::new(value, path)?;
        if table.entries.is_empty() {
            return Err(format!(
                "`[{}]` must name at least one {entry_kind}",
                table.path
            ));
        }

        Ok(table)
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
