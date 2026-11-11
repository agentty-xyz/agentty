//! Complete line and function coverage gate over an LCOV report.
//!
//! A file passes when it has no zero-count `DA` line, `FNF` equals `FNH`, and
//! every function has a record that ran. `llvm-cov export` writes `DA` lines
//! and the `FNF`, `FNH`, `LF`, and `LH` totals in every file record, so a
//! record without them cannot prove coverage and is rejected.
//! `llvm-cov export` writes one `FN`/`FNDA` pair per function instantiation:
//! every generic monomorphization and every crate build, such as a unit-test
//! binary and the library linked into other tests, has its own record, while
//! `FNF` and `FNH` count each definition once. Records are therefore grouped
//! by start line and demangled definition to name each uncovered function.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{env, fs};

use clap::Args;

/// Per-file totals `llvm-cov export` writes in every record, in report order.
const REQUIRED_TOTALS: [&str; 4] = ["FNF", "FNH", "LF", "LH"];

/// Coverage report and the repository paths it must cover completely.
#[derive(Args)]
pub(crate) struct Options {
    /// LCOV report written by `cargo llvm-cov --lcov`.
    lcov: PathBuf,
    /// Repository-relative file or directory to check; repeat to check
    /// several. Every file in the report is checked when omitted.
    #[arg(long = "path", value_name = "PREFIX")]
    paths: Vec<PathBuf>,
}

/// Prints every uncovered line and function in scope.
///
/// Source paths are shown relative to the current directory, which callers
/// set to the repository root.
///
/// # Errors
/// Returns an error for an unreadable, empty, or malformed report, a path
/// selection that matches no files, or any coverage gap.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let report = fs::read_to_string(&options.lcov)
        .map_err(|error| format!("Failed to read {}: {error}", options.lcov.display()))?;
    let root = env::current_dir().unwrap_or_default();

    check(&report, &root, &options.paths, &mut io::stdout().lock())
}

/// Writes the gap report or a confirmation, then fails when any selected file
/// has a gap.
fn check(
    report: &str,
    root: &Path,
    prefixes: &[PathBuf],
    output: &mut dyn Write,
) -> Result<(), String> {
    let mut files: Vec<FileCoverage> = parse(report, root)?
        .into_iter()
        .filter(|file| {
            prefixes.is_empty() || prefixes.iter().any(|prefix| file.path.starts_with(prefix))
        })
        .collect();
    if files.is_empty() {
        let prefixes: Vec<String> = prefixes
            .iter()
            .map(|prefix| prefix.display().to_string())
            .collect();

        return Err(format!(
            "No coverage records match the selected paths: {}",
            prefixes.join(", ")
        ));
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let gaps: Vec<FileGap> = files.iter().filter_map(FileCoverage::gap).collect();
    let (text, result) = if gaps.is_empty() {
        (
            format!(
                "Coverage complete: {} files have no uncovered lines or functions.\n",
                files.len()
            ),
            Ok(()),
        )
    } else {
        (
            gaps.iter().map(FileGap::render).collect(),
            Err(summary(&gaps, files.len())),
        )
    };
    output
        .write_all(text.as_bytes())
        .map_err(|error| format!("Failed to write the coverage report: {error}"))?;

    result
}

/// Parses every `SF` section and rejects reports that cannot prove coverage.
fn parse(report: &str, root: &Path) -> Result<Vec<FileCoverage>, String> {
    let mut files = Vec::new();
    let mut current: Option<FileCoverage> = None;
    for (index, line) in report.lines().enumerate() {
        let line_number = index + 1;
        if line == "end_of_record" {
            let file = current.take().ok_or_else(|| {
                format!("LCOV line {line_number}: `end_of_record` outside a source file record")
            })?;
            file.validate(line_number)?;
            files.push(file);

            continue;
        }
        let Some((kind, value)) = line.split_once(':') else {
            continue;
        };
        match (kind, current.as_mut()) {
            ("SF", Some(file)) => {
                return Err(format!(
                    "LCOV line {line_number}: the record for {} has no `end_of_record`",
                    file.path.display()
                ));
            }
            ("SF", None) => current = Some(FileCoverage::new(value, root)),
            ("TN", None) => {}
            (_, None) => {
                return Err(format!(
                    "LCOV line {line_number}: `{kind}` appears outside a source file record"
                ));
            }
            (_, Some(file)) => file.record(kind, value, line_number)?,
        }
    }
    if let Some(file) = current {
        return Err(format!(
            "LCOV report ends inside the record for {}",
            file.path.display()
        ));
    }
    if files.is_empty() {
        return Err("LCOV report has no source file records".to_string());
    }

    Ok(files)
}

/// Coverage records and totals from one `SF` section.
struct FileCoverage {
    function_lines: BTreeMap<String, u64>,
    functions: BTreeMap<(u64, String), FunctionGroup>,
    has_line_records: bool,
    path: PathBuf,
    totals: BTreeMap<String, u64>,
    uncovered_lines: Vec<u64>,
}

impl FileCoverage {
    /// Starts a section, showing paths under `root` relative to it.
    fn new(source: &str, root: &Path) -> Self {
        let source = Path::new(source);

        Self {
            function_lines: BTreeMap::new(),
            functions: BTreeMap::new(),
            has_line_records: false,
            path: source.strip_prefix(root).unwrap_or(source).to_path_buf(),
            totals: BTreeMap::new(),
            uncovered_lines: Vec::new(),
        }
    }

    /// Rejects a finished section that lacks line records or a total, or
    /// whose hit totals exceed the found totals.
    fn validate(&self, line_number: usize) -> Result<(), String> {
        let path = self.path.display();
        let missing: Vec<String> = REQUIRED_TOTALS
            .iter()
            .filter(|kind| !self.totals.contains_key(**kind))
            .map(|kind| format!("`{kind}`"))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "LCOV line {line_number}: the record for {path} lacks totals: {}",
                missing.join(", ")
            ));
        }
        if !self.has_line_records {
            return Err(format!(
                "LCOV line {line_number}: the record for {path} has no `DA` line records"
            ));
        }
        for (found, hit) in [("FNF", "FNH"), ("LF", "LH")] {
            if self.totals[hit] > self.totals[found] {
                return Err(format!(
                    "LCOV line {line_number}: the record for {path} has `{hit}` {} above \
                     `{found}` {}",
                    self.totals[hit], self.totals[found]
                ));
            }
        }

        Ok(())
    }

    /// Applies one record; other record kinds are ignored.
    fn record(&mut self, kind: &str, value: &str, line_number: usize) -> Result<(), String> {
        match kind {
            "DA" => {
                let (line, count) = Self::pair(value, line_number)?;
                let line = Self::number(line, line_number)?;
                if Self::number(count, line_number)? == 0 {
                    self.uncovered_lines.push(line);
                }
                self.has_line_records = true;
            }
            "FN" => {
                let (start, name) = Self::pair(value, line_number)?;
                let start = Self::number(start, line_number)?;
                self.function_lines.insert(name.to_string(), start);
            }
            "FNDA" => {
                let (count, name) = Self::pair(value, line_number)?;
                let executed = Self::number(count, line_number)? > 0;
                let start = *self.function_lines.get(name).ok_or_else(|| {
                    format!("LCOV line {line_number}: `FNDA` names a function without `FN`")
                })?;
                self.add_instantiation(start, name, executed);
            }
            "FNF" | "FNH" | "LF" | "LH" => {
                self.totals
                    .insert(kind.to_string(), Self::number(value, line_number)?);
            }
            _ => {}
        }

        Ok(())
    }

    fn pair(value: &str, line_number: usize) -> Result<(&str, &str), String> {
        value
            .split_once(',')
            .ok_or_else(|| format!("LCOV line {line_number}: expected two comma-separated fields"))
    }

    fn number(value: &str, line_number: usize) -> Result<u64, String> {
        value
            .parse()
            .map_err(|error| format!("LCOV line {line_number}: invalid count `{value}`: {error}"))
    }

    /// Counts one instantiation toward the definition LLVM counts once.
    fn add_instantiation(&mut self, start: u64, mangled: &str, executed: bool) {
        let demangled = format!("{:#}", rustc_demangle::demangle(mangled));
        let (name, definition) = definition_names(&demangled);
        let group = self
            .functions
            .entry((start, definition))
            .or_insert_with(|| FunctionGroup {
                executed: false,
                name,
            });
        group.executed |= executed;
    }

    /// Returns this file's gaps, or `None` when its lines and functions are
    /// complete.
    ///
    /// A function record that never ran is a gap even when `FNF` equals
    /// `FNH`. Totals are present because [`Self::validate`] accepted the
    /// section.
    fn gap(&self) -> Option<FileGap> {
        let function_gap = self.totals["FNF"] - self.totals["FNH"];
        let mut uncovered_lines = self.uncovered_lines.clone();
        uncovered_lines.sort_unstable();
        uncovered_lines.dedup();
        let uncovered_functions: Vec<String> = self
            .functions
            .iter()
            .filter(|(_, group)| !group.executed)
            .map(|((start, _), group)| format!("{start} {}", group.name))
            .collect();
        if uncovered_lines.is_empty() && function_gap == 0 && uncovered_functions.is_empty() {
            return None;
        }

        Some(FileGap {
            function_gap,
            path: self.path.clone(),
            uncovered_functions,
            uncovered_lines,
        })
    }
}

/// Instantiations of one function definition; it is covered when any ran.
struct FunctionGroup {
    executed: bool,
    name: String,
}

/// Returns a readable name with generic arguments shortened to `<..>`, and a
/// definition key that also omits implementing types.
///
/// Instantiations of one definition differ only inside angle brackets, so
/// together with the start line the key matches the definition LLVM counts.
fn definition_names(demangled: &str) -> (String, String) {
    let mut name = String::new();
    let mut definition = String::new();
    // `true` marks a generic argument list; `false` a qualified
    // `<Type as Trait>` path.
    let mut brackets: Vec<bool> = Vec::new();
    let mut previous = None;
    for character in demangled.chars() {
        let in_arguments = brackets.contains(&true);
        match character {
            '<' => {
                let opens_arguments = matches!(
                    previous,
                    Some(prior) if prior == '_' || prior == ':' || prior == '>'
                        || char::is_alphanumeric(prior)
                );
                if !in_arguments {
                    name.push_str(if opens_arguments { "<..>" } else { "<" });
                }
                if brackets.is_empty() {
                    definition.push('<');
                }
                brackets.push(opens_arguments);
            }
            '>' if previous != Some('-') && !brackets.is_empty() => {
                let closes_path = brackets.pop() == Some(false);
                if closes_path && !in_arguments {
                    name.push('>');
                }
                if brackets.is_empty() {
                    definition.push('>');
                }
            }
            _ => {
                if !in_arguments {
                    name.push(character);
                }
                if brackets.is_empty() {
                    definition.push(character);
                }
            }
        }
        previous = Some(character);
    }

    (name, definition)
}

/// Gaps reported for one selected file.
struct FileGap {
    function_gap: u64,
    path: PathBuf,
    uncovered_functions: Vec<String>,
    uncovered_lines: Vec<u64>,
}

impl FileGap {
    /// Renders the file's gaps as an indented block.
    fn render(&self) -> String {
        let mut lines = vec![self.path.display().to_string()];
        if !self.uncovered_lines.is_empty() {
            lines.push(format!(
                "  uncovered lines: {}",
                Self::line_ranges(&self.uncovered_lines)
            ));
        }
        if !self.uncovered_functions.is_empty() {
            lines.push("  uncovered functions (start line, name):".to_string());
            lines.extend(
                self.uncovered_functions
                    .iter()
                    .map(|function| format!("    {function}")),
            );
        }
        if self.function_gap != self.uncovered_functions.len() as u64 {
            lines.push(format!(
                "  FNF-FNH is {} but the function records show {} uncovered functions",
                self.function_gap,
                self.uncovered_functions.len()
            ));
        }

        let mut text = lines.join("\n");
        text.push('\n');

        text
    }

    /// Formats sorted line numbers as compact ranges such as `12-15,22`.
    fn line_ranges(lines: &[u64]) -> String {
        let mut ranges: Vec<(u64, u64)> = Vec::new();
        for &line in lines {
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == line => *end = line,
                _ => ranges.push((line, line)),
            }
        }
        let ranges: Vec<String> = ranges
            .into_iter()
            .map(|(start, end)| {
                if start == end {
                    start.to_string()
                } else {
                    format!("{start}-{end}")
                }
            })
            .collect();

        ranges.join(",")
    }
}

/// Totals every gap kind across the selected files, counting each file's
/// uncovered functions as the larger of `FNF-FNH` and its never-run records.
fn summary(gaps: &[FileGap], checked_files: usize) -> String {
    let uncovered_lines: usize = gaps.iter().map(|gap| gap.uncovered_lines.len()).sum();
    let uncovered_functions: u64 = gaps
        .iter()
        .map(|gap| gap.function_gap.max(gap.uncovered_functions.len() as u64))
        .sum();

    format!(
        "Coverage incomplete in {} of {checked_files} files: {uncovered_lines} uncovered lines \
         and {uncovered_functions} uncovered functions",
        gaps.len()
    )
}

#[cfg(test)]
#[path = "check_coverage_test.rs"]
mod tests;
