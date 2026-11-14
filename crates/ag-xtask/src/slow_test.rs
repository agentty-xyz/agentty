//! Slow-test reports and a duration ratchet over nextest `JUnit` reports.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::PathBuf;

use clap::Args;
use serde::Deserialize;

use crate::timing::{self, FileReader, Reader, TestCase};

/// Exclusive upper bounds, in seconds, of the reported duration bands.
const BANDS: [(f64, &str); 7] = [
    (0.01, "<10ms"),
    (0.05, "10-50ms"),
    (0.1, "50-100ms"),
    (0.5, "100-500ms"),
    (1.0, "0.5-1s"),
    (5.0, "1-5s"),
    (f64::INFINITY, ">=5s"),
];

/// Report input, ranking limits, and the optional duration check.
#[derive(Args)]
pub(crate) struct Options {
    /// Known slow tests allowed over `--max-seconds`, each with a reason.
    #[arg(long, requires = "max_seconds")]
    allowlist: Option<PathBuf>,
    /// Fail when a test missing from the allowlist runs longer than this.
    #[arg(long, value_parser = timing::number)]
    max_seconds: Option<f64>,
    /// Leading test-path module segments that group tests within a binary.
    #[arg(long, default_value_t = 2)]
    module_depth: usize,
    /// Nextest `JUnit` report, such as `target/nextest/ci/junit.xml`.
    report: PathBuf,
    /// Entries listed in each ranking.
    #[arg(long, default_value_t = 20)]
    top: usize,
}

/// Prints the slow-test report and fails when an unlisted test is too slow.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    report(options, &FileReader, &mut io::stdout().lock())
}

/// A known slow test and why it may exceed the threshold.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowedTest {
    binary: String,
    reason: String,
    test: String,
}

/// Number of tests and their summed duration.
#[derive(Default)]
struct Total {
    seconds: f64,
    tests: usize,
}

impl Total {
    fn add(&mut self, seconds: f64) {
        self.seconds += seconds;
        self.tests += 1;
    }

    fn render(&self) -> String {
        format!("{:>9.3} s  {:>6} tests", self.seconds, self.tests)
    }
}

fn report(options: &Options, reader: &impl Reader, writer: &mut impl Write) -> Result<(), String> {
    let path = &options.report;
    let junit = timing::parse_junit(&reader.read(path)?)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut cases: Vec<&TestCase> = junit.cases.iter().collect();
    cases.sort_by(|left, right| {
        right
            .seconds
            .total_cmp(&left.seconds)
            .then_with(|| (&left.binary, &left.name).cmp(&(&right.binary, &right.name)))
    });
    let mut lines = summary(&cases, junit.seconds, options);
    let mut offenders = Vec::new();
    if let Some(max_seconds) = options.max_seconds {
        let allowlist = match &options.allowlist {
            Some(path) => parse_allowlist(&reader.read(path)?)
                .map_err(|error| format!("{}: {error}", path.display()))?,
            None => Vec::new(),
        };
        offenders = check(&cases, &allowlist, max_seconds, &mut lines);
    }
    lines.push(String::new());
    writer
        .write_all(lines.join("\n").as_bytes())
        .and_then(|()| writer.flush())
        .map_err(|error| error.to_string())?;
    if offenders.is_empty() {
        return Ok(());
    }

    Err(format!(
        "{} tests exceed the slow-test threshold and are not allowlisted:\n{}\nSpeed them up, or \
         add each to the allowlist with a reason.",
        offenders.len(),
        offenders.join("\n")
    ))
}

fn summary(cases: &[&TestCase], suite_seconds: f64, options: &Options) -> Vec<String> {
    let mut binaries: BTreeMap<&str, Total> = BTreeMap::new();
    let mut modules: BTreeMap<(&str, String), Total> = BTreeMap::new();
    let mut bands: [Total; BANDS.len()] = Default::default();
    for case in cases {
        binaries.entry(&case.binary).or_default().add(case.seconds);
        modules
            .entry((&case.binary, module(&case.name, options.module_depth)))
            .or_default()
            .add(case.seconds);
        let band = BANDS
            .iter()
            .position(|(bound, _)| case.seconds < *bound)
            .unwrap_or(BANDS.len() - 1);
        bands[band].add(case.seconds);
    }
    let summed: f64 = cases.iter().map(|case| case.seconds).sum();
    let mut lines = vec![format!(
        "{} tests: {summed:.3} s summed test time, {suite_seconds:.3} s suite time",
        cases.len()
    )];
    lines.push(format!(
        "\nSlowest tests (top {} of {}):",
        options.top,
        cases.len()
    ));
    lines.extend(cases.iter().take(options.top).map(|case| describe(case)));
    lines.push(format!(
        "\nBinaries by summed time (top {} of {}):",
        options.top,
        binaries.len()
    ));
    lines.extend(
        ranked(binaries)
            .into_iter()
            .take(options.top)
            .map(|(binary, total)| format!("{}  {binary}", total.render())),
    );
    lines.push(format!(
        "\nModules at depth {} by summed time (top {} of {}):",
        options.module_depth,
        options.top,
        modules.len()
    ));
    lines.extend(
        ranked(modules)
            .into_iter()
            .take(options.top)
            .map(|((binary, module), total)| format!("{}  {binary}  {module}", total.render())),
    );
    lines.push("\nDuration bands:".into());
    lines.extend(
        BANDS
            .iter()
            .zip(&bands)
            .map(|((_, label), total)| format!("{}  {label}", total.render())),
    );

    lines
}

fn describe(case: &TestCase) -> String {
    format!("{:>9.3} s  {}  {}", case.seconds, case.binary, case.name)
}

/// Returns the first `depth` module segments of a test path, excluding the
/// test function itself.
fn module(name: &str, depth: usize) -> String {
    let segments: Vec<&str> = name.split("::").collect();
    let modules = &segments[..segments.len() - 1];
    let kept = &modules[..modules.len().min(depth)];
    if kept.is_empty() {
        return "(root)".into();
    }

    kept.join("::")
}

/// Orders groups by summed time, slowest first, then by key.
fn ranked<K: Ord>(groups: BTreeMap<K, Total>) -> Vec<(K, Total)> {
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by(|(left_key, left), (right_key, right)| {
        right
            .seconds
            .total_cmp(&left.seconds)
            .then_with(|| left_key.cmp(right_key))
    });

    groups
}

fn parse_allowlist(text: &str) -> Result<Vec<AllowedTest>, String> {
    let entries: Vec<AllowedTest> =
        serde_yaml_ng::from_str(text).map_err(|error| format!("Invalid allowlist: {error}"))?;
    let mut seen = BTreeSet::new();
    for entry in &entries {
        let id = format!("{}  {}", entry.binary, entry.test);
        if [&entry.binary, &entry.test, &entry.reason]
            .iter()
            .any(|field| field.trim().is_empty())
        {
            return Err(format!(
                "Allowlist entry `{id}` needs a binary, test, and reason"
            ));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("Duplicate allowlist entry `{id}`"));
        }
    }

    Ok(entries)
}

/// Appends the threshold summary and stale-entry warnings, and returns each
/// unlisted test over the threshold.
fn check(
    cases: &[&TestCase],
    allowlist: &[AllowedTest],
    max_seconds: f64,
    lines: &mut Vec<String>,
) -> Vec<String> {
    let allowed: BTreeSet<(&str, &str)> = allowlist
        .iter()
        .map(|entry| (entry.binary.as_str(), entry.test.as_str()))
        .collect();
    let over: Vec<&&TestCase> = cases
        .iter()
        .filter(|case| case.seconds > max_seconds)
        .collect();
    let offenders: Vec<String> = over
        .iter()
        .filter(|case| !allowed.contains(&(case.binary.as_str(), case.name.as_str())))
        .map(|case| describe(case))
        .collect();
    lines.push(format!(
        "\nThreshold {max_seconds} s: {} over, {} allowlisted, {} new",
        over.len(),
        over.len() - offenders.len(),
        offenders.len()
    ));
    for entry in allowlist {
        let id = format!("{}  {}", entry.binary, entry.test);
        match cases
            .iter()
            .find(|case| case.binary == entry.binary && case.name == entry.test)
        {
            None => lines.push(format!(
                "warning: allowlisted test not in report; remove it if it is gone: {id}"
            )),
            Some(case) if case.seconds <= max_seconds => lines.push(format!(
                "warning: allowlisted test took {:.3} s, within {max_seconds} s; remove it once \
                 it stays fast: {id}",
                case.seconds
            )),
            Some(_) => {}
        }
    }

    offenders
}

#[cfg(test)]
#[path = "slow_test_test.rs"]
mod tests;
