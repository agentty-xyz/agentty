//! Release workflow configuration validation unit tests.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::config::ConfigTable;
use crate::workflow_config::{CustomJob, Distribution, JobStage, ToolSource, WorkflowConfig};

/// Placeholder for `workspace.metadata.release.workflow` in expected errors.
const WORKFLOW: &str = "~";

/// A valid commit hash.
const REV: &str = "0123456789abcdef0123456789abcdef01234567";

/// Parses `workflow` as the `[workspace.metadata.release.workflow]` table.
fn parse(workflow: &Value) -> Result<WorkflowConfig, String> {
    let table = ConfigTable::new(workflow, "workspace.metadata.release.workflow".to_string())?;

    WorkflowConfig::parse(&table)
}

/// Asserts that each `(workflow table, expected error)` case is rejected,
/// with [`WORKFLOW`] standing for the table path.
fn assert_rejected(cases: &[(Value, &str)]) {
    for (workflow, expected) in cases {
        let config = parse(workflow);

        assert_eq!(
            config,
            Err(expected.replace(WORKFLOW, "workspace.metadata.release.workflow")),
            "{workflow}"
        );
    }
}

/// Returns a one-job workflow table whose job `release-gate` has `job`.
fn job(job: &Value) -> Value {
    json!({"jobs": {"release-gate": job}})
}

#[test]
fn parse_reads_every_workflow_key_in_configured_order() {
    // Arrange
    let workflow = json!({
        "distributions": {"demo-os:2": "", "demo-old:1": "needs glibc"},
        "jobs": {
            "release-gate": {
                "stage": "check",
                "uses": "./.github/workflows/gate.yml",
                "permissions": {"contents": "read"},
                "plan": true,
            },
            "_publish_registry": {
                "stage": "publish",
                "uses": "demo-org/demo-ci/.github/workflows/publish.yml@v1.2",
                "permissions": {"id-token": "write", "contents": "read"},
                "prerelease": true,
            },
            "publish-docs": {"stage": "publish", "uses": "$/.github/workflows/docs.yml"},
        },
        "node-version": "24.x",
        "smoke-paths": [".github/workflows/publish.yml", "docs/release notes.md"],
        "tool": {"git": "https://example.com/demo-org/ag-release", "rev": REV},
    });

    // Act
    let config = parse(&workflow);

    // Assert
    let custom_job = |id: &str, stage, uses: &str| CustomJob {
        id: id.to_string(),
        permissions: BTreeMap::new(),
        plan: false,
        prerelease: false,
        stage,
        uses: uses.to_string(),
    };
    assert_eq!(
        config,
        Ok(WorkflowConfig {
            distributions: vec![
                Distribution {
                    error: String::new(),
                    image: "demo-os:2".to_string(),
                },
                Distribution {
                    error: "needs glibc".to_string(),
                    image: "demo-old:1".to_string(),
                },
            ],
            jobs: vec![
                CustomJob {
                    permissions: BTreeMap::from([("contents", "read")]),
                    plan: true,
                    ..custom_job(
                        "release-gate",
                        JobStage::Check,
                        "./.github/workflows/gate.yml",
                    )
                },
                CustomJob {
                    permissions: BTreeMap::from([("contents", "read"), ("id-token", "write")]),
                    prerelease: true,
                    ..custom_job(
                        "_publish_registry",
                        JobStage::Publish,
                        "demo-org/demo-ci/.github/workflows/publish.yml@v1.2",
                    )
                },
                custom_job(
                    "publish-docs",
                    JobStage::Publish,
                    "$/.github/workflows/docs.yml",
                ),
            ],
            node_version: Some("24.x".to_string()),
            smoke_paths: vec![
                ".github/workflows/publish.yml".to_string(),
                "docs/release notes.md".to_string(),
            ],
            tool: Some(ToolSource {
                git: "https://example.com/demo-org/ag-release".to_string(),
                rev: REV.to_string(),
            }),
        })
    );
}

#[test]
fn parse_rejects_invalid_scalars_paths_and_images() {
    // Arrange
    let expression = "must not contain `${{`, which GitHub Actions evaluates as an expression";
    let cases = [
        (
            json!({"unknown": 1}),
            "unknown key `~.unknown`; expected one of: distributions, jobs, node-version, \
             smoke-paths, tool",
        ),
        (
            json!({"node-version": ""}),
            "`~.node-version` must be a nonempty string",
        ),
        (
            json!({"node-version": "24.x\nrun: evil"}),
            "`~.node-version` must not contain control characters or line breaks, found \
             \"24.x\\nrun: evil\"",
        ),
        (
            json!({"node-version": "24\u{2028}x"}),
            "`~.node-version` must not contain control characters or line breaks, found \
             \"24\\u{2028}x\"",
        ),
        (
            json!({"node-version": "${{ secrets.NPM_TOKEN }}"}),
            &format!("`~.node-version` {expression}, found `${{{{ secrets.NPM_TOKEN }}}}`"),
        ),
        (
            json!({"smoke-paths": ["../outside"]}),
            "`~.smoke-paths` entry `../outside` must be a relative path inside the workspace root",
        ),
        (
            json!({"smoke-paths": ["docs/${{ github.token }}"]}),
            &format!("`~.smoke-paths` {expression}, found `docs/${{{{ github.token }}}}`"),
        ),
        (
            json!({"distributions": ["ubuntu:22.04"]}),
            "`~.distributions` must be a table",
        ),
        (
            json!({"distributions": {"--privileged": ""}}),
            "`~.distributions.--privileged` must name a container image such as `ubuntu:22.04`",
        ),
        (
            json!({"distributions": {"demo os": ""}}),
            "`~.distributions.demo os` must name a container image such as `ubuntu:22.04`",
        ),
        (
            json!({"distributions": {"": ""}}),
            "`~.distributions.` must name a container image such as `ubuntu:22.04`",
        ),
        (
            json!({"distributions": {"demo-os:${{github.token}}": ""}}),
            &format!(
                "`~.distributions.demo-os:${{{{github.token}}}}` {expression}, found \
                 `demo-os:${{{{github.token}}}}`"
            ),
        ),
        (
            json!({"distributions": {"demo-os:1": false}}),
            "`~.distributions.demo-os:1` must be a string: empty when the install must succeed, \
             else text that the installer's failure contains",
        ),
        (
            json!({"distributions": {"demo-os:1": "fails\n"}}),
            "`~.distributions.demo-os:1` must not contain control characters or line breaks, \
             found \"fails\\n\"",
        ),
    ];

    // Act & Assert
    assert_rejected(&cases);
}

#[test]
fn parse_rejects_an_invalid_tool_source() {
    // Arrange
    let git_error = "`~.tool.git` must be an `https://` repository URL, found";
    let rev_error = "`~.tool.rev` must be a full 40-digit lowercase hexadecimal commit hash, found";
    let cases = [
        (json!({"tool": "ag-release"}), "`~.tool` must be a table"),
        (
            json!({"tool": {"git": "https://example.com/r", "rev": REV, "branch": "main"}}),
            "unknown key `~.tool.branch`; expected one of: git, rev",
        ),
        (json!({"tool": {"rev": REV}}), "missing `~.tool.git`"),
        (
            json!({"tool": {"git": "http://example.com/r", "rev": REV}}),
            &format!("{git_error} `http://example.com/r`"),
        ),
        (
            json!({"tool": {"git": "https://", "rev": REV}}),
            &format!("{git_error} `https://`"),
        ),
        (
            json!({"tool": {"git": "https:///r", "rev": REV}}),
            &format!("{git_error} `https:///r`"),
        ),
        (
            json!({"tool": {"git": "https://example.com/a b", "rev": REV}}),
            &format!("{git_error} `https://example.com/a b`"),
        ),
        (
            json!({"tool": {"git": "https://example.com/${{x}}", "rev": REV}}),
            "`~.tool.git` must not contain `${{`, which GitHub Actions evaluates as an \
             expression, found `https://example.com/${{x}}`",
        ),
        (
            json!({"tool": {"git": "https://example.com/r"}}),
            "missing `~.tool.rev`",
        ),
        (
            json!({"tool": {"git": "https://example.com/r", "rev": "main"}}),
            &format!("{rev_error} `main`"),
        ),
        (
            json!({"tool": {"git": "https://example.com/r", "rev": REV.to_uppercase()}}),
            &format!("{rev_error} `{}`", REV.to_uppercase()),
        ),
    ];

    // Act & Assert
    assert_rejected(&cases);
}

#[test]
fn parse_rejects_invalid_or_colliding_job_ids() {
    // Arrange
    let id_error = "must be a job id that starts with a letter or `_` and contains only letters, \
                    digits, `-`, and `_`";
    let valid = json!({"stage": "check", "uses": "./gate.yml"});
    let cases = [
        (
            json!({"jobs": ["gate"]}),
            "`~.jobs` must be a table".to_string(),
        ),
        (
            json!({"jobs": {"1-gate": valid}}),
            format!("`~.jobs.1-gate` {id_error}"),
        ),
        (
            json!({"jobs": {"gate.yml": valid}}),
            format!("`~.jobs.gate.yml` {id_error}"),
        ),
        (
            json!({"jobs": {"": valid}}),
            format!("`~.jobs.` {id_error}"),
        ),
        (
            json!({"jobs": {"Host": valid}}),
            "`~.jobs.Host` collides with the built-in `host` job; built-in jobs: plan, smoke, \
             build, assemble, distributions, host"
                .to_string(),
        ),
        (
            json!({"jobs": {"gate": valid, "Gate": valid}}),
            "`~.jobs.Gate` and `~.jobs.gate` differ only in case, but GitHub Actions expressions \
             such as `needs.<id>` ignore case"
                .to_string(),
        ),
        (
            json!({"jobs": {"gate": "./gate.yml"}}),
            "`~.jobs.gate` must be a table".to_string(),
        ),
    ];
    let cases = cases
        .iter()
        .map(|(workflow, expected)| (workflow.clone(), expected.as_str()))
        .collect::<Vec<_>>();

    // Act & Assert
    assert_rejected(&cases);
}

#[test]
fn parse_rejects_invalid_job_settings() {
    // Arrange
    let uses_error = "`~.jobs.release-gate.uses` must be a reusable workflow such as \
                      `./.github/workflows/<file>.yml` or \
                      `<owner>/<repo>/.github/workflows/<file>.yml@<ref>`, found";
    let check = |extra: Value| {
        let mut settings = json!({"stage": "check", "uses": "./gate.yml"});
        settings
            .as_object_mut()
            .expect("job table")
            .extend(extra.as_object().expect("extra settings").clone());

        job(&settings)
    };
    let mut cases = vec![
        (
            check(json!({"needs": "plan"})),
            "unknown key `~.jobs.release-gate.needs`; expected one of: permissions, plan, \
             prerelease, stage, uses"
                .to_string(),
        ),
        (
            job(&json!({"uses": "./gate.yml"})),
            "missing `~.jobs.release-gate.stage`".to_string(),
        ),
        (
            check(json!({"stage": "deploy"})),
            "`~.jobs.release-gate.stage` must be `check` or `publish`, found `deploy`".to_string(),
        ),
        (
            job(&json!({"stage": "check"})),
            "missing `~.jobs.release-gate.uses`".to_string(),
        ),
        (
            check(json!({"prerelease": true})),
            "`~.jobs.release-gate.prerelease` applies only to `publish` jobs".to_string(),
        ),
        (
            check(json!({"plan": "yes"})),
            "`~.jobs.release-gate.plan` must be `true` or `false`".to_string(),
        ),
        (
            check(json!({"permissions": "read-all"})),
            "`~.jobs.release-gate.permissions` must be a table".to_string(),
        ),
        (
            check(json!({"permissions": {"content": "read"}})),
            "`~.jobs.release-gate.permissions.content` is not a `GITHUB_TOKEN` permission; \
             permissions: actions, artifact-metadata, attestations, checks, code-quality, \
             contents, copilot-requests, deployments, discussions, id-token, issues, packages, \
             pages, pull-requests, security-events, statuses, vulnerability-alerts"
                .to_string(),
        ),
        (
            check(json!({"permissions": {"contents": "admin"}})),
            "`~.jobs.release-gate.permissions.contents` must be one of: read, write, none"
                .to_string(),
        ),
        (
            check(json!({"permissions": {"id-token": "read"}})),
            "`~.jobs.release-gate.permissions.id-token` must be one of: write, none".to_string(),
        ),
        (
            check(json!({"permissions": {"contents": true}})),
            "`~.jobs.release-gate.permissions.contents` must be one of: read, write, none"
                .to_string(),
        ),
    ];
    for uses in [
        "gate.yml",
        "./",
        "$/",
        "./gate.yml\nsecrets: inherit",
        "./gate yml",
        "./${{ github.token }}",
        "demo-org/demo-ci@v1",
        "demo-org//gate.yml@v1",
        "demo-org/demo-ci/gate.yml@",
        "demo-org/demo-ci/gate.yml@v1@v2",
        "demo-org/demo-ci/gate.yml@v1 # pin",
        "-org/demo-ci/gate.yml@v1",
    ] {
        cases.push((
            check(json!({"uses": uses})),
            format!("{uses_error} `{uses}`"),
        ));
    }
    let cases = cases
        .iter()
        .map(|(workflow, expected)| (workflow.clone(), expected.as_str()))
        .collect::<Vec<_>>();

    // Act & Assert
    assert_rejected(&cases);
}
