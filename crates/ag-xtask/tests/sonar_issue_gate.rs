//! Process-level contracts for the pull-request `SonarQube` issue gate.

#![cfg(unix)]

use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::{fs, io};

use serde_yaml_ng::Value;
use tempfile::TempDir;

/// Records each request's arguments and answers with the saved response for
/// the requested endpoint. Without a saved response it fails like
/// `curl --fail-with-body` does on an HTTP error.
const CURL_STUB: &str = r#"#!/bin/sh
printf '%s\n' "$@" >> curl-arguments
for argument; do url=$argument; done
case "$url" in
*/api/measures/component) response=measures.json;;
*/api/issues/search) response=issues.json;;
*) exit 99;;
esac
if [ ! -f "$response" ]; then
  echo 'curl: (22) The requested URL returned error: 404' >&2
  exit 22
fi
cat "$response"
"#;

/// One issue on a line and one file-level issue whose message needs escaping.
const ISSUES: &str = r#"{"total":2,"issues":[
  {"rule":"javascript:S8786","component":"fixture_project:docs/site/scripts/social-card.mjs",
   "line":54,"message":"Simplify this regular expression."},
  {"rule":"css:S4666","component":"fixture_project:docs/site/sass/site.scss",
   "message":"Keep 100% of selectors unique."}
]}"#;

const MEASURES_ENDPOINT: &str = "https://sonarcloud.io/api/measures/component";
const ISSUES_ENDPOINT: &str = "https://sonarcloud.io/api/issues/search";

struct Fixture {
    directory: TempDir,
    script: String,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let action: Value = serde_yaml_ng::from_str(include_str!(
            "../../../.github/actions/run-coverage/action.yml"
        ))?;
        let script = action["runs"]["steps"]
            .as_sequence()
            .ok_or("steps")?
            .iter()
            .find(|step| step["id"].as_str() == Some("sonar-issues"))
            .ok_or("issue gate step")?["run"]
            .as_str()
            .ok_or("run command")?
            .to_string();
        let directory = tempfile::tempdir()?;
        let curl = directory.path().join("bin/curl");
        fs::create_dir(directory.path().join("bin"))?;
        fs::write(&curl, CURL_STUB)?;
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o755))?;
        fs::write(
            directory.path().join("sonar-project.properties"),
            "sonar.projectKey=fixture_project\nsonar.organization=fixture\n",
        )?;

        Ok(Self { directory, script })
    }

    /// Saves a measures response whose analysis counts `count` new issues.
    fn save_issue_count(&self, count: &str) -> io::Result<()> {
        self.save(
            "measures.json",
            &format!(
                r#"{{"component":{{"key":"fixture_project","measures":[{{"metric":"new_violations","periods":[{{"index":1,"value":"{count}"}}]}}],"pullRequest":"7"}}}}"#
            ),
        )
    }

    fn save(&self, name: &str, contents: &str) -> io::Result<()> {
        fs::write(self.path(name), contents)
    }

    /// Runs the step the way GitHub Actions runs a `shell: bash` step.
    fn run(&self) -> io::Result<Output> {
        Command::new("bash")
            .args(["--noprofile", "--norc", "-e", "-o", "pipefail", "-c"])
            .arg(&self.script)
            .current_dir(self.directory.path())
            .env_clear()
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("PULL_REQUEST", "7")
            .env("SONAR_TOKEN", "fixture-token")
            .output()
    }

    fn requests(&self) -> io::Result<Vec<String>> {
        Ok(fs::read_to_string(self.path("curl-arguments"))?
            .lines()
            .map(str::to_string)
            .collect())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
}

#[test]
fn gate_passes_when_the_analysis_counts_no_issue() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.save_issue_count("0")?;

    // Act
    let output = fixture.run()?;

    // Assert
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "SonarQube reports no open issues in pull request #7.\n"
    );
    let requests = fixture.requests()?;
    for argument in [
        "Authorization: Bearer fixture-token",
        "pullRequest=7",
        "component=fixture_project",
        "metricKeys=new_violations",
        MEASURES_ENDPOINT,
    ] {
        assert!(
            requests.iter().any(|request| request == argument),
            "{argument}"
        );
    }
    assert!(!requests.iter().any(|request| request == ISSUES_ENDPOINT));

    Ok(())
}

#[test]
fn gate_fails_and_annotates_each_issue_the_analysis_counts() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;
    fixture.save_issue_count("2")?;
    fixture.save("issues.json", ISSUES)?;

    // Act
    let output = fixture.run()?;

    // Assert
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stdout)?.lines().collect::<Vec<_>>(),
        [
            "::error file=docs/site/scripts/social-card.mjs,line=54,title=SonarQube \
             issue::Simplify this regular expression. (javascript:S8786)",
            "::error file=docs/site/sass/site.scss,line=1,title=SonarQube issue::Keep 100%25 of \
             selectors unique. (css:S4666)",
            "::error::SonarQube reports 2 open issue(s) in this pull request. Fix each one: \
             https://sonarcloud.io/project/issues?id=fixture_project&pullRequest=7&\
             issueStatuses=OPEN,CONFIRMED",
        ]
    );
    let requests = fixture.requests()?;
    for argument in [
        "componentKeys=fixture_project",
        "issueStatuses=OPEN,CONFIRMED",
        ISSUES_ENDPOINT,
    ] {
        assert!(
            requests.iter().any(|request| request == argument),
            "{argument}"
        );
    }

    Ok(())
}

#[test]
fn gate_fails_without_a_usable_issue_count() -> Result<(), Box<dyn Error>> {
    // Arrange
    let no_count = "::error::SonarQube returned no new-issue count for pull request #7.\n";
    let cases = [
        ("missing measure", r#"{"component":{"measures":[]}}"#),
        (
            "error response",
            r#"{"errors":[{"msg":"Component not found"}]}"#,
        ),
        (
            "count that is not a number",
            r#"{"component":{"measures":[{"metric":"new_violations","periods":[{"value":"none"}]}]}}"#,
        ),
    ];

    for (case, measures) in cases {
        let fixture = Fixture::new()?;
        fixture.save("measures.json", measures)?;

        // Act
        let output = fixture.run()?;

        // Assert
        assert_eq!(output.status.code(), Some(1), "{case}");
        assert_eq!(String::from_utf8(output.stdout)?, no_count, "{case}");
    }

    Ok(())
}

#[test]
fn gate_fails_when_the_count_request_fails() -> Result<(), Box<dyn Error>> {
    // Arrange
    let fixture = Fixture::new()?;

    // Act
    let output = fixture.run()?;

    // Assert
    assert_eq!(output.status.code(), Some(22));
    assert_eq!(String::from_utf8(output.stdout)?, "");
    assert_eq!(
        String::from_utf8(output.stderr)?,
        "curl: (22) The requested URL returned error: 404\n"
    );

    Ok(())
}
