//! Target triple unit tests.

use crate::target::{Libc, Target};

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
fn all_targets_follow_triple_order() {
    // Arrange
    let targets = Target::ALL;

    // Act
    let mut sorted = targets;
    sorted.sort();

    // Assert
    assert_eq!(sorted, targets);
    assert!(targets.is_sorted_by_key(|target| target.triple()));
}

#[test]
fn from_triple_rejects_unsupported_targets() {
    // Arrange
    let triples = ["x86_64-pc-windows-msvc", "i686-unknown-linux-musl", ""];

    for triple in triples {
        // Act
        let parsed = Target::from_triple(triple);

        // Assert
        assert_eq!(parsed, None, "{triple}");
    }
}

#[test]
fn uname_names_and_libc_follow_the_triple() {
    // Arrange
    let arm = ["arm64", "aarch64"];
    let intel = ["x86_64", "amd64"];
    let expected = [
        (Target::Aarch64AppleDarwin, "Darwin", arm, Libc::None),
        (Target::Aarch64UnknownLinuxGnu, "Linux", arm, Libc::Glibc),
        (Target::Aarch64UnknownLinuxMusl, "Linux", arm, Libc::Musl),
        (Target::X8664AppleDarwin, "Darwin", intel, Libc::None),
        (Target::X8664UnknownLinuxGnu, "Linux", intel, Libc::Glibc),
        (Target::X8664UnknownLinuxMusl, "Linux", intel, Libc::Musl),
    ];

    for (target, system, machines, libc) in expected {
        // Act
        let names = (
            target.uname_system(),
            target.uname_machines(),
            target.libc(),
        );

        // Assert
        assert_eq!(names, (system, &machines[..], libc), "{target}");
    }
}

#[test]
fn libc_names_are_the_installers_match_words() {
    // Arrange
    let libcs = [Libc::Glibc, Libc::Musl, Libc::None];

    // Act
    let names = libcs.map(Libc::name);

    // Assert
    assert_eq!(names, ["gnu", "musl", "none"]);
}

#[test]
fn node_names_follow_the_triple() {
    // Arrange
    let expected = [
        (Target::Aarch64AppleDarwin, "darwin", "arm64"),
        (Target::Aarch64UnknownLinuxGnu, "linux", "arm64"),
        (Target::Aarch64UnknownLinuxMusl, "linux", "arm64"),
        (Target::X8664AppleDarwin, "darwin", "x64"),
        (Target::X8664UnknownLinuxGnu, "linux", "x64"),
        (Target::X8664UnknownLinuxMusl, "linux", "x64"),
    ];

    for (target, platform, arch) in expected {
        // Act
        let names = (target.node_platform(), target.node_arch());

        // Assert
        assert_eq!(names, (platform, arch), "{target}");
    }
}

#[test]
fn display_names_name_the_platform() {
    // Arrange
    let targets = Target::ALL;

    // Act
    let names = targets.map(Target::display_name);

    // Assert
    assert_eq!(
        names,
        [
            "Apple Silicon macOS",
            "ARM64 Linux",
            "ARM64 Linux (static musl)",
            "Intel macOS",
            "x64 Linux",
            "x64 Linux (static musl)",
        ]
    );
}
