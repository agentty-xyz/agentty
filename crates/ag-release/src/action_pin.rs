//! Version pins of the actions that the generated release workflow uses.
//!
//! Dependabot updates `uses: <action>@<ref> # <version>` lines in the
//! committed workflow, so generation reuses the pin it finds there for each
//! action and falls back to a default pin only for an action the file does
//! not use yet.

/// Default pin of each action: its name, commit, and version comment.
const DEFAULT_PINS: [(&str, &str, &str); 5] = [
    (
        "actions/attest-build-provenance",
        "4d101475d8b20a2381f78447822ac1eab6504dd8",
        "v4.2.2",
    ),
    (
        "actions/checkout",
        "3d3c42e5aac5ba805825da76410c181273ba90b1",
        "v7.0.1",
    ),
    (
        "actions/download-artifact",
        "9000827ccba6bdab643e8b6fd33ac0654aef8333",
        "v8.0.2",
    ),
    (
        "actions/setup-node",
        "949feb2413d6458794dcd2491c4babbbce0c15c1",
        "v7.1.0",
    ),
    (
        "actions/upload-artifact",
        "cf430e030ddbb5b0abf93d22962f4752f3646cd9",
        "v7.0.2",
    ),
];

/// The `uses:` value, with its trailing comment, of every action the
/// workflow uses.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ActionPins {
    pub(crate) attest_build_provenance: String,
    pub(crate) checkout: String,
    pub(crate) download_artifact: String,
    pub(crate) setup_node: String,
    pub(crate) upload_artifact: String,
}

impl ActionPins {
    /// Returns the pins of the `existing` workflow, or the default pin of
    /// each action that it does not use.
    pub(crate) fn resolve(existing: Option<&str>) -> Self {
        let [
            attest_build_provenance,
            checkout,
            download_artifact,
            setup_node,
            upload_artifact,
        ] = DEFAULT_PINS.map(|(action, reference, version)| {
            existing
                .and_then(|workflow| Self::find(workflow, action))
                .unwrap_or_else(|| format!("{action}@{reference} # {version}"))
        });

        Self {
            attest_build_provenance,
            checkout,
            download_artifact,
            setup_node,
            upload_artifact,
        }
    }

    /// Returns the first `uses: <action>@<ref>` value in `workflow`, with
    /// its trailing comment when there is one.
    fn find(workflow: &str, action: &str) -> Option<String> {
        workflow.lines().find_map(|line| {
            let line = line.trim_start();
            let value = line.strip_prefix("- ").unwrap_or(line).trim_start();
            let value = value.strip_prefix("uses:")?.trim();
            let (reference, comment) = match value.split_once(char::is_whitespace) {
                Some((reference, rest)) => (reference, Some(rest.trim_start())),
                None => (value, None),
            };
            let reference = reference
                .strip_prefix(action)?
                .strip_prefix('@')
                .filter(|reference| Self::is_reference(reference))?;

            let comment = match comment {
                None => "",
                Some(comment) => comment.strip_prefix('#')?.trim(),
            };
            if comment.is_empty() {
                return Some(format!("{action}@{reference}"));
            }

            Some(format!("{action}@{reference} # {comment}"))
        })
    }

    /// Returns whether `reference` is a Git ref that needs no YAML quoting.
    fn is_reference(reference: &str) -> bool {
        !reference.is_empty()
            && reference
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._/-".contains(character))
    }
}

#[cfg(test)]
#[path = "action_pin_test.rs"]
mod tests;
