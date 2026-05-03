//! Hardcoded QE recommendation registry.
//!
//! Adding or removing a recommendation is a single-source edit to the
//! [`REGISTRY`] slice plus its associated acceptance function. Each entry
//! must carry a stable `id`, human-readable `title`, single-sentence
//! `rationale`, single-sentence `remediation`, and a pure acceptance closure
//! that evaluates the probe against the worktree path.

use std::path::{Path, PathBuf};

use super::types::{QeAcceptance, QeCheckSpec};
use crate::infra::qe::QeProbe;

const AGENTS_MD_FILENAME: &str = "AGENTS.md";

/// Hardcoded list of recommendations evaluated on every `/qe:check`
/// invocation. The order in this slice is the order of the rendered report.
pub const REGISTRY: &[QeCheckSpec] = &[
    QeCheckSpec {
        id: "root_agents_md",
        title: "Root AGENTS.md exists",
        rationale: "AGENTS.md is the vendor-neutral entry point every agent reads first.",
        remediation: "Create AGENTS.md at the repo root describing project purpose, mandatory \
                      rules, and entry points.",
        acceptance: check_root_agents_md as QeAcceptance,
    },
    QeCheckSpec {
        id: "agents_md_symlinks",
        title: "CLAUDE.md and GEMINI.md symlinks point to AGENTS.md",
        rationale: "Symlinking vendor-specific filenames to one source keeps agent guidance from \
                    drifting between near-identical copies.",
        remediation: "Run `ln -s AGENTS.md CLAUDE.md && ln -s AGENTS.md GEMINI.md` at the repo \
                      root.",
        acceptance: check_agents_md_symlinks as QeAcceptance,
    },
    QeCheckSpec {
        id: "gitignore_excludes_agentty",
        title: ".gitignore excludes .agentty/",
        rationale: "Agentty creates per-session scratch under `.agentty/`; without ignore \
                    coverage these files risk accidental commits.",
        remediation: "Add `.agentty/` to .gitignore at the repo root.",
        acceptance: check_gitignore_excludes_agentty as QeAcceptance,
    },
    QeCheckSpec {
        id: "readme_present",
        title: "README.md exists at root",
        rationale: "Both humans and agents use README.md as the first orientation pass on an \
                    unfamiliar repo.",
        remediation: "Add README.md covering project purpose, prerequisites, and a pointer to \
                      AGENTS.md.",
        acceptance: check_readme_present as QeAcceptance,
    },
    QeCheckSpec {
        id: "roadmap_present",
        title: "docs/plan/roadmap.md exists with the standard sections",
        rationale: "A roadmap with `Ready Now` / `Queued Next` / `Parked` sections lets agents \
                    pull the next task without re-deriving priorities.",
        remediation: "Create docs/plan/roadmap.md with `Ready Now`, `Queued Next`, and `Parked` \
                      headings.",
        acceptance: check_roadmap_present as QeAcceptance,
    },
    QeCheckSpec {
        id: "pre_commit_config_present",
        title: ".pre-commit-config.yaml exists",
        rationale: "Agents need an executable hook catalog to run before finalizing each turn; \
                    the YAML is the single source of truth for quality gates.",
        remediation: "Initialize .pre-commit-config.yaml with at minimum one formatter and one \
                      validator covering the project's languages.",
        acceptance: check_pre_commit_config_present as QeAcceptance,
    },
];

/// Returns whether `<worktree>/AGENTS.md` is a regular file.
fn check_root_agents_md(probe: &dyn QeProbe, worktree: &Path) -> bool {
    probe.is_file(worktree.join(AGENTS_MD_FILENAME))
}

/// Returns whether both `CLAUDE.md` and `GEMINI.md` at the worktree root
/// are symlinks whose literal target is `AGENTS.md`.
fn check_agents_md_symlinks(probe: &dyn QeProbe, worktree: &Path) -> bool {
    let expected_target = PathBuf::from(AGENTS_MD_FILENAME);
    let claude_ok = probe
        .read_link(worktree.join("CLAUDE.md"))
        .is_some_and(|target| target == expected_target);
    let gemini_ok = probe
        .read_link(worktree.join("GEMINI.md"))
        .is_some_and(|target| target == expected_target);

    claude_ok && gemini_ok
}

/// Returns whether `<worktree>/.gitignore` excludes the `.agentty/` scratch
/// directory.
///
/// Accepts the common anchored and unanchored variants
/// (`.agentty`, `.agentty/`, `/.agentty`, `/.agentty/`) and treats a later
/// matching `!` negation as cancelling the exclusion. Trailing whitespace is
/// trimmed; leading whitespace is preserved because gitignore treats it as
/// part of the pattern. Comment lines (`#`-prefixed) and blank lines are
/// ignored, matching gitignore semantics for the patterns this check cares
/// about.
fn check_gitignore_excludes_agentty(probe: &dyn QeProbe, worktree: &Path) -> bool {
    let Some(content) = probe.read_to_string(worktree.join(".gitignore")) else {
        return false;
    };

    let is_match = |pattern: &str| {
        matches!(
            pattern,
            ".agentty" | ".agentty/" | "/.agentty" | "/.agentty/",
        )
    };

    let mut excluded = false;
    for raw_line in content.lines() {
        let line = raw_line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(negated) = line.strip_prefix('!') {
            if is_match(negated) {
                excluded = false;
            }
        } else if is_match(line) {
            excluded = true;
        }
    }

    excluded
}

/// Returns whether `<worktree>/README.md` is a regular file.
fn check_readme_present(probe: &dyn QeProbe, worktree: &Path) -> bool {
    probe.is_file(worktree.join("README.md"))
}

/// Returns whether `<worktree>/docs/plan/roadmap.md` exists and contains
/// the three standard `## Ready Now`, `## Queued Next`, and `## Parked`
/// headings used by the implementation-plan skill. Heading lines are
/// matched exactly so prose mentions of those phrases do not satisfy the
/// recommendation.
fn check_roadmap_present(probe: &dyn QeProbe, worktree: &Path) -> bool {
    let roadmap_path = worktree.join("docs/plan/roadmap.md");
    if !probe.is_file(roadmap_path.clone()) {
        return false;
    }
    let Some(content) = probe.read_to_string(roadmap_path) else {
        return false;
    };

    let mut headings = content.lines().map(str::trim_end);
    let has_ready_now = headings.clone().any(|line| line == "## Ready Now");
    let has_queued_next = headings.clone().any(|line| line == "## Queued Next");
    let has_parked = headings.any(|line| line == "## Parked");

    has_ready_now && has_queued_next && has_parked
}

/// Returns whether `<worktree>/.pre-commit-config.yaml` is a regular file.
fn check_pre_commit_config_present(probe: &dyn QeProbe, worktree: &Path) -> bool {
    probe.is_file(worktree.join(".pre-commit-config.yaml"))
}
