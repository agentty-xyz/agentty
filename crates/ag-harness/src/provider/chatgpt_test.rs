use std::path::PathBuf;
use std::{env, fs};

use ag_router::chatgpt::ChatGptCredentials;

use super::{auth_file, is_signed_in};

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

#[test]
fn is_signed_in_requires_a_readable_sign_in_that_openai_still_accepts() {
    // Arrange
    let directory = tempfile::tempdir().expect("temporary directory");
    let signed_in = directory.path().join("signed-in.json");
    let rejected = directory.path().join("rejected.json");
    let unreadable = directory.path().join("unreadable.json");
    let credentials = |refresh_token: &str| ChatGptCredentials {
        access_token: "access".to_string(),
        client_id: "oaiapp_client".to_string(),
        email: None,
        expires_at: 0,
        ext_agent_host_id: "urn:uuid:host".to_string(),
        id_token: String::new(),
        refresh_token: refresh_token.to_string(),
        scopes: vec!["chatgpt.tokens.use.direct".to_string()],
    };
    credentials("refresh")
        .save(&signed_in)
        .expect("signed-in file");
    credentials("").save(&rejected).expect("rejected file");
    fs::write(&unreadable, "not json").expect("unreadable file");

    // Act
    let statuses = [
        is_signed_in(&signed_in),
        is_signed_in(&rejected),
        is_signed_in(&unreadable),
        is_signed_in(&directory.path().join("missing.json")),
    ];

    // Assert
    assert_eq!(statuses, [true, false, false, false]);
}
