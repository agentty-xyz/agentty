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
import subprocess
import sys

tool = Path(sys.argv[0]).name
if tool == "uname":
    print(os.environ.get("STUB_UNAME", "Linux"))
    sys.exit(0)
report = Path("coverage.lcov")
with Path("calls.jsonl").open("a", encoding="utf-8") as calls:
    calls.write(json.dumps({
        "tool": tool,
        "args": sys.argv[1:],
        "report": report.read_text() if report.exists() else None,
        "gif_mode": os.environ.get("TESTTY_GIF_MODE"),
        "coverage_base": os.environ.get("AGENTTY_COVERAGE_BASE"),
        "rustflags": os.environ.get("RUSTFLAGS"),
    }) + "\\n")
if tool == "cargo":
    status = int(os.environ.get("STUB_CARGO_EXIT", "0"))
    if not status and sys.argv[1:3] == ["llvm-cov", "nextest"]:
        mode = os.environ.get("STUB_REPORT", "fresh")
        if mode != "missing":
            report.write_text("fresh report" if mode == "fresh" else "")
elif tool == "diff-cover":
    status = int(os.environ.get("STUB_DIFF_EXIT", "0"))
elif tool in ("sudo", "bwrap"):
    status = 47 if os.environ.get("STUB_NATIVE_FAILURE") in sys.argv[1:] else 0
elif tool == "timeout":
    status = subprocess.call(sys.argv[2:])
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
        for name in ("cargo", "diff-cover", "prek", "uname", "sudo", "bwrap", "timeout"):
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

        result = self.run_hook("coverage", AGENTTY_TEST_FILTER="none()", RUSTFLAGS="-C debuginfo=1")

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
        self.assertTrue({"sandbox", "public_api", "lifecycle", "telemetry", "benchmark_summary"} <= cli_targets)
        self.assertEqual(
            generation["rustflags"],
            "-C debuginfo=1 -C llvm-args=-runtime-counter-relocation",
        )
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

    def test_macos_coverage_aligns_all_continuous_profile_sections(self):
        # Arrange / Act
        result = self.run_hook("coverage", STUB_UNAME="Darwin", RUSTFLAGS="-C debuginfo=1")

        # Assert
        self.assertEqual(result.returncode, 0, result.stderr)
        flags = self.calls()[0]["rustflags"]
        self.assertTrue(flags.startswith("-C debuginfo=1 -C llvm-args=-runtime-counter-relocation"))
        for section in ("cnts", "bits", "data"):
            self.assertIn(f"-C link-arg=-Wl,-sectalign,__DATA,__llvm_prf_{section},0x4000", flags)

    def test_generation_failure_cannot_reuse_an_old_report(self):
        report = self.directory / "coverage.lcov"
        report.write_text("stale report")

        result = self.run_hook("coverage", STUB_CARGO_EXIT="12")

        self.assertEqual(result.returncode, 12)
        self.assertEqual(len(self.calls()), 1)
        self.assertFalse(report.exists())

    def test_native_coverage_includes_the_instrumented_sandbox_target(self):
        # Arrange / Act
        result = self.run_hook("coverage-ag-harness-sandbox", AGENTTY_TEST_FILTER="none()", RUSTFLAGS="")

        # Assert
        self.assertEqual(result.returncode, 0, result.stderr)
        generation, = self.calls()
        args = generation["args"]
        self.assertEqual(args[:2], ["llvm-cov", "nextest"])
        self.assertEqual(args[args.index("-p") + 1], "ag-harness")
        self.assertEqual(args[args.index("--test") + 1], "sandbox")
        self.assertIn("--lib", args)
        targets = {args[index + 1] for index, argument in enumerate(args) if argument == "--test"}
        self.assertEqual(targets, {"sandbox", "public_api", "lifecycle", "telemetry", "benchmark_summary"})
        self.assertIn("--lcov", args)
        self.assertNotIn("-E", args)
        self.assertIn("-runtime-counter-relocation", generation["rustflags"])

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

    def test_ci_installs_native_isolation_before_coverage(self):
        # Arrange
        action = yaml.safe_load(
            (ROOT / ".github/actions/run-coverage/action.yml").read_text(encoding="utf-8")
        )
        steps = action["runs"]["steps"]

        # Act
        installation = next(
            step for step in steps if step.get("name") == "Install native sandbox coverage dependency"
        )

        # Assert
        self.assertEqual(installation["if"], "runner.os == 'Linux'")
        self.assertEqual(installation["uses"], "$/.github/actions/setup-native-sandbox")
        self.assertLess(steps.index(installation), steps.index(self.coverage_step()))

    def test_native_setup_precedes_every_linux_sandbox_suite(self):
        # Arrange
        cases = (
            ("harness-sandbox.yml", "native", "runner.os == 'Linux'",
             "Native sandbox public contract and launcher coverage"),
            ("workspace-validation.yml", "validate", "${{ inputs['workspace-tests'] }}",
             "Full test suite"),
        )

        # Act / Assert
        for filename, job, condition, suite in cases:
            with self.subTest(workflow=filename):
                workflow = yaml.safe_load(
                    (ROOT / ".github/workflows" / filename).read_text(encoding="utf-8")
                )
                steps = workflow["jobs"][job]["steps"]
                setup = next(step for step in steps if step.get("uses") ==
                             "$/.github/actions/setup-native-sandbox")
                tests = next(step for step in steps if step.get("name") == suite)
                self.assertEqual(setup["if"], condition)
                self.assertLess(steps.index(setup), steps.index(tests))

    def native_setup_command(self):
        action = yaml.safe_load(
            (ROOT / ".github/actions/setup-native-sandbox/action.yml").read_text(encoding="utf-8")
        )
        script = "\n".join(step["run"] for step in action["runs"]["steps"])
        return ["bash", "-eu", "-o", "pipefail", "-c", script + "\nprek run coverage"]

    def test_native_setup_loads_scoped_policy_then_probes_without_sudo(self):
        # Arrange
        action_path = self.directory / "action with spaces"

        # Act
        result = self.run_command(
            self.native_setup_command(), NATIVE_SANDBOX_ACTION_PATH=str(action_path)
        )

        # Assert
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.calls()
        self.assertEqual([call["tool"] for call in calls],
                         ["sudo", "sudo", "sudo", "bwrap", "timeout", "bwrap", "prek"])
        self.assertEqual(calls[1]["args"], ["apt-get", "install", "--yes", "bubblewrap", "apparmor"])
        self.assertEqual(calls[2]["args"],
                         ["apparmor_parser", "--replace", "--skip-cache", str(action_path / "bwrap.apparmor")])
        self.assertEqual(calls[4]["args"][:2], ["10s", "bwrap"])
        self.assertEqual(calls[5]["args"][:7],
                         ["--unshare-all", "--uid", "0", "--gid", "0", "--cap-drop", "ALL"])
        self.assertIn("CapEff:", calls[5]["args"][-1])
        self.assertIn("unpriv_bwrap /proc/self/attr/current", calls[5]["args"][-1])

    def test_native_setup_failure_prevents_test_execution(self):
        # Arrange
        for failure in ("update", "install", "apparmor_parser", "--version", "--unshare-all"):
            with self.subTest(failure=failure):
                (self.directory / "calls.jsonl").write_text("")

                # Act
                result = self.run_command(
                    self.native_setup_command(), NATIVE_SANDBOX_ACTION_PATH="fixture",
                    STUB_NATIVE_FAILURE=failure,
                )

                # Assert
                self.assertEqual(result.returncode, 47, result.stderr)
                self.assertNotIn("prek", [call["tool"] for call in self.calls()])

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
