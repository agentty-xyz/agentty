//! Module router discovery and statement scanning unit tests.

use std::io;
use std::path::{Path, PathBuf};

use mockall::predicate::eq;

use crate::check_module_router::{ModuleRouterCheck, run};
use crate::file_system::MockFileSystem;

/// Router text exercising every allowed top-level form.
const ROUTER: &str = r##"//! Router documentation with `fn` and { braces }.
#![doc = "inner } attribute"]

/* Block comment /* nested */ struct Hidden; */
mod child;
pub mod public_child;
pub(crate) mod crate_child;
pub(in crate::parent) mod scoped_child;

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;

use std::fmt;
pub use child::{Item, Other};
pub(super) use public_child::Value as Renamed;
extern crate alloc;
#[doc = r#"raw "string" with fn"#]
pub use crate_child::Thing;
#[cfg_attr(any(), example(']', '\'', '\u{5d}', "]", [1, 2]))]
use scoped_child::Last<'static>;
use crate::r#type;

/// Re-exports grouped in an inline module.
pub mod grouped {
    pub use super::Item;
}
mod unbalanced {
"##;

fn check(file_system: &MockFileSystem) -> Result<(), String> {
    ModuleRouterCheck { file_system }.run(Path::new("crates"))
}

/// Mocks a workspace with one member whose `src/` holds `entries`, where
/// directories are the entries without an extension.
fn workspace(entries: &[&str], router: &str) -> MockFileSystem {
    let entries = entries
        .iter()
        .map(|entry| Path::new("crates/example/src").join(entry))
        .collect::<Vec<_>>();
    let directories = entries
        .iter()
        .filter(|entry| entry.extension().is_none())
        .cloned()
        .collect::<Vec<_>>();
    let router = router.to_string();
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates")))
        .return_once(|_| {
            Ok(vec![
                PathBuf::from("crates/example"),
                PathBuf::from("crates/README.md"),
            ])
        });
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates/example/src")))
        .return_once(move |_| Ok(entries));
    file_system.expect_read_dir().returning(|_| Ok(Vec::new()));
    file_system.expect_is_dir().returning(move |path| {
        Ok(path == Path::new("crates/example/src")
            || directories.iter().any(|directory| directory == path))
    });
    file_system
        .expect_read_to_string()
        .returning(move |_| Ok(router.clone()));

    file_system
}

#[test]
fn routers_with_declarations_imports_and_attributes_pass() {
    // Arrange
    let file_system = workspace(&["app.rs", "app", "lib.rs"], ROUTER);

    // Act
    let result = check(&file_system);

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn router_definitions_are_reported_with_their_keyword_and_fix() {
    // Arrange
    let router = "mod child;\npub struct Config {\n    value: u8,\n}\nconst LIMIT: [u8; 2] = [1, \
                  2];\nstatic VALUE: Config = Config { value: 1 };\npub(crate) fn run() {}\nimpl \
                  Config {}\nmacro_rules! example { () => {}; }\nmod inline { pub use \
                  child::Item; fn hidden() {} }\nthread_local!(static KEY: u8 = 0);\npubby();\n# \
                  value;\n#[unclosed\n";
    let file_system = workspace(&["app.rs", "app"], router);

    // Act
    let result = check(&file_system);

    // Assert
    let violations = [
        "struct",
        "const",
        "static",
        "fn",
        "impl",
        "macro_rules!",
        "fn",
        "thread_local!",
        "pubby",
        "#",
        "#",
    ]
    .map(|kind| {
        format!(
            "crates/example/src/app.rs: router module `app.rs` defines `{kind}`; move it into a \
             child module under `app/`"
        )
    });
    assert_eq!(
        result,
        Err(format!(
            "Module router violations:\n{}",
            violations.join("\n")
        ))
    );
}

#[test]
fn test_files_test_directories_and_unpaired_files_are_skipped() {
    // Arrange
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates")))
        .return_once(|_| Ok(vec![PathBuf::from("crates/example")]));
    file_system
        .expect_read_dir()
        .with(eq(Path::new("crates/example/src")))
        .return_once(|_| {
            Ok(vec![
                PathBuf::from("crates/example/src/app_test.rs"),
                PathBuf::from("crates/example/src/app_test"),
                PathBuf::from("crates/example/src/notes.md"),
                PathBuf::from("crates/example/src/lone.rs"),
            ])
        });
    file_system.expect_is_dir().returning(|path| {
        Ok(path == Path::new("crates/example/src")
            || path == Path::new("crates/example/src/app_test")
            || path == Path::new("crates/example/src/app"))
    });

    // Act
    let result = check(&file_system);

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn unterminated_literals_comments_and_trailing_attributes_define_nothing() {
    // Arrange
    for router in [
        "mod child;\n\"unterminated \\\" fn",
        "mod child;\nr#\"unterminated fn",
        "mod child;\n/* unterminated fn",
        "mod child;\n// trailing comment fn",
        "mod child;\n#[cfg(test)]",
    ] {
        let file_system = workspace(&["app.rs", "app"], router);

        // Act
        let result = check(&file_system);

        // Assert
        assert_eq!(result, Ok(()), "{router}");
    }
}

#[test]
fn unreadable_directories_metadata_and_files_fail_the_check() {
    // Arrange
    let mut unreadable_root = MockFileSystem::new();
    unreadable_root
        .expect_read_dir()
        .return_once(|_| Err(io::Error::other("denied")));
    let mut unreadable_metadata = MockFileSystem::new();
    unreadable_metadata
        .expect_read_dir()
        .return_once(|_| Ok(vec![PathBuf::from("crates/example")]));
    unreadable_metadata
        .expect_is_dir()
        .return_once(|_| Err(io::Error::other("denied")));
    let mut unreadable_file = MockFileSystem::new();
    unreadable_file
        .expect_read_dir()
        .with(eq(Path::new("crates")))
        .return_once(|_| Ok(vec![PathBuf::from("crates/example")]));
    unreadable_file
        .expect_read_dir()
        .return_once(|_| Ok(vec![PathBuf::from("crates/example/src/app.rs")]));
    unreadable_file.expect_is_dir().returning(|path| {
        Ok(path == Path::new("crates/example/src") || path == Path::new("crates/example/src/app"))
    });
    unreadable_file
        .expect_read_to_string()
        .return_once(|_| Err(io::Error::other("denied")));

    // Act
    let root = check(&unreadable_root);
    let metadata = check(&unreadable_metadata);
    let file = check(&unreadable_file);

    // Assert
    assert_eq!(root, Err("Failed to read crates: denied".to_string()));
    assert_eq!(
        metadata,
        Err("Failed to inspect crates/example/src: denied".to_string())
    );
    assert_eq!(
        file,
        Err("Failed to read crates/example/src/app.rs: denied".to_string())
    );
}

#[test]
fn production_composition_reports_a_missing_workspace_root() {
    // Arrange
    // Cargo runs unit tests in the package directory, outside the workspace
    // root.
    assert!(!Path::new("crates").exists());

    // Act
    let result = run();

    // Assert
    assert!(
        result
            .expect_err("missing workspace root")
            .starts_with("Failed to read crates:")
    );
}
