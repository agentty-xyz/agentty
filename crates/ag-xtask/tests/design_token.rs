//! Public CLI coverage for design token generation and checks without
//! modifying Git state.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::{env, fs, io};

use tempfile::{TempDir, tempdir};

/// Repository files the fixture writes and its stub `git ls-files` lists.
const FILES: [(&str, &str); 8] = [
    ("design/base.css", "body { color: var(--color-bg); }\n"),
    (
        "design/mark.svg",
        "<svg viewBox=\"0 0 24 24\" stroke=\"currentColor\"></svg>\n",
    ),
    ("design/favicon.ico", "icon\n"),
    (
        "docs/site/static/site.js",
        "const bg = style.getPropertyValue('--color-bg');\n",
    ),
    (
        "docs/site/templates/head.html",
        "<meta name=\"theme-color\" content=\"#123456\">\n",
    ),
    (
        "design/tokens.json",
        r##"{
  "color": {
    "$type": "color",
    "bg": { "$value": { "colorSpace": "srgb", "components": [0, 0, 0], "hex": "#000000" } }
  }
}
"##,
    ),
    (
        "docs/site/sass/site.scss",
        "a { color: var(--color-bg); }\n",
    ),
    (
        "docs/site/static/app.css",
        "a { color: var(--color-bg); }\n",
    ),
];

struct Fixture {
    directory: TempDir,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let fixture = Self {
            directory: tempdir()?,
        };
        let mut listing = String::new();
        for (path, text) in FILES {
            fixture.write(path, text)?;
            listing.push_str(path);
            listing.push_str("\\000");
        }
        fixture.write(
            "bin/git",
            &format!(
                "#!/bin/sh\ntest \"$GIT_OPTIONAL_LOCKS\" = '0' || exit 3\ntest \"$*\" = 'ls-files \
                 --cached --others --exclude-standard -z' || exit 2\nprintf '{listing}'\n"
            ),
        )?;
        fs::set_permissions(
            fixture.root().join("bin/git"),
            fs::Permissions::from_mode(0o755),
        )?;

        Ok(fixture)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn write(&self, path: &str, text: &str) -> io::Result<()> {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap_or(self.root()))?;

        fs::write(path, text)
    }

    fn run(&self, args: &[&str]) -> io::Result<Output> {
        let mut paths = vec![self.root().join("bin")];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));

        Command::new(env!("CARGO_BIN_EXE_ag-xtask"))
            .arg("design-tokens")
            .args(args)
            .current_dir(self.root())
            .env("PATH", env::join_paths(paths).map_err(io::Error::other)?)
            .env("GIT_OPTIONAL_LOCKS", "1")
            .output()
    }
}

#[test]
fn cli_generates_stylesheets_that_the_check_accepts_until_one_goes_stale() {
    // Arrange
    let fixture = Fixture::new().expect("design token fixture");

    // Act
    let generated = fixture.run(&[]).expect("generation");
    let current = fixture
        .run(&["--check"])
        .expect("check of fresh stylesheets");
    fixture
        .write("docs/site/sass/_tokens.scss", "/* Edited. */\n")
        .expect("hand edit");
    let stale = fixture
        .run(&["--check"])
        .expect("check of a stale stylesheet");

    // Assert
    assert!(generated.status.success(), "{generated:?}");
    assert_eq!(
        fs::read_to_string(fixture.root().join("design/tokens.css")).expect("shared stylesheet"),
        "/* Generated from tokens.json. Do not edit. */\n:root {\n  --color-bg: #000000;\n}\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root().join("docs/site/templates/head.html"))
            .expect("site template"),
        "<meta name=\"theme-color\" content=\"#000000\">\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.root().join("docs/site/static/favicon.ico"))
            .expect("favicon copy"),
        "icon\n"
    );
    assert!(current.status.success(), "{current:?}");
    assert!(
        String::from_utf8_lossy(&current.stdout).contains(
            "Design token stylesheets are current (tokens: 1, font faces: 0; 5 hand-written \
             stylesheets, scripts, and templates use no raw colour)."
        ),
        "{current:?}"
    );
    assert!(!stale.status.success());
    assert!(
        String::from_utf8_lossy(&stale.stdout).contains(
            "docs/site/sass/_tokens.scss is stale; run `cargo run -p ag-xtask -- design-tokens`"
        ),
        "{stale:?}"
    );
}
