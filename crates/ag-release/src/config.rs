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
use crate::workflow_config::WorkflowConfig;

/// Dotted path of the release configuration table.
pub(crate) const TABLE: &str = "workspace.metadata.release";

/// Cargo profile used when `profile` is not set.
const DEFAULT_PROFILE: &str = "release";

/// Keys accepted in `[workspace.metadata.release]`.
const RELEASE_KEYS: [&str; 7] = [
    "apps",
    "include",
    "installers",
    "min-glibc",
    "profile",
    "targets",
    "workflow",
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
    /// Settings of the generated release workflow.
    pub(crate) workflow: WorkflowConfig,
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
            include: table.relative_paths("include")?,
            installers: Self::parse_installers(&table)?,
            min_glibc: table
                .optional_string("min-glibc")?
                .map(|version| GlibcVersion::parse(version, &table.key("min-glibc")))
                .transpose()?,
            profile: Self::parse_profile(&table)?,
            targets: Self::parse_targets(&table)?,
            workflow: table
                .optional_table("workflow")?
                .map(|workflow| WorkflowConfig::parse(&workflow))
                .transpose()?
                .unwrap_or_default(),
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
pub(crate) struct ConfigTable<'value> {
    /// The table's keys and values, in configured order.
    pub(crate) entries: &'value Map<String, Value>,
    /// Dotted path of the table, such as `workspace.metadata.release`.
    pub(crate) path: String,
}

impl<'value> ConfigTable<'value> {
    /// Wraps `value`, which must be a table, at the dotted `path`.
    ///
    /// # Errors
    /// Returns an error naming `path` when `value` is not a table.
    pub(crate) fn new(value: &'value Value, path: String) -> Result<Self, String> {
        let entries = value
            .as_object()
            .ok_or_else(|| format!("`{path}` must be a table"))?;

        Ok(Self { entries, path })
    }

    /// Returns the dotted path of `key` in this table.
    pub(crate) fn key(&self, key: &str) -> String {
        format!("{}.{key}", self.path)
    }

    /// Fails on the first key that is not in `known`.
    ///
    /// # Errors
    /// Returns an error naming the unknown key and listing `known`.
    pub(crate) fn reject_unknown(&self, known: &[&str]) -> Result<(), String> {
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

    /// Returns the nonempty string at `key`, or `None` when it is absent.
    ///
    /// # Errors
    /// Returns an error naming the key when the value is not a nonempty
    /// string.
    pub(crate) fn optional_string(&self, key: &str) -> Result<Option<&'value str>, String> {
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

    /// Returns the nonempty string at `key`.
    ///
    /// # Errors
    /// Returns an error naming the key when it is absent or not a nonempty
    /// string.
    pub(crate) fn required_string(&self, key: &str) -> Result<&'value str, String> {
        self.optional_string(key)?
            .ok_or_else(|| format!("missing `{}`", self.key(key)))
    }

    /// Returns the boolean at `key`, `false` when it is absent.
    ///
    /// # Errors
    /// Returns an error naming the key when the value is not a boolean.
    pub(crate) fn optional_bool(&self, key: &str) -> Result<bool, String> {
        self.entries.get(key).map_or(Ok(false), |value| {
            value
                .as_bool()
                .ok_or_else(|| format!("`{}` must be `true` or `false`", self.key(key)))
        })
    }

    /// Returns the table at `key`, or `None` when it is absent.
    ///
    /// # Errors
    /// Returns an error naming the key when the value is not a table.
    pub(crate) fn optional_table(&self, key: &str) -> Result<Option<Self>, String> {
        self.entries
            .get(key)
            .map(|value| Self::new(value, self.key(key)))
            .transpose()
    }

    /// Returns a list of unique nonempty strings, empty when the key is absent.
    ///
    /// # Errors
    /// Returns an error naming the key when the value is not such a list.
    pub(crate) fn optional_strings(&self, key: &str) -> Result<Vec<&'value str>, String> {
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

    /// Returns a list of unique relative paths inside the workspace root,
    /// empty when the key is absent.
    ///
    /// # Errors
    /// Returns an error naming the key when the value is not a list of
    /// unique nonempty strings or an entry leaves the workspace root.
    pub(crate) fn relative_paths(&self, key: &str) -> Result<Vec<String>, String> {
        self.optional_strings(key)?
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
                        self.key(key)
                    ))
                }
            })
            .collect()
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
