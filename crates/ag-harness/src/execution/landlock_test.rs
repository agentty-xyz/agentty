use super::{
    ACCESS_REFER, ACCESS_TRUNCATE, DEVICE_ACCESS, GRANT_ACCESS, enforceable, ruleset_access,
    validate_write_grants, verify_grant_identity,
};
use crate::execution::contract::ExecutionError;

#[test]
fn write_grants_require_the_truncate_capable_abi() {
    // Arrange / Act / Assert
    assert_eq!(validate_write_grants(0, -1), Ok(()));
    assert_eq!(validate_write_grants(0, 3), Ok(()));
    assert_eq!(
        validate_write_grants(1, 2),
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(
        validate_write_grants(1, -1),
        Err(ExecutionError::Unsupported)
    );
    assert_eq!(validate_write_grants(1, 3), Ok(()));
    assert!(!enforceable(2));
    assert!(enforceable(4));
}

#[test]
fn handled_rights_cover_link_rename_and_truncate_bypasses() {
    // Arrange / Act
    let access = ruleset_access(3).expect("supported ABI");
    let unavailable = ruleset_access(2);

    // Assert
    assert_eq!(access, GRANT_ACCESS);
    assert_eq!(access & ACCESS_REFER, ACCESS_REFER);
    assert_eq!(access & ACCESS_TRUNCATE, ACCESS_TRUNCATE);
    assert_eq!(DEVICE_ACCESS & GRANT_ACCESS, DEVICE_ACCESS);
    assert!(unavailable.is_err());
}

#[test]
fn grant_identity_requires_the_validated_device_and_inode() {
    // Arrange / Act / Assert
    assert!(verify_grant_identity(Some((7, 9)), (7, 9)).is_ok());
    assert!(verify_grant_identity(Some((7, 9)), (7, 8)).is_err());
    assert!(verify_grant_identity(None, (7, 9)).is_err());
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;

    use rustix::fs::{Mode, OFlags};

    use super::super::{GRANT_ACCESS, abi, add_rule, confine, enforceable, restrict, ruleset};
    use crate::execution::wire::Launch;

    fn launch(workspace: &std::path::Path) -> Launch {
        Launch {
            arguments: vec![],
            directory: workspace.to_path_buf(),
            environment: BTreeMap::new(),
            executable: "/bin/bash".into(),
            external_reads: vec![],
            git_metadata: vec![workspace.join(".git")],
            host_information: true,
            launcher: "/trusted/launcher".into(),
            linux_bubblewrap: None,
            workspace: workspace.to_path_buf(),
            workspace_write_nodes: vec![],
            workspace_writes: vec![],
        }
    }

    #[test]
    fn kernel_rejections_surface_as_errors_without_restricting_the_caller() {
        // Arrange
        let device = rustix::fs::open("/dev/null", OFlags::PATH | OFlags::CLOEXEC, Mode::empty())
            .expect("device descriptor");

        // Act / Assert
        assert!(
            ruleset(0).is_err(),
            "an empty handled-access set is rejected"
        );
        if !enforceable(abi()) {
            assert!(ruleset(GRANT_ACCESS).is_err(), "unavailable Landlock");

            return;
        }
        let handled = ruleset(GRANT_ACCESS).expect("ruleset");
        assert!(
            add_rule(&handled, &device, GRANT_ACCESS).is_err(),
            "directory rights cannot attach to a device file"
        );
        assert!(
            restrict(&device).is_err(),
            "a non-ruleset descriptor cannot restrict the thread"
        );
    }

    #[test]
    fn confinement_without_grants_probes_nothing_and_failures_precede_restriction() {
        // Arrange
        let directory = tempfile::tempdir().expect("workspace");
        let workspace = directory.path().canonicalize().expect("canonical");
        std::fs::create_dir(workspace.join("output")).expect("grant");
        let mut configuration = launch(&workspace);

        // Act / Assert
        assert_ne!(abi(), 0);
        confine(&configuration).expect("no grants apply no ruleset");
        configuration.workspace_writes.push("missing".into());
        configuration.workspace_write_nodes.push((1, 1));
        assert!(confine(&configuration).is_err(), "missing grant directory");
        configuration.workspace_writes = vec!["output".into()];
        configuration.workspace_write_nodes = vec![(u64::MAX, u64::MAX)];
        assert!(confine(&configuration).is_err(), "swapped grant identity");
        configuration.workspace_write_nodes = vec![];
        assert!(confine(&configuration).is_err(), "absent grant identity");
    }
}
