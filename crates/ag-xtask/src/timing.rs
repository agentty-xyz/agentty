//! Comparable repeated measurements from fresh CI artifacts.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Metadata files from at least three independent invocations per revision.
#[derive(Args)]
pub(crate) struct Options {
    #[arg(long, required = true, num_args = 1..)]
    after: Vec<PathBuf>,
    #[arg(long, required = true, num_args = 1..)]
    before: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Metadata {
    environment: Value,
    hook_seconds: f64,
    identity: Identity,
    outcome: String,
    prepared: bool,
    reports: BTreeMap<String, String>,
    schema: u32,
}

#[derive(Deserialize)]
struct Identity {
    attempt: String,
    job: String,
    run_id: String,
    sha: String,
    suite: String,
}

struct Sample {
    metadata: Metadata,
    metrics: BTreeMap<&'static str, f64>,
}

#[cfg_attr(test, mockall::automock)]
trait Reader {
    fn read(&self, path: &Path) -> Result<String, String>;
}

struct FileReader;

impl Reader for FileReader {
    fn read(&self, path: &Path) -> Result<String, String> {
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Prints a JSON comparison or fails when the evidence is incomplete or mixed.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let report = compare(options, &FileReader)?;

    write_report(&report, &mut io::stdout().lock())
}

fn write_report(report: &Value, writer: &mut impl Write) -> Result<(), String> {
    serde_json::to_writer_pretty(writer, report).map_err(|error| error.to_string())
}

fn compare(options: &Options, reader: &impl Reader) -> Result<Value, String> {
    if options.before.len() < 3 || options.after.len() < 3 {
        return Err(
            "Provide at least three samples per revision; one run cannot establish a gain".into(),
        );
    }
    let before = load(&options.before, reader)?;
    let after = load(&options.after, reader)?;
    let reference = &before[0].metadata;
    let mut identities = HashSet::new();
    for arm in [&before, &after] {
        let sha = &arm[0].metadata.identity.sha;
        for sample in arm {
            let metadata = &sample.metadata;
            let identity = &metadata.identity;
            if &identity.sha != sha
                || identity.suite != reference.identity.suite
                || metadata.environment != reference.environment
            {
                return Err(
                    "Mixed source revisions, suites, environments, or cache evidence".into(),
                );
            }
            if !identities.insert((
                &identity.run_id,
                &identity.attempt,
                &identity.job,
                &identity.suite,
            )) {
                return Err(
                    "Duplicate run/attempt/job/suite; repeated downloads are not samples".into(),
                );
            }
        }
    }
    let mut metrics = BTreeMap::new();
    for name in before[0].metrics.keys() {
        let baseline = statistics(&before, name);
        let candidate = statistics(&after, name);
        let reduction = if baseline["median"] == 0.0 {
            None
        } else {
            Some((baseline["median"] - candidate["median"]) / baseline["median"] * 100.0)
        };
        metrics.insert(
            *name,
            json!({
                "before": baseline, "after": candidate, "reduction_percent": reduction,
            }),
        );
    }

    Ok(json!({
        "schema": 1, "suite": reference.identity.suite,
        "environment": reference.environment,
        "before_sha": reference.identity.sha, "after_sha": after[0].metadata.identity.sha,
        "before_samples": options.before, "after_samples": options.after,
        "metrics": metrics,
        "limits": "Hook time excludes setup and queueing. Summed test process time is not wall time. Exact cache miss does not imply cold. Ranges overlap unless demonstrated otherwise; these are descriptive samples, not a causal or workspace-wide speed estimate.",
    }))
}

fn load(paths: &[PathBuf], reader: &impl Reader) -> Result<Vec<Sample>, String> {
    paths
        .iter()
        .map(|path| {
            let metadata: Metadata = serde_json::from_str(&reader.read(path)?)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if metadata.schema != 1
                || !metadata.prepared
                || metadata.outcome != "success"
                || !metadata.hook_seconds.is_finite()
                || metadata.hook_seconds < 0.0
                || [
                    &metadata.identity.run_id,
                    &metadata.identity.attempt,
                    &metadata.identity.job,
                    &metadata.identity.sha,
                    &metadata.identity.suite,
                ]
                .iter()
                .any(|value| value.is_empty())
                || ["runner_os", "runner_arch", "rustc", "nextest"]
                    .iter()
                    .any(|field| {
                        metadata.environment[field]
                            .as_str()
                            .is_none_or(str::is_empty)
                    })
            {
                return Err(format!(
                    "{}: unsuccessful or incomplete measurement",
                    path.display()
                ));
            }
            let mut reports = Vec::new();
            for name in ["junit.xml", "cargo-timing.html"] {
                let report_path = path.with_file_name(name);
                let report = reader.read(&report_path)?;
                let hash: String = Sha256::digest(report.as_bytes())
                    .into_iter()
                    .flat_map(|byte| {
                        let digits = b"0123456789abcdef";
                        [
                            char::from(digits[usize::from(byte >> 4)]),
                            char::from(digits[usize::from(byte & 15)]),
                        ]
                    })
                    .collect();
                if metadata.reports.get(name) != Some(&hash) {
                    return Err(format!(
                        "{}: report hash mismatch or missing report",
                        report_path.display()
                    ));
                }
                reports.push(report);
            }
            let mut metrics = parse_reports(&reports[0], &reports[1])?;
            metrics.insert("hook_seconds", metadata.hook_seconds);

            Ok(Sample { metadata, metrics })
        })
        .collect()
}

fn parse_reports(junit: &str, cargo: &str) -> Result<BTreeMap<&'static str, f64>, String> {
    let document =
        roxmltree::Document::parse(junit).map_err(|error| format!("Invalid JUnit: {error}"))?;
    let root = document.root_element();
    if !root.has_tag_name("testsuites") {
        return Err("Expected nextest testsuites root".into());
    }
    let attribute = |name| number(root.attribute(name).unwrap_or(""));
    if attribute("failures")? != 0.0 || attribute("errors")? != 0.0 {
        return Err("JUnit contains failed or errored tests".into());
    }
    let process_seconds = root
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
        .map(|node| number(node.attribute("time").unwrap_or("")))
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .sum();
    let mut metrics = BTreeMap::from([
        ("execution_seconds", attribute("time")?),
        ("tests", attribute("tests")?),
        ("summed_test_process_seconds", process_seconds),
    ]);
    for (name, label) in [
        ("build_seconds", "Total time"),
        ("fresh_units", "Fresh units"),
        ("dirty_units", "Dirty units"),
    ] {
        let value = cargo
            .split_once(&format!("<td>{label}:</td>"))
            .and_then(|(_, rest)| rest.trim_start().strip_prefix("<td>"))
            .and_then(|rest| rest.split_once("</td>"))
            .map(|(value, _)| value)
            .ok_or_else(|| format!("Missing Cargo {label}"))?;
        let value = if name == "build_seconds" {
            value.split_once('s').ok_or("Missing Cargo seconds unit")?.0
        } else {
            value
        };
        metrics.insert(name, number(value)?);
    }

    Ok(metrics)
}

fn number(value: &str) -> Result<f64, String> {
    let value: f64 = value
        .parse()
        .map_err(|error| format!("Invalid numeric metric: {error}"))?;
    if !value.is_finite() || value < 0.0 {
        return Err("Timing metrics must be finite and nonnegative".into());
    }

    Ok(value)
}

fn statistics(samples: &[Sample], name: &str) -> BTreeMap<&'static str, f64> {
    let mut values: Vec<_> = samples.iter().map(|sample| sample.metrics[name]).collect();
    values.sort_by(f64::total_cmp);
    let median = values[(values.len() - 1) / 2].midpoint(values[values.len() / 2]);

    BTreeMap::from([
        ("min", values[0]),
        ("max", values[values.len() - 1]),
        ("median", median),
    ])
}

#[cfg(test)]
#[path = "timing_test.rs"]
mod tests;
