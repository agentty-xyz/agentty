use super::{allowed_calls, program};

#[test]
fn filter_has_native_architecture_guard_and_denies_unlisted_calls() {
    // Arrange
    let calls = allowed_calls();

    // Act
    let bytes = program().expect("native filter");

    // Assert
    assert_eq!(bytes.len(), (10 + 2 * calls.len()) * 8);
    assert_eq!(&bytes[..4], &[0x20, 0, 0, 0]);
    assert_eq!(&bytes[16..24], &[6, 0, 0, 0, 0, 0, 0, 0x80]);
    assert_eq!(&bytes[bytes.len() - 8..], &[6, 0, 0, 0, 1, 0, 5, 0]);
    for forbidden in [
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_socket,
        libc::SYS_uname,
        libc::SYS_sysinfo,
        libc::SYS_ptrace,
        libc::SYS_mount,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_bpf,
        libc::SYS_io_uring_setup,
        libc::SYS_open_by_handle_at,
        libc::SYS_perf_event_open,
        libc::SYS_ioctl,
    ] {
        assert!(!calls.contains(&forbidden), "forbidden syscall {forbidden}");
    }
}
