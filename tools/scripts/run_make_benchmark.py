#!/usr/bin/env python3
"""
run_benchmark.py — Benchmark Makefile targets and report timing, status, and target/ size.

Runs two groups of scenarios:
  Group all-gears (1–7):    targets without GEAR= scope
  Group specific-gear (8–14): targets with GEAR=<name> (default: file-parser)

Each scenario records wall-clock execution time, pass/fail exit status, and
the total size of ``target/`` after the step completes.  Scenarios marked as
"clean" remove ``target/`` before running; the removal time is excluded from
the measurement.

Usage examples:
  python run_benchmark.py                       # run all 14 scenarios
  python run_benchmark.py --group all-gears      # all-gears group only
  python run_benchmark.py --group specific-gear   # specific-gear group only
  python run_benchmark.py --gear credstore      # implies --group=specific-gear
  python run_benchmark.py --scenario 3          # run only scenario #3
  python run_benchmark.py --scenario 1 --scenario 10  # run scenarios 1 and 10
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time
from datetime import datetime
from dataclasses import dataclass
from pathlib import Path
from typing import List, Optional

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

ROOT_DIR = Path(__file__).resolve().parent.parent.parent
TARGET_DIR = ROOT_DIR / "target"


# ---------------------------------------------------------------------------
# Color helpers (same style as run_make_matrix.py)
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
# Scenario / Result data
# ---------------------------------------------------------------------------

@dataclass
class Scenario:
    number: int
    group: str          # "all-gears" or "specific-gear"
    name: str
    command: str
    clean_target: bool


@dataclass
class Result:
    scenario: Scenario
    status: str             # "OK" or "FAIL"
    elapsed_minutes: float
    target_size_mb: float
    log_file: Path


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def get_target_size_mb() -> float:
    """Return total size of target/ in megabytes (``du -sm``)."""
    if not TARGET_DIR.exists():
        return 0.0
    proc = subprocess.run(
        ["du", "-sm", str(TARGET_DIR)],
        capture_output=True, text=True,
    )
    if proc.returncode == 0:
        try:
            return float(proc.stdout.split()[0])
        except (IndexError, ValueError):
            pass
    return 0.0


def stream_command_to_log(command: str, log_file: Path, verbose: bool) -> int:
    """Run ``command`` and stream combined stdout/stderr to ``log_file``."""
    with log_file.open("w", encoding="utf-8") as lf:
        # Use a NON-login shell (`-c`, not `-lc`): a login shell re-sources
        # profile files and can reorder $PATH (e.g. putting /usr/bin — Apple's
        # LibreSSL `openssl` — ahead of Homebrew/MacPorts OpenSSL), which does
        # not match how a developer runs `make all` interactively. Inheriting
        # the caller's environment keeps benchmark runs faithful to manual ones.
        proc = subprocess.Popen(
            ["bash", "-c", command],
            cwd=str(ROOT_DIR),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
            bufsize=1,
        )

        assert proc.stdout is not None
        for line in proc.stdout:
            lf.write(line)
            lf.flush()
            if verbose:
                sys.stdout.write(line)
                sys.stdout.flush()

        proc.stdout.close()
        return proc.wait()


def display_path(path: Path) -> str:
    """Prefer repo-relative paths for user-facing output."""
    try:
        return str(path.relative_to(ROOT_DIR))
    except ValueError:
        return str(path)


# ---------------------------------------------------------------------------
# Scenario definitions
# ---------------------------------------------------------------------------

def build_scenarios(gear: str) -> List[Scenario]:
    scenarios: List[Scenario] = []

    # Group A — all gears
    group_a = [
        (True,  "make all",       "rm -rf target/; make all"),
        (False, "make all (warm)", "make all"),
        (True,  "make build",     "rm -rf target/; make build"),
        (False, "make check",     "make check"),
        (False, "make test",      "make test"),
        (False, "make e2e-local", "make e2e-local"),
        (False, "make dist",      "make dist"),
    ]
    for i, (clean, name, cmd) in enumerate(group_a, start=1):
        scenarios.append(Scenario(
            number=i, group="all-gears", name=name,
            command=cmd, clean_target=clean,
        ))

    # Group B — specific gear
    group_b = [
        (True,  f"make all GEAR={gear}",       f"rm -rf target/; make all GEAR={gear}"),
        (False, f"make all GEAR={gear} (warm)", f"make all GEAR={gear}"),
        (True,  f"make build GEAR={gear}",     f"rm -rf target/; make build GEAR={gear}"),
        (False, f"make check GEAR={gear}",     f"make check GEAR={gear}"),
        (False, f"make test GEAR={gear}",      f"make test GEAR={gear}"),
        (False, f"make e2e-local GEAR={gear}", f"make e2e-local GEAR={gear}"),
        (False, f"make dist GEAR={gear}",      f"make dist GEAR={gear}"),
    ]
    for i, (clean, name, cmd) in enumerate(group_b, start=8):
        scenarios.append(Scenario(
            number=i, group="specific-gear", name=name,
            command=cmd, clean_target=clean,
        ))

    return scenarios


# ---------------------------------------------------------------------------
# Runner
# ---------------------------------------------------------------------------

def run_scenario(scenario: Scenario, verbose: bool, run_dir: Path) -> Result:
    log_file = run_dir / f"scenario-{scenario.number:02d}.log"

    # Clean phase (not timed)
    if scenario.clean_target:
        print(f"  {C.dim}Removing target/ ...{C.reset}", flush=True)
        shutil.rmtree(TARGET_DIR, ignore_errors=True)

    print(f"  {C.dim}Log: {display_path(log_file)}{C.reset}", flush=True)
    print(f"  {C.dim}Running: {scenario.command}{C.reset}", flush=True)

    start = time.monotonic()
    returncode = stream_command_to_log(scenario.command, log_file, verbose)
    elapsed = time.monotonic() - start

    status = "OK" if returncode == 0 else "FAIL"
    target_size = get_target_size_mb()

    return Result(
        scenario=scenario,
        status=status,
        elapsed_minutes=elapsed / 60.0,
        target_size_mb=target_size,
        log_file=log_file,
    )


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def format_time(minutes: float) -> str:
    if minutes < 1.0:
        return f"{minutes * 60:.1f}s"
    return f"{minutes:.2f}m"


def format_size(mb: float) -> str:
    if mb >= 1024:
        return f"{mb / 1024:.2f} GB"
    return f"{mb:.0f} MB"


def print_table(results: List[Result]) -> None:
    # Column headers
    headers = ["#", "Scenario", "Clean target/", "Status", "Time", "target/ size"]

    # Build rows
    rows: list[tuple[str, ...]] = []
    separator_after: set[int] = set()

    for r in results:
        rows.append((
            str(r.scenario.number),
            r.scenario.name,
            "YES" if r.scenario.clean_target else "NO",
            r.status,
            format_time(r.elapsed_minutes),
            format_size(r.target_size_mb),
        ))

    # Detect group boundary for separator
    for i in range(len(results) - 1):
        if results[i].scenario.group != results[i + 1].scenario.group:
            separator_after.add(i)

    # Column widths
    col_widths = [len(h) for h in headers]
    for row in rows:
        for j, cell in enumerate(row):
            col_widths[j] = max(col_widths[j], len(cell))

    def fmt_row(cells: tuple[str, ...] | list[str], align: Optional[List[str]] = None) -> str:
        parts: list[str] = []
        for j, cell in enumerate(cells):
            w = col_widths[j]
            a = (align[j] if align else "l")
            if a == "r":
                parts.append(cell.rjust(w))
            elif a == "c":
                parts.append(cell.center(w))
            else:
                parts.append(cell.ljust(w))
        return "| " + " | ".join(parts) + " |"

    def sep_line() -> str:
        return "+-" + "-+-".join("-" * w for w in col_widths) + "-+"

    # Alignment: #=right, scenario=left, clean=center, status=center, time=right, size=right
    aligns = ["r", "l", "c", "c", "r", "r"]

    # Print
    print()
    print(sep_line())
    print(fmt_row(tuple(headers), ["c"] * len(headers)))
    print(sep_line())
    for i, row in enumerate(rows):
        color = C.red if row[3] == "FAIL" else ""
        line = fmt_row(row, aligns)
        if color:
            print(f"{color}{line}{C.reset}")
        else:
            print(line)
        if i in separator_after:
            print(sep_line())
    print(sep_line())
    print()


def print_report(results: List[Result], gear: str) -> None:
    print()
    print(f"{C.bold}{'=' * 78}{C.reset}")
    print(f"{C.bold}  Benchmark Report{C.reset}")
    print(f"{C.bold}{'=' * 78}{C.reset}")
    print()
    print("  This report shows the execution results of Makefile target benchmarks.")
    print("  Two groups of scenarios were defined:")
    print()
    print(f"    {C.bold}all-gears (1–7){C.reset}:      targets without GEAR= scoping")
    print(f"    {C.bold}specific-gear (8–14){C.reset}: targets with GEAR={gear}")
    print()
    print("  Columns:")
    print("    - #              Scenario number")
    print("    - Scenario       Description of the make target")
    print("    - Clean target/  Whether target/ was removed before execution (YES/NO)")
    print("    - Status         Exit status of the make command (OK/FAIL)")
    print("    - Time           Wall-clock execution time (rm target/ excluded)")
    print("    - target/ size   Total size of target/ directory after the step")

    print_table(results)

    passed = sum(1 for r in results if r.status == "OK")
    failed = sum(1 for r in results if r.status == "FAIL")
    total_time = sum(r.elapsed_minutes for r in results)

    print(f"  {C.bold}Total:{C.reset}  {len(results)} scenarios  "
          f"{C.green}{passed} OK{C.reset}  "
          f"{C.red if failed else ''}{failed} FAIL{C.reset if failed else ''}  "
          f"{format_time(total_time)} elapsed")

    if failed:
        print()
        print(f"  {C.bold}Failed scenario logs:{C.reset}")
        for result in results:
            if result.status == "FAIL":
                print(f"    - #{result.scenario.number} {result.scenario.name}")
                print(f"      {display_path(result.log_file)}")
    print()


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Benchmark Makefile targets: measure time, status, and target/ size.",
    )
    parser.add_argument(
        "--group", choices=["all-gears", "specific-gear"],
        help="Run only the specified group (all-gears or specific-gear).",
    )
    parser.add_argument(
        "--gear", default=None,
        help="Gear name for specific-gear group scenarios (default: file-parser).",
    )
    parser.add_argument(
        "--scenario", type=int, action="append", dest="scenarios",
        help="Run only the specified scenario number (1–14). Can be repeated.",
    )
    parser.add_argument(
        "--verbose", "-v", action="store_true",
        help="Stream make output to stdout instead of capturing it.",
    )
    return parser.parse_args()


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    args = parse_args()
    gear_explicit = args.gear is not None
    gear: str = args.gear or "file-parser"

    # When --gear is explicitly provided but --group is not, automatically
    # select the specific-gear group.  This makes
    #   make make-benchmark BENCH_GEAR=credstore
    # imply BENCH_GROUP=specific-gear without requiring both flags.
    group: Optional[str] = args.group
    if group is None and gear_explicit:
        group = "specific-gear"

    log_root = Path(os.environ.get("LOG_ROOT", str(ROOT_DIR / ".logs" / "make-benchmark")))
    run_id = os.environ.get("RUN_ID", datetime.now().strftime("%Y%m%d-%H%M%S"))
    run_dir = log_root / run_id
    run_dir.mkdir(parents=True, exist_ok=True)
    results_file = run_dir / "results.tsv"
    results_file.write_text("scenario\tstatus\tminutes\ttarget_mb\tlog_file\tcommand\n", encoding="utf-8")

    all_scenarios = build_scenarios(gear)

    # Filter scenarios
    selected = all_scenarios
    if group:
        selected = [s for s in selected if s.group == group]
    if args.scenarios:
        nums = set(args.scenarios)
        selected = [s for s in selected if s.number in nums]

    if not selected:
        print("No scenarios matched the given filters.", file=sys.stderr)
        return 2

    # Header
    print(f"\n{C.bold}Benchmark: {len(selected)} scenario(s){C.reset}")
    print(f"{C.bold}Root:{C.reset}  {ROOT_DIR}")
    print(f"{C.bold}Gear:{C.reset}  {gear}")
    print(f"{C.bold}Logs:{C.reset}  {display_path(run_dir)}")
    print()

    # Execute
    results: List[Result] = []
    for i, scenario in enumerate(selected, start=1):
        label_color = C.green if scenario.group == "all-gears" else C.yellow
        print(
            f"{C.cyan}[{i}/{len(selected)}]{C.reset} "
            f"{label_color}#{scenario.number}{C.reset} "
            f"{C.bold}{scenario.name}{C.reset}"
        )
        result = run_scenario(scenario, verbose=args.verbose, run_dir=run_dir)
        with results_file.open("a", encoding="utf-8") as rf:
            rf.write(
                f"{result.scenario.number}\t{result.status}\t{result.elapsed_minutes:.6f}\t"
                f"{result.target_size_mb:.0f}\t{display_path(result.log_file)}\t"
                f"{result.scenario.command}\n"
            )
        status_color = C.green if result.status == "OK" else C.red
        print(
            f"  {status_color}{result.status}{C.reset}  "
            f"{format_time(result.elapsed_minutes)}  "
            f"target/={format_size(result.target_size_mb)}  "
            f"log={display_path(result.log_file)}"
        )
        print()
        results.append(result)

    # Report
    print_report(results, gear)
    print(f"{C.bold}Results:{C.reset}  {display_path(results_file)}")
    print()

    return 1 if any(r.status == "FAIL" for r in results) else 0


if __name__ == "__main__":
    sys.exit(main())
