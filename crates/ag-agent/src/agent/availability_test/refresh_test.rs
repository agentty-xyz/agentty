use std::ffi::OsStr;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, fs};

use tempfile::tempdir;

use crate::agent::availability::{
    available_agent_clis_from_path, detect_agent_cli_version_with_timeout,
    parse_agent_cli_version_output, refresh_agent_cli_version, refresh_agent_cli_versions,
    run_agent_cli_update_with_timeout,
};
use crate::model::agent::{AgentCliInfo, AgentKind};

#[test]
/// Ensures available CLI metadata includes parsed command versions.
fn test_available_agent_clis_from_path_includes_versions() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(&codex_path, "#!/bin/sh\nprintf 'codex-cli 1.2.3\\n'\n")
        .expect("failed to create codex executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let available_agent_clis = available_agent_clis_from_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(
        available_agent_clis,
        vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("1.2.3".to_string())
        )]
    );
}

#[test]
/// Ensures the startup CLI refresh runs `update` before probing the
/// visible version.
fn test_available_agent_clis_from_path_updates_before_version_probe() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    let version_path = temp_directory.path().join("codex-version");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then printf '9.9.9-updated\\n' > \"{}\"; exit 0; \
         fi\nif [ \"$1\" = \"--version\" ]; then if [ -f \"{}\" ]; then read version < \"{}\"; \
         else version='1.0.0-old'; fi; printf 'codex-cli %s\\n' \"$version\"; exit 0; fi\nexit 1\n",
        version_path.display(),
        version_path.display(),
        version_path.display(),
    );
    fs::write(&codex_path, script).expect("failed to create codex executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");
    let path_value = env::join_paths([temp_directory.path()]).expect("valid path");

    // Act
    let available_agent_clis = available_agent_clis_from_path(Some(path_value.as_os_str()));

    // Assert
    assert_eq!(
        available_agent_clis,
        vec![AgentCliInfo::new(
            AgentKind::Codex,
            Some("9.9.9-updated".to_string())
        )]
    );
    assert!(version_path.exists());
}

#[test]
/// Ensures CLI refreshes start independently so one slow provider does
/// not delay every following provider.
fn test_refresh_agent_cli_versions_runs_providers_concurrently() {
    // Arrange
    let codex_started = Arc::new(AtomicBool::new(false));
    let refresh_cli_version = {
        let codex_started = Arc::clone(&codex_started);

        move |_agent_kind: AgentKind, executable_path: &Path| {
            if executable_path.file_name() == Some(OsStr::new("agy")) {
                let started_at = Instant::now();
                while !codex_started.load(Ordering::SeqCst)
                    && started_at.elapsed() < Duration::from_millis(200)
                {
                    std::thread::sleep(Duration::from_millis(1));
                }

                return if codex_started.load(Ordering::SeqCst) {
                    Some("agy-concurrent".to_string())
                } else {
                    Some("agy-sequential".to_string())
                };
            }

            if executable_path.file_name() == Some(OsStr::new("codex")) {
                codex_started.store(true, Ordering::SeqCst);

                return Some("codex-current".to_string());
            }

            None
        }
    };
    let executable_agent_clis = vec![
        (AgentKind::Antigravity, PathBuf::from("agy")),
        (AgentKind::Codex, PathBuf::from("codex")),
    ];

    // Act
    let agent_clis = refresh_agent_cli_versions(executable_agent_clis, refresh_cli_version);

    // Assert
    assert_eq!(
        agent_clis,
        vec![
            AgentCliInfo::new(AgentKind::Antigravity, Some("agy-concurrent".to_string())),
            AgentCliInfo::new(AgentKind::Codex, Some("codex-current".to_string())),
        ]
    );
}

#[test]
/// Ensures failed CLI updates do not prevent the post-update version
/// probe from refreshing the row.
fn test_refresh_agent_cli_version_probes_version_when_update_fails() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(
        &codex_path,
        "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 1; fi\nif [ \"$1\" = \"--version\" ]; \
         then printf 'codex-cli 1.2.3\\n'; exit 0; fi\nexit 1\n",
    )
    .expect("failed to create codex executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");

    // Act
    let detected_version = refresh_agent_cli_version(AgentKind::Codex, &codex_path, None);

    // Assert
    assert_eq!(detected_version, Some("1.2.3".to_string()));
}

#[test]
/// Ensures npm-global Gemini installations update through npm and expose
/// the refreshed version.
fn test_npm_global_gemini_update_refreshes_version() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let bin_directory = temp_directory.path().join("bin");
    let gemini_package_directory = temp_directory
        .path()
        .join("lib/node_modules/@google/gemini-cli/bundle");
    let gemini_package_path = gemini_package_directory.join("gemini.js");
    let gemini_path = bin_directory.join("gemini");
    let npm_path = bin_directory.join("npm");
    let version_path = temp_directory.path().join("gemini-version");
    fs::create_dir_all(&bin_directory).expect("failed to create bin directory");
    fs::create_dir_all(&gemini_package_directory)
        .expect("failed to create Gemini package directory");
    fs::write(
        &gemini_package_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then exit 91; fi\nif [ \"$1\" = \"--version\" \
             ]; then if [ -f \"{}\" ]; then read version < \"{}\"; else version='1.0.0-old'; fi; \
             printf 'gemini %s\\n' \"$version\"; exit 0; fi\nexit 1\n",
            version_path.display(),
            version_path.display(),
        ),
    )
    .expect("failed to create Gemini executable");
    fs::write(
        &npm_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"install\" ] && [ \"$2\" = \"-g\" ] && [ \"$3\" = \
             \"@google/gemini-cli@latest\" ]; then printf '9.9.9-updated\\n' > \"{}\"; exit 0; \
             fi\nexit 1\n",
            version_path.display(),
        ),
    )
    .expect("failed to create npm executable");
    fs::set_permissions(&gemini_package_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark Gemini executable");
    fs::set_permissions(&npm_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark npm executable");
    symlink(&gemini_package_path, &gemini_path).expect("failed to link Gemini executable");
    let path_value = env::join_paths([&bin_directory]).expect("valid path");

    // Act
    let did_update = run_agent_cli_update_with_timeout(
        AgentKind::Gemini,
        &gemini_path,
        Some(path_value.as_os_str()),
        Duration::from_secs(10),
    );
    let detected_version =
        detect_agent_cli_version_with_timeout(&gemini_path, Duration::from_secs(10));

    // Assert
    assert!(did_update);
    assert_eq!(detected_version, Some("9.9.9-updated".to_string()));
    assert_eq!(
        fs::read_to_string(version_path).expect("updated Gemini version"),
        "9.9.9-updated\n"
    );
}

#[test]
/// Ensures Gemini installations with an unknown owner do not launch the
/// removed native update command.
fn test_run_agent_cli_update_skips_unknown_gemini_installation() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let gemini_path = temp_directory.path().join("gemini");
    let update_marker_path = temp_directory.path().join("gemini-update");
    fs::write(
        &gemini_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then touch \"{}\"; exit 0; fi\nexit 1\n",
            update_marker_path.display(),
        ),
    )
    .expect("failed to create Gemini executable");
    fs::set_permissions(&gemini_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark Gemini executable");

    // Act
    let did_update = run_agent_cli_update_with_timeout(
        AgentKind::Gemini,
        &gemini_path,
        None,
        Duration::from_millis(100),
    );

    // Assert
    assert!(!did_update);
    assert!(!update_marker_path.exists());
}

#[test]
/// Ensures noisy CLI update commands cannot block on unread pipe buffers.
fn test_run_agent_cli_update_discards_output_without_pipe_backpressure() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(
        &codex_path,
        "#!/bin/sh\nif [ \"$1\" = \"update\" ]; then i=0; while [ \"$i\" -lt 4096 ]; do \
         printf \
         '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\\n'; \
         printf \
         'fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\\n' \
         >&2; i=$((i + 1)); done; exit 0; fi\nexit 1\n",
    )
    .expect("failed to create noisy codex executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");

    // Act
    let did_finish = run_agent_cli_update_with_timeout(
        AgentKind::Codex,
        &codex_path,
        None,
        Duration::from_secs(10),
    );

    // Assert
    assert!(did_finish);
}

#[test]
/// Ensures unresponsive CLI version commands time out without returning a
/// version.
fn test_detect_agent_cli_version_with_timeout_handles_hanging_commands() {
    // Arrange
    let temp_directory = tempdir().expect("failed to create temp dir");
    let codex_path = temp_directory.path().join("codex");
    fs::write(&codex_path, "#!/bin/sh\nwhile :; do :; done\n")
        .expect("failed to create hanging codex executable");
    fs::set_permissions(&codex_path, fs::Permissions::from_mode(0o750))
        .expect("failed to mark codex executable");

    // Act
    let detected_version =
        detect_agent_cli_version_with_timeout(&codex_path, Duration::from_millis(50));

    // Assert
    assert_eq!(detected_version, None);
}

#[test]
/// Ensures non-version text falls back to the first useful output line.
fn test_parse_agent_cli_version_output_falls_back_to_line() {
    // Arrange
    let output = "Claude Code development build\n";

    // Act
    let parsed_version = parse_agent_cli_version_output(output);

    // Assert
    assert_eq!(
        parsed_version,
        Some("Claude Code development build".to_string())
    );
}
