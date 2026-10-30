"""Check the make help script, the repository make files and the gears.mk library contract."""
from contextlib import redirect_stderr, redirect_stdout
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import types
import unittest

ROOT = Path(__file__).resolve().parents[3]
GEARS_MK = ROOT / ".gears-dna" / "gears.mk"
MAKE = shutil.which("make")


def load_help_module():
    """Load the help script that gears.mk embeds between `define gears_help_py` and `endef`."""
    text = GEARS_MK.read_text(encoding="utf-8")
    body = text.split("define gears_help_py\n", 1)[1].split("\nendef\n", 1)[0]
    module = types.ModuleType("make_help")
    sys.modules["make_help"] = module  # dataclasses look the module up
    exec(compile(body, "gears.mk (gears_help_py)", "exec"), module.__dict__)
    return module


help_module = load_help_module()


def clean_env():
    """Environment without an outer make's flags and command-line variables.

    Also drops the gears.mk settings that CI exports (NEXTEST_PROFILE=ci,
    JUNIT_DIR=...), so the tests see the gears.mk defaults.
    """
    env = dict(os.environ)
    for name in ("MAKEFLAGS", "MFLAGS", "MAKELEVEL", "MAKEOVERRIDES", "GNUMAKEFLAGS",
                 "NEXTEST_PROFILE", "JUNIT_DIR"):
        env.pop(name, None)
    return env


class MakeHelpTests(unittest.TestCase):
    def test_help_lists_all_modules_in_order_without_duplicate_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            first, second = root / "first.mk", root / "second.mk"
            first.write_text("# -------- Build --------\n# Build the server.\nbuild:\n", encoding="utf-8")
            second.write_text("# -------- Tests --------\n# Run tests.\ntest:\n", encoding="utf-8")
            output = io.StringIO()
            with redirect_stdout(output):
                help_module.generate_help(first, second, first)
            text = output.getvalue()
            self.assertLess(text.index("Build:"), text.index("Tests:"))
            self.assertEqual(text.count("Build the server."), 1)
            self.assertIn("Run tests.", text)

    def test_existing_single_file_interface_still_works(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Makefile"
            path.write_text("# Run checks.\ncheck:\n", encoding="utf-8")
            output = io.StringIO()
            with redirect_stdout(output):
                help_module.main(["help", str(path)])
            self.assertIn("Run checks.", output.getvalue())

    def test_help_skips_define_blocks(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Makefile"
            path.write_text("# A macro.\ndefine macro\n# Inside the macro.\nelse:\nendef\n# Real target.\nreal:\n",
                            encoding="utf-8")
            output = io.StringIO()
            with redirect_stdout(output):
                help_module.generate_help(path, params=[])
            text = output.getvalue()
            self.assertIn("real", text)
            self.assertNotIn("else", text)

    def test_target_help_prints_the_whole_comment_block_and_its_parameters(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Makefile"
            path.write_text("# Run checks.\n## Details on a second line.\ncheck:\n\n# Build.\nbuild:\n", encoding="utf-8")
            params = help_module.parse_params(
                "MODE=<mode>\tcheck build\tHow to run.\ta b\n"
                "ONLY=1\tcheck\tFor check only.\t\n"
                "OTHER=1\tbuild\tFor build only.\t\n")
            output = io.StringIO()
            with redirect_stdout(output):
                code = help_module.generate_help(path, target="check", params=params)
            text = output.getvalue()
            self.assertEqual(code, 0)
            self.assertIn("Details on a second line.", text)
            self.assertNotIn("For build only.", text)
            self.assertIn("Values: a b", text)
            self.assertLess(text.index("ONLY=1"), text.index("MODE=<mode>"))  # specific parameters first

    def test_target_help_rejects_an_unknown_target(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Makefile"
            path.write_text("# Run checks.\ncheck:\n", encoding="utf-8")
            with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
                self.assertEqual(help_module.generate_help(path, target="nope", params=[]), 1)


@unittest.skipUnless(MAKE, "GNU Make is required")
class RepositoryMakeTests(unittest.TestCase):
    """The make files of this repository: Makefile (driver), gears-rust.mk and .gears-dna/gears.mk."""

    FILES = ("Makefile", "gears-rust.mk", ".gears-dna/gears.mk")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gears make includes ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in self.FILES:
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, self.root / name)

    def make(self, *arguments):
        # Stub parse-time metadata only. Runtime scope fixes belong to the CI change.
        return subprocess.run([MAKE, "--no-print-directory", "EXAMPLE_SERVER_ALL_FEATURES=fixture",
                               "GEAR_PKGS=-p fixture", 'PYTHON_BOOTSTRAP="' + Path(sys.executable).as_posix() + '"',
                               *arguments], cwd=self.root, env=clean_env(), text=True, encoding="utf-8",
                              errors="replace", capture_output=True)

    def test_help_uses_all_files_without_creating_a_virtual_environment(self):
        result = self.make("help")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for target in ("fmt", "test-integration", "bench-toolkit-db-outbox", "e2e-local", "coverage", "build"):
            self.assertIn(target, result.stdout)
        self.assertFalse((self.root / ".venv").exists())
        self.assertNotIn("pip install", result.stdout)
        # Library sections first, then the repository's own.
        self.assertLess(result.stdout.index("Code safety checks:"), result.stdout.index("Main targets:"))
        # Parameter values come from the make variables the targets use; library parameters first.
        parameters = result.stdout[result.stdout.index("Parameters"):]
        for text in ('LANE="<lane> ..."', "oagw-db", "PRESET=<preset>", "oop-example", "DB=<db>", "GEAR=<gear>"):
            self.assertIn(text, parameters)
        self.assertLess(parameters.index("GEAR=<gear>"), parameters.index('LANE="<lane> ..."'))

    def test_target_help_shows_the_description_and_parameters(self):
        result = self.make("help-test-integration")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("defined in gears-rust.mk", result.stdout)
        self.assertIn('LANE="<lane> ..."', result.stdout)
        self.assertIn("toolkit-db-sqlite", result.stdout)
        self.assertNotIn("PRESET=", result.stdout)

        library = self.make("help-test")
        self.assertEqual(library.returncode, 0, library.stdout + library.stderr)
        self.assertIn("defined in .gears-dna/gears.mk", library.stdout)

        unknown = self.make("help-no-such-target")
        self.assertNotEqual(unknown.returncode, 0)
        self.assertIn("No help for 'no-such-target'", unknown.stderr)

    def test_junit_report_is_read_from_the_nextest_store_not_the_target_dir(self):
        # nextest writes reports under the workspace's target/nextest even when
        # CARGO_TARGET_DIR points elsewhere.
        result = self.make("-n", "test-integration", "LANE=toolkit-db-sqlite", "CARGO_TARGET_DIR=/elsewhere")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('junit="target/nextest/default/junit.xml"', result.stdout)
        self.assertIn(f'"{Path(sys.executable).as_posix()}" tools/scripts/junit_enrich.py', result.stdout)

    def test_make_file_change_invalidates_setup_stamp(self):
        now = time.time()
        for name in self.FILES:
            os.utime(self.root / name, (now - 30, now - 30))
        stamp = self.root / ".setup-stamp"
        stamp.touch()
        os.utime(stamp, (now - 20, now - 20))
        current = self.make("-q", ".setup-stamp")
        self.assertEqual(current.returncode, 0, current.stdout + current.stderr)
        os.utime(self.root / ".gears-dna/gears.mk", (now - 10, now - 10))
        stale = self.make("-q", ".setup-stamp")
        self.assertEqual(stale.returncode, 1, stale.stdout + stale.stderr)

    def test_aggregate_targets_accept_extra_prerequisites(self):
        (self.root / "extra.mk").write_text("ci: project-check\nproject-check:\n\t@echo project-check\n", encoding="utf-8")
        result = self.make("-f", "Makefile", "-f", "extra.mk", "-n", "ci")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for command in ("cargo fmt", "cargo clippy", "cargo nextest", "cargo deny", "cargo gears lint", "echo project-check"):
            self.assertIn(command, result.stdout)

    def test_gear_scoped_openapi_builds_a_focused_server(self):
        # The docs targets read GEAR_HAS_SERVER_FEATURE in ifeq, so gears-rust.mk
        # must define them after its GEAR= scope section.
        result = self.make("-n", ".example-server-build", "GEAR=file-parser", "EXAMPLE_SERVER_ALL_FEATURES=file-parser")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cargo build --bin cf-gears-example-server --no-default-features --features file-parser,", result.stdout)
        self.assertNotIn("SKIP", result.stdout)

        skipped = self.make("-n", ".example-server-build", "GEAR=toolkit-db", "EXAMPLE_SERVER_ALL_FEATURES=file-parser")
        self.assertEqual(skipped.returncode, 0, skipped.stdout + skipped.stderr)
        self.assertIn("SKIP: GEAR=toolkit-db", skipped.stdout)


# Paths and names that exist only in gears-rust.
REPOSITORY_ONLY = ("apps/", "testing/", "tools/", "example-server", "toolkit", "mini-chat", "PY_ENV", "$(PYTHON)")


class LibraryContentTests(unittest.TestCase):
    def test_driver_only_includes_the_two_files(self):
        lines = [line for line in (ROOT / "Makefile").read_text(encoding="utf-8").splitlines()
                 if line.strip() and not line.startswith("#")]
        self.assertEqual(lines, [".DEFAULT_GOAL := help", "include gears-rust.mk", "include .gears-dna/gears.mk"])

    def test_library_is_one_versioned_file(self):
        text = GEARS_MK.read_text(encoding="utf-8")
        self.assertIsNone(re.search(r"^\s*-?s?include\b", text, re.MULTILINE))
        header = re.search(r"^# Version: (\S+)$", text, re.MULTILINE)
        variable = re.search(r"^GEARS_MK_VERSION := (\S+)$", text, re.MULTILINE)
        self.assertIsNotNone(header)
        self.assertIsNotNone(variable)
        self.assertEqual(header.group(1), variable.group(1))

    def test_library_has_no_repository_specific_commands(self):
        for number, line in enumerate(GEARS_MK.read_text(encoding="utf-8").splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            for word in REPOSITORY_ONLY:
                self.assertNotIn(word, line, f"gears.mk:{number}")


CONSUMER_DRIVER = """.DEFAULT_GOAL := help

include my-gear.mk
include .gears-dna/gears.mk
"""

CONSUMER_LOCAL = """CLIPPY_HACK_CRATES := -p my-gear

ci: project-integration

# Run tests with the integration feature.
project-integration:
\tcargo nextest run -p my-gear --features integration

# Run one nextest lane with a JUnit report.
project-lane:
\t$(call nextest_run,my-lane,-p my-gear)
"""


@unittest.skipUnless(MAKE, "GNU Make is required")
class LibraryConsumerTests(unittest.TestCase):
    """A Gear project outside this repository: a driver, its own make file and a copy of gears.mk."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="gears make consumer ")
        self.addCleanup(self.temp.cleanup)
        self.project = Path(self.temp.name)
        (self.project / ".gears-dna").mkdir()
        shutil.copyfile(GEARS_MK, self.project / ".gears-dna" / "gears.mk")
        (self.project / "Makefile").write_text(CONSUMER_DRIVER, encoding="utf-8")
        (self.project / "my-gear.mk").write_text(CONSUMER_LOCAL, encoding="utf-8")

    def make(self, *arguments, env=None):
        return subprocess.run([MAKE, "--no-print-directory", 'PYTHON_BOOTSTRAP="' + Path(sys.executable).as_posix() + '"',
                               *arguments], cwd=self.project, env=clean_env() if env is None else env, text=True, encoding="utf-8", errors="replace",
                              capture_output=True)

    def test_preinstalled_tools_checks_each_executable_without_installing(self):
        bin_dir = self.project / "bin"
        bin_dir.mkdir()
        suffix = ".exe" if os.name == "nt" else ""
        for tool in ("fixture-first", "fixture-second"):
            executable = bin_dir / f"cargo-{tool}{suffix}"
            shutil.copyfile(sys.executable, executable)
            executable.chmod(0o755)
        env = clean_env()
        env["PATH"] = str(bin_dir) + os.pathsep + env.get("PATH", "")
        present = self.make("install-tools", "CI_PREINSTALLED_TOOLS=fixture-first fixture-second", env=env)
        self.assertEqual(present.returncode, 0, present.stdout + present.stderr)
        self.assertNotIn("Installing", present.stdout)
        for tools in ("fixture-missing fixture-second", "fixture-first fixture-missing"):
            with self.subTest(tools=tools):
                missing = self.make("install-tools", f"CI_PREINSTALLED_TOOLS={tools}", env=env)
                self.assertNotEqual(missing.returncode, 0, missing.stdout + missing.stderr)
                self.assertIn("cargo-fixture-missing is not installed", missing.stdout + missing.stderr)

    def test_help_lists_library_and_project_targets(self):
        result = self.make("help")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for target in ("fmt", "clippy", "test", "deny", "ci", "project-integration"):
            self.assertIn(f" {target} ", result.stdout)
        for target in ("test-integration", "e2e-local", "openapi", "make build", "make all"):
            self.assertNotIn(target, result.stdout)
        # The project's own targets have no library section.
        self.assertIn("Other targets:", result.stdout)
        # Only the library's parameters, and only its targets.
        self.assertIn("GEAR=<gear>", result.stdout)
        for text in ("LANE=", "PRESET=", "build dist run"):
            self.assertNotIn(text, result.stdout)

    def test_target_help_works_in_the_project(self):
        result = self.make("help-test")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for text in ("make test", "defined in .gears-dna/gears.mk", "GEAR=<gear>", "GEAR_TEST_ARGS"):
            self.assertIn(text, result.stdout)

    def test_ci_runs_library_checks_for_the_project_only(self):
        result = self.make("-n", "ci")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for command in ("cargo fmt --all --check", "cargo clippy --workspace",
                        "cargo hack clippy -p my-gear", "cargo nextest run --workspace", "cargo deny check",
                        "cargo gears lint", "cargo nextest run -p my-gear --features integration"):
            self.assertIn(command, result.stdout)
        for word in REPOSITORY_ONLY + ("cf-gears-",):
            self.assertNotIn(word, result.stdout)

    def test_nextest_run_saves_reports_without_an_enrich_script(self):
        result = self.make("-n", "project-lane", "JUNIT_DIR=reports")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('cp "$junit" "reports/my-lane.xml"', result.stdout)
        self.assertNotIn("python3", result.stdout)

    def test_security_skips_fips_policy_without_its_config(self):
        result = self.make("-n", "security")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cargo deny check", result.stdout)
        self.assertNotIn("deny-fips.toml", result.stdout)

    def test_clippy_skips_cargo_hack_without_crates(self):
        result = self.make("-n", "clippy", "CLIPPY_HACK_CRATES=")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("cargo clippy --workspace", result.stdout)
        self.assertNotIn("cargo hack", result.stdout)
        self.assertNotIn("cargo-hack", result.stdout)


if __name__ == "__main__":
    unittest.main()
