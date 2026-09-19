use super::filter;

fn evaluate(program: &[u8], syscall: u32, arch: u32, flags: u32) -> u32 {
    let mut accumulator = 0;
    let mut position = 0;
    loop {
        let instruction = &program[position * 8..][..8];
        let code = u16::from_ne_bytes(instruction[..2].try_into().expect("opcode"));
        let value = u32::from_ne_bytes(instruction[4..].try_into().expect("operand"));
        let jump = match code {
            0x20 => {
                accumulator = match value {
                    0 => syscall,
                    4 => arch,
                    16 => flags,
                    _ => unreachable!("unexpected seccomp load"),
                };
                None
            }
            0x15 => Some(accumulator == value),
            0x45 => Some(accumulator & value != 0),
            0x06 => return value,
            _ => unreachable!("unexpected BPF instruction"),
        };
        position += 1 + jump.map_or(0, |yes| usize::from(instruction[if yes { 2 } else { 3 }]));
    }
}

#[test]
fn kernel_filter_rejects_alternate_abis_network_keyrings_and_nested_namespaces() {
    // Arrange
    for (architecture, arch, clone, socket, keyctl, uname) in [
        ("x86_64", 0xc000_003e, 56, 41, 250, 63),
        ("aarch64", 0xc000_00b7, 220, 198, 219, 160),
    ] {
        let program = filter(true, architecture).expect("filter");

        // Act / Assert
        assert_eq!(evaluate(&program, 0, 0, 0), 0x8000_0000);
        assert_eq!(evaluate(&program, 0x4000_0000, arch, 0), 0x8000_0000);
        for syscall in [socket, keyctl, 428, 429, 430, 431, 432, 433, 442] {
            assert_eq!(evaluate(&program, syscall, arch, 0), 0x0005_0001);
        }
        assert_eq!(evaluate(&program, 435, arch, 0), 0x0005_0026);
        for flags in [
            0x0002_0000,
            0x0200_0000,
            0x0400_0000,
            0x0800_0000,
            0x1000_0000,
            0x2000_0000,
            0x4000_0000,
        ] {
            assert_eq!(evaluate(&program, clone, arch, flags), 0x0005_0001);
        }
        assert_eq!(evaluate(&program, clone, arch, 17), 0x7fff_0000);
        assert_eq!(evaluate(&program, uname, arch, 0), 0x7fff_0000);
        let restricted = filter(false, architecture).expect("restricted filter");
        assert_eq!(evaluate(&restricted, uname, arch, 0), 0x0005_0001);
    }
}

#[test]
fn unsupported_architecture_cannot_produce_a_filter() {
    // Arrange / Act / Assert
    assert!(filter(true, "unknown").is_err());
}
