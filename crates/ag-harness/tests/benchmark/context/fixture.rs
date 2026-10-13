//! Deterministic fixtures and scorers for the context benchmark.
//!
//! Content comes from a seeded generator rather than the moving source tree,
//! so every run reads the same bytes. Wording, identifiers, and numbers vary
//! line to line because some providers stall on highly repetitive tool output.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

const NOUNS: [&str; 40] = [
    "anchor", "beacon", "bramble", "canyon", "cinder", "copper", "delta", "dynamo", "ember",
    "estuary", "falcon", "fern", "garnet", "glacier", "harbor", "heron", "hollow", "island",
    "jasper", "kernel", "lantern", "ledger", "lumen", "marble", "meadow", "nectar", "onyx",
    "orbit", "prairie", "quartz", "quiver", "raven", "signal", "summit", "tundra", "umber",
    "velvet", "willow", "yarrow", "zephyr",
];
const VERBS: [&str; 24] = [
    "audit", "bind", "compact", "decode", "drain", "encode", "flush", "index", "load", "merge",
    "parse", "prune", "render", "resolve", "rotate", "route", "sample", "scan", "seal", "settle",
    "split", "stage", "trace", "weigh",
];
const PAST_VERBS: [&str; 12] = [
    "counted", "weighed", "sorted", "stamped", "loaded", "checked", "routed", "sealed", "logged",
    "tagged", "scanned", "cleared",
];
const QUALIFIERS: [&str; 12] = [
    "in arrival order",
    "without blocking readers",
    "for the current window",
    "against the cached totals",
    "before the next flush",
    "when the queue is idle",
    "across every shard",
    "using the stored weights",
    "after validation succeeds",
    "within the retry budget",
    "for downstream consumers",
    "under the shared lock",
];
const ADJECTIVES: [&str; 10] = [
    "quiet", "brisk", "dusty", "amber", "narrow", "steady", "hollow", "patient", "crowded", "late",
];
const TIMES: [&str; 6] = [
    "dawn",
    "noon",
    "dusk",
    "midnight",
    "the second shift",
    "the weekly audit",
];
/// Public-function counts per read-loop file; the single maximum is
/// mid-sequence so its result is elided before the model answers.
const READ_LOOP_PUBLIC_COUNTS: [usize; 16] = [4, 6, 3, 5, 2, 9, 4, 7, 3, 5, 6, 2, 4, 3, 5, 6];
pub(super) const PART_CODES: [&str; 6] = [
    "amber-17",
    "basalt-29",
    "cobalt-31",
    "dune-43",
    "ember-59",
    "fjord-61",
];
pub(super) const LARGE_CODE: &str = "ivory-97";
/// Stays below the read tool's per-line limit while exceeding the share of
/// the default benchmark budget that one tool result may keep.
const LARGE_LINE_BYTES: usize = 48_000;

/// One repository file written before a scenario starts.
pub(super) struct FixtureFile {
    pub(super) contents: String,
    pub(super) path: String,
}

/// Deterministic check results for one scenario run.
#[derive(Debug, Default)]
pub(super) struct Score {
    pub(super) failures: Vec<String>,
    pub(super) passed: usize,
    pub(super) total: usize,
}

impl Score {
    /// Returns the fraction of checks that passed.
    pub(super) fn correctness(&self) -> f64 {
        if self.total == 0 {
            return 0.0;
        }

        f64::from(u32::try_from(self.passed).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(self.total).unwrap_or(u32::MAX))
    }

    /// Returns whether every check passed.
    pub(super) fn is_perfect(&self) -> bool {
        self.total > 0 && self.passed == self.total
    }

    fn check(&mut self, passed: bool, failure: impl FnOnce() -> String) {
        self.total += 1;
        if passed {
            self.passed += 1;
        } else {
            self.failures.push(failure());
        }
    }
}

/// Many mid-sized Rust-like files whose public functions must be listed.
pub(super) struct ReadLoopFixture {
    files: Vec<FixtureFile>,
    most_public_functions: String,
    public_functions: BTreeMap<String, BTreeSet<String>>,
}

impl ReadLoopFixture {
    /// Generates the same sixteen files on every call.
    pub(super) fn generate() -> Self {
        let mut rng = Lcg::new(0x5eed_0001);
        let mut modules = NOUNS.to_vec();
        rng.shuffle(&mut modules);
        let mut files = Vec::new();
        let mut public_functions = BTreeMap::new();
        let mut most_public_functions = String::new();
        let mut most = 0;

        for (module, public) in modules.into_iter().zip(READ_LOOP_PUBLIC_COUNTS) {
            let private = 4 + rng.below(4);
            let source = RustSource::generate(&mut rng, module, public, private);
            let path = format!("src/{module}.rs");
            let names = source
                .functions
                .iter()
                .filter(|function| function.visibility == Visibility::Public)
                .map(|function| function.name.clone())
                .collect::<BTreeSet<_>>();
            if names.len() > most {
                most = names.len();
                most_public_functions.clone_from(&path);
            }
            public_functions.insert(path.clone(), names);
            files.push(FixtureFile {
                contents: source.contents,
                path,
            });
        }

        Self {
            files,
            most_public_functions,
            public_functions,
        }
    }

    /// Returns the files to commit before the run.
    pub(super) fn files(&self) -> &[FixtureFile] {
        &self.files
    }

    /// Returns the task prompt naming every file.
    pub(super) fn prompt(&self) -> String {
        let paths = self
            .files
            .iter()
            .map(|file| format!("`{}`", file.path))
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "This repository contains {} Rust source files: {paths}. Read every one of them with \
             the read tool. A public function is one declared with exactly `pub fn`, including \
             methods inside `impl` blocks; `pub(crate) fn` and plain `fn` are not public. Return \
             one entry per file with its path exactly as listed and the names of its public \
             functions, and set `most_public_functions` to the path of the file that declares the \
             most public functions.",
            self.files.len()
        )
    }

    /// Returns the output schema without any expected answer.
    pub(super) fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "public_functions": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["path", "public_functions"],
                        "additionalProperties": false
                    }
                },
                "most_public_functions": { "type": "string" }
            },
            "required": ["files", "most_public_functions"],
            "additionalProperties": false
        })
    }

    /// Scores one check per file (exact public-function set) plus the
    /// maximum file.
    pub(super) fn score(&self, output: &Value) -> Score {
        let mut answered = BTreeMap::<String, BTreeSet<String>>::new();
        for entry in output["files"].as_array().into_iter().flatten() {
            let path = normalize_path(entry["path"].as_str().unwrap_or_default());
            let names = entry["public_functions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|name| name.trim().trim_end_matches("()").to_string())
                .collect::<BTreeSet<_>>();
            answered.entry(path).or_default().extend(names);
        }
        let mut score = Score::default();
        for (path, expected) in &self.public_functions {
            let actual = answered.get(path);
            score.check(actual == Some(expected), || {
                format!("{path}: expected {expected:?}, got {actual:?}")
            });
        }
        let most = normalize_path(output["most_public_functions"].as_str().unwrap_or_default());
        score.check(most == self.most_public_functions, || {
            format!(
                "most_public_functions: expected {}, got {most}",
                self.most_public_functions
            )
        });

        score
    }
}

/// Three doc-comment edits across two long Rust-like files.
pub(super) struct WriteFixture {
    pub(super) edits: Vec<DocEdit>,
    pub(super) expected: Vec<FixtureFile>,
    files: Vec<FixtureFile>,
}

impl WriteFixture {
    /// Generates the same two files and edits on every call.
    pub(super) fn generate() -> Self {
        let mut rng = Lcg::new(0x5eed_0002);
        let ledger = RustSource::generate(&mut rng, "ledger", 14, 8);
        let courier = RustSource::generate(&mut rng, "courier", 13, 8);
        let ledger_public = ledger.public_functions();
        let courier_public = courier.public_functions();
        let targets = [
            (
                "src/ledger.rs",
                ledger_public[2],
                "before the nightly settlement window closes",
            ),
            (
                "src/ledger.rs",
                ledger_public[ledger_public.len() - 1],
                "and keeps the audit trail append-only",
            ),
            (
                "src/courier.rs",
                courier_public[courier_public.len() / 2],
                "once per delivery route, in stop order",
            ),
        ];
        let edits = targets
            .into_iter()
            .map(|(path, function, suffix)| DocEdit {
                function: function.name.clone(),
                new_line: format!(
                    "/// {} the {} {suffix}.",
                    capitalize(&third_person(&function.verb)),
                    function.noun
                ),
                old_line: function.doc.clone(),
                path: path.to_string(),
            })
            .collect::<Vec<_>>();
        let files = vec![
            FixtureFile {
                contents: courier.contents,
                path: "src/courier.rs".to_string(),
            },
            FixtureFile {
                contents: ledger.contents,
                path: "src/ledger.rs".to_string(),
            },
        ];
        let expected = files
            .iter()
            .map(|file| FixtureFile {
                contents: edits
                    .iter()
                    .filter(|edit| edit.path == file.path)
                    .fold(file.contents.clone(), |contents, edit| {
                        contents.replace(&edit.old_line, &edit.new_line)
                    }),
                path: file.path.clone(),
            })
            .collect();

        Self {
            edits,
            expected,
            files,
        }
    }

    /// Returns the files to commit before the run.
    pub(super) fn files(&self) -> &[FixtureFile] {
        &self.files
    }

    /// Returns the task prompt listing the exact edits.
    pub(super) fn prompt(&self) -> String {
        let edits = self
            .edits
            .iter()
            .enumerate()
            .map(|(index, edit)| {
                format!(
                    "{}. In `{}`, replace the doc comment line `{}` directly above `pub fn {}` \
                     with `{}`.",
                    index + 1,
                    edit.path,
                    edit.old_line,
                    edit.function,
                    edit.new_line
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "Make exactly these three doc-comment edits with the write tool, keeping indentation \
             and changing nothing else. Read the files first to find the lines.\n{edits}\nWhen \
             all three edits are applied, set answer to `done`."
        )
    }

    /// Scores each edit, then the exact final worktree: `contents` maps each
    /// fixture path to its final text and `status` is `git status
    /// --porcelain` output.
    pub(super) fn score(&self, contents: &BTreeMap<String, String>, status: &str) -> Score {
        let mut score = Score::default();
        for edit in &self.edits {
            let text = contents.get(&edit.path).map_or("", String::as_str);
            let new_lines = text
                .lines()
                .filter(|line| line.trim() == edit.new_line)
                .count();
            let old_lines = text
                .lines()
                .filter(|line| line.trim() == edit.old_line)
                .count();
            score.check(new_lines == 1 && old_lines == 0, || {
                format!(
                    "{} `{}`: {new_lines} new and {old_lines} old doc lines",
                    edit.path, edit.function
                )
            });
        }
        let exact = self
            .expected
            .iter()
            .all(|file| contents.get(&file.path) == Some(&file.contents));
        let expected_status = self
            .expected
            .iter()
            .map(|file| format!(" M {}", file.path))
            .collect::<Vec<_>>()
            .join("\n");
        score.check(exact && status.trim_end() == expected_status, || {
            format!("final diff differs: exact_contents={exact} status={status:?}")
        });

        score
    }
}

/// Small varied notes followed by one oversized single-line file.
pub(super) struct HugeResultFixture {
    files: Vec<FixtureFile>,
}

impl HugeResultFixture {
    /// Generates the same notes on every call.
    pub(super) fn generate() -> Self {
        let mut rng = Lcg::new(0x5eed_0003);
        let mut files = Vec::new();
        for (index, code) in PART_CODES.iter().enumerate() {
            let line_count = 24 + rng.below(16);
            let mut lines = (0..line_count)
                .map(|_| sentence(&mut rng))
                .collect::<Vec<_>>();
            let position = 3 + rng.below(line_count - 6);
            lines.insert(position, format!("The code word of this part is {code}."));
            files.push(FixtureFile {
                contents: lines.join("\n") + "\n",
                path: format!("notes/part{}.txt", index + 1),
            });
        }
        let mut large = format!("The code word of the large file is {LARGE_CODE}.");
        while large.len() < LARGE_LINE_BYTES {
            large.push(' ');
            large.push_str(&sentence(&mut rng));
        }
        large.push('\n');
        files.push(FixtureFile {
            contents: large,
            path: "notes/large.txt".to_string(),
        });

        Self { files }
    }

    /// Returns the files to commit before the run.
    pub(super) fn files(&self) -> &[FixtureFile] {
        &self.files
    }

    /// Returns the task prompt.
    pub(super) fn prompt() -> String {
        format!(
            "Read notes/part1.txt through notes/part{count}.txt in order with the read tool, one \
             file per call and never several in one response, then read notes/large.txt. Each \
             part contains one sentence naming its code word, and large.txt names one at the \
             start of its only line. After reading them all, set answer to the {total} code words \
             in that order, separated by commas.",
            count = PART_CODES.len(),
            total = PART_CODES.len() + 1
        )
    }

    /// Scores one check per expected code word.
    pub(super) fn score(output: &Value) -> Score {
        let answer = output["answer"].as_str().unwrap_or_default().to_lowercase();
        let mut score = Score::default();
        for code in PART_CODES.iter().chain([&LARGE_CODE]) {
            score.check(answer.contains(code), || {
                format!("missing code word {code}")
            });
        }

        score
    }
}

/// Output schema with one free-form string answer.
pub(super) fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"],
        "additionalProperties": false
    })
}

/// One requested doc-comment replacement.
pub(super) struct DocEdit {
    pub(super) function: String,
    pub(super) new_line: String,
    pub(super) old_line: String,
    pub(super) path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Visibility {
    Crate,
    Private,
    Public,
}

impl Visibility {
    fn keyword(self) -> &'static str {
        match self {
            Self::Crate => "pub(crate) fn",
            Self::Private => "fn",
            Self::Public => "pub fn",
        }
    }
}

struct GeneratedFunction {
    doc: String,
    name: String,
    noun: String,
    verb: String,
    visibility: Visibility,
}

struct RustSource {
    contents: String,
    functions: Vec<GeneratedFunction>,
}

impl RustSource {
    /// Builds one module: a struct, an inherent `impl` holding about two
    /// thirds of the functions, a `Display` impl, then free functions.
    fn generate(rng: &mut Lcg, module: &str, public: usize, private: usize) -> Self {
        let type_name = format!("{}{}", capitalize(module), capitalize(rng.pick(&NOUNS)));
        let fields = [
            format!("{}_{}", rng.pick(&NOUNS), rng.pick(&VERBS)),
            format!("{}_count", rng.pick(&NOUNS)),
            format!("{}_by_{}", rng.pick(&NOUNS), rng.pick(&NOUNS)),
        ];
        let mut visibilities = vec![Visibility::Public; public];
        for index in 0..private {
            visibilities.push(if index % 2 == 0 {
                Visibility::Private
            } else {
                Visibility::Crate
            });
        }
        rng.shuffle(&mut visibilities);
        let mut used = BTreeSet::from(["fmt".to_string()]);
        let functions = visibilities
            .into_iter()
            .map(|visibility| {
                let (verb, noun) = loop {
                    let verb = rng.pick(&VERBS);
                    let noun = rng.pick(&NOUNS);
                    if used.insert(format!("{verb}_{noun}")) {
                        break (verb, noun);
                    }
                };
                GeneratedFunction {
                    doc: format!(
                        "/// {} the {noun} {}.",
                        capitalize(&third_person(verb)),
                        rng.pick(&QUALIFIERS)
                    ),
                    name: format!("{verb}_{noun}"),
                    noun: noun.to_string(),
                    verb: verb.to_string(),
                    visibility,
                }
            })
            .collect::<Vec<_>>();
        let methods = functions.len() * 2 / 3;
        let mut lines = vec![
            format!(
                "//! {} bookkeeping for the {} pipeline.",
                capitalize(module),
                rng.pick(&NOUNS)
            ),
            String::new(),
            "use std::collections::BTreeMap;".to_string(),
            "use std::fmt;".to_string(),
            String::new(),
            format!(
                "/// {} state tracked {}.",
                capitalize(rng.pick(&NOUNS)),
                rng.pick(&QUALIFIERS)
            ),
            "#[derive(Clone, Debug, Default)]".to_string(),
            format!("pub struct {type_name} {{"),
            format!("    {}: u64,", fields[0]),
            format!("    {}: Vec<u32>,", fields[1]),
            format!("    {}: BTreeMap<String, i64>,", fields[2]),
            "}".to_string(),
            String::new(),
            format!("impl {type_name} {{"),
        ];
        for (index, function) in functions[..methods].iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            Self::push_function(rng, &mut lines, function, "    ", Some(&fields[0]));
        }
        lines.extend([
            "}".to_string(),
            String::new(),
            format!("impl fmt::Display for {type_name} {{"),
            "    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {".to_string(),
            format!(
                "        write!(formatter, \"{module}:{{}}\", self.{})",
                fields[0]
            ),
            "    }".to_string(),
            "}".to_string(),
        ]);
        for function in &functions[methods..] {
            lines.push(String::new());
            Self::push_function(rng, &mut lines, function, "", None);
        }
        lines.push(String::new());

        Self {
            contents: lines.join("\n"),
            functions,
        }
    }

    fn public_functions(&self) -> Vec<&GeneratedFunction> {
        self.functions
            .iter()
            .filter(|function| function.visibility == Visibility::Public)
            .collect()
    }

    fn push_function(
        rng: &mut Lcg,
        lines: &mut Vec<String>,
        function: &GeneratedFunction,
        indent: &str,
        field: Option<&String>,
    ) {
        let argument = format!("{}_{}", rng.pick(&NOUNS), rng.pick(&["id", "size", "step"]));
        let receiver = if field.is_some() { "&self, " } else { "" };
        lines.push(format!("{indent}{}", function.doc));
        lines.push(format!(
            "{indent}{} {}({receiver}{argument}: u32) -> u64 {{",
            function.visibility.keyword(),
            function.name
        ));
        let mut previous = field.map_or_else(
            || format!("u64::from({argument})"),
            |field| format!("self.{field}.wrapping_add(u64::from({argument}))"),
        );
        for step in 0..2 + rng.below(4) {
            let variable = format!("{}_{step}", rng.pick(&NOUNS));
            let number = 3 + rng.below(997);
            let small = 2 + rng.below(9);
            let statement = match rng.below(6) {
                0 => format!("let {variable} = {previous}.wrapping_mul({number});"),
                1 => format!("let {variable} = {previous}.rotate_left({small}) ^ {number};"),
                2 => format!("let {variable} = {previous}.saturating_sub({number}).max({small});"),
                3 => format!(
                    "let {variable} = [{number}, {small}, {}].iter().sum::<u64>() + {previous};",
                    rng.below(500)
                ),
                4 => {
                    lines.push(format!(
                        "{indent}    // The {} {} stays {} here.",
                        rng.pick(&ADJECTIVES),
                        rng.pick(&NOUNS),
                        rng.pick(&QUALIFIERS)
                    ));
                    format!("let {variable} = {previous} % {number};")
                }
                _ => {
                    lines.push(format!("{indent}    if {previous} % {small} == 0 {{"));
                    lines.push(format!("{indent}        return {previous} / {small};"));
                    lines.push(format!("{indent}    }}"));
                    format!("let {variable} = {previous} + {number};")
                }
            };
            lines.push(format!("{indent}    {statement}"));
            previous = variable;
        }
        lines.push(format!("{indent}    {previous}"));
        lines.push(format!("{indent}}}"));
    }
}

/// Seeded linear congruential generator; quality only needs to avoid
/// visible repetition.
struct Lcg(u64);

impl Lcg {
    const INCREMENT: u64 = 1_442_695_040_888_963_407;
    const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT);

        usize::try_from(self.0 >> 33).unwrap_or(0) % bound
    }

    fn pick<'item>(&mut self, items: &[&'item str]) -> &'item str {
        items[self.below(items.len())]
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }
}

fn sentence(rng: &mut Lcg) -> String {
    let noun = rng.pick(&NOUNS);
    let other = rng.pick(&NOUNS);
    let number = 2 + rng.below(480);
    match rng.below(5) {
        0 => format!(
            "The {noun} crew {} {number} {other} crossings before {}.",
            rng.pick(&PAST_VERBS),
            rng.pick(&TIMES)
        ),
        1 => format!(
            "Inspectors near the {noun} gate {} {number} crates of {other} on day {}.",
            rng.pick(&PAST_VERBS),
            1 + rng.below(30)
        ),
        2 => format!(
            "A {} {noun} report listed {number} delays and {} {other} repairs.",
            rng.pick(&ADJECTIVES),
            rng.below(60)
        ),
        3 => format!(
            "By {} the {noun} station had {} {number} parcels for {other}.",
            rng.pick(&TIMES),
            rng.pick(&PAST_VERBS)
        ),
        _ => format!(
            "Nobody at the {} {noun} desk expected {number} {other} tickets this week.",
            rng.pick(&ADJECTIVES)
        ),
    }
}

fn normalize_path(path: &str) -> String {
    path.trim().trim_start_matches("./").to_string()
}

fn third_person(verb: &str) -> String {
    if verb.ends_with(['h', 's', 'x']) {
        format!("{verb}es")
    } else {
        format!("{verb}s")
    }
}

fn capitalize(word: &str) -> String {
    let mut characters = word.chars();
    characters
        .next()
        .map_or_default(|first| first.to_uppercase().chain(characters).collect())
}
