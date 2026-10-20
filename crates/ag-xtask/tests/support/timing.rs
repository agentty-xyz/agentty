//! Shared evidence fixture for pure comparison and CLI contracts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;
use sha2::{Digest, Sha256};

pub(super) const JUNIT: &str = r#"<testsuites tests="2" failures="0" errors="0" time="2.5"><testsuite><testcase time="1"/><testcase time="2"/></testsuite></testsuites>"#;
pub(super) const CARGO: &str = "<td>Total time:</td><td>4.0s</td><td>Fresh \
                                units:</td><td>12</td><td>Dirty units:</td><td>0</td>";

pub(super) fn files() -> BTreeMap<PathBuf, String> {
    let mut files = BTreeMap::new();
    for (arm, duration) in [("before", 12), ("after", 6)] {
        for index in 0..3 {
            let directory = PathBuf::from(format!("{arm}/{index}"));
            let metadata = json!({
                "schema": 1, "prepared": true, "outcome": "success",
                "identity": {"run_id": format!("{arm}-{index}"), "attempt": "1", "job": "tests", "suite": "workspace", "sha": arm},
                "environment": {"runner_os": "Linux", "runner_arch": "X64", "rustc": "rustc pinned", "nextest": "nextest pinned", "exact_cache_hit": false,
                    "cpu_models": ["fixture CPU"], "available_parallelism": 4},
                "hook_seconds": duration + index,
                "reports": {
                    "junit.xml": hash(JUNIT),
                    "cargo-timing.html": hash(CARGO),
                },
            });
            files.insert(directory.join("metadata.json"), metadata.to_string());
            files.insert(directory.join("junit.xml"), JUNIT.to_string());
            files.insert(directory.join("cargo-timing.html"), CARGO.to_string());
        }
    }

    files
}

fn hash(input: &str) -> String {
    Sha256::digest(input)
        .into_iter()
        .flat_map(|byte| {
            let digits = b"0123456789abcdef";
            [
                char::from(digits[usize::from(byte >> 4)]),
                char::from(digits[usize::from(byte & 15)]),
            ]
        })
        .collect()
}
