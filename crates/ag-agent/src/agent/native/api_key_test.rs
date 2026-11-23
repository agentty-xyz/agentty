//! API key file unit tests covering import, precedence, unreadable files,
//! and saving pasted keys.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::{save_api_key, with_stored_api_keys};

/// Returns an environment whose data directory is `root`, plus `pairs`.
fn environment(root: &Path, pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    let mut environment = pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect::<BTreeMap<_, _>>();
    environment.insert(
        "AGENTTY_ROOT".to_string(),
        root.to_string_lossy().into_owned(),
    );

    environment
}

/// Returns the API key file under `root`.
fn key_file(root: &TempDir) -> std::path::PathBuf {
    root.path().join("auth/api_keys.json")
}

/// Writes `contents` as the API key file under `root`.
fn write_key_file(root: &TempDir, contents: &str) {
    fs::create_dir_all(root.path().join("auth")).expect("auth directory");
    fs::write(key_file(root), contents).expect("API key file");
}

/// Reads the API key file under `root` as JSON.
fn read_key_file(root: &TempDir) -> Value {
    serde_json::from_slice(&fs::read(key_file(root)).expect("API key file")).expect("JSON")
}

/// Returns the permission bits of `path`.
fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

/// Makes the auth directory under `root` read-only, so new files fail.
fn read_only_auth_directory(root: &TempDir) {
    let directory = root.path().join("auth");
    fs::create_dir_all(&directory).expect("auth directory");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o500)).expect("read-only");
}

#[tokio::test]
async fn exported_keys_are_imported_into_a_private_file() {
    // Arrange
    let root = tempfile::tempdir().expect("data root");
    let exported = environment(
        root.path(),
        &[
            ("CHATGPT_AUTH_FILE", "/tmp/sign-in.json"),
            ("KIMI_API_KEY", " kimi-key "),
            ("MODEL_API_KEY", " "),
            ("PATH", "/bin"),
        ],
    );

    // Act
    let environment = with_stored_api_keys(exported).await;

    // Assert
    assert_eq!(
        environment.get("KIMI_API_KEY").map(String::as_str),
        Some("kimi-key")
    );
    assert_eq!(environment.get("MODEL_API_KEY"), None);
    assert_eq!(
        environment.get("CHATGPT_AUTH_FILE").map(String::as_str),
        Some("/tmp/sign-in.json")
    );
    assert_eq!(environment.get("PATH").map(String::as_str), Some("/bin"));
    assert_eq!(
        read_key_file(&root),
        json!({"providers": {"kimi": {"api_key": "kimi-key"}}, "version": 1})
    );
    assert_eq!(mode(&key_file(&root)), 0o600);
    assert_eq!(mode(&root.path().join("auth")), 0o700);
    assert_eq!(
        fs::read_dir(root.path().join("auth"))
            .expect("auth directory")
            .count(),
        1,
        "no temporary file is left behind"
    );
}

#[tokio::test]
async fn a_stored_key_wins_over_an_exported_one_without_rewriting_the_file() {
    // Arrange
    let root = tempfile::tempdir().expect("data root");
    let contents =
        r#"{"version":1,"providers":{"kimi":{"api_key":"stored"},"retired":{"api_key":"kept"}}}"#;
    write_key_file(&root, contents);
    let exported = environment(root.path(), &[("KIMI_API_KEY", "exported")]);

    // Act
    let environment = with_stored_api_keys(exported).await;

    // Assert
    assert_eq!(
        environment.get("KIMI_API_KEY").map(String::as_str),
        Some("stored")
    );
    assert_eq!(fs::read_to_string(key_file(&root)).expect("file"), contents);
}

#[tokio::test]
async fn a_blank_stored_key_is_replaced_by_an_exported_one() {
    // Arrange
    let root = tempfile::tempdir().expect("data root");
    write_key_file(
        &root,
        r#"{"version":1,"providers":{"openai":{"api_key":"  "}}}"#,
    );
    let exported = environment(root.path(), &[("OPENAI_API_KEY", "sk-exported")]);

    // Act
    let environment = with_stored_api_keys(exported).await;

    // Assert
    assert_eq!(
        environment.get("OPENAI_API_KEY").map(String::as_str),
        Some("sk-exported")
    );
    assert_eq!(
        read_key_file(&root)["providers"]["openai"]["api_key"],
        "sk-exported"
    );
}

#[tokio::test]
async fn exported_keys_are_ignored_without_a_data_directory() {
    // Arrange
    let exported = BTreeMap::from([
        ("ANTHROPIC_API_KEY".to_string(), "sk-ant".to_string()),
        ("PATH".to_string(), "/bin".to_string()),
    ]);

    // Act
    let environment = with_stored_api_keys(exported).await;

    // Assert
    assert_eq!(
        environment,
        BTreeMap::from([("PATH".to_string(), "/bin".to_string())])
    );
}

#[tokio::test]
async fn an_unreadable_file_leaves_every_key_unset_and_untouched() {
    for contents in ["not json", r#"{"version":2,"providers":{}}"#] {
        // Arrange
        let root = tempfile::tempdir().expect("data root");
        write_key_file(&root, contents);
        let exported = environment(root.path(), &[("KIMI_API_KEY", "exported")]);

        // Act
        let environment = with_stored_api_keys(exported).await;

        // Assert
        assert_eq!(environment.get("KIMI_API_KEY"), None, "{contents}");
        assert_eq!(fs::read_to_string(key_file(&root)).expect("file"), contents);
    }
}

#[tokio::test]
async fn a_file_other_users_can_read_still_serves_its_keys() {
    // Arrange
    let root = tempfile::tempdir().expect("data root");
    write_key_file(
        &root,
        r#"{"version":1,"providers":{"qwen":{"api_key":"q"}}}"#,
    );
    fs::set_permissions(key_file(&root), fs::Permissions::from_mode(0o644)).expect("shared");

    // Act
    let environment = with_stored_api_keys(environment(root.path(), &[])).await;

    // Assert
    assert_eq!(
        environment.get("DASHSCOPE_API_KEY").map(String::as_str),
        Some("q")
    );
}

#[tokio::test]
async fn a_failed_import_still_serves_the_exported_key_for_this_run() {
    // Arrange
    let root = tempfile::tempdir().expect("data root");
    read_only_auth_directory(&root);
    let exported = environment(root.path(), &[("MODEL_API_KEY", "muse-key")]);

    // Act
    let environment = with_stored_api_keys(exported).await;

    // Assert
    assert_eq!(
        environment.get("MODEL_API_KEY").map(String::as_str),
        Some("muse-key")
    );
    assert!(!key_file(&root).exists());
}

#[tokio::test]
async fn saving_a_key_replaces_only_that_provider() {
    // Arrange
    let root = tempfile::tempdir().expect("data root");
    write_key_file(
        &root,
        r#"{"version":1,"providers":{"kimi":{"api_key":"old"},"muse":{"api_key":"muse"}}}"#,
    );

    // Act
    let saved = save_api_key(&environment(root.path(), &[]), "kimi", " new-key\n").await;

    // Assert
    assert_eq!(saved, Ok(()));
    assert_eq!(
        read_key_file(&root),
        json!({
            "providers": {"kimi": {"api_key": "new-key"}, "muse": {"api_key": "muse"}},
            "version": 1
        })
    );
    assert_eq!(mode(&key_file(&root)), 0o600);
}

#[tokio::test]
async fn saving_a_key_reports_each_failure() {
    // Arrange
    let corrupt = tempfile::tempdir().expect("corrupt root");
    write_key_file(&corrupt, "not json");
    let read_only = tempfile::tempdir().expect("read-only root");
    read_only_auth_directory(&read_only);
    let writable = tempfile::tempdir().expect("writable root");
    let file_root = tempfile::NamedTempFile::new().expect("file in place of a directory");

    // Act
    let sign_in = save_api_key(&environment(writable.path(), &[]), "chatgpt", "key").await;
    let blank = save_api_key(&environment(writable.path(), &[]), "kimi", "  ").await;
    let unlocated = save_api_key(&BTreeMap::new(), "kimi", "key").await;
    let unreadable = save_api_key(&environment(corrupt.path(), &[]), "kimi", "key").await;
    let unwritable = save_api_key(&environment(read_only.path(), &[]), "kimi", "key").await;
    let not_a_directory = save_api_key(&environment(file_root.path(), &[]), "kimi", "key").await;

    // Assert
    assert_eq!(
        sign_in,
        Err("The `chatgpt` provider takes no API key.".to_string())
    );
    assert_eq!(blank, Err("Paste a nonblank API key.".to_string()));
    assert_eq!(
        unlocated,
        Err("Set AGENTTY_ROOT or HOME to locate Agentty's API key file, then retry.".to_string())
    );
    let unreadable = unreadable.expect_err("a corrupt file should fail");
    assert!(unreadable.starts_with("Cannot read `"), "{unreadable}");
    assert!(
        unreadable.ends_with("Fix or delete it, then retry."),
        "{unreadable}"
    );
    let unwritable = unwritable.expect_err("a read-only directory should fail");
    assert!(unwritable.starts_with("Cannot save `"), "{unwritable}");
    assert!(
        not_a_directory.is_err_and(|error| error.starts_with("Cannot read `")),
        "a data directory that is a file should fail"
    );
    assert!(!key_file(&writable).exists());
    assert_eq!(
        fs::read_to_string(key_file(&corrupt)).expect("file"),
        "not json"
    );
}
