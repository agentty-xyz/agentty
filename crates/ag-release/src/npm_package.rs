//! The npm package published as `<app>-npm-package.tar.gz`.
//!
//! The package has no dependencies. Its `postinstall` script, `install.js`,
//! downloads the app's release archive for the current platform, verifies
//! the SHA-256 that `binary.js` embeds, and puts the binaries in
//! `<package>/node_modules/.bin_real/`. Each `run-<binary>.js` command runs
//! its binary from there, installing the binaries first when install scripts
//! were skipped. The scripts are rendered from `src/template/npm_*.js`, and
//! the same inputs always give a byte-identical tarball.

use std::collections::{BTreeMap, BTreeSet};

use askama::Template;
use serde_json::{Map, Value, json};

use crate::archive::{Archive, FileMode};
use crate::plan::{AppPlan, ReleasePlan};
use crate::target::Target;

/// Files the package includes when the app's package directory or the
/// workspace root holds them.
pub(crate) const DOCUMENTS: [&str; 3] = ["CHANGELOG.md", "LICENSE", "README.md"];

/// Top-level directory of every npm package tarball.
const PACKAGE_DIRECTORY: &str = "package";

/// Node versions the scripts support: built-in `fetch` needs Node 18.
const NODE_ENGINE: &str = ">=18";

/// The `postinstall` script, which has no release-specific values.
const INSTALL_SCRIPT: &str = include_str!("template/npm_install.js");

/// One app's npm package for one release.
pub(crate) struct NpmPackage<'release> {
    pub(crate) app: &'release AppPlan,
    /// Files added to the package root by name, such as `README.md`.
    pub(crate) documents: Vec<(&'static str, Vec<u8>)>,
    /// npm package name, `<scope>/<app>` or `<app>`.
    pub(crate) name: &'release str,
    pub(crate) plan: &'release ReleasePlan,
    /// SHA-256 of every app archive by file name.
    pub(crate) sha256: &'release BTreeMap<String, String>,
    /// Release tag whose GitHub release holds the archives.
    pub(crate) tag: &'release str,
}

impl NpmPackage<'_> {
    /// Returns the package tarball, every entry below `package/` and stamped
    /// with `mtime` seconds since the Unix epoch.
    ///
    /// # Errors
    /// Returns an error when the SHA-256 of one of the app's archives is not
    /// recorded or the tarball cannot be encoded.
    pub(crate) fn to_tar_gz(&self, mtime: u64) -> Result<Vec<u8>, String> {
        let binary_script = self.binary_script()?;
        let mut archive = Archive::files_only(PACKAGE_DIRECTORY.to_string());
        archive.add_file(
            "package.json",
            FileMode::Regular,
            format!("{:#}\n", self.package_json()).into_bytes(),
        )?;
        archive.add_file("binary.js", FileMode::Regular, binary_script.into_bytes())?;
        archive.add_file(
            "install.js",
            FileMode::Regular,
            INSTALL_SCRIPT.as_bytes().to_vec(),
        )?;
        for binary in &self.app.binaries {
            let script = RunScript {
                binary,
                binary_json: json!(binary).to_string(),
            };
            archive.add_file(
                &Self::run_script(binary),
                FileMode::Executable,
                format!("{script}\n").into_bytes(),
            )?;
        }
        for (name, contents) in &self.documents {
            archive.add_file(name, FileMode::Regular, contents.clone())?;
        }

        archive.to_tar_gz(mtime)
    }

    /// Returns `package.json`: identity from Cargo metadata, one command per
    /// binary, the `postinstall` script, and the supported platforms.
    fn package_json(&self) -> Value {
        let app = self.app;
        let plan = self.plan;
        let mut package = Map::new();
        package.insert("name".to_string(), json!(self.name));
        package.insert("version".to_string(), json!(plan.version));
        if let Some(description) = &app.description {
            package.insert("description".to_string(), json!(description));
        }
        if let Some(license) = &app.license {
            package.insert("license".to_string(), json!(license));
        }
        package.insert(
            "repository".to_string(),
            json!({"type": "git", "url": format!("git+{}.git", plan.repository.url)}),
        );
        if !app.authors.is_empty() {
            package.insert("contributors".to_string(), json!(app.authors));
        }
        let commands = app
            .binaries
            .iter()
            .map(|binary| (binary.clone(), json!(Self::run_script(binary))))
            .collect::<Map<_, _>>();
        package.insert("bin".to_string(), Value::Object(commands));
        package.insert(
            "scripts".to_string(),
            json!({"postinstall": "node ./install.js"}),
        );
        package.insert("engines".to_string(), json!({"node": NODE_ENGINE}));
        let targets = app.archives.iter().map(|archive| archive.target);
        let platforms = targets
            .clone()
            .map(Target::node_platform)
            .collect::<BTreeSet<_>>();
        let arches = targets.map(Target::node_arch).collect::<BTreeSet<_>>();
        package.insert("os".to_string(), json!(platforms));
        package.insert("cpu".to_string(), json!(arches));
        // Yarn's Plug'n'Play must unpack the package for `postinstall` to
        // write the binaries into it.
        package.insert("preferUnplugged".to_string(), json!(true));

        Value::Object(package)
    }

    /// Returns `binary.js` with the release's values baked in.
    fn binary_script(&self) -> Result<String, String> {
        let plan = self.plan;
        let targets = self
            .app
            .archives
            .iter()
            .map(|archive| {
                let digest = self
                    .sha256
                    .get(&archive.name)
                    .ok_or_else(|| format!("no SHA-256 is recorded for `{}`", archive.name))?;

                Ok(json!({
                    "arch": archive.target.node_arch(),
                    "archive": archive.name,
                    "directory": archive.directory,
                    "libc": archive.target.libc().name(),
                    "platform": archive.target.node_platform(),
                    "sha256": digest,
                    "target": archive.target,
                }))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let download_url_variable = format!("{}_DOWNLOAD_URL", self.app.environment_prefix());
        let release = json!({
            "app": self.app.name,
            "binaries": self.app.binaries,
            "downloadUrl": plan.download_url(self.tag),
            "downloadUrlVariable": download_url_variable,
            "minGlibc": plan.min_glibc,
            "targets": targets,
            "version": plan.version,
        });
        let script = BinaryScript {
            ag_release_version: env!("CARGO_PKG_VERSION"),
            app: &self.app.name,
            download_url_variable: &download_url_variable,
            release: format!("{release:#}"),
            repository_url: &plan.repository.url,
            version: &plan.version,
        };

        Ok(format!("{script}\n"))
    }

    /// Returns the file name of the command that runs `binary`.
    fn run_script(binary: &str) -> String {
        format!("run-{binary}.js")
    }
}

/// Values of `binary.js`.
#[derive(Template)]
#[template(path = "npm_binary.js", escape = "none")]
struct BinaryScript<'release> {
    /// Version of `ag-release`, recorded in the header.
    ag_release_version: &'static str,
    /// App name as written in comments.
    app: &'release str,
    /// Environment variable that overrides the download base URL.
    download_url_variable: &'release str,
    /// JSON object of the values the script reads.
    release: String,
    repository_url: &'release str,
    /// Version as written in comments.
    version: &'release str,
}

/// Values of one `run-<binary>.js` command.
#[derive(Template)]
#[template(path = "npm_run.js", escape = "none")]
struct RunScript<'release> {
    /// Binary name as written in comments.
    binary: &'release str,
    /// JSON string of the binary name.
    binary_json: String,
}

#[cfg(test)]
#[path = "npm_package_test.rs"]
mod tests;
