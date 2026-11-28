//! Standards manifest, export existence, and portability check unit tests.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use tempfile::tempdir;

use crate::check_standard::{
    ASSET_LIMIT, Host, MockHost, PROTOCOL_LIMIT, RealHost, StandardCheck, run,
};

const ROOT: &str = "repo";

const MANIFEST: &str = r#"version = 4
files = [".rustfmt.toml"]
skills = ["review"]

[toolchain]
file = "rust-toolchain.toml"

[lints]
manifest = "Cargo.toml"

[rules]
directory = "lint/shared"
tests = "lint/shared-tests"

[[instructions]]
file = "crates/AGENTS.md"
sections = ["Shared"]

[design]
guide = "design/DESIGN.md"
files = ["design/tokens.css", "design/base.css"]
tokens = "design/tokens.json"
assets = ["design/mark.svg", "design/icon.ico", "design/gallery.html"]

[protocol]
schema = "crates/ag-demo/schema/protocol.json"
types = "crates/ag-demo/schema/protocol.ts"
"#;

const SCHEMA_PATH: &str = "crates/ag-demo/schema/protocol.json";

const TYPES_PATH: &str = "crates/ag-demo/schema/protocol.ts";

/// A protocol schema that names its crate, references its root and a
/// definition, and has a property named like a reference keyword.
const SCHEMA: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "description": "Frames of ag-demo.",
  "x-protocol-version": 2,
  "oneOf": [{ "$ref": "#/$defs/Hello" }, { "$ref": "#" }],
  "$defs": {
    "Hello": {
      "type": "object",
      "properties": { "$ref": { "type": "string" }, "type": { "const": "hello" } }
    }
  }
}
"##;

/// Protocol types whose comments and literal types mention imports and
/// versions without making either.
const TYPES: &str = "// Generated from the `ag-demo` protocol types; see https://example.com/x.
/**
 * import { Other } from \"./other\"; export type ProtocolVersion = 9;
 */
export type ProtocolVersion = 2;

export type Hello = {
  type: \"hello\";
  from: \"import 'x'\";
  note: 'from \"a\"';
  path: \"require(`b`)\";
  label: \"héllo // not a comment\";
};
";

const DESIGN_GUIDE: &str = "# Design

## Colors

- Load `tokens.css`, then `design/base.css`, then page rules.
- Read values from `design/tokens.json` and draw `design/mark.svg`.
";

/// A token file whose extension namespace repeats a crate name as one label
/// of a reverse-domain name.
const TOKENS: &str = r##"{
  "color": {
    "bg": {
      "$type": "color",
      "$value": { "colorSpace": "srgb", "components": [0, 0, 0], "hex": "#000000" },
      "$extensions": { "org.ag_demo.theme": { "dark": "#000000" } }
    }
  }
}
"##;

const MARK: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 24 24\">\n  \
                    <defs><path id=\"p\" d=\"M0 0\"/></defs>\n  <use href=\"#p\" \
                    fill=\"url(#g)\"/>\n  <image href=\"data:image/png;base64,AA\"/>\n  <a \
                    href=\"#p\">mark</a>\n</svg>\n";

const GALLERY: &str = "<!DOCTYPE html>\n<link rel=\"stylesheet\" href=\"tokens.css\">\n<link \
                       rel=\"icon\" href=\"icon.ico?v=1\">\n<a href=\"https://example.com/\">More</a>\n<img src='mark.svg#p' alt=\"\">\n<script type=\"module\">\n  import \"./mark.svg\";\n  const mark = await fetch(\"mark.svg\");\n</script>\n";

/// Bytes that are not UTF-8, as in an icon.
const ICON: [u8; 4] = [0, 0, 1, 0xff];

const GUIDE: &str = "# Crates

## Shared

- Follow `AGENTS.md`, `./lint/shared/no-todo.yml`, and `tests/support/`.

### Detail

- Keep `CLAUDE.md` beside each guide, as pre-ag-demo, ag-demo-tools, and ag_demo_x do.

## Local

- Run `prek run check` in `ag-demo` and read `crates/ag-demo/`.
";

const RULE: &str = "id: no-todo
language: rust
message: Finish the code instead of leaving `todo!()`.
files:
  - \"**/src/**/*.rs\"
ignores:
  - \"**/*_test.rs\"
rule:
  pattern: todo!()
";

/// In-memory repository whose inventory lists every written or listed path.
struct Fixture {
    binaries: BTreeMap<PathBuf, Vec<u8>>,
    failures: BTreeMap<PathBuf, io::ErrorKind>,
    files: BTreeMap<PathBuf, String>,
    links: BTreeSet<PathBuf>,
    listed: BTreeSet<PathBuf>,
    unlisted: BTreeSet<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        let mut fixture = Self {
            binaries: BTreeMap::new(),
            failures: BTreeMap::new(),
            files: BTreeMap::new(),
            links: BTreeSet::new(),
            listed: BTreeSet::new(),
            unlisted: BTreeSet::new(),
        };
        fixture.write("standards.toml", MANIFEST);
        fixture.write("AGENTS.md", "# Instructions\n");
        fixture.write("CLAUDE.md", "# Instructions\n");
        fixture.write(".rustfmt.toml", "edition = \"2024\"\n");
        fixture.write(
            "rust-toolchain.toml",
            "[toolchain]\nchannel = \"nightly\"\n",
        );
        fixture.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.lints.rust]\nunsafe_code = \
             \"deny\"\n",
        );
        fixture.write(
            "crates/ag-demo/Cargo.toml",
            "[package]\nname = \"ag-demo\"\n",
        );
        fixture.write("crates/AGENTS.md", GUIDE);
        fixture.write(
            ".pre-commit-config.yaml",
            "repos:\n- repo: local\n  hooks:\n  - id: check\n",
        );
        fixture.write(
            "skills/review/SKILL.md",
            "# Review\n\nRead `skills/review/notes.md`.\n",
        );
        fixture.write("skills/review/notes.md", "# Notes\n");
        fixture.write("lint/shared/no-todo.yml", RULE);
        fixture.write(
            "lint/shared-tests/no-todo-test.yml",
            "id: no-todo\nvalid:\n  - fn done() {}\n",
        );
        fixture.write("design/DESIGN.md", DESIGN_GUIDE);
        fixture.write("design/tokens.css", ":root {\n  --color-bg: #000000;\n}\n");
        fixture.write(
            "design/base.css",
            ".mark {\n  background: url(\"data:image/svg+xml,<svg/>\"), url(#mark), \
             url(mark.svg);\n}\n",
        );
        fixture.write("design/tokens.json", TOKENS);
        fixture.write("design/mark.svg", MARK);
        fixture.write_binary("design/icon.ico", &ICON);
        fixture.write("design/gallery.html", GALLERY);
        fixture.write(SCHEMA_PATH, SCHEMA);
        fixture.write(TYPES_PATH, TYPES);

        fixture
    }

    fn write(&mut self, path: &str, text: &str) {
        self.files.insert(PathBuf::from(path), text.to_owned());
    }

    fn write_binary(&mut self, path: &str, bytes: &[u8]) {
        self.binaries.insert(PathBuf::from(path), bytes.to_vec());
    }

    fn remove(&mut self, path: &str) {
        self.files.remove(Path::new(path));
        self.binaries.remove(Path::new(path));
    }

    /// Lists `path` in the Git inventory without a readable file, as for a
    /// tracked file deleted from the working tree.
    fn list(&mut self, path: &str) {
        self.listed.insert(PathBuf::from(path));
    }

    fn fail(&mut self, path: &str, kind: io::ErrorKind) {
        self.failures.insert(PathBuf::from(path), kind);
    }

    /// Makes an existing `path` read as a symbolic link rather than a
    /// regular file, though reading through it still succeeds.
    fn link(&mut self, path: &str) {
        self.links.insert(PathBuf::from(path));
    }

    /// Keeps an existing `path` out of the Git inventory, as for an ignored
    /// file or one reached through a linked directory.
    fn unlist(&mut self, path: &str) {
        self.unlisted.insert(PathBuf::from(path));
    }

    fn check(&self) -> Result<usize, String> {
        let mut host = MockHost::new();
        let inventory: Vec<_> = self
            .files
            .keys()
            .chain(self.binaries.keys())
            .chain(&self.listed)
            .filter(|path| !self.unlisted.contains(*path))
            .cloned()
            .collect();
        host.expect_inventory()
            .withf(|root| root == Path::new(ROOT))
            .return_once(|_| Ok(inventory));
        let mut contents: BTreeMap<PathBuf, Vec<u8>> = self
            .files
            .iter()
            .map(|(path, text)| (path.clone(), text.clone().into_bytes()))
            .collect();
        contents.extend(self.binaries.clone());
        let entries: BTreeMap<PathBuf, bool> = contents
            .keys()
            .map(|path| (path.clone(), !self.links.contains(path)))
            .collect();
        host.expect_is_file().returning(move |path| {
            let relative = path.strip_prefix(ROOT).unwrap_or(path);

            entries
                .get(relative)
                .copied()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        });
        let read_bytes = move |path: &Path| -> io::Result<Vec<u8>> {
            let relative = path.strip_prefix(ROOT).unwrap_or(path);

            contents
                .get(relative)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        };
        let failures = self.failures.clone();
        let read = move |path: &Path| -> io::Result<Vec<u8>> {
            let relative = path.strip_prefix(ROOT).unwrap_or(path);
            if let Some(kind) = failures.get(relative) {
                return Err(io::Error::new(*kind, "denied"));
            }

            read_bytes(path)
        };
        let read_text = read.clone();
        host.expect_read().returning(move |path| {
            String::from_utf8(read_text(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        });
        host.expect_read_bytes().returning(read);

        StandardCheck::new(&host)
            .expect("patterns")
            .run(Path::new(ROOT))
    }

    fn errors(&self) -> Vec<String> {
        self.check()
            .expect_err("standards violations")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn portable_exports_pass_and_count_each_checked_document() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.list("skills/review/deleted.md");
    fixture.list("lint/shared/deleted.yml");
    fixture.list("lint/shared-tests/deleted-test.yml");
    fixture.list("crates/ag-deleted/Cargo.toml");

    // Act
    let result = fixture.check();

    // Assert
    assert_eq!(result, Ok(14));
}

#[test]
fn exported_text_naming_workspace_crates_hooks_or_paths_fails() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "crates/AGENTS.md",
        "# Crates

## Shared

- Use `ag-demo`, `ag_demo::Item`, and the `check` hook.
- Run `prek run check` but not `prek run unknown`.

### Detail

- Read `crates/ag-demo/`, `./skills/other/`, and ``crates/quoted``.

## Local

- Run `prek run check`.
",
    );
    fixture.write(
        "skills/review/notes.md",
        "# Notes\n\nSee `.pre-commit-config.yaml`.\n",
    );
    fixture.write(
        "lint/shared/no-todo.yml",
        &RULE.replace("pattern: todo!()", "pattern: ag_demo::todo!()"),
    );

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        [
            "skills/review/notes.md:3: names .pre-commit-config.yaml, which standards.toml does \
             not export",
            "lint/shared/no-todo.yml:9: names workspace crate ag_demo",
            "crates/AGENTS.md:5: names workspace crate ag-demo",
            "crates/AGENTS.md:5: names workspace crate ag_demo",
            "crates/AGENTS.md:10: names workspace crate ag-demo",
            "crates/AGENTS.md:5: names workspace hook check",
            "crates/AGENTS.md:10: names crates/ag-demo/, which standards.toml does not export",
            "crates/AGENTS.md:10: names ./skills/other/, which standards.toml does not export",
            "crates/AGENTS.md:6: names workspace hook check",
        ]
    );
}

#[test]
fn manifest_paths_that_do_not_exist_are_reported() {
    // Arrange
    let mut fixture = Fixture::new();
    for path in [
        ".rustfmt.toml",
        "rust-toolchain.toml",
        "Cargo.toml",
        "skills/review/SKILL.md",
        "crates/AGENTS.md",
        "design/DESIGN.md",
        "design/base.css",
        "design/tokens.json",
        "design/icon.ico",
        SCHEMA_PATH,
    ] {
        fixture.remove(path);
    }
    fixture.fail(
        "lint/shared-tests/no-todo-test.yml",
        io::ErrorKind::NotADirectory,
    );

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        [
            "standards.toml: missing path .rustfmt.toml",
            "standards.toml: missing path rust-toolchain.toml",
            "standards.toml: missing path Cargo.toml",
            "standards.toml: missing path skills/review/SKILL.md",
            "lint/shared-tests: rule no-todo has no no-todo-test.yml",
            "standards.toml: missing path crates/AGENTS.md",
            "standards.toml: missing path design/DESIGN.md",
            "standards.toml: missing path design/base.css",
            "standards.toml: missing path design/tokens.json",
            "standards.toml: missing path design/icon.ico",
            "standards.toml: missing path crates/ag-demo/schema/protocol.json",
        ]
    );
}

#[test]
fn design_exports_must_be_portable_plain_css() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "standards.toml",
        &MANIFEST.replace("design/base.css", "design/base.scss"),
    );
    fixture.write(
        "design/DESIGN.md",
        "# Design\n\n## Colors\n\n- Read `crates/ag-demo/` and run `prek run check`.\n",
    );
    fixture.write(
        "design/tokens.css",
        "@import url(\"fonts.css\");\n:root {\n  --mark: url(data:image/png;base64,AA);\n}\n.a { \
         background: url( 'https://example.com/a.png'); }\n",
    );
    fixture.write(
        "design/base.scss",
        "/* Copied from ag-demo. */ @use \"tokens\";\n",
    );

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        [
            "design/DESIGN.md:5: names workspace crate ag-demo",
            "design/DESIGN.md:5: names crates/ag-demo/, which standards.toml does not export",
            "design/DESIGN.md:5: names workspace hook check",
            "design/tokens.css:1: uses @import; an exported stylesheet is plain CSS that stands \
             alone",
            "design/tokens.css:1: loads fonts.css; an exported stylesheet loads only data: URIs, \
             fragments, and exported files beside it",
            "design/tokens.css:5: loads https://example.com/a.png; an exported stylesheet loads \
             only data: URIs, fragments, and exported files beside it",
            "design/base.scss: a design stylesheet must be a plain .css file",
            "design/base.scss:1: names workspace crate ag-demo",
            "design/base.scss:1: uses @use; an exported stylesheet is plain CSS that stands alone",
        ]
    );
}

#[test]
fn token_files_name_a_crate_only_as_a_reverse_domain_label() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "design/tokens.json",
        "{\n  \"$description\": \"Shared by ag_demo::Theme and ~/.ag_demo/.\",\n  \
         \"$extensions\": { \"org.ag_demo.contrast\": [], \"ag_demo.first\": [] }\n}\n",
    );

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        [
            "design/tokens.json:2: names workspace crate ag_demo",
            "design/tokens.json:2: names workspace crate ag_demo",
            "design/tokens.json:3: names workspace crate ag_demo",
        ]
    );
}

#[test]
fn token_files_must_be_json_token_groups() {
    // Arrange
    for (path, text) in [
        ("design/tokens.json", "[]\n"),
        ("design/tokens.json", "{ unterminated\n"),
        ("design/tokens.yaml", TOKENS),
    ] {
        let mut fixture = Fixture::new();
        fixture.write(
            "standards.toml",
            &MANIFEST.replace("design/tokens.json", path),
        );
        fixture.write(
            "design/DESIGN.md",
            &DESIGN_GUIDE.replace("design/tokens.json", path),
        );
        fixture.write(path, text);

        // Act
        let errors = fixture.errors();

        // Assert
        assert_eq!(
            errors,
            [format!(
                "{path}: the design token file must be a .json file whose root is a token group"
            )],
            "{path}"
        );
    }
}

#[test]
fn design_assets_load_nothing_beyond_their_exported_siblings() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "design/mark.svg",
        "<svg xmlns=\"http://www.w3.org/2000/svg\">\n<script>alert(1)</script>\n<use \
         xlink:href='https://example.com/s.svg#a'/>\n<rect style=\"fill: url(other.svg#g)\"/>\n<a \
         href=\"tokens.css\">tokens</a>\n<g onload=\"fetch('/x')\"><a \
         href=\"javascript:void(0)\">x</a></g>\n</svg>\n",
    );
    fixture.write(
        "design/gallery.html",
        "<link rel=stylesheet href=../site.css>\n<script \
         src=\"https://cdn.example.com/x.js\"></script>\n<style>@import \"fonts.css\"; body { \
         background: url('/bg.png'); }</style>\n<video poster=\"design/mark.svg\"></video>\n<script \
         type=\"module\">import { x } from \"https://esm.example.com/x\"; \
         fetch('/api');</script>\n<img alt=\"1 > 0\" src=\"https://img.example.com/a.png\" \
         srcset=\"mark.svg 1x, https://img.example.com/b.png 2x\">\n<object \
         data=\"https://obj.example.com/o\"></object><a href=\"https://example.com/\" \
         ping=\"https://ping.example.com/\">p</a>\n<button \
         onclick=\"fetch('https://api.example.com/')\">go</button><a \
         href=\"javascript:fetch('/y')\">y</a>\n<a href=\" javascript:fetch('/z')\">z</a><a \
         href=\"java&#9;script:t\">t</a><a href=\"javascript&#58;c\">c</a><a \
         href=\"guide.html#top\">g</a><a href=\"mailto:team@example.com\">m</a>\n",
    );

    // Act
    let errors = fixture.errors();

    // Assert
    let svg = "an exported SVG references only its own fragments and data: URIs";
    let page = "an exported page loads only data: URIs, fragments, and exported files beside it";
    assert_eq!(
        errors,
        [
            "design/mark.svg:2: holds a script; an exported SVG draws without one".to_owned(),
            format!("design/mark.svg:3: loads https://example.com/s.svg#a; {svg}"),
            format!("design/mark.svg:5: loads tokens.css; {svg}"),
            "design/mark.svg:6: holds a script in onload; an exported SVG draws without one"
                .to_owned(),
            format!("design/mark.svg:6: loads javascript:void(0); {svg}"),
            format!("design/mark.svg:4: loads other.svg#g; {svg}"),
            format!("design/mark.svg:6: loads /x; {svg}"),
            format!("design/gallery.html:1: loads ../site.css; {page}"),
            format!("design/gallery.html:2: loads https://cdn.example.com/x.js; {page}"),
            format!("design/gallery.html:4: loads design/mark.svg; {page}"),
            format!("design/gallery.html:6: loads https://img.example.com/a.png; {page}"),
            format!("design/gallery.html:6: loads https://img.example.com/b.png; {page}"),
            format!("design/gallery.html:7: loads https://obj.example.com/o; {page}"),
            format!("design/gallery.html:7: loads https://ping.example.com/; {page}"),
            format!("design/gallery.html:8: loads javascript:fetch('/y'); {page}"),
            format!("design/gallery.html:9: loads javascript:fetch('/z'); {page}"),
            format!("design/gallery.html:9: loads java&#9;script:t; {page}"),
            format!("design/gallery.html:9: loads javascript&#58;c; {page}"),
            format!("design/gallery.html:3: loads /bg.png; {page}"),
            format!("design/gallery.html:3: loads fonts.css; {page}"),
            format!("design/gallery.html:5: loads https://esm.example.com/x; {page}"),
            format!("design/gallery.html:5: loads /api; {page}"),
            format!("design/gallery.html:8: loads https://api.example.com/; {page}"),
        ]
    );
}

#[test]
fn design_assets_are_bounded_typed_and_listed_once() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "standards.toml",
        &MANIFEST.replace("\"design/gallery.html\"]", "\"design/base.css\"]"),
    );
    fixture.write_binary("design/icon.ico", &vec![0; ASSET_LIMIT + 1]);
    fixture.write_binary("design/mark.svg", &ICON);

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        [
            "standards.toml: design export design/base.css is listed more than once".to_owned(),
            "design/mark.svg: an HTML or SVG asset must be UTF-8 text".to_owned(),
            format!(
                "design/icon.ico: {} bytes is over the {ASSET_LIMIT}-byte limit for a design asset",
                ASSET_LIMIT + 1
            ),
            "design/base.css: list a stylesheet under files, in load order, not under assets"
                .to_owned(),
        ]
    );
}

#[test]
fn protocol_files_reference_nothing_outside_themselves() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        SCHEMA_PATH,
        r##"{
  "x-protocol-version": 2,
  "oneOf": [{ "$ref": "other.json#/$defs/A" }, { "items": [{ "$ref": "#/$defs/Missing" }] }],
  "$defs": { "A": { "$dynamicRef": "#meta", "$recursiveRef": "https://example.com/s" } }
}
"##,
    );
    fixture.write(
        TYPES_PATH,
        "/// <reference types=\"node\" />\nimport type { A } from \"./a\";\nexport * from \
         'b';\nexport type C = import(\"c\").C;\nimport d = require(\"d\");\nexport type \
         ProtocolVersion = 2;\n  /// <amd-dependency path=\"e\" />\n",
    );

    // Act
    let errors = fixture.errors();

    // Assert
    let schema = "an exported protocol schema references only JSON pointers into itself";
    let types = "exported protocol types import nothing";
    assert_eq!(
        errors,
        [
            format!("{SCHEMA_PATH}: references other.json#/$defs/A; {schema}"),
            format!("{SCHEMA_PATH}: references #/$defs/Missing; {schema}"),
            format!("{SCHEMA_PATH}: references #meta; {schema}"),
            format!("{SCHEMA_PATH}: references https://example.com/s; {schema}"),
            format!("{TYPES_PATH}:1: holds a triple-slash directive; {types}"),
            format!("{TYPES_PATH}:7: holds a triple-slash directive; {types}"),
            format!("{TYPES_PATH}:2: imports ./a; {types}"),
            format!("{TYPES_PATH}:3: imports b; {types}"),
            format!("{TYPES_PATH}:4: imports c; {types}"),
            format!("{TYPES_PATH}:5: imports d; {types}"),
        ]
    );
}

#[test]
fn protocol_schemas_name_no_base_uri_anchor_or_other_dialect() {
    // Arrange
    let mut identified = Fixture::new();
    identified.write(
        SCHEMA_PATH,
        r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://example.com/protocol.json",
  "x-protocol-version": 2,
  "$defs": {
    "A": {
      "$id": "a.json",
      "$anchor": "a",
      "$dynamicAnchor": "d",
      "$recursiveAnchor": true,
      "$schema": "https://json-schema.org/draft/2020-12/schema"
    }
  }
}
"#,
    );
    let mut dialect = Fixture::new();
    dialect.write(
        SCHEMA_PATH,
        &SCHEMA.replace(
            "https://json-schema.org/draft/2020-12/schema",
            "https://example.com/meta",
        ),
    );

    // Act
    let identified = identified.errors();
    let dialect = dialect.errors();

    // Assert
    let named = "an exported protocol schema names no base URI or anchor, so every reference is a \
                 JSON pointer into it";
    let declared =
        "an exported protocol schema declares only https://json-schema.org/draft/2020-12/schema, \
         at its root";
    assert_eq!(
        identified,
        [
            format!("{SCHEMA_PATH}: holds $id; {named}"),
            format!("{SCHEMA_PATH}: holds $id; {named}"),
            format!("{SCHEMA_PATH}: holds $anchor; {named}"),
            format!("{SCHEMA_PATH}: holds $dynamicAnchor; {named}"),
            format!("{SCHEMA_PATH}: holds $recursiveAnchor; {named}"),
            format!(
                "{SCHEMA_PATH}: declares the dialect \
                 \"https://json-schema.org/draft/2020-12/schema\"; {declared}"
            ),
        ]
    );
    assert_eq!(
        dialect,
        [format!(
            "{SCHEMA_PATH}: declares the dialect \"https://example.com/meta\"; {declared}"
        )]
    );
}

#[test]
fn protocol_types_hold_no_template_literal_or_unterminated_string() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        TYPES_PATH,
        "export type ProtocolVersion = 2;\nexport type A = `${import(\"x\")}`;\nexport type B = \
         \"open;\nexport type C = 'closed';\n",
    );

    // Act
    let errors = fixture.errors();

    // Assert
    let template =
        "holds a template literal; template literals are not allowed in the exported types";
    assert_eq!(
        errors,
        [
            format!("{TYPES_PATH}:2: {template}"),
            format!("{TYPES_PATH}:2: {template}"),
            format!(
                "{TYPES_PATH}:3: holds an unterminated string; a string literal in the exported \
                 types ends on its own line"
            ),
            format!("{TYPES_PATH}:2: imports x; exported protocol types import nothing"),
        ]
    );
}

#[test]
fn protocol_files_name_one_version_and_agree_on_it() {
    // Arrange
    let unnamed = format!(
        "{SCHEMA_PATH}: the protocol schema must name its version in an integer x-protocol-version"
    );
    let undeclared = format!(
        "{TYPES_PATH}: the protocol types must declare one export type ProtocolVersion = \
         <integer>;"
    );
    for (path, text, expected) in [
        (
            SCHEMA_PATH,
            SCHEMA.replace("\"x-protocol-version\": 2,", ""),
            unnamed.clone(),
        ),
        (
            SCHEMA_PATH,
            SCHEMA.replace("\"x-protocol-version\": 2", "\"x-protocol-version\": \"2\""),
            unnamed,
        ),
        (
            TYPES_PATH,
            TYPES.replace("ProtocolVersion = 2;", "ProtocolVersion = 3;"),
            format!(
                "{TYPES_PATH}: ProtocolVersion 3 differs from x-protocol-version 2 in \
                 {SCHEMA_PATH}"
            ),
        ),
        (
            TYPES_PATH,
            TYPES.replace("export type ProtocolVersion = 2;", ""),
            undeclared.clone(),
        ),
        (
            TYPES_PATH,
            format!("{TYPES}export type ProtocolVersion = 2;\n"),
            undeclared.clone(),
        ),
        (
            TYPES_PATH,
            TYPES.replace(
                "ProtocolVersion = 2;",
                "ProtocolVersion = 99999999999999999999999;",
            ),
            undeclared.clone(),
        ),
        (
            TYPES_PATH,
            TYPES.replace("ProtocolVersion = 2;", "ProtocolVersion = 02;"),
            undeclared.clone(),
        ),
        (
            TYPES_PATH,
            TYPES.replace("ProtocolVersion = 2;", "ProtocolVersion = 2 | 3;"),
            undeclared.clone(),
        ),
        (
            TYPES_PATH,
            TYPES.replace("ProtocolVersion = 2;", "ProtocolVersion = -2;"),
            undeclared,
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.write(path, &text);

        // Act
        let errors = fixture.errors();

        // Assert
        assert_eq!(errors, [expected], "{text}");
    }
}

#[test]
fn comments_between_the_protocol_version_tokens_are_whitespace() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        TYPES_PATH,
        &TYPES.replace(
            "export type ProtocolVersion = 2;",
            "export /* the */ type ProtocolVersion =\n  // version\n  2 /* now */;",
        ),
    );

    // Act
    let result = fixture.check();

    // Assert
    assert_eq!(result, Ok(14));
}

#[test]
fn protocol_files_are_bounded_regular_text_of_their_kind() {
    // Arrange
    let mut linked = Fixture::new();
    linked.link(SCHEMA_PATH);
    let mut behind_link = Fixture::new();
    behind_link.unlist(TYPES_PATH);
    let mut binary = Fixture::new();
    binary.remove(TYPES_PATH);
    binary.write_binary(TYPES_PATH, &vec![0xff; PROTOCOL_LIMIT + 1]);
    let mut marked = Fixture::new();
    marked.write(TYPES_PATH, &format!("\u{feff}{TYPES}"));
    let mut unparsed = Fixture::new();
    unparsed.write(SCHEMA_PATH, "{\n  unterminated\n");
    let mut listed = Fixture::new();
    listed.write(SCHEMA_PATH, "[]\n");
    let mut mistyped = Fixture::new();
    mistyped.write(
        "standards.toml",
        &MANIFEST
            .replace("protocol.json", "protocol.yaml")
            .replace("protocol.ts", "protocol.js"),
    );
    mistyped.write("crates/ag-demo/schema/protocol.yaml", SCHEMA);
    mistyped.write("crates/ag-demo/schema/protocol.js", TYPES);
    for (fixture, expected) in [
        (
            linked,
            vec![format!(
                "{SCHEMA_PATH}: a protocol file must be a regular file, not a link or directory"
            )],
        ),
        (
            behind_link,
            vec![format!(
                "{TYPES_PATH}: Git does not list this protocol file; it must not be ignored or \
                 lie behind a linked directory"
            )],
        ),
        (
            binary,
            vec![
                format!(
                    "{TYPES_PATH}: {} bytes is over the {PROTOCOL_LIMIT}-byte limit for a \
                     protocol file",
                    PROTOCOL_LIMIT + 1
                ),
                format!("{TYPES_PATH}: a protocol file must be UTF-8 text"),
            ],
        ),
        (
            marked,
            vec![format!(
                "{TYPES_PATH}: starts with a byte order mark; a protocol file is plain UTF-8 text"
            )],
        ),
        (
            unparsed,
            vec![format!(
                "{SCHEMA_PATH}: the protocol schema is not valid JSON: key must be a string at \
                 line 2 column 3"
            )],
        ),
        (
            listed,
            vec![format!(
                "{SCHEMA_PATH}: the protocol schema's root must be a schema object"
            )],
        ),
        (
            mistyped,
            vec![
                "crates/ag-demo/schema/protocol.yaml: the protocol schema must be a .json file"
                    .to_owned(),
                "crates/ag-demo/schema/protocol.js: the protocol types must be a .ts file"
                    .to_owned(),
            ],
        ),
    ] {
        // Act
        let errors = fixture.errors();

        // Assert
        assert_eq!(errors, expected);
    }
}

#[test]
fn invalid_manifests_and_unsupported_versions_fail() {
    // Arrange
    for (manifest, expected) in [
        (
            "version = 1\n".to_owned(),
            "standards.toml: invalid manifest:",
        ),
        (
            format!("{MANIFEST}unknown = true\n"),
            "standards.toml: invalid manifest:",
        ),
        (
            MANIFEST.replace("version = 4", "version = 3"),
            "standards.toml: unsupported version 3; expected 4",
        ),
        // A version 3 manifest has no `[protocol]` table.
        (
            MANIFEST
                .replace("version = 4", "version = 3")
                .split("\n[protocol]")
                .next()
                .unwrap_or_default()
                .to_owned(),
            "standards.toml: invalid manifest:",
        ),
        // A version 2 manifest has no design `tokens` or `assets`.
        (
            MANIFEST
                .replace("version = 4", "version = 2")
                .replace("tokens = \"design/tokens.json\"\n", ""),
            "standards.toml: invalid manifest:",
        ),
        (
            MANIFEST.replace("assets = [", "assets = 1\nicons = ["),
            "standards.toml: invalid manifest:",
        ),
        (
            MANIFEST.replace("types = ", "typescript = "),
            "standards.toml: invalid manifest:",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.write("standards.toml", &manifest);

        // Act
        let result = fixture.check();

        // Assert
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.starts_with(expected)),
            "{manifest}: {result:?}"
        );
    }
}

#[test]
fn toolchain_and_workspace_manifests_need_their_shared_tables() {
    // Arrange
    for (path, text, expected) in [
        (
            "rust-toolchain.toml",
            "[toolchain]\nprofile = \"minimal\"\n",
            "rust-toolchain.toml: missing [toolchain] channel",
        ),
        (
            "rust-toolchain.toml",
            "[unterminated",
            "rust-toolchain.toml: missing [toolchain] channel",
        ),
        (
            "Cargo.toml",
            "[package]\nname = \"single\"\n",
            "Cargo.toml: invalid workspace manifest:",
        ),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\"]\n",
            "Cargo.toml: missing [workspace.lints]",
        ),
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/*\", \"apps/*\"]\n\n[workspace.lints.rust]\n",
            "Cargo.toml: workspace member apps/* matches no manifest",
        ),
        (
            "crates/ag-demo/Cargo.toml",
            "[workspace]\n",
            "crates/ag-demo/Cargo.toml: invalid member manifest:",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.write(path, text);

        // Act
        let result = fixture.check();

        // Assert
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.starts_with(expected)),
            "{path}: {result:?}"
        );
    }
}

#[test]
fn member_patterns_match_one_component_per_wildcard() {
    // Arrange
    let cases = [
        ("crates/*", "crates/ag-demo", true),
        ("crates/ag-demo", "crates/ag-demo", true),
        ("crates/*", "", false),
        ("crates/*", "apps/demo", false),
        ("crates/*", "crates/ag-demo/fixture", false),
        ("crates/*", "./crates", false),
    ];

    // Act
    let matches: Vec<_> = cases
        .iter()
        .map(|(pattern, directory, _)| StandardCheck::matches_member(pattern, Path::new(directory)))
        .collect();

    // Assert
    let expected: Vec<_> = cases.iter().map(|(_, _, matches)| *matches).collect();
    assert_eq!(matches, expected);
}

#[test]
fn shared_rules_must_match_by_pattern_and_pair_with_tests() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "lint/shared/no-todo.yml",
        &RULE
            .replace("\"**/src/**/*.rs\"", "crates/*/src/**/*.rs")
            .replace("\"**/*_test.rs\"", "crates/shared/src/lib.rs"),
    );
    fixture.write("lint/shared/misnamed.yml", "id: no-panic\nrule: {}\n");
    fixture.write("lint/shared/broken.yml", "[unterminated");
    fixture.write("lint/shared-tests/orphan-test.yml", "id: orphan\n");
    fixture.write("lint/shared-tests/README.md", "# Tests\n");

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(errors.len(), 7, "{errors:#?}");
    assert!(errors[0].starts_with("lint/shared/broken.yml: invalid rule:"));
    assert_eq!(
        errors[1..],
        [
            "lint/shared/misnamed.yml: rule id no-panic must match the file name",
            "lint/shared/no-todo.yml: glob crates/*/src/**/*.rs starts at the repository path \
             crates; match shared rules by pattern, such as **/src/**/*.rs, or move the rule out \
             of the shared directory",
            "lint/shared/no-todo.yml: glob crates/shared/src/lib.rs starts at the repository path \
             crates; match shared rules by pattern, such as **/src/**/*.rs, or move the rule out \
             of the shared directory",
            "lint/shared-tests/README.md: tests no rule in lint/shared",
            "lint/shared-tests/orphan-test.yml: tests no rule in lint/shared",
            "lint/shared-tests: rule no-panic has no no-panic-test.yml",
        ]
    );
}

#[test]
fn an_empty_rules_directory_is_reported() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.remove("lint/shared/no-todo.yml");
    fixture.remove("lint/shared-tests/no-todo-test.yml");

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        ["standards.toml: rules directory lint/shared holds no rules"]
    );
}

#[test]
fn sections_need_exactly_one_matching_heading_including_inline_code() {
    // Arrange
    let mut fixture = Fixture::new();
    fixture.write(
        "standards.toml",
        &MANIFEST.replace(
            "sections = [\"Shared\"]",
            "sections = [\"Shared\", \"The Tokio rules\", \"Missing\", \"Twice\"]",
        ),
    );
    fixture.write(
        "crates/AGENTS.md",
        &format!(
            "{GUIDE}\n## The `Tokio` rules\n\n- Name `crates/ag-demo/`.\n\n## Twice\n\n## Twice\n"
        ),
    );

    // Act
    let errors = fixture.errors();

    // Assert
    assert_eq!(
        errors,
        [
            "crates/AGENTS.md:17: names workspace crate ag-demo",
            "crates/AGENTS.md:17: names crates/ag-demo/, which standards.toml does not export",
            "crates/AGENTS.md: missing section heading Missing",
            "crates/AGENTS.md: ambiguous section heading Twice",
        ]
    );
}

#[test]
fn unreadable_inputs_are_errors() {
    // Arrange
    for (path, expected) in [
        (
            "standards.toml",
            "Failed to read repo/standards.toml: denied",
        ),
        (".rustfmt.toml", "Failed to read repo/.rustfmt.toml: denied"),
        (
            ".pre-commit-config.yaml",
            "Failed to read repo/.pre-commit-config.yaml: denied",
        ),
        (
            "crates/ag-demo/Cargo.toml",
            "Failed to read repo/crates/ag-demo/Cargo.toml: denied",
        ),
        (
            "skills/review/notes.md",
            "Failed to read repo/skills/review/notes.md: denied",
        ),
        (
            "design/icon.ico",
            "Failed to read repo/design/icon.ico: denied",
        ),
        (
            TYPES_PATH,
            "Failed to read repo/crates/ag-demo/schema/protocol.ts: denied",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.fail(path, io::ErrorKind::PermissionDenied);

        // Act
        let result = fixture.check();

        // Assert
        assert_eq!(result, Err(expected.to_owned()), "{path}");
    }
}

#[test]
fn inventory_failures_cannot_pass_as_an_empty_repository() {
    // Arrange
    let mut host = MockHost::new();
    host.expect_read().returning(|_| Ok(MANIFEST.to_owned()));
    host.expect_inventory()
        .returning(|_| Err("git unavailable".to_owned()));
    let check = StandardCheck::new(&host).expect("patterns");

    // Act
    let result = check.run(Path::new(ROOT));

    // Assert
    assert_eq!(result, Err("git unavailable".to_owned()));
}

#[test]
fn invalid_regex_is_an_error() {
    // Arrange / Act
    let result = StandardCheck::pattern("[");

    // Assert
    assert!(
        result
            .expect_err("invalid pattern")
            .starts_with("Invalid standards pattern:")
    );
}

#[test]
fn real_adapter_reads_files_and_lists_the_git_checkout() {
    // Arrange
    let host = RealHost {
        git: PathBuf::from("git"),
    };
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // Act
    let manifest = host.read(&crate_root.join("Cargo.toml"));
    let bytes = host.read_bytes(&crate_root.join("Cargo.toml"));
    let is_file = host.is_file(&crate_root.join("Cargo.toml"));
    let is_directory_a_file = host.is_file(crate_root);
    let listing = host.inventory(crate_root);

    // Assert
    assert!(manifest.expect("crate manifest").contains("ag-xtask"));
    assert!(is_file.expect("crate manifest metadata"));
    assert!(!is_directory_a_file.expect("crate directory metadata"));
    assert!(
        bytes
            .expect("crate manifest bytes")
            .starts_with(b"[package]")
    );
    assert!(
        listing
            .expect("real Git inventory")
            .contains(&PathBuf::from("src/check_standard.rs"))
    );
}

#[cfg(unix)]
#[test]
fn real_git_lists_a_linked_directory_but_not_the_files_behind_it() {
    // Arrange
    let directory = tempdir().expect("Git fixture directory");
    let root = directory.path();
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status();
    fs::create_dir(root.join("generated")).expect("generated directory");
    fs::write(root.join("generated/protocol.ts"), TYPES).expect("generated types");
    symlink("generated", root.join("schema")).expect("linked directory");
    let host = RealHost {
        git: PathBuf::from("git"),
    };

    // Act
    let listing = host.inventory(root).expect("real Git inventory");
    let is_file = host.is_file(&root.join("schema/protocol.ts"));

    // Assert
    assert!(init.expect("git init").success());
    assert!(listing.contains(&PathBuf::from("generated/protocol.ts")));
    assert!(listing.contains(&PathBuf::from("schema")));
    assert!(!listing.contains(&PathBuf::from("schema/protocol.ts")));
    assert!(is_file.expect("metadata through the linked directory"));
}

#[test]
fn production_composition_requires_the_workspace_root() {
    // Arrange
    assert!(!Path::new("standards.toml").exists());

    // Act
    let result = run();

    // Assert
    assert!(
        result
            .expect_err("missing manifest")
            .starts_with("Failed to read ./standards.toml:")
    );
}
