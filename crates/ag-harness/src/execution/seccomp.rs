//! Deny native and alternate-ABI access to networking and host kernel services.

use std::io;

pub(super) fn filter(host_information: bool, architecture: &str) -> io::Result<Vec<u8>> {
    let (architecture, denied, uname, clone) = match architecture {
        "x86_64" => (
            0xc000_003e_u32,
            &[
                41, 53, 101, 155, 165, 166, 169, 175, 176, 246, 248, 249, 250, 272, 298, 303, 304,
                308, 310, 311, 321, 323, 425, 426, 427, 428, 429, 430, 431, 432, 433, 442,
            ][..],
            63,
            56,
        ),
        "aarch64" => (
            0xc000_00b7_u32,
            &[
                40, 41, 97, 104, 105, 106, 117, 142, 198, 199, 217, 218, 219, 241, 264, 265, 268,
                270, 271, 280, 282, 425, 426, 427, 428, 429, 430, 431, 432, 433, 442,
            ][..],
            160,
            220,
        ),
        _ => return Err(io::Error::other("unsupported seccomp architecture")),
    };
    let mut program = Vec::new();
    // BPF LD ABS arch; JEQ native; RET KILL_PROCESS; LD ABS syscall.
    instruction(&mut program, 0x20, 0, 0, 4);
    instruction(&mut program, 0x15, 1, 0, architecture);
    instruction(&mut program, 0x06, 0, 0, 0x8000_0000);
    instruction(&mut program, 0x20, 0, 0, 0);
    // Reject the x32 syscall bit even when AUDIT_ARCH_X86_64 matches.
    instruction(&mut program, 0x45, 0, 1, 0x4000_0000);
    instruction(&mut program, 0x06, 0, 0, 0x8000_0000);
    for number in denied
        .iter()
        .copied()
        .chain((!host_information).then_some(uname))
    {
        instruction(&mut program, 0x15, 0, 1, number);
        instruction(&mut program, 0x06, 0, 0, 0x0005_0001);
    }
    // clone3 lacks inspectable flags; ENOSYS lets runtimes use ordinary clone.
    instruction(&mut program, 0x15, 0, 1, 435);
    instruction(&mut program, 0x06, 0, 0, 0x0005_0026);
    // Ordinary forks/threads remain available. A nested user namespace must
    // not regain capabilities or change the mount isolation.
    instruction(&mut program, 0x15, 0, 3, clone);
    instruction(&mut program, 0x20, 0, 0, 16);
    instruction(&mut program, 0x45, 0, 1, 0x7e02_0000);
    instruction(&mut program, 0x06, 0, 0, 0x0005_0001);
    instruction(&mut program, 0x06, 0, 0, 0x7fff_0000);

    Ok(program)
}

#[cfg(test)]
#[path = "seccomp_test.rs"]
mod tests;

fn instruction(program: &mut Vec<u8>, code: u16, yes: u8, no: u8, value: u32) {
    program.extend_from_slice(&code.to_ne_bytes());
    program.extend_from_slice(&[yes, no]);
    program.extend_from_slice(&value.to_ne_bytes());
}
