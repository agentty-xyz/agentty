//! Target triple unit tests.

use crate::target::Target;

#[test]
fn triples_round_trip_display_and_serialize() {
    // Arrange
    let targets = Target::ALL;

    for target in targets {
        // Act
        let parsed = Target::from_triple(target.triple());
        let displayed = target.to_string();
        let serialized = serde_json::to_value(target).expect("serialize target");

        // Assert
        assert_eq!(parsed, Some(target));
        assert_eq!(displayed, target.triple());
        assert_eq!(serialized, serde_json::Value::from(target.triple()));
    }
}

#[test]
fn from_triple_rejects_unsupported_targets() {
    // Arrange
    let triples = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-musl", ""];

    for triple in triples {
        // Act
        let parsed = Target::from_triple(triple);

        // Assert
        assert_eq!(parsed, None, "{triple}");
    }
}

#[test]
fn uname_names_and_glibc_follow_the_triple() {
    // Arrange
    let expected = [
        (
            Target::Aarch64AppleDarwin,
            "Darwin",
            ["arm64", "aarch64"],
            false,
        ),
        (
            Target::Aarch64UnknownLinuxGnu,
            "Linux",
            ["arm64", "aarch64"],
            true,
        ),
        (
            Target::X8664AppleDarwin,
            "Darwin",
            ["x86_64", "amd64"],
            false,
        ),
        (
            Target::X8664UnknownLinuxGnu,
            "Linux",
            ["x86_64", "amd64"],
            true,
        ),
    ];

    for (target, system, machines, glibc) in expected {
        // Act
        let names = (
            target.uname_system(),
            target.uname_machines(),
            target.is_glibc(),
        );

        // Assert
        assert_eq!(names, (system, &machines[..], glibc), "{target}");
    }
}
