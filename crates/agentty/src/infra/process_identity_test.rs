use super::ProcessIdentity;

#[test]
fn linux_creation_ticks_preserve_subsecond_identity_and_complex_names() {
    // Arrange
    let prefix = "42 (worker (tool) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18";

    // Act
    let first = ProcessIdentity::from_linux_stat(&format!("{prefix} 1001 1234"));
    let reused = ProcessIdentity::from_linux_stat(&format!("{prefix} 1002 1234"));

    // Assert
    assert_eq!(first, Some(ProcessIdentity(1001)));
    assert_eq!(reused, Some(ProcessIdentity(1002)));
    for malformed in ["", "42 (worker)", &format!("{prefix} invalid")] {
        assert!(ProcessIdentity::from_linux_stat(malformed).is_none());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn native_identity_is_stable_for_owned_process_and_missing_for_invalid_pid() {
    // Arrange
    let pid = std::process::id();

    // Act
    let first = ProcessIdentity::read(pid).expect("current process identity");
    let second = ProcessIdentity::read(pid);

    // Assert
    assert_eq!(second, Some(first));
    assert!(first.0 > 0);
    assert!(ProcessIdentity::read(u32::MAX).is_none());
    assert!(ProcessIdentity::read(i32::MAX as u32).is_none());
}
