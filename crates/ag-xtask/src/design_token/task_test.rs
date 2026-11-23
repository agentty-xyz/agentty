//! Design token generation and check orchestration unit tests.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::{DesignTokens, Host, MockHost, OUTPUTS, Options, RealHost, run};

const ROOT: &str = "repo";

/// Hand-written base stylesheet that each surface receives a copy of.
const BASE: &str = "a {\n  color: var(--color-text);\n}\n";

/// Brand mark that the docs site receives a copy of.
const MARK: &str = "<svg viewBox=\"0 0 24 24\" stroke=\"currentColor\"></svg>\n";

/// Favicon bytes, which are not UTF-8.
const ICON: [u8; 4] = [0, 0, 1, 0xff];

/// Token file with one shared colour, one value for each generated surface,
/// and a contrast pair that passes.
fn tokens() -> String {
    let white = json!({ "colorSpace": "srgb", "components": [1, 1, 1], "hex": "#ffffff" });
    let black = json!({ "colorSpace": "srgb", "components": [0, 0, 0], "hex": "#000000" });

    json!({
        "color": {
            "$type": "color",
            "bg": { "$value": black, "$extensions": { "xyz.agentty.theme": { "dark": black } } },
            "text": { "$value": white }
        },
        "surfaces": {
            "docs-site": { "glyph": { "hot": { "$type": "color", "$value": white } } },
            "remote-client": { "font": { "mono": { "$type": "fontFamily", "$value": "monospace" } } }
        },
        "$extensions": {
            "xyz.agentty.contrast": [{
                "use": "Text",
                "minimum": 4.5,
                "foregrounds": ["{color.text}"],
                "backgrounds": ["{color.bg}"]
            }]
        }
    })
    .to_string()
}

/// In-memory repository whose inventory lists every file and listed path.
struct Fixture {
    failures: BTreeMap<PathBuf, io::ErrorKind>,
    files: Arc<Mutex<BTreeMap<PathBuf, Vec<u8>>>>,
    inventory_error: bool,
    listed: Vec<PathBuf>,
    write_error: bool,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self {
            failures: BTreeMap::new(),
            files: Arc::default(),
            inventory_error: false,
            listed: Vec::new(),
            write_error: false,
        };
        fixture.write("design/tokens.json", &tokens());
        fixture.write("design/base.css", BASE);
        fixture.write(
            "docs/site/sass/site.scss",
            "a { color: var(--color-text); }\n",
        );
        fixture.write(
            "crates/ag-remote/assets/app.css",
            "a { color: var(--color-text); }\n",
        );
        fixture.write("design/mark.svg", MARK);
        fixture.write_bytes("design/favicon.ico", &ICON);
        fixture.write(
            "crates/ag-remote/assets/app.js",
            "const bg = getComputedStyle(root).getPropertyValue('--color-bg');\n",
        );
        fixture.write(
            "crates/ag-remote/assets/index.html",
            "<meta name=\"theme-color\" content=\"#000000\">\n<script>const a = \
             'green';</script>\n",
        );
        fixture.write(
            "docs/site/templates/seo.html",
            "<title>{{ title }}</title>\n<meta name=\"theme-color\" content=\"#123456\">\n<meta \
             content=\"#654321\" name=\"theme-color\">\n",
        );
        // Scripts and templates outside the surfaces' pages may name colours.
        fixture.write("docs/site/tests/card.test.mjs", "const bg = '#070b08';\n");
        fixture.write("docs/site/README.html", "<p style=\"color: red\"></p>\n");

        fixture
    }

    fn write(&self, path: &str, text: &str) {
        self.write_bytes(path, text.as_bytes());
    }

    fn write_bytes(&self, path: &str, bytes: &[u8]) {
        self.files
            .lock()
            .expect("files")
            .insert(PathBuf::from(path), bytes.to_vec());
    }

    fn file(&self, path: &str) -> Option<String> {
        self.bytes(path)
            .map(|bytes| String::from_utf8(bytes).expect("text file"))
    }

    fn bytes(&self, path: &str) -> Option<Vec<u8>> {
        self.files
            .lock()
            .expect("files")
            .get(Path::new(path))
            .cloned()
    }

    fn run(&self, check: bool) -> Result<String, String> {
        let mut host = MockHost::new();
        let mut inventory: Vec<_> = self.files.lock().expect("files").keys().cloned().collect();
        inventory.extend(self.listed.iter().cloned());
        let inventory_error = self.inventory_error;
        host.expect_inventory()
            .withf(|root| root == Path::new(ROOT))
            .returning(move |_| {
                if inventory_error {
                    return Err("inventory failed".to_owned());
                }

                Ok(inventory.clone())
            });
        let files = Arc::clone(&self.files);
        let failures = self.failures.clone();
        let read = move |path: &Path| -> io::Result<Vec<u8>> {
            let relative = path.strip_prefix(ROOT).unwrap_or(path);
            if let Some(kind) = failures.get(relative) {
                return Err(io::Error::new(*kind, "denied"));
            }

            files
                .lock()
                .expect("files")
                .get(relative)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        };
        let read_text = read.clone();
        host.expect_read().returning(move |path| {
            String::from_utf8(read_text(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        });
        host.expect_read_bytes().returning(read);
        let files = Arc::clone(&self.files);
        let write_error = self.write_error;
        host.expect_write().returning(move |path, contents| {
            if write_error {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            let relative = path.strip_prefix(ROOT).unwrap_or(path).to_path_buf();
            files
                .lock()
                .expect("files")
                .insert(relative, contents.to_vec());

            Ok(())
        });

        DesignTokens { host: &host }.run(Path::new(ROOT), check)
    }

    fn errors(&self, check: bool) -> Vec<String> {
        self.run(check)
            .expect_err("design token errors")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

#[test]
fn generation_writes_every_stylesheet_and_a_check_then_passes() {
    // Arrange
    let fixture = Fixture::new();

    // Act
    let generated = fixture.run(false);
    let checked = fixture.run(true);

    // Assert
    assert_eq!(
        generated.as_deref(),
        Ok(
            "Design token stylesheets generated (4 tokens; 6 hand-written stylesheets, scripts, \
             and templates use no raw colour)."
        )
    );
    assert_eq!(
        checked.as_deref(),
        Ok(
            "Design token stylesheets are current (4 tokens; 6 hand-written stylesheets, scripts, \
             and templates use no raw colour)."
        )
    );
    assert_eq!(
        fixture.file("docs/site/templates/seo.html").as_deref(),
        Some(
            "<title>{{ title }}</title>\n<meta name=\"theme-color\" content=\"#000000\">\n<meta \
             content=\"#000000\" name=\"theme-color\">\n"
        )
    );
    assert_eq!(
        fixture.bytes("docs/site/static/favicon.ico"),
        Some(ICON.to_vec())
    );
    assert_eq!(
        fixture.file("docs/site/static/mark.svg").as_deref(),
        Some(MARK)
    );
    let copy = format!(
        "/* Generated from design/base.css by `cargo run -p ag-xtask -- design-tokens`. Do not \
         edit. */\n{BASE}"
    );
    assert_eq!(
        fixture.file("docs/site/sass/_base.scss"),
        Some(copy.clone())
    );
    assert_eq!(fixture.file("crates/ag-remote/assets/base.css"), Some(copy));
    assert_eq!(
        fixture.file("design/tokens.css").as_deref(),
        Some(
            "/* Generated from tokens.json. Do not edit. */\n:root {\n  --color-bg: #000000;\n  \
             --color-text: #ffffff;\n}\n\n:root.theme-dark {\n  --color-bg: #000000;\n}\n"
        )
    );
    assert!(
        fixture
            .file("docs/site/sass/_tokens.scss")
            .is_some_and(|text| text.contains("  --glyph-hot: #ffffff;\n"))
    );
    assert!(
        fixture
            .file("crates/ag-remote/assets/tokens.css")
            .is_some_and(|text| text.contains("  --font-mono: monospace;\n"))
    );
}

#[test]
fn check_reports_stale_and_missing_stylesheets() {
    // Arrange
    let fixture = Fixture::new();
    fixture.run(false).expect("generated stylesheets");
    fixture.write("design/tokens.css", "/* Edited. */\n");
    fixture
        .files
        .lock()
        .expect("files")
        .remove(Path::new("docs/site/sass/_tokens.scss"));
    fixture.write("design/base.css", "b {\n  color: var(--color-bg);\n}\n");
    fixture.write_bytes("design/favicon.ico", &[1]);
    fixture.write(
        "crates/ag-remote/assets/index.html",
        "<p>\n<meta name=\"theme-color\" content=\"#ffffff\">\n",
    );

    // Act
    let errors = fixture.errors(true);

    // Assert
    assert_eq!(
        errors,
        [
            "design/tokens.css is stale; run `cargo run -p ag-xtask -- design-tokens`",
            "docs/site/sass/_tokens.scss is missing; run `cargo run -p ag-xtask -- design-tokens`",
            "docs/site/sass/_base.scss is stale; run `cargo run -p ag-xtask -- design-tokens`",
            "docs/site/static/favicon.ico is stale; run `cargo run -p ag-xtask -- design-tokens`",
            "crates/ag-remote/assets/base.css is stale; run `cargo run -p ag-xtask -- \
             design-tokens`",
            "crates/ag-remote/assets/index.html:2: theme-color #ffffff is stale; run `cargo run \
             -p ag-xtask -- design-tokens`",
        ]
    );
}

#[test]
fn raw_colours_contrast_failures_and_unknown_surfaces_fail_both_modes() {
    // Arrange
    let mut fixture = Fixture::new();
    let tokens = tokens()
        .replace("\"remote-client\"", "\"print\"")
        .replace("\"minimum\":4.5", "\"minimum\":22");
    fixture.write("design/tokens.json", &tokens);
    fixture.write("docs/site/sass/site.scss", "a {\n  color: red;\n}\n");
    fixture.write("crates/ag-remote/assets/app.css", "a { color: #fff; }\n");
    fixture.write(
        "crates/ag-remote/assets/app.js",
        "// '#000' in a comment\nconst red = '#ff0000';\n",
    );
    fixture.write(
        "docs/site/templates/page.html",
        "<svg>\n  <path fill=\"#fff\" />\n</svg>\n<script>\n  ctx.fillStyle = 'rgba(0, 0, 0, \
         0.2)';\n</script>\n",
    );
    fixture
        .listed
        .push(PathBuf::from("docs/site/sass/deleted.scss"));

    // Act
    let generated = fixture.errors(false);
    let checked = fixture.errors(true);

    // Assert
    let expected = [
        "design/tokens.json: surface print has no generated stylesheet",
        "green: {color.text} on {color.bg} (Text) is 21.00:1, below 22:1; change a colour or \
         record a shortfall with its reason",
        "dark: {color.text} on {color.bg} (Text) is 21.00:1, below 22:1; change a colour or \
         record a shortfall with its reason",
        "crates/ag-remote/assets/app.css:1: raw colour #fff; add it to design/tokens.json and use \
         its custom property",
        "crates/ag-remote/assets/app.js:2: raw colour #ff0000; add it to design/tokens.json and \
         use its custom property",
        "docs/site/sass/site.scss:2: raw colour red; add it to design/tokens.json and use its \
         custom property",
        "docs/site/templates/page.html:2: raw colour #fff; add it to design/tokens.json and use \
         its custom property",
        "docs/site/templates/page.html:5: raw colour rgba(); add it to design/tokens.json and use \
         its custom property",
    ];
    assert_eq!(generated, expected);
    assert_eq!(checked, expected);
}

#[test]
fn theme_color_metas_need_the_page_background_token() {
    // Arrange
    let fixture = Fixture::new();
    fixture.write("design/tokens.json", &tokens().replace("bg", "page"));

    // Act
    let generated = fixture.errors(false);
    let checked = fixture.errors(true);

    // Assert
    let expected = [
        "crates/ag-remote/assets/index.html: a theme-color meta takes --color-bg from \
         design/tokens.json, which has none",
        "docs/site/templates/seo.html: a theme-color meta takes --color-bg from \
         design/tokens.json, which has none",
    ];
    assert_eq!(generated, expected);
    assert_eq!(checked, expected);
}

#[test]
fn a_surface_that_replaces_the_page_background_gets_it_in_its_own_metas() {
    // Arrange
    let fixture = Fixture::new();
    let white = r##"{"colorSpace":"srgb","components":[1,1,1],"hex":"#ffffff"}"##;
    fixture.write(
        "design/tokens.json",
        &tokens().replace(
            "\"docs-site\":{",
            &format!(
                "\"docs-site\":{{\"color\":{{\"bg\":{{\"$type\":\"color\",\"$value\":{white}}}}},"
            ),
        ),
    );

    // Act
    let generated = fixture.run(false);

    // Assert
    assert!(generated.is_ok(), "{generated:?}");
    assert!(
        fixture
            .file("docs/site/templates/seo.html")
            .is_some_and(|text| text.contains("content=\"#ffffff\"") && !text.contains("#000000"))
    );
    assert_eq!(
        fixture
            .file("crates/ag-remote/assets/index.html")
            .as_deref(),
        Some(
            "<meta name=\"theme-color\" content=\"#000000\">\n<script>const a = \
             'green';</script>\n"
        )
    );
}

#[test]
fn invalid_or_unreadable_inputs_fail_before_any_check() {
    // Arrange
    let invalid = Fixture::new();
    invalid.write("design/tokens.json", "[]");
    let mut unreadable = Fixture::new();
    unreadable.failures.insert(
        PathBuf::from("design/tokens.json"),
        io::ErrorKind::PermissionDenied,
    );

    // Act
    let invalid = invalid.run(true);
    let unreadable = unreadable.run(true);

    // Assert
    assert_eq!(
        invalid.err().as_deref(),
        Some("design/tokens.json: expected a group at the root")
    );
    assert_eq!(
        unreadable.err().as_deref(),
        Some("Failed to read repo/design/tokens.json: denied")
    );
}

#[test]
fn host_failures_stop_generation_and_checks() {
    // Arrange
    let mut unwritable = Fixture::new();
    unwritable.write_error = true;
    let mut unreadable_output = Fixture::new();
    unreadable_output.failures.insert(
        PathBuf::from(OUTPUTS[0].path),
        io::ErrorKind::PermissionDenied,
    );
    let mut unreadable_stylesheet = Fixture::new();
    unreadable_stylesheet.failures.insert(
        PathBuf::from("crates/ag-remote/assets/app.css"),
        io::ErrorKind::PermissionDenied,
    );
    let mut no_inventory = Fixture::new();
    no_inventory.inventory_error = true;
    let mut unreadable_base = Fixture::new();
    unreadable_base.failures.insert(
        PathBuf::from("design/base.css"),
        io::ErrorKind::PermissionDenied,
    );

    // Act
    let results = [
        unwritable.run(false),
        unreadable_output.run(true),
        unreadable_stylesheet.run(false),
        no_inventory.run(false),
        unreadable_base.run(false),
    ];

    // Assert
    assert_eq!(
        results.map(Result::err),
        [
            Some("Failed to write repo/design/tokens.css: permission denied".to_owned()),
            Some("Failed to read repo/design/tokens.css: denied".to_owned()),
            Some("Failed to read repo/crates/ag-remote/assets/app.css: denied".to_owned()),
            Some("inventory failed".to_owned()),
            Some("Failed to read repo/design/base.css: denied".to_owned()),
        ]
    );
}

#[test]
fn real_adapter_reads_writes_and_lists_the_git_checkout() {
    // Arrange
    let host = RealHost {
        git: PathBuf::from("git"),
    };
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("tokens.css");

    // Act
    let written = host.write(&path, b":root {}\n");
    let read = host.read(&path);
    let bytes = host.read_bytes(&path);
    let listing = host.inventory(crate_root);

    // Assert
    assert!(written.is_ok());
    assert_eq!(read.expect("written stylesheet"), ":root {}\n");
    assert_eq!(bytes.expect("written bytes"), b":root {}\n");
    assert!(
        listing
            .expect("real Git inventory")
            .contains(&PathBuf::from("src/design_token.rs"))
    );
}

#[test]
fn production_composition_requires_the_workspace_root() {
    // Arrange
    assert!(!Path::new("design/tokens.json").exists());

    // Act
    let result = run(&Options { check: true });

    // Assert
    assert!(
        result
            .expect_err("missing token file")
            .starts_with("Failed to read ./design/tokens.json:")
    );
}
