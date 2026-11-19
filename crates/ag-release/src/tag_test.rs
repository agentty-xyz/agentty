//! Release tag parsing unit tests.

use semver::Version;

use crate::tag::ReleaseTag;

#[test]
fn parse_accepts_prefixed_and_bare_semver_tags() {
    // Arrange
    let cases = [
        ("v1.2.3", "1.2.3"),
        ("1.2.3", "1.2.3"),
        ("v2.0.0-rc.1", "2.0.0-rc.1"),
        ("v1.0.0+build.5", "1.0.0+build.5"),
    ];

    for (tag, version) in cases {
        // Act
        let parsed = ReleaseTag::parse(tag);

        // Assert
        assert_eq!(
            parsed,
            Ok(ReleaseTag {
                name: tag.to_string(),
                version: Version::parse(version).expect("version"),
            }),
            "{tag}"
        );
    }
}

#[test]
fn parse_rejects_tags_that_are_not_semver() {
    // Arrange
    let cases = [
        "",
        "v",
        "V1.2.3",
        "release-1.2.3",
        "v1.2",
        "vv1.2.3",
        "v01.2.3",
    ];

    for tag in cases {
        // Act
        let parsed = ReleaseTag::parse(tag);

        // Assert
        let error = parsed.expect_err(tag);
        assert!(
            error.starts_with(&format!(
                "release tag `{tag}` must be `v<semver>` or `<semver>`: "
            )),
            "{error}"
        );
    }
}
