use std::env;
use std::path::PathBuf;

use super::auth_file;

fn environment(
    values: &'static [(&'static str, &'static str)],
) -> impl FnMut(&str) -> Result<String, env::VarError> {
    move |name| {
        values
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
            .ok_or(env::VarError::NotPresent)
    }
}

#[test]
fn auth_file_prefers_the_explicit_file_then_agentty_root_then_home() {
    // Arrange
    let explicit = environment(&[
        ("CHATGPT_AUTH_FILE", "/tmp/sign-in.json"),
        ("AGENTTY_ROOT", "/data/agentty"),
        ("HOME", "/home/user"),
    ]);
    let root = environment(&[
        ("CHATGPT_AUTH_FILE", ""),
        ("AGENTTY_ROOT", "/data/agentty"),
        ("HOME", "/home/user"),
    ]);
    let home = environment(&[("HOME", "/home/user")]);

    // Act
    let explicit = auth_file(explicit).expect("explicit file");
    let root = auth_file(root).expect("root file");
    let home = auth_file(home).expect("home file");

    // Assert
    assert_eq!(explicit, PathBuf::from("/tmp/sign-in.json"));
    assert_eq!(root, PathBuf::from("/data/agentty/auth/chatgpt.json"));
    assert_eq!(home, PathBuf::from("/home/user/.agentty/auth/chatgpt.json"));
}

#[test]
fn auth_file_is_unavailable_without_any_location() {
    // Arrange
    let empty = environment(&[]);

    // Act
    let result = auth_file(empty);

    // Assert
    assert!(result.is_err());
    assert!(
        result
            .err()
            .is_some_and(|error| error.to_string().contains("CHATGPT_AUTH_FILE"))
    );
}
