//! Artifact naming unit tests.

use crate::artifact;
use crate::target::Target;

#[test]
fn artifact_names_follow_the_published_contract() {
    // Arrange
    let app = "demo-app";
    let target = Target::X8664UnknownLinuxGnu;

    // Act
    let names = [
        artifact::archive_directory(app, target),
        artifact::archive(app, target),
        artifact::checksum(&artifact::archive(app, target)),
        artifact::checksum(artifact::SOURCE_ARCHIVE),
        artifact::source_directory("demo-repo", "1.2.3"),
        artifact::shell_installer(app),
        artifact::homebrew_formula(app),
        artifact::npm_tarball(app),
        artifact::npm_package(app, Some("@demo-org")),
        artifact::npm_package(app, None),
        artifact::CHECKSUM_LIST.to_string(),
        artifact::MANIFEST.to_string(),
    ];

    // Assert
    assert_eq!(
        names,
        [
            "demo-app-x86_64-unknown-linux-gnu",
            "demo-app-x86_64-unknown-linux-gnu.tar.gz",
            "demo-app-x86_64-unknown-linux-gnu.tar.gz.sha256",
            "source.tar.gz.sha256",
            "demo-repo-1.2.3",
            "demo-app-installer.sh",
            "demo-app.rb",
            "demo-app-npm-package.tar.gz",
            "@demo-org/demo-app",
            "demo-app",
            "sha256.sum",
            "release-manifest.json",
        ]
    );
}

#[test]
fn binstall_templates_expand_to_the_archive_and_its_binaries() {
    // Arrange
    let app = "demo-app";
    let target = Target::Aarch64UnknownLinuxMusl;
    let expand = |template: &str, version: &str| {
        template
            .replace("{ repo }", "https://github.com/demo-org/demo-app")
            .replace("{ version }", version)
            .replace("{ name }", app)
            .replace("{ target }", target.triple())
            .replace("{ bin }", "demo-helper")
            .replace("{ binary-ext }", "")
    };

    // Act
    let urls = [
        expand(artifact::BINSTALL_PKG_URL, "1.2.3"),
        expand(artifact::BINSTALL_UNPREFIXED_PKG_URL, "1.2.3"),
    ];
    let binary = expand(artifact::BINSTALL_BIN_DIR, "1.2.3");

    // Assert
    let download = "https://github.com/demo-org/demo-app/releases/download";
    let archive = artifact::archive(app, target);
    assert_eq!(
        urls,
        [
            format!("{download}/v1.2.3/{archive}"),
            format!("{download}/1.2.3/{archive}"),
        ]
    );
    assert_eq!(
        binary,
        format!("{}/demo-helper", artifact::archive_directory(app, target))
    );
    assert_eq!(artifact::BINSTALL_PKG_FMT, "tgz");
}
