//! Sandbox choice, availability checks, warnings, and helper registration.

#[cfg(feature = "native-sandbox")]
use std::os::unix::fs::PermissionsExt;
#[cfg(feature = "native-sandbox")]
use std::path::{Path, PathBuf};
#[cfg(feature = "native-sandbox")]
use std::sync::Arc;
#[cfg(feature = "native-sandbox")]
use std::time::Duration;

use serde_json::json;

#[cfg(feature = "native-sandbox")]
use super::{DISPATCHED_HELPER, IfUnavailable, Sandbox, SandboxGate, dispatch};
use super::{SandboxUnavailableReason, SandboxWarning};
#[cfg(feature = "native-sandbox")]
use crate::bash::{BashConfig, Enforcement, UnsandboxedExecutor};
#[cfg(feature = "native-sandbox")]
use crate::execution::Host;
#[cfg(feature = "native-sandbox")]
use crate::turn::TurnError;

/// Every reason, in declaration order.
const REASONS: [SandboxUnavailableReason; 9] = [
    SandboxUnavailableReason::UnsupportedPlatform,
    SandboxUnavailableReason::HelperMissing,
    SandboxUnavailableReason::HelperUntrusted,
    SandboxUnavailableReason::BubblewrapMissing,
    SandboxUnavailableReason::UserNamespacesUnavailable,
    SandboxUnavailableReason::LandlockUnavailable,
    SandboxUnavailableReason::SeatbeltMissing,
    SandboxUnavailableReason::SeatbeltUnavailable,
    SandboxUnavailableReason::UnixSocketsUnsupported,
];

#[test]
fn reasons_state_what_is_missing_and_how_to_fix_it() {
    for reason in REASONS {
        // Act
        let warning = SandboxWarning::new(reason);

        // Assert
        assert!(reason.message().ends_with('.'), "{reason:?}");
        assert!(reason.remedy().ends_with('.'), "{reason:?}");
        for api in ["::", "BashConfig", "Sandbox", "dispatch", "ag-harness"] {
            assert!(
                !reason.remedy().contains(api),
                "{reason:?} remedy names an API: {api}"
            );
        }
        assert_eq!(reason.to_string(), reason.message());
        assert_eq!(warning.reason, reason);
        assert_eq!(warning.message, reason.message());
        assert_eq!(warning.remedy, reason.remedy());
    }
    assert!(
        SandboxUnavailableReason::BubblewrapMissing
            .remedy()
            .contains("apt install bubblewrap")
    );
}

#[test]
fn warnings_encode_their_reason_in_snake_case() {
    // Arrange
    let warning = SandboxWarning::new(SandboxUnavailableReason::UserNamespacesUnavailable);

    // Act
    let encoded = serde_json::to_value(&warning).expect("encoded warning");

    // Assert
    assert_eq!(
        encoded,
        json!({
            "message": warning.message,
            "reason": "user_namespaces_unavailable",
            "remedy": warning.remedy,
        })
    );
    assert_eq!(
        serde_json::from_value::<SandboxWarning>(encoded).expect("decoded warning"),
        warning
    );
}

#[cfg(feature = "native-sandbox")]
#[test]
fn default_sandbox_is_native_and_runs_unsandboxed_where_unavailable() {
    // Act / Assert
    assert_eq!(
        Sandbox::default(),
        Sandbox::Native {
            if_unavailable: IfUnavailable::Unsandboxed
        }
    );
}

#[cfg(feature = "native-sandbox")]
#[tokio::test]
async fn host_executors_and_chosen_unsandboxed_execution_skip_the_check() {
    // Arrange
    let fixture = GateFixture::new();
    let host = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("host executor");
    let native = fixture.native(&fixture.workspace.join("helper"));
    let unsandboxed = SandboxGate::new(Sandbox::Unsandboxed);
    let refusing = SandboxGate::new(Sandbox::Native {
        if_unavailable: IfUnavailable::Refuse,
    });

    // Act
    let (chosen, chosen_warning) = unsandboxed
        .resolve(&native, &fixture.workspace)
        .await
        .expect("chosen unsandboxed execution");
    let (kept, kept_warning) = refusing
        .resolve(&host, &fixture.workspace)
        .await
        .expect("host executor");

    // Assert
    assert_eq!(chosen.enforcement(), Enforcement::None);
    assert!(chosen.executor.is_some());
    assert_eq!(chosen.fingerprint()["executor"], "unsandboxed");
    assert_eq!(chosen.fingerprint()["launcher"], serde_json::Value::Null);
    assert_eq!(native.fingerprint().get("executor"), None);
    assert_eq!(chosen_warning, None);
    assert_eq!(kept, host);
    assert_eq!(kept_warning, None);
}

#[cfg(feature = "native-sandbox")]
#[tokio::test]
async fn unavailable_sandbox_runs_unsandboxed_with_a_warning_or_refuses() {
    // Arrange
    let fixture = GateFixture::new();
    let native = fixture.native(&fixture.workspace.join("helper"));
    let refusing = SandboxGate::new(Sandbox::Native {
        if_unavailable: IfUnavailable::Refuse,
    });

    // Act
    let (fallback, warning) = SandboxGate::default()
        .resolve(&native, &fixture.workspace)
        .await
        .expect("unsandboxed fallback");
    let refused = refusing.resolve(&native, &fixture.workspace).await;

    // Assert
    assert_eq!(fallback.enforcement(), Enforcement::None);
    assert!(fallback.executor.is_some());
    assert_eq!(
        warning,
        Some(SandboxWarning::new(
            SandboxUnavailableReason::HelperUntrusted
        ))
    );
    assert!(matches!(
        refused,
        Err(TurnError::SandboxUnavailable(
            SandboxUnavailableReason::HelperUntrusted
        ))
    ));
}

#[cfg(feature = "native-sandbox")]
#[tokio::test]
async fn available_sandbox_launches_through_the_helper_it_checked_once() {
    // Arrange
    let fixture = GateFixture::new();
    let native = fixture.native(&fixture.helper);
    let gate = fixture.linux_gate();

    // Act
    let (first, first_warning) = gate
        .resolve(&native, &fixture.workspace)
        .await
        .expect("available sandbox");
    fixture.script("bwrap", 1);
    let (second, second_warning) = gate
        .resolve(&native, &fixture.workspace)
        .await
        .expect("checked once");

    // Assert
    assert_eq!(first.snapshot.launcher.as_ref(), Some(&fixture.helper));
    assert_eq!(
        first.snapshot.linux_bubblewrap,
        Some(fixture.tools.join("bwrap"))
    );
    assert_eq!(first.enforcement(), Enforcement::FileSystemAndNetwork);
    assert!(first.executor.is_none());
    assert_eq!(first_warning, None);
    assert_eq!(second, first);
    assert_eq!(second_warning, None);
}

#[cfg(feature = "native-sandbox")]
#[tokio::test]
async fn unavailable_sandbox_is_checked_again_until_a_remedy_applies() {
    // Arrange
    let fixture = GateFixture::new();
    let native = fixture.native(&fixture.helper);
    let gate = fixture.linux_gate();
    fixture.script("bwrap", 1);

    // Act
    let (first, first_warning) = gate
        .resolve(&native, &fixture.workspace)
        .await
        .expect("unsandboxed fallback");
    let (_, repeated_warning) = gate
        .resolve(&native, &fixture.workspace)
        .await
        .expect("unsandboxed fallback");
    fixture.script("bwrap", 0);
    let (remedied, remedied_warning) = gate
        .resolve(&native, &fixture.workspace)
        .await
        .expect("available sandbox");

    // Assert
    let warning = SandboxWarning::new(SandboxUnavailableReason::UserNamespacesUnavailable);
    assert_eq!(first.enforcement(), Enforcement::None);
    assert_eq!(first_warning, Some(warning.clone()));
    assert_eq!(repeated_warning, Some(warning));
    assert_eq!(remedied.enforcement(), Enforcement::FileSystemAndNetwork);
    assert_eq!(remedied_warning, None);
}

#[cfg(feature = "native-sandbox")]
#[tokio::test]
async fn socket_grants_outside_macos_warn_inside_the_sandbox_or_refuse() {
    // Arrange
    let fixture = GateFixture::new();
    fixture.script("sandbox-exec", 0);
    // Socket paths are limited to about 100 bytes, so keep them short.
    let sockets = tempfile::tempdir_in("/tmp").expect("socket directory");
    let socket = sockets.path().join("agent");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("socket");
    let native = fixture
        .native(&fixture.helper)
        .with_unix_socket(socket)
        .expect("socket grant");
    let refusing = SandboxGate {
        sandbox: Sandbox::Native {
            if_unavailable: IfUnavailable::Refuse,
        },
        ..fixture.linux_gate()
    };
    let macos = SandboxGate {
        host: Host {
            landlock: false,
            platform: "macos",
            seatbelt: fixture.tools.join("sandbox-exec"),
        },
        ..SandboxGate::default()
    };

    // Act
    let (warned, warning) = fixture
        .linux_gate()
        .resolve(&native, &fixture.workspace)
        .await
        .expect("sandboxed with a warning");
    let refused = refusing.resolve(&native, &fixture.workspace).await;
    let (_, macos_warning) = macos
        .resolve(&native, &fixture.workspace)
        .await
        .expect("socket grants allowed");

    // Assert
    // The commands still run inside the sandbox, without the sockets.
    assert_eq!(warned.enforcement(), Enforcement::FileSystemAndNetwork);
    assert!(warned.executor.is_none());
    assert_eq!(
        warning,
        Some(SandboxWarning::new(
            SandboxUnavailableReason::UnixSocketsUnsupported
        ))
    );
    assert!(matches!(
        refused,
        Err(TurnError::SandboxUnavailable(
            SandboxUnavailableReason::UnixSocketsUnsupported
        ))
    ));
    assert_eq!(macos_warning, None);
}

#[cfg(feature = "native-sandbox")]
#[tokio::test]
async fn dispatch_registers_this_executable_as_the_helper() {
    // Arrange
    let fixture = GateFixture::new();
    let native = BashConfig::native(
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("config")
    .with_host_information()
    .with_linux_bubblewrap(fixture.tools.join("bwrap"))
    .expect("bubblewrap");

    // Act
    let dispatched = dispatch();
    let (resolved, warning) = SandboxGate::default()
        .resolve(&native, &fixture.workspace)
        .await
        .expect("dispatched helper");

    // Assert
    let executable = std::env::current_exe().expect("test executable");
    assert_eq!(dispatched, None);
    assert_eq!(DISPATCHED_HELPER.get(), Some(&executable));
    assert_eq!(resolved.snapshot.launcher, Some(executable));
    assert_eq!(warning, None);
}

/// A workspace beside a helper and a Bubblewrap stand-in that succeeds.
#[cfg(feature = "native-sandbox")]
struct GateFixture {
    _directory: tempfile::TempDir,
    helper: PathBuf,
    tools: PathBuf,
    workspace: PathBuf,
}

#[cfg(feature = "native-sandbox")]
impl GateFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("directory");
        let root = directory.path().canonicalize().expect("root");
        let tools = root.join("tools");
        let workspace = root.join("workspace");
        std::fs::create_dir(&tools).expect("tools");
        std::fs::create_dir(&workspace).expect("workspace");
        let fixture = Self {
            _directory: directory,
            helper: tools.join("helper"),
            tools,
            workspace,
        };
        fixture.script("helper", 0);
        fixture.script("bwrap", 0);

        fixture
    }

    /// Returns a default gate for a Linux host with Landlock, so the
    /// Bubblewrap stand-in decides availability on every platform.
    fn linux_gate(&self) -> SandboxGate {
        SandboxGate {
            host: Host {
                landlock: true,
                platform: "linux",
                seatbelt: self.tools.join("sandbox-exec"),
            },
            ..SandboxGate::default()
        }
    }

    /// Writes an executable script exiting with `code` among the tools.
    fn script(&self, name: &str, code: u8) {
        let path = self.tools.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nexit {code}\n")).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("executable script");
    }

    /// Returns a native configuration launched by `helper` through the
    /// Bubblewrap stand-in.
    fn native(&self, helper: &Path) -> BashConfig {
        BashConfig::new(
            helper.to_path_buf(),
            "/bin/bash".into(),
            "revision".into(),
            Duration::from_secs(1),
            1024,
        )
        .expect("config")
        .with_host_information()
        .with_linux_bubblewrap(self.tools.join("bwrap"))
        .expect("bubblewrap")
    }
}
