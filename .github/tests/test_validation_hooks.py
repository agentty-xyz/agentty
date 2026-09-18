"""Exercise validation hook entries with isolated subprocess fixtures."""

import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
CONFIG = yaml.safe_load((ROOT / ".pre-commit-config.yaml").read_text(encoding="utf-8"))
HOOKS = {
    hook["id"]: hook
    for repository in CONFIG["repos"]
    if repository["repo"] == "local"
    for hook in repository["hooks"]
}
STUB = """#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

tool = Path(sys.argv[0]).name
report = Path("coverage.lcov")
with Path("calls.jsonl").open("a", encoding="utf-8") as calls:
    calls.write(json.dumps({
        "tool": tool,
        "args": sys.argv[1:],
        "report": report.read_text() if report.exists() else None,
        "gif_mode": os.environ.get("TESTTY_GIF_MODE"),
        "coverage_base": os.environ.get("AGENTTY_COVERAGE_BASE"),
    }) + "\\n")
if tool == "cargo":
    status = int(os.environ.get("STUB_CARGO_EXIT", "0"))
    if not status and sys.argv[1:3] == ["llvm-cov", "nextest"]:
        mode = os.environ.get("STUB_REPORT", "fresh")
        if mode != "missing":
            report.write_text("fresh report" if mode == "fresh" else "")
elif tool == "diff-cover":
    status = int(os.environ.get("STUB_DIFF_EXIT", "0"))
else:
    status = int(os.environ.get("STUB_PREK_EXIT", "0"))
sys.exit(status)
"""


class ValidationHookTests(unittest.TestCase):
    """Check command ordering, failures, and selection at the process boundary."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="validation-hooks-")
        self.addCleanup(directory.cleanup)
        self.directory = Path(directory.name)
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        for name in ("cargo", "diff-cover", "prek"):
            command = self.bin / name
            command.write_text(STUB, encoding="utf-8")
            command.chmod(0o755)

    def run_hook(self, hook, **overrides):
        return self.run_command(shlex.split(HOOKS[hook]["entry"]), **overrides)

    def run_command(self, command, **overrides):
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(
                ("AGENTTY_TEST_", "AGENTTY_COVERAGE_", "AGENTTY_MERGE_GROUP_", "STUB_")
            )
        }
        environment.update(overrides)
        environment["PATH"] = str(self.bin) + os.pathsep + os.environ["PATH"]
        return subprocess.run(
            command,
            cwd=self.directory,
            env=environment,
            capture_output=True,
            text=True,
            timeout=10,
            check=False,
        )

    def calls(self):
        path = self.directory / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()]

    def e2e_exclusion(self):
        """Derive the expected exclusion from the separately executed E2E targets."""
        arguments = shlex.split(HOOKS["test-agentty-e2e"]["entry"])
        targets = [
            arguments[index + 1]
            for index, argument in enumerate(arguments)
            if argument == "--test"
        ]
        self.assertTrue(targets)
        binaries = " or ".join(f"binary(={target})" for target in targets)
        return f"not (package(=agentty) and kind(test) and ({binaries}))"

    def coverage_step(self):
        action = yaml.safe_load(
            (ROOT / ".github/actions/run-coverage/action.yml").read_text(encoding="utf-8")
        )
        generation, = (
            step for step in action["runs"]["steps"]
            if step.get("name") == "Generate and check coverage report"
        )
        return generation

    def test_prompt_evaluation_is_explicit_and_runs_only_live_cases(self):
        result = self.run_hook("prompt-evaluation")

        self.assertEqual(result.returncode, 0, result.stderr)
        call, = self.calls()
        self.assertEqual(call["args"], [
            "test", "--locked", "-p", "agentty", "--test", "prompt_evaluation",
            "live_prompt_evaluation", "--", "--ignored", "--exact", "--nocapture",
        ])
        self.assertEqual(HOOKS["prompt-evaluation"]["stages"], ["manual"])

    def test_coverage_generates_once_before_checking_all_thresholds(self):
        (self.directory / "coverage.lcov").write_text("stale report")

        result = self.run_hook("coverage", AGENTTY_TEST_FILTER="none()")

        self.assertEqual(result.returncode, 0, result.stderr)
        generation, comparison = self.calls()
        self.assertEqual(generation["tool"], "cargo")
        self.assertIsNone(generation["report"])
        args = generation["args"]
        self.assertEqual(args[:2], ["llvm-cov", "nextest"])
        self.assertEqual(args[args.index("--fail-under-lines") + 1], "93")
        self.assertEqual(args[args.index("--fail-under-functions") + 1], "91")
        for flag in ("--workspace", "--lib", "--bins", "--examples", "--locked"):
            self.assertIn(flag, args)
        cli_targets = {
            args[index + 1]
            for index, argument in enumerate(args)
            if argument == "--test"
        }
        utility_suites = {
            path.stem for path in (ROOT / "crates/ag-xtask/tests").glob("*.rs")
        }
        self.assertTrue(utility_suites <= cli_targets)
        self.assertNotIn("-E", args)
        self.assertEqual(comparison["tool"], "diff-cover")
        self.assertEqual(comparison["report"], "fresh report")
        for flag in (
            "--compare-branch=main",
            "--fail-under=100",
            "--include-untracked",
            "--show-uncovered",
        ):
            self.assertIn(flag, comparison["args"])

    def test_generation_failure_cannot_reuse_an_old_report(self):
        report = self.directory / "coverage.lcov"
        report.write_text("stale report")

        result = self.run_hook("coverage", STUB_CARGO_EXIT="12")

        self.assertEqual(result.returncode, 12)
        self.assertEqual(len(self.calls()), 1)
        self.assertFalse(report.exists())

    def test_generation_must_produce_a_nonempty_report(self):
        for mode in ("missing", "empty"):
            with self.subTest(mode=mode):
                (self.directory / "coverage.lcov").write_text("stale report")
                (self.directory / "calls.jsonl").write_text("")

                result = self.run_hook("coverage", STUB_REPORT=mode)

                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(self.calls()), 1)

    def test_diff_failure_fails_the_combined_gate(self):
        result = self.run_hook("coverage", STUB_DIFF_EXIT="2")

        self.assertEqual(result.returncode, 2)
        self.assertEqual(len(self.calls()), 2)

    def test_base_ref_is_passed_literally(self):
        base = "origin/release $(touch injected)"

        result = self.run_hook("coverage", AGENTTY_COVERAGE_BASE=base)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--compare-branch=" + base, self.calls()[1]["args"])
        self.assertFalse((self.directory / "injected").exists())

    def test_focus_requires_an_explicit_nonempty_filter(self):
        for environment in ({}, {"AGENTTY_TEST_FILTER": ""}):
            with self.subTest(environment=environment):
                result = self.run_hook("test-focused", **environment)

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("AGENTTY_TEST_FILTER", result.stderr)
                self.assertFalse((self.directory / "calls.jsonl").exists())

    def test_focus_filter_is_one_literal_argument_and_retains_integration_tests(self):
        selection = "deps(=ag-git) or rdeps(=ag-git) or test($(touch injected))"

        result = self.run_hook("test-focused", AGENTTY_TEST_FILTER=selection)

        self.assertEqual(result.returncode, 0, result.stderr)
        call, = self.calls()
        args = call["args"]
        self.assertEqual(args[:2], ["nextest", "run"])
        self.assertIn("--workspace", args)
        self.assertIn("--no-tests=fail", args)
        self.assertEqual(
            args[args.index("-E") + 1],
            f"({selection}) and {self.e2e_exclusion()}",
        )
        self.assertNotIn("--lib", args)
        self.assertNotIn("--bins", args)
        self.assertFalse((self.directory / "injected").exists())

    def test_focus_propagates_runner_errors(self):
        result = self.run_hook(
            "test-focused", AGENTTY_TEST_FILTER="none()", STUB_CARGO_EXIT="4"
        )

        self.assertEqual(result.returncode, 4)

    def test_full_gates_cannot_be_narrowed_by_the_focus_filter(self):
        workspace = self.run_hook("test-workspace", AGENTTY_TEST_FILTER="none()")
        e2e = self.run_hook("test-agentty-e2e", AGENTTY_TEST_FILTER="none()")

        self.assertEqual(workspace.returncode, 0, workspace.stderr)
        self.assertEqual(e2e.returncode, 0, e2e.stderr)
        source, integration = self.calls()
        self.assertIn("--workspace", source["args"])
        self.assertNotIn("--lib", source["args"])
        self.assertNotIn("none()", " ".join(source["args"]))
        self.assertEqual(
            source["args"][source["args"].index("-E") + 1], self.e2e_exclusion()
        )
        self.assertNotIn("-E", integration["args"])
        for target in ("showcase", "protocol_compliance_e2e", "e2e"):
            self.assertIn(target, integration["args"])
        self.assertEqual(integration["gif_mode"], "check")

    def test_ci_supplies_event_base_refs_through_environment(self):
        generation = self.coverage_step()

        self.assertEqual(
            generation["env"],
            {
                "AGENTTY_MERGE_GROUP_BASE_REF": "${{ github.event.merge_group.base_ref }}",
                "AGENTTY_COVERAGE_BASE_BRANCH": (
                    "${{ github.base_ref || github.event.repository.default_branch }}"
                ),
            },
        )

    def test_ci_selects_the_merge_group_or_pull_request_or_default_base(self):
        generation = self.coverage_step()
        cases = (
            ("refs/heads/release/1.0", "main", "origin/release/1.0"),
            ("refs/heads/main", "release/1.0", "origin/main"),
            ("", "release/2.0", "origin/release/2.0"),
            ("", "main", "origin/main"),
            ("refs/heads/$(touch injected)", "main", "origin/$(touch injected)"),
        )
        for merge_group_ref, fallback_branch, expected in cases:
            with self.subTest(merge_group_ref=merge_group_ref, fallback=fallback_branch):
                (self.directory / "calls.jsonl").write_text("")

                result = self.run_command(
                    ["bash", "-eu", "-o", "pipefail", "-c", generation["run"]],
                    AGENTTY_MERGE_GROUP_BASE_REF=merge_group_ref,
                    AGENTTY_COVERAGE_BASE_BRANCH=fallback_branch,
                )

                self.assertEqual(result.returncode, 0, result.stderr)
                call, = self.calls()
                self.assertEqual(call["tool"], "prek")
                self.assertEqual(
                    call["args"], ["run", "coverage", "--all-files", "--hook-stage", "manual"]
                )
                self.assertEqual(call["coverage_base"], expected)
                self.assertFalse((self.directory / "injected").exists())

    def test_ci_propagates_coverage_failure(self):
        result = self.run_command(
            ["bash", "-eu", "-o", "pipefail", "-c", self.coverage_step()["run"]],
            AGENTTY_MERGE_GROUP_BASE_REF="refs/heads/release/1.0",
            AGENTTY_COVERAGE_BASE_BRANCH="main",
            STUB_PREK_EXIT="12",
        )

        self.assertEqual(result.returncode, 12)


if __name__ == "__main__":
    unittest.main()
