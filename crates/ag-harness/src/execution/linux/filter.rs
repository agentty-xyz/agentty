//! Native-ABI-only seccomp allowlist. Unknown calls (including compatibility
//! ABIs, keyrings, sockets, host inspection and mount APIs) fail closed.

use std::io;

pub(super) fn program() -> io::Result<Vec<u8>> {
    #[cfg(target_arch = "x86_64")]
    let architecture = 0xc000_003e;
    #[cfg(target_arch = "aarch64")]
    let architecture = 0xc000_00b7;

    let mut program = Vec::new();
    instruction(&mut program, 0x20, 0, 0, 4); // seccomp_data.arch
    instruction(&mut program, 0x15, 1, 0, architecture);
    instruction(&mut program, 0x06, 0, 0, 0x8000_0000); // KILL_PROCESS
    instruction(&mut program, 0x20, 0, 0, 0); // seccomp_data.nr
    instruction(
        &mut program,
        0x15,
        0,
        4,
        u32::try_from(libc::SYS_clone).map_err(io::Error::other)?,
    );
    instruction(&mut program, 0x20, 0, 0, 16); // clone flags, args[0]
    // CLONE_NEW{NS,CGROUP,UTS,IPC,USER,PID,NET} and CLONE_PARENT.
    instruction(&mut program, 0x45, 0, 1, 0x7e02_8000);
    instruction(&mut program, 0x06, 0, 0, 0x0005_0001);
    instruction(&mut program, 0x20, 0, 0, 0);
    for syscall in allowed_calls() {
        let syscall = u32::try_from(syscall).map_err(io::Error::other)?;
        instruction(&mut program, 0x15, 0, 1, syscall);
        instruction(&mut program, 0x06, 0, 0, 0x7fff_0000); // ALLOW
    }
    instruction(&mut program, 0x06, 0, 0, 0x0005_0001); // ERRNO(EPERM)

    Ok(program)
}

fn instruction(bytes: &mut Vec<u8>, code: u16, yes: u8, no: u8, value: u32) {
    bytes.extend(code.to_ne_bytes());
    bytes.extend([yes, no]);
    bytes.extend(value.to_ne_bytes());
}

fn allowed_calls() -> Vec<libc::c_long> {
    let calls = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_openat,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx,
        libc::SYS_lseek,
        libc::SYS_getdents64,
        libc::SYS_ftruncate,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_readlinkat,
        libc::SYS_mkdirat,
        libc::SYS_unlinkat,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_linkat,
        libc::SYS_symlinkat,
        libc::SYS_fchdir,
        libc::SYS_chdir,
        libc::SYS_getcwd,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_futex,
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_getppid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_clone,
        libc::SYS_wait4,
        libc::SYS_waitid,
        libc::SYS_execve,
        libc::SYS_execveat,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    #[cfg(target_arch = "x86_64")]
    let calls = {
        let mut calls = calls;
        calls.extend([
            libc::SYS_arch_prctl,
            libc::SYS_open,
            libc::SYS_stat,
            libc::SYS_lstat,
            libc::SYS_readlink,
            libc::SYS_access,
            libc::SYS_fork,
            libc::SYS_vfork,
        ]);

        calls
    };

    calls
}

#[cfg(test)]
#[path = "filter_test.rs"]
mod tests;
