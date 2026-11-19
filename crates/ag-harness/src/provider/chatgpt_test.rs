//! `ChatGPT` provider unit tests for auth file location and sign-in status.

use std::path::{Path, PathBuf};
use std::{env, fs};

use ag_router::chatgpt::ChatGptCredentials;

use super::{auth_file, auth_file_in, is_signed_in};

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
fn auth_file_reads_only_the_named_file() {
    // Arrange
    let explicit = environment(&[
        ("CHATGPT_AUTH_FILE", "/tmp/sign-in.json"),
        ("AGENTTY_ROOT", "/data/agentty"),
        ("HOME", "/home/user"),
    ]);

    // Act
    let explicit = auth_file(explicit).expect("explicit file");

    // Assert
    assert_eq!(explicit, PathBuf::from("/tmp/sign-in.json"));
}

#[test]
fn auth_file_has_no_host_default_location() {
    // Arrange
    let host_locations = environment(&[
        ("CHATGPT_AUTH_FILE", " "),
        ("AGENTTY_ROOT", "/data/agentty"),
        ("HOME", "/home/user"),
    ]);

    // Act
    let result = auth_file(host_locations);

    // Assert
    assert!(
        result
            .err()
            .is_some_and(|error| error.to_string().contains("CHATGPT_AUTH_FILE"))
    );
}

#[test]
fn auth_file_in_prefers_the_named_file_then_the_host_data_directory() {
    // Arrange
    let data_dir = Path::new("/data/agentty");

    // Act
    let locations = [
        auth_file_in(
            environment(&[("CHATGPT_AUTH_FILE", "/tmp/sign-in.json")]),
            Some(data_dir),
        ),
        auth_file_in(environment(&[("CHATGPT_AUTH_FILE", " ")]), Some(data_dir)),
        auth_file_in(environment(&[]), None),
    ];

    // Assert
    assert_eq!(
        locations,
        [
            Some(PathBuf::from("/tmp/sign-in.json")),
            Some(PathBuf::from("/data/agentty/auth/chatgpt.json")),
            None,
        ]
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
