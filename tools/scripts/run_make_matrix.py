#!/usr/bin/env python3
"""
run_make_matrix.py — Run the full Makefile target matrix across all tracked gears.

Discovers every gear in gears/ (via ``git ls-files``, ignoring untracked dirs),
builds a step sequence from a preset (smoke / default / extended / custom),
and executes each step sequentially, logging output per step and printing a
pass/fail summary at the end.  The runner continues after failures so a single
broken gear does not block the rest of the matrix.

Usage:
  python run_make_matrix.py [smoke|default|extended|custom [CMD ...]]

Environment:
  CLEAN_CMD               Command used for reset steps.  Default: cargo clean
  LOG_ROOT                Log root directory.  Default: .logs/make-matrix
  RUN_ID                  Run identifier.  Default: current timestamp
  GEAR_FILTER             Regex filter applied to discovered gear names/packages
  CLEAN_BETWEEN_GEARS     Insert a reset step after each gear bundle.  Default: 1

Notes:
  - The repo does not define a top-level ``make clean`` target, so reset
    steps use CLEAN_CMD.
  - The runner continues after failures and prints a summary at the end.
  - Long-running server targets like ``make run`` and ``make mini-chat``
    are intentionally excluded from presets.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tomllib
from datetime import datetime
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional, Tuple

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

CLEAN_SENTINEL = "__CLEAN__"

ROOT_DIR = Path(__file__).resolve().parent.parent.parent


# ---------------------------------------------------------------------------
# Color helpers
# ---------------------------------------------------------------------------

@dataclass
class Colors:
    bold: str = ""
    dim: str = ""
    green: str = ""
    red: str = ""
    yellow: str = ""
    cyan: str = ""
    reset: str = ""


def make_colors() -> Colors:
    if sys.stdout.isatty() and not os.environ.get("NO_COLOR"):
        return Colors(
            bold="\033[1m",
            dim="\033[2m",
            green="\033[32m",
            red="\033[31m",
            yellow="\033[33m",
            cyan="\033[36m",
            reset="\033[0m",
        )
    return Colors()


C = make_colors()


# ---------------------------------------------------------------------------
# Gear discovery
# ---------------------------------------------------------------------------

def package_name_from_manifest(manifest: Path) -> Optional[str]:
    """Extract ``[package].name`` from a Cargo.toml."""
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return None
    return data.get("package", {}).get("name")


def gear_name_from_package(package_name: str) -> str:
    if package_name.startswith("cf-gears-"):
        return package_name[len("cf-gears-"):]
    if package_name.startswith("cf-"):
        return package_name[len("cf-"):]
    return package_name


def should_include_gear_manifest(relpath: str) -> bool:
    if "/plugins/" in relpath or "/libs/" in relpath:
        return False
    if relpath.endswith("-sdk/Cargo.toml"):
        return False
    if relpath.endswith("/cluster-conformance/Cargo.toml"):
        return False
    return True


def discover_gear_manifests() -> List[Path]:
    """Return tracked Cargo.toml paths under ``gears/`` that represent impl crates."""
    result = subprocess.run(
        ["git", "-C", str(ROOT_DIR), "ls-files", "--", "gears/"],
        capture_output=True, text=True, check=True,
    )
    manifests: list[Path] = []
    for line in sorted(result.stdout.splitlines()):
        if not line.endswith("/Cargo.toml") and line != "Cargo.toml":
            continue
        if not line.endswith("Cargo.toml"):
            continue
        if not should_include_gear_manifest(line):
            continue
        manifests.append(ROOT_DIR / line)
    return manifests


# ---------------------------------------------------------------------------
# Scope-var builder (mirrors Makefile GEAR / GEAR_PKG / GEAR_SDK_* vars)
# ---------------------------------------------------------------------------

@dataclass
class GearScope:
    gear_name: str
    package_name: str
    sdk_package_name: Optional[str] = None

    def env_fragment(self) -> str:
        """Return the ``GEAR=… GEAR_PKG=…`` prefix for ``make`` invocations."""
        parts = [f"GEAR={self.gear_name}", f"GEAR_PKG={self.package_name}"]
        if self.sdk_package_name:
            parts.append(f"GEAR_SDK_PKG={self.sdk_package_name}")
            parts.append(f"GEAR_SDK_FLAG=-p\\ {self.sdk_package_name}")
        return " ".join(parts)


def build_gear_scope(manifest: Path) -> Optional[GearScope]:
    pkg = package_name_from_manifest(manifest)
    if not pkg:
        return None
    gear = gear_name_from_package(pkg)
    impl_dir = manifest.parent
    sdk_dir = impl_dir.parent / f"{impl_dir.name}-sdk"
    sdk_manifest = sdk_dir / "Cargo.toml"
    sdk_pkg = None
    if sdk_manifest.is_file():
        sdk_pkg = package_name_from_manifest(sdk_manifest)
    return GearScope(gear_name=gear, package_name=pkg, sdk_package_name=sdk_pkg)


# ---------------------------------------------------------------------------
# Step builder
# ---------------------------------------------------------------------------

@dataclass
class MatrixBuilder:
    steps: List[str] = field(default_factory=list)
    discovered_gears: List[str] = field(default_factory=list)
    gear_filter: Optional[re.Pattern] = None  # type: ignore[type-arg]
    clean_between_gears: bool = True

    def append(self, *commands: str) -> None:
        self.steps.extend(commands)

    def clean(self) -> None:
        self.steps.append(CLEAN_SENTINEL)

    def append_gear_steps(self, *actions: str) -> None:
        """Discover gears and append per-gear make targets."""
        for manifest in discover_gear_manifests():
            scope = build_gear_scope(manifest)
            if scope is None:
                continue
            if self.gear_filter and not (
                self.gear_filter.search(scope.gear_name)
                or self.gear_filter.search(scope.package_name)
            ):
                continue
            self.discovered_gears.append(scope.gear_name)
            for action in actions:
                self.steps.append(f"{scope.env_fragment()} make {action}")
            if self.clean_between_gears:
                self.clean()

    def trim_trailing_clean(self) -> None:
        if self.steps and self.steps[-1] == CLEAN_SENTINEL:
            self.steps.pop()


# ---------------------------------------------------------------------------
# Presets
# ---------------------------------------------------------------------------

def load_smoke(b: MatrixBuilder) -> None:
    b.append("make help", "make fmt", "make validate-gear-names",
             "make check-packaging-metadata", "make check-release-config")
    b.clean()
    b.append_gear_steps("clippy", "lint", "build")
    b.clean()
    b.append("make gts-docs", "make lychee")
    b.trim_trailing_clean()


def load_default(b: MatrixBuilder) -> None:
    b.append("make help", "make fmt", "make validate-gear-names",
             "make check-packaging-metadata", "make check-release-config")
    b.clean()
    b.append("make test-no-macros", "make test-macros")
    b.clean()
    b.append_gear_steps("clippy", "lint", "build", "test")
    b.clean()
    b.append("make deny", "make fips-policy", "make dylint",
             "make gts-docs", "make lychee", "make build", "make dist")
    b.trim_trailing_clean()


def load_extended(b: MatrixBuilder) -> None:
    b.append("make help", "make fmt", "make validate-gear-names",
             "make check-packaging-metadata", "make check-release-config")
    b.clean()
    b.append("make test-no-macros", "make test-macros", "make test-sqlite")
    b.clean()
    b.append("make test-pg", "make test-mysql", "make test-users-info-pg",
             "make test-usage-collector-pg", "make test-cluster-pg",
             "make test-fips")
    b.clean()
    b.append_gear_steps("clippy", "lint", "build", "test")
    b.clean()
    b.append("make deny", "make fips-policy", "make dylint",
             "make gts-docs", "make lychee", "make build", "make dist")
    b.clean()
    b.append("make openapi GEAR=file-parser", "make openapi GEAR=file-storage",
             "make openapi",
             "make e2e-local-smoke", "make e2e-local",
             "make e2e-mini-chat", "make e2e-usage-collector",
             "make e2e-tr-authz")
    b.trim_trailing_clean()


PRESETS = {
    "smoke": load_smoke,
    "default": load_default,
    "extended": load_extended,
}


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------

@dataclass
class StepResult:
    index: int
    command: str
    log_file: Path
    exit_code: int

    @property
    def ok(self) -> bool:
        return self.exit_code == 0


def run_step(
    index: int,
    total: int,
    command: str,
    run_dir: Path,
    clean_cmd: str,
) -> StepResult:
    log_file = run_dir / f"step-{index:02d}.log"

    is_clean = command == CLEAN_SENTINEL
    actual_cmd = clean_cmd if is_clean else command
    label = "reset" if is_clean else "command"

    print(
        f"{C.cyan}[{index:02d}/{total:02d}]{C.reset} "
        f"{C.bold}{label}{C.reset} {C.dim}{actual_cmd}{C.reset}"
    )

    with open(log_file, "w") as lf:
        proc = subprocess.run(
            ["bash", "-lc", actual_cmd],
            cwd=str(ROOT_DIR),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        output = proc.stdout.decode(errors="replace")
        lf.write(output)
        sys.stdout.write(output)

    return StepResult(
        index=index, command=actual_cmd, log_file=log_file,
        exit_code=proc.returncode,
    )


def print_rule() -> None:
    print(f"{C.cyan}{'=' * 80}{C.reset}")


def print_section(title: str) -> None:
    print_rule()
    print(f"{C.bold}{title}{C.reset}")
    print_rule()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the full Makefile target matrix across all tracked gears.",
    )
    parser.add_argument(
        "preset", nargs="?", default="default",
        choices=list(PRESETS.keys()) + ["custom"],
        help="Preset to run (default: default).",
    )
    parser.add_argument(
        "commands", nargs="*",
        help="Commands for the 'custom' preset.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    preset: str = args.preset

    clean_cmd = os.environ.get("CLEAN_CMD", "cargo clean")
    log_root = Path(os.environ.get("LOG_ROOT", str(ROOT_DIR / ".logs" / "make-matrix")))
    run_id = os.environ.get("RUN_ID", datetime.now().strftime("%Y%m%d-%H%M%S"))
    run_dir = log_root / run_id
    run_dir.mkdir(parents=True, exist_ok=True)

    gear_filter_raw = os.environ.get("GEAR_FILTER")
    gear_filter = re.compile(gear_filter_raw) if gear_filter_raw else None
    clean_between = os.environ.get("CLEAN_BETWEEN_GEARS", "1") != "0"

    builder = MatrixBuilder(gear_filter=gear_filter, clean_between_gears=clean_between)

    if preset == "custom":
        if not args.commands:
            print("custom preset requires one or more commands", file=sys.stderr)
            return 2
        builder.append(*args.commands)
    else:
        PRESETS[preset](builder)

    steps = builder.steps

    # --- header -----------------------------------------------------------
    print_section(f"Make matrix preset: {preset}")
    print(f"{C.bold}Root:{C.reset} {ROOT_DIR}")
    print(f"{C.bold}Logs:{C.reset} {run_dir}")
    print(f"{C.bold}Reset command:{C.reset} {clean_cmd}")
    if builder.discovered_gears:
        print(f"{C.bold}Discovered gears:{C.reset} {len(builder.discovered_gears)}")
    print(f"{C.bold}Total steps:{C.reset} {len(steps)}\n")

    # --- execute ----------------------------------------------------------
    results_file = run_dir / "results.tsv"
    results_file.write_text("")

    passed = 0
    failed = 0
    failed_details: list[StepResult] = []

    for idx, step in enumerate(steps, start=1):
        result = run_step(idx, len(steps), step, run_dir, clean_cmd)

        with open(results_file, "a") as rf:
            rf.write(f"{result.index}\t{result.exit_code}\t{result.log_file}\t{result.command}\n")

        if result.ok:
            passed += 1
            print(f"{C.green}OK{C.reset}\n")
        else:
            failed += 1
            failed_details.append(result)
            print(f"{C.red}FAILED{C.reset}\n")

    # --- summary ----------------------------------------------------------
    print_section("Summary")
    print(f"{C.bold}Passed:{C.reset} {passed}")
    print(f"{C.bold}Failed:{C.reset} {failed}")
    print(f"{C.bold}Results:{C.reset} {results_file}\n")

    if failed_details:
        for r in failed_details:
            print(f"{C.yellow}[{r.index}]{C.reset} cmd={r.command}")
            print(f"    log: {r.log_file}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
