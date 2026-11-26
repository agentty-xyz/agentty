//! Generation and `--check` orchestration for the design token stylesheets,
//! brand asset copies, and `theme-color` metas.

use std::ffi::OsStr;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};

use clap::Args;
use tracing::info;

use crate::design_token::font_face::{self, Embedded};
use crate::design_token::stylesheet::RawColor;
use crate::design_token::token::TokenSet;
use crate::design_token::{contrast, script, stylesheet, template};
use crate::repository;

/// Token file, relative to the repository root.
const SOURCE: &str = "design/tokens.json";

/// Command that regenerates the stylesheets.
const REGENERATE: &str = "cargo run -p ag-xtask -- design-tokens";

/// Shared foundations and components that each surface loads a copy of.
const BASE: &str = "design/base.css";

/// Header of the generated surface stylesheets.
const SURFACE_HEADER: &str = "/* Generated from design/tokens.json by `cargo run -p ag-xtask -- \
                              design-tokens`. Do not edit. */";

/// Header of the surface copies of the base stylesheet.
const BASE_HEADER: &str = "/* Generated from design/base.css by `cargo run -p ag-xtask -- \
                           design-tokens`. Do not edit. */";

/// Custom property whose default theme value, as the page's surface sees
/// it, every `theme-color` meta carries, so the browser's own interface
/// matches the page before any script runs.
const THEME_COLOR: &str = "color-bg";

/// Directories of the surfaces' pages, whose scripts and templates must
/// read colours from the token custom properties too, and their surfaces.
const PAGE_DIRECTORIES: [(&str, &str); 2] = [
    ("docs/site/static", "docs-site"),
    ("docs/site/templates", "docs-site"),
];

/// Header of the shared stylesheets, which name no workspace path so another
/// workspace can copy them unchanged.
const SHARED_HEADER: &str = "/* Generated from tokens.json. Do not edit. */";

/// Generated files, each surface's stylesheets in the order it loads them.
/// The brand assets are copied byte for byte.
const OUTPUTS: [Output; 7] = [
    Output {
        content: Content::Tokens {
            header: SHARED_HEADER,
            surface: None,
        },
        path: "design/tokens.css",
    },
    Output {
        content: Content::Fonts {
            header: SHARED_HEADER,
        },
        path: "design/fonts.css",
    },
    Output {
        content: Content::Tokens {
            header: SURFACE_HEADER,
            surface: Some("docs-site"),
        },
        path: "docs/site/sass/_tokens.scss",
    },
    Output {
        content: Content::Fonts {
            header: SURFACE_HEADER,
        },
        path: "docs/site/sass/_fonts.scss",
    },
    Output {
        content: Content::Copy {
            header: Some(BASE_HEADER),
            source: BASE,
        },
        path: "docs/site/sass/_base.scss",
    },
    Output {
        content: Content::Copy {
            header: None,
            source: "design/favicon.ico",
        },
        path: "docs/site/static/favicon.ico",
    },
    Output {
        content: Content::Copy {
            header: None,
            source: "design/mark.svg",
        },
        path: "docs/site/static/mark.svg",
    },
];

/// Options for `design-tokens`.
#[derive(Args)]
pub(crate) struct Options {
    /// Fail when a generated stylesheet is stale instead of writing it.
    #[arg(long)]
    check: bool,
}

/// Generates or checks the design token stylesheets, brand asset copies, and
/// `theme-color` metas, and checks contrast and hand-written sources.
///
/// # Errors
/// Returns an error for unreadable or invalid inputs, a stale generated file
/// or `theme-color` meta under `--check`, a raw colour in a hand-written
/// stylesheet or page source, or a colour pair below its contrast minimum.
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
    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
}

/// One generated file.
struct Output {
    content: Content,
    path: &'static str,
}

/// What a generated file holds.
enum Content {
    /// A copy of a hand-written file, after a header comment when it has
    /// one, so a surface that must not load files from outside its own
    /// directory can still use it.
    Copy {
        header: Option<&'static str>,
        source: &'static str,
    },
    /// The self-hosted font faces, each embedded after its notice.
    Fonts { header: &'static str },
    /// The shared tokens, with a surface's own tokens when it names one.
    Tokens {
        header: &'static str,
        surface: Option<&'static str>,
    },
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
            if !OUTPUTS.iter().any(|output| {
                matches!(output.content, Content::Tokens { surface: Some(name), .. } if name == surface)
            }) {
                errors.push(format!(
                    "{SOURCE}: surface {surface} has no generated stylesheet"
                ));
            }
        }
        let faces = self.embed_font_faces(root, &set, &mut errors)?;
        for output in &OUTPUTS {
            self.generate(root, &set, &faces, output, check, &mut errors)?;
        }
        contrast::check(&set, &mut errors);
        let sources = self.check_sources(root, &set, check, &mut errors)?;
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }
        let verb = if check { "are current" } else { "generated" };

        Ok(format!(
            "Design token stylesheets {verb} (tokens: {}, font faces: {}; {sources} hand-written \
             stylesheets, scripts, and templates use no raw colour).",
            set.tokens.len(),
            faces.len()
        ))
    }

    /// Reads and checks the files each font face names, relative to the
    /// token file, and returns the faces that can be embedded.
    fn embed_font_faces<'set>(
        &self,
        root: &Path,
        set: &'set TokenSet,
        errors: &mut Vec<String>,
    ) -> Result<Vec<Embedded<'set>>, String> {
        let directory = root.join(Path::new(SOURCE).parent().unwrap_or(Path::new("")));
        let mut faces = Vec::new();
        for face in &set.font_faces {
            let escaping: Vec<&String> = [&face.source, &face.license]
                .into_iter()
                .filter(|path| !font_face::is_relative_inside(path))
                .collect();
            if !escaping.is_empty() {
                for path in escaping {
                    errors.push(format!(
                        "{SOURCE}: font face {}: {path} must be a relative path inside the token \
                         file's directory",
                        face.family
                    ));
                }
                continue;
            }
            let font_path = directory.join(&face.source);
            let font = self
                .host
                .read_bytes(&font_path)
                .map_err(|error| format!("Failed to read {}: {error}", font_path.display()))?;
            let license_path = directory.join(&face.license);
            let license = self
                .host
                .read(&license_path)
                .map_err(|error| format!("Failed to read {}: {error}", license_path.display()))?;
            match Embedded::new(face, font, &license) {
                Ok(embedded) => faces.push(embedded),
                Err(face_errors) => errors.extend(
                    face_errors
                        .into_iter()
                        .map(|error| format!("{SOURCE}: {error}")),
                ),
            }
        }

        Ok(faces)
    }

    fn generate(
        &self,
        root: &Path,
        set: &TokenSet,
        faces: &[Embedded<'_>],
        output: &Output,
        check: bool,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        let contents = match output.content {
            Content::Fonts { header } => font_face::stylesheet(header, faces).into_bytes(),
            Content::Copy { header, source } => {
                let source = root.join(source);
                let bytes = self
                    .host
                    .read_bytes(&source)
                    .map_err(|error| format!("Failed to read {}: {error}", source.display()))?;
                let mut contents =
                    header.map_or_else(Vec::new, |header| format!("{header}\n").into_bytes());
                contents.extend(bytes);

                contents
            }
            Content::Tokens { header, surface } => set.stylesheet(header, surface).into_bytes(),
        };
        let path = root.join(output.path);
        if !check {
            return self.write(&path, &contents);
        }
        match self.host.read_bytes(&path) {
            Ok(current) if current == contents => {}
            Ok(_) => errors.push(format!("{} is stale; run `{REGENERATE}`", output.path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                errors.push(format!("{} is missing; run `{REGENERATE}`", output.path));
            }
            Err(error) => return Err(format!("Failed to read {}: {error}", path.display())),
        }

        Ok(())
    }

    fn write(&self, path: &Path, contents: &[u8]) -> Result<(), String> {
        self.host
            .write(path, contents)
            .map_err(|error| format!("Failed to write {}: {error}", path.display()))
    }

    /// Reports raw colours in every hand-written stylesheet and in the
    /// scripts and templates of the surfaces' pages, writes or checks their
    /// `theme-color` metas, and returns how many sources were checked.
    fn check_sources(
        &self,
        root: &Path,
        set: &TokenSet,
        check: bool,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let mut count = 0;
        for path in self.host.inventory(root)? {
            let extension = path.extension().and_then(OsStr::to_str);
            let surface = PAGE_DIRECTORIES
                .iter()
                .find(|(directory, _)| path.starts_with(directory))
                .map(|(_, surface)| *surface);
            let is_page = surface.is_some();
            let kind = match extension {
                Some("css") => Source::Stylesheet { scss: false },
                Some("scss") => Source::Stylesheet { scss: true },
                Some("js" | "mjs") if is_page => Source::Script,
                Some("htm" | "html") if is_page => Source::Template,
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
            let colors = match kind {
                Source::Script => script::raw_colors(&text, 1),
                Source::Stylesheet { scss } => stylesheet::raw_colors(&text, scss),
                Source::Template => {
                    let scan = template::scan(&text);
                    let theme_color = set
                        .for_surface(THEME_COLOR, surface)
                        .map(|token| token.value.css());
                    let updated = Self::theme_colors(
                        &path,
                        &text,
                        &scan.theme_colors,
                        theme_color.as_deref(),
                        check,
                        errors,
                    );
                    if let Some(updated) = updated {
                        self.write(&source, updated.as_bytes())?;
                    }

                    scan.colors
                }
            };
            for RawColor { line, text } in colors {
                errors.push(format!(
                    "{}:{line}: raw colour {text}; add it to {SOURCE} and use its custom property",
                    path.display(),
                ));
            }
            count += 1;
        }

        Ok(count)
    }

    /// Checks that each `theme-color` meta, given by the byte range of its
    /// content in `text`, the template at `path`, carries `expected`, the
    /// default theme's page background. Returns the updated template when
    /// generating and a meta is stale; under `check`, reports it instead.
    fn theme_colors(
        path: &Path,
        text: &str,
        metas: &[Range<usize>],
        expected: Option<&str>,
        check: bool,
        errors: &mut Vec<String>,
    ) -> Option<String> {
        if metas.is_empty() {
            return None;
        }
        let Some(expected) = expected else {
            errors.push(format!(
                "{}: a theme-color meta takes --{THEME_COLOR} from {SOURCE}, which has none",
                path.display()
            ));

            return None;
        };
        let stale: Vec<_> = metas
            .iter()
            .filter(|range| text[(*range).clone()] != *expected)
            .collect();
        if check {
            for range in stale {
                let line = template::line_at(text, range.start);
                errors.push(format!(
                    "{}:{line}: theme-color {} is stale; run `{REGENERATE}`",
                    path.display(),
                    &text[range.clone()]
                ));
            }

            return None;
        }
        if stale.is_empty() {
            return None;
        }
        let mut updated = text.to_owned();
        for range in stale.into_iter().rev() {
            updated.replace_range(range.clone(), expected);
        }

        Some(updated)
    }
}

/// How a hand-written source is searched for raw colours.
#[derive(Clone, Copy)]
enum Source {
    Script,
    Stylesheet { scss: bool },
    Template,
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

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)
    }
}

#[cfg(test)]
#[path = "task_test.rs"]
mod tests;
