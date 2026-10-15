"""Exercise GNU Make + real subprocess boundaries without compiling the workspace."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
MAKE = shutil.which("make")

FAKE_TOOL = r'''
import json, os, pathlib, sys
name, args = sys.argv[1], sys.argv[2:]
root = pathlib.Path.cwd()
with open(os.environ["CF_FAKE_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps({"tool":name,"args":args,"cwd":str(root)}) + "\n")
if "--version" in args:
    versions = {"cargo-nextest":"0.9.146", "cargo-deny":"0.20.2", "cargo-gears":"0.0.8"}
    if name == "cargo-nextest" and os.environ.get("CF_FAKE_NEXTEST_VERSION"):
        versions[name] = os.environ["CF_FAKE_NEXTEST_VERSION"]
    print(name, versions.get(name, "1.98.1"))
elif name == "rustup":
    print("rustfmt-x86_64-unknown-linux-gnu\nclippy-x86_64-unknown-linux-gnu")
elif args[:4] == ["gears", "ls", "packages", "--path"]:
    if os.environ.get("CF_FAKE_SCOPE_FAIL"):
        sys.exit(42)
    print(os.environ.get("CF_FAKE_SCOPE", "foo foo-sdk"))
elif args[:2] == ["nextest", "run"]:
    if not os.environ.get("CF_FAKE_NO_REPORT"):
        profile = args[args.index("--profile")+1]
        report = root / "target/nextest" / profile / "junit.xml"
        report.parent.mkdir(parents=True, exist_ok=True)
        report.write_text('<testsuites><testsuite name="fixture" tests="1"><testcase name="fixture"/></testsuite></testsuites>')
    sys.exit(int(os.environ.get("CF_FAKE_TEST_EXIT", "0")))
'''


@unittest.skipUnless(MAKE, "GNU Make is required (CI runs this on all three OSes)")
class MakeIncludes(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gears make ")
        self.addCleanup(self.temp.cleanup)
        self.project = Path(self.temp.name)
        shutil.copytree(ROOT / "tools/ci", self.project / "tools/ci", ignore=shutil.ignore_patterns("__pycache__", "tests"))
        scripts = self.project / "tools/scripts"
        scripts.mkdir(parents=True)
        for script in ("make_help.py", "junit_enrich.py"):
            shutil.copyfile(ROOT / "tools/scripts" / script, scripts / script)
        manifest = self.project / "apps/cf-gears-example-server/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text("[features]\nfoo = []\n", encoding="utf-8")
        self.bin = self.project / "fake-bin"
        self.bin.mkdir()
        (self.bin / "fake_tool.py").write_text(FAKE_TOOL, encoding="utf-8")
        for tool in ("cargo", "cargo-nextest", "cargo-gears", "cargo-deny", "cargo-hack", "rustup"):
            wrapper = self.bin / tool
            wrapper.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{self.bin / "fake_tool.py"}" {tool} "$@"\n', encoding="utf-8")
            wrapper.chmod(0o755)
            (self.bin / f"{tool}.cmd").write_text(f'@"{sys.executable}" "%~dp0fake_tool.py" {tool} %*\n', encoding="utf-8")
        self.log = self.project / "commands.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"], CF_FAKE_LOG=str(self.log))
        self.env.pop("GITHUB_ACTIONS", None)
        self.env.pop("GITHUB_OUTPUT", None)
        self.env.pop("GITHUB_STEP_SUMMARY", None)
        self.env.pop("CARGO_TARGET_DIR", None)
        self.write_makefile()

    def write_makefile(self, after=""):
        # Use the current interpreter, including on Windows with Git Bash recipes.
        python = Path(sys.executable).as_posix()
        (self.project / "Makefile").write_text(
            f'CF_PYTHON := "{python}"\n' + (ROOT / "Makefile").read_text(encoding="utf-8") + "\n" + after,
            encoding="utf-8",
        )

    def make(self, *arguments, **environment):
        return subprocess.run([MAKE, "--no-print-directory", *arguments], cwd=self.project,
                              env=dict(self.env, **environment), text=True, capture_output=True)

    def commands(self):
        return [json.loads(line) for line in self.log.read_text(encoding="utf-8").splitlines()] if self.log.exists() else []

    def test_root_ci_runs_required_checks_and_added_prerequisites(self):
        self.write_makefile('project-check:\n\t@echo project-check\nci: tail-check\ntail-check:\n\t@echo tail-check\n')
        result = self.make("ci", "CI_EXTRA_TARGETS=project-check")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        args = [x["args"] for x in self.commands() if x["tool"] == "cargo"]
        for command in ("fmt", "clippy", "nextest", "deny", "gears"):
            self.assertTrue(any(a[0] == command for a in args), (command, args))
        self.assertTrue(any(a[:2] == ["hack", "clippy"] for a in args), args)
        self.assertIn("project-check", result.stdout)
        self.assertIn("tail-check", result.stdout)
        self.assertTrue(all(Path(x["cwd"]) == self.project for x in self.commands()))
        self.assertFalse(any("install" in a for a in args))

    def test_help_and_dry_run_do_not_invoke_cargo_or_install(self):
        for arguments in (("help",), ("-n", "ci", "GEAR=missing")):
            result = self.make(*arguments)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.commands(), [])
        self.assertFalse((self.project / ".venv").exists())

    def test_explicit_package_features_flags_and_profile(self):
        result = self.make("test", "CF_PACKAGES=foo foo-sdk", "GEAR_FEATURES=integration", "GEAR_TEST_ARGS=--retries 1", "NEXTEST_PROFILE=ci")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        args = next(x["args"] for x in self.commands() if x["tool"] == "cargo")
        self.assertIn("integration", args)
        self.assertIn("foo-sdk", args)
        self.assertIn("ci", args)
        self.assertIn("--retries", args)
        self.assertNotIn("--workspace", args)

    def test_scoped_test_uses_reverse_dependencies(self):
        result = self.make("test", "GEAR=foo")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = [x["args"] for x in self.commands() if x["tool"] == "cargo"]
        self.assertIn("--include-rdeps", commands[0])
        self.assertIn("foo-sdk", commands[1])
        self.assertNotIn("--workspace", commands[1])

    def test_test_arguments_cannot_bypass_gear_selection(self):
        result = self.make("test", "GEAR=foo", "GEAR_TEST_ARGS=-- --package")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        commands = [x["args"] for x in self.commands() if x["tool"] == "cargo"]
        self.assertEqual(commands[0][:3], ["gears", "ls", "packages"])
        self.assertIn("foo-sdk", commands[1][:commands[1].index("--")])

    def test_empty_or_failed_scope_never_runs_an_unscoped_check(self):
        for variable in ({"CF_FAKE_SCOPE": ""}, {"CF_FAKE_SCOPE_FAIL": "1"}):
            self.log.unlink(missing_ok=True)
            result = self.make("clippy", "GEAR=foo", **variable)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(any(x["args"] and x["args"][0] == "clippy" for x in self.commands()))

    def test_missing_tool_fails_without_installing(self):
        # A PATH containing only the fake tools ensures no developer's deny is found.
        for suffix in ("", ".cmd"):
            (self.bin / f"cargo-deny{suffix}").unlink()
        result = self.make("deny", PATH=str(self.bin))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any("install" in x["args"] for x in self.commands()))

    def test_ci_rejects_a_different_tool_version_without_installing(self):
        result = self.make("test", GITHUB_ACTIONS="true", CF_FAKE_NEXTEST_VERSION="0.9.147")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CI requires cargo-nextest 0.9.146", result.stderr)
        self.assertFalse(any(x["tool"] == "cargo" for x in self.commands()))

    def test_report_is_saved_on_failure_and_stale_report_is_removed(self):
        result = self.make("test", "JUNIT_DIR=reports", CF_FAKE_TEST_EXIT="17")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Error 17", result.stderr)
        self.assertTrue((self.project / "reports/test.xml").is_file())
        result = self.make("test", "JUNIT_DIR=reports", CF_FAKE_NO_REPORT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no JUnit report", result.stdout)
        self.assertFalse((self.project / "target/nextest/default/junit.xml").exists())
        self.assertFalse((self.project / "reports/test.xml").exists())

    def test_versions_target_needs_no_cargo(self):
        result = self.make("ci-tool-versions")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("nextest=0.9.146", result.stdout)
        self.assertEqual(self.commands(), [])


if __name__ == "__main__":
    unittest.main()
