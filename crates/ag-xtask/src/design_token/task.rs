//! Generation and `--check` orchestration for the design token stylesheets.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use clap::Args;
use tracing::info;

use crate::design_token::token::TokenSet;
use crate::design_token::{contrast, stylesheet};
use crate::repository;

/// Token file, relative to the repository root.
const SOURCE: &str = "design/tokens.json";

/// Command that regenerates the stylesheets.
const REGENERATE: &str = "cargo run -p ag-xtask -- design-tokens";

/// Header of the generated surface stylesheets.
const SURFACE_HEADER: &str = "/* Generated from design/tokens.json by `cargo run -p ag-xtask -- \
                              design-tokens`. Do not edit. */";

/// Generated stylesheets. The shared one names no workspace path, so another
/// workspace can copy it unchanged.
const OUTPUTS: [Output; 3] = [
    Output {
        header: "/* Generated from tokens.json. Do not edit. */",
        path: "design/tokens.css",
        surface: None,
    },
    Output {
        header: SURFACE_HEADER,
        path: "docs/site/sass/_tokens.scss",
        surface: Some("docs-site"),
    },
    Output {
        header: SURFACE_HEADER,
        path: "crates/ag-remote/assets/tokens.css",
        surface: Some("remote-client"),
    },
];

/// Options for `design-tokens`.
#[derive(Args)]
pub(crate) struct Options {
    /// Fail when a generated stylesheet is stale instead of writing it.
    #[arg(long)]
    check: bool,
}

/// Generates or checks the design token stylesheets and checks contrast and
/// hand-written stylesheets.
///
/// # Errors
/// Returns an error for unreadable or invalid inputs, a stale stylesheet
/// under `--check`, a raw colour in a hand-written stylesheet, or a colour
/// pair below its contrast minimum.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let host = RealHost {
        git: PathBuf::from("git"),
    };
    let summary = DesignTokens { host: &host }.run(Path::new("."), options.check)?;
    info!("{summary}");

    Ok(())
}

/// External operations used by generation and checks.
#[cfg_attr(test, mockall::automock)]
trait Host {
    fn inventory(&self, root: &Path) -> Result<Vec<PathBuf>, String>;
    fn read(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, text: &str) -> io::Result<()>;
}

/// One generated stylesheet.
struct Output {
    header: &'static str,
    path: &'static str,
    surface: Option<&'static str>,
}

/// Token policy, independent of the host filesystem and Git process.
struct DesignTokens<'host> {
    host: &'host dyn Host,
}

impl DesignTokens<'_> {
    /// Returns a summary of what was generated or checked.
    fn run(&self, root: &Path, check: bool) -> Result<String, String> {
        let path = root.join(SOURCE);
        let source = self
            .host
            .read(&path)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
        let set = TokenSet::parse(&source).map_err(|errors| {
            errors
                .iter()
                .map(|error| format!("{SOURCE}: {error}"))
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        let mut errors = Vec::new();
        for surface in set.surfaces() {
            if !OUTPUTS.iter().any(|output| output.surface == Some(surface)) {
                errors.push(format!(
                    "{SOURCE}: surface {surface} has no generated stylesheet"
                ));
            }
        }
        for output in &OUTPUTS {
            self.generate(root, &set, output, check, &mut errors)?;
        }
        contrast::check(&set, &mut errors);
        let sheets = self.check_stylesheets(root, &mut errors)?;
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        let verb = if check { "are current" } else { "generated" };

        Ok(format!(
            "Design token stylesheets {verb} ({} tokens; {sheets} hand-written stylesheets use no \
             raw colour).",
            set.tokens.len()
        ))
    }

    fn generate(
        &self,
        root: &Path,
        set: &TokenSet,
        output: &Output,
        check: bool,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        let text = set.stylesheet(output.header, output.surface);
        let path = root.join(output.path);
        if !check {
            return self
                .host
                .write(&path, &text)
                .map_err(|error| format!("Failed to write {}: {error}", path.display()));
        }
        match self.host.read(&path) {
            Ok(current) if current == text => {}
            Ok(_) => errors.push(format!("{} is stale; run `{REGENERATE}`", output.path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                errors.push(format!("{} is missing; run `{REGENERATE}`", output.path));
            }
            Err(error) => return Err(format!("Failed to read {}: {error}", path.display())),
        }

        Ok(())
    }

    /// Reports raw colours in every hand-written stylesheet and returns how
    /// many were checked.
    fn check_stylesheets(&self, root: &Path, errors: &mut Vec<String>) -> Result<usize, String> {
        let mut count = 0;
        for path in self.host.inventory(root)? {
            let extension = path.extension().and_then(OsStr::to_str);
            let scss = match extension {
                Some("css") => false,
                Some("scss") => true,
                _ => continue,
            };
            if OUTPUTS.iter().any(|output| path == Path::new(output.path)) {
                continue;
            }
            let source = root.join(&path);
            let text = match self.host.read(&source) {
                Ok(text) => text,
                // A tracked file deleted from the working tree.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("Failed to read {}: {error}", source.display())),
            };
            for color in stylesheet::raw_colors(&text, scss) {
                errors.push(format!(
                    "{}:{}: raw colour {}; add it to {SOURCE} and use its custom property",
                    path.display(),
                    color.line,
                    color.text
                ));
            }
            count += 1;
        }

        Ok(count)
    }
}

/// Host adapter; policy stays in `DesignTokens`.
struct RealHost {
    git: PathBuf,
}

impl Host for RealHost {
    fn inventory(&self, root: &Path) -> Result<Vec<PathBuf>, String> {
        repository::git_files(&self.git, root)
    }

    fn read(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        std::fs::write(path, text)
    }
}

#[cfg(test)]
#[path = "task_test.rs"]
mod tests;
