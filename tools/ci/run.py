#!/usr/bin/env python3
"""Stdlib-only runtime helpers for this repository's make targets.

No Cargo process is started while make reads its includes or prints a dry run.
All subprocesses run in the repository root selected by make.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib

from tool_versions import versions


def version(value: str) -> tuple[int, int, int]:
    match = re.search(r"\b(\d+)\.(\d+)\.(\d+)\b", value)
    if not match:
        raise ValueError(f"Cannot read tool version: {value!r}")
    return tuple(map(int, match.groups()))


def require(binary: str, minimum: str, pinned: str | None = None) -> None:
    executable = shutil.which(binary)
    if not executable:
        raise ValueError(f"{binary} is missing; run make setup (CI: install it in a setup step)")
    result = subprocess.run([executable, "--version"], text=True, capture_output=True, check=True)
    actual = version(result.stdout)
    if actual < version(minimum):
        raise ValueError(f"{binary} >= {minimum} required, got {result.stdout.strip()}")
    if os.environ.get("GITHUB_ACTIONS") == "true" and pinned and actual != version(pinned):
        raise ValueError(f"CI requires {binary} {pinned}, got {result.stdout.strip()}")


def scope(args: argparse.Namespace) -> list[str]:
    if args.gear:
        require("cargo-gears", versions()["cargo-gears"])
        command = [shutil.which("cargo") or "cargo", "gears", "ls", "packages", "--path", str(args.root)]
        if args.scope_dirs:
            command += ["--dirs", args.scope_dirs]
        command += ["--filter", args.package_filter, "-f", "list"]
        if args.include_rdeps:
            command.append("--include-rdeps")
        result = subprocess.run(command, cwd=args.root, text=True, capture_output=True, check=True)
        packages = result.stdout.split()
        if not packages:
            raise ValueError(f"GEAR={args.gear}: package selection is empty; refusing an unscoped Cargo run")
        if any(not re.fullmatch(r"[A-Za-z0-9_-]+", p) for p in packages):
            raise ValueError(f"Unexpected package selection: {result.stdout!r}")
        print(f"Selected packages ({'with reverse deps' if args.include_rdeps else 'direct'}): {' '.join(packages)}", file=sys.stderr)
        if os.environ.get("GITHUB_STEP_SUMMARY"):
            with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as out:
                out.write(f"\nGEAR `{args.gear}` packages: " + ", ".join(f"`{p}`" for p in packages) + "\n")
        return packages
    return args.packages.split()


def cargo(args: argparse.Namespace) -> int:
    command = list(args.arguments)
    if not command:
        raise ValueError("Missing Cargo subcommand")
    subcommand = command.pop(0)
    prefix = [shutil.which("cargo") or "cargo", subcommand]
    if subcommand == "nextest":
        if not command or command.pop(0) != "run":
            raise ValueError("Only nextest run is supported")
        prefix.append("run")
    # Dedicated integration/macro passes already specify their package selection.
    # Arguments after -- belong to rustc/libtest, never to Cargo selection.
    cargo_arguments = command[:command.index("--")] if "--" in command else command
    selectors = {"-p", "--package", "--workspace", "--all"}
    explicit = any(x in selectors or x.startswith("--package=") or
                   (x.startswith("-p") and len(x) > 2) for x in cargo_arguments)
    if not explicit:
        packages = scope(args)
        prefix += [item for p in packages for item in ("-p", p)] if packages else ["--all" if subcommand == "fmt" else "--workspace"]
    if subcommand != "fmt" and args.features and not any(x == "--features" or x.startswith("--features=") for x in cargo_arguments):
        prefix += ["--features", args.features]
    print("+ " + " ".join(prefix + command), flush=True)
    return subprocess.run(prefix + command, cwd=args.root).returncode


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--gear", default="")
    parser.add_argument("--scope-dirs", default="")
    parser.add_argument("--package-filter", default="")
    parser.add_argument("--packages", default="")
    parser.add_argument("--features", default="")
    parser.add_argument("--include-rdeps", action="store_true")
    sub = parser.add_subparsers(dest="action", required=True)
    check = sub.add_parser("require")
    check.add_argument("binary")
    check.add_argument("minimum")
    check.add_argument("pinned", nargs="?")
    selected = sub.add_parser("scope")
    selected.add_argument("--first", action="store_true")
    features = sub.add_parser("features")
    features.add_argument("--manifest", required=True)
    run = sub.add_parser("cargo")
    run.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    args.root = args.root.resolve()
    try:
        if args.action == "require":
            require(args.binary, args.minimum, args.pinned)
        elif args.action == "scope":
            packages = scope(args)
            print(packages[0] if args.first and packages else " ".join(f"-p {p}" for p in packages))
        elif args.action == "features":
            with (args.root / args.manifest).open("rb") as source:
                print(" ".join(tomllib.load(source).get("features", {})))
        else:
            return cargo(args)
        return 0
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr, file=sys.stderr)
        return error.returncode if isinstance(error, subprocess.CalledProcessError) else 1


if __name__ == "__main__":
    sys.exit(main())
