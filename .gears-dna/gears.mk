# gears.mk: shared make rules for projects built on the gears framework.
#
# Version: 1.0.0
# Source:  https://github.com/constructorfabric/gears-rust/blob/main/.gears-dna/gears.mk
#
# Do not edit a copy of this file. To update it, replace it with a newer version
# from the source. Send changes to the source repository.
#
# A project uses it from a driver Makefile that it does not edit either:
#
#   .DEFAULT_GOAL := help
#   include <project>.mk    # settings for this file, then the project's targets
#   include .gears-dna/gears.mk
#
# All settings below use ?=, so values from <project>.mk win. Run `make help`
# for the targets and their parameters. Guide: .gears-dna/docs/GEARS_MK.md in the source.

ifndef GEARS_MK_VERSION
GEARS_MK_VERSION := 1.0.0
# This file, as make found it (help reads it first).
GEARS_MK_FILE := $(lastword $(MAKEFILE_LIST))

# -------- Settings --------
# Each one uses ?=: set it in the project's make file (read before this file),
# in the environment, or on the command line.

# Python for helpers that need only the standard library (help, JUnit report enrichment).
PYTHON_BOOTSTRAP ?= $(shell command -v python3 2>/dev/null || command -v python 2>/dev/null || echo python3)

# Minimum tool versions — checked via `cargo gears tools check-version`.
DENY_MIN_VERSION ?= 0.20.0
NEXTEST_MIN_VERSION ?= 0.9.143
CARGO_GEARS_VERSION ?= 0.0.8

# CI sets NEXTEST_PROFILE=ci (see .config/nextest.toml) and JUNIT_DIR=<dir>;
# locally both stay inert and nextest behaves as before.
NEXTEST_PROFILE ?= default
JUNIT_DIR ?=
# nextest's store directory ([store] dir in .config/nextest.toml). nextest
# resolves it from the workspace root, not from CARGO_TARGET_DIR.
NEXTEST_STORE_DIR ?= target/nextest
# Optional script that adds source locations to each saved JUnit report:
# <script> [--github] <report.xml>. Empty: reports are saved as nextest wrote them.
GEARS_JUNIT_ENRICH ?=

# Nightly toolchain for targets that need unstable rustc flags (currently only
# `shear`, which drives -Zunpretty=expanded). This default serves local runs;
# CI overrides it via `make shear RUST_NIGHTLY=...` so the toolchain it installs
# and caches cannot drift from the one that actually compiles.
RUST_NIGHTLY ?= nightly-2026-08-21

# Clippy flags shared by the workspace pass and the cargo-hack pass.
CLIPPY_FLAGS ?= -- -D warnings -D clippy::perf
# Packages with real #[cfg(feature = "...")] guards, linted once per feature by
# cargo-hack (e.g. `-p my-gear`). Leave empty to skip the cargo-hack pass.
CLIPPY_HACK_CRATES ?=
# Extra cargo-hack options for that pass, e.g. `--exclude-features <feature>`.
CLIPPY_HACK_EXCLUDE ?=

# cargo-deny config for the FIPS dependency-graph policy. `security` runs
# `fips-policy` only when this file exists.
DENY_FIPS_CONFIG ?= deny-fips.toml

# Constructor Studio CLI.
CFS ?= cfs
CFS_PIPX_SPEC ?= git+https://github.com/constructorfabric/studio.git

# cargo-fuzz directory (the one holding the fuzz Cargo.toml).
FUZZ_DIR ?= fuzz

# Targets that `make ci` runs. Add project targets as prerequisites
# (`ci: my-check`) or replace the list in the project's make file.
GEARS_CI_TARGETS ?= fmt clippy test deny dylint

# -------- Optional GEAR= scope for top-level targets --------
#
# Scope fmt, clippy, lint and test to a single gear instead of the whole workspace.
#
# Package resolution uses `cargo gears ls packages` with a regex filter
# on Cargo package names, so naming conventions (cf-gears-*, bss-*, bare)
# are handled automatically.
#
# Examples:
#   make fmt GEAR=file-parser         -> cargo fmt -p cf-gears-file-parser -p cf-gears-file-parser-sdk --check
#   make test GEAR=ledger             -> cargo nextest run -p bss-ledger -p bss-ledger-sdk ... (+ reverse deps)

# --- User-facing knobs ---
GEAR ?=
GEAR_TEST_ARGS ?=
# Extra Cargo features to pass with --features (e.g. GEAR_FEATURES=integration).
GEAR_FEATURES ?=
# Clippy flags for gear-scoped runs.
GEAR_CLIPPY_ARGS ?= --all-targets --all-features -- -D warnings
# Workspace directories searched for the gear's packages.
GEAR_PKG_DIRS ?= gears,libs

# --- Package resolution ---
# Regex to match Cargo package names belonging to this gear.
# Handles: <gear>, <gear>-sdk, cf-gears-<gear>, cf-gears-<gear>-sdk,
#          <prefix>-<gear>, <prefix>-<gear>-sdk (e.g. bss-ledger).
# Override with GEAR_NAME_REGEXP= for non-standard naming.
GEAR_NAME_REGEXP ?= ^(?:(?:cf-gears-)?(?:[a-z]+-)?)?$(GEAR)(?:-sdk)?$$
ifdef GEAR
  GEAR_PKGS := $(shell cargo gears ls packages --dirs $(GEAR_PKG_DIRS) --filter '$(GEAR_NAME_REGEXP)' -f cargo-flags)
endif

# --- Derived variables ---
GEAR_FEATURE_ARGS := $(if $(GEAR_FEATURES),--features $(GEAR_FEATURES),)
GEAR_NO_TESTS_FLAG := $(if $(GEAR),--no-tests=warn)

help.GEAR.usage := GEAR=<gear>
help.GEAR.targets := fmt clippy lint test $(help.GEAR.targets)
help.GEAR.text = Work on one gear only: packages under $(GEAR_PKG_DIRS) whose names match GEAR_NAME_REGEXP. test also runs the crates that depend on them.
help.GEAR_FEATURES.usage := GEAR_FEATURES=<f1,f2>
help.GEAR_FEATURES.targets := test $(help.GEAR_FEATURES.targets)
help.GEAR_FEATURES.text = Extra Cargo features, passed as --features.
help.GEAR_TEST_ARGS.usage := GEAR_TEST_ARGS="<args>"
help.GEAR_TEST_ARGS.targets := test
help.GEAR_TEST_ARGS.text = Extra cargo nextest arguments, e.g. GEAR_TEST_ARGS="--retries 2".
help.GEAR_CLIPPY_ARGS.usage := GEAR_CLIPPY_ARGS="<args>"
help.GEAR_CLIPPY_ARGS.targets := clippy
help.GEAR_CLIPPY_ARGS.text = Clippy arguments for GEAR= runs. Default: $(GEAR_CLIPPY_ARGS)

# -------- Utility macros --------

BANNER_WIDTH ?= 80
BANNER_RULE ?= ━
BANNER_PREFIX ?= ▶

define print_target_banner
	@timestamp=$$(date '+%Y-%m-%d %H:%M:%S %Z'); \
	line=$$(printf '%*s' "$(BANNER_WIDTH)" '' | sed 's/ /$(BANNER_RULE)/g'); \
	if [ -t 1 ] && [ -z "$${NO_COLOR:-}" ]; then \
		bold=$$(printf '\033[1m'); dim=$$(printf '\033[2m'); cyan=$$(printf '\033[36m'); reset=$$(printf '\033[0m'); \
	else \
		bold=; dim=; cyan=; reset=; \
	fi; \
	printf '\n%s%s%s\n%s%s make %s%s %s[%s]%s\n%s%s%s\n' "$$cyan" "$$line" "$$reset" "$$bold" "$(BANNER_PREFIX)" "$@" "$$reset" "$$dim" "$$timestamp" "$$reset" "$$cyan" "$$line" "$$reset"
endef

define check_tool
    @command -v $(1) >/dev/null || (echo "ERROR: $(1) is not installed. Run 'make setup' to install required tools." && exit 1)
endef

# check_tool_version(tool, requirement)
# Verify a tool satisfies a semver requirement. Exits with an error if not.
# Skipped in CI (GITHUB_ACTIONS is set) where tools are pinned by the workflow.
ifndef GITHUB_ACTIONS
define check_tool_version
    @cargo gears tools check-version $(1) '$(2)' >/dev/null 2>&1 \
	|| (echo "ERROR: $(1) $(2) is required. Run: cargo install $(1) --version '$(2)' to install/upgrade." && exit 1)
endef
else
check_tool_version = @true
endif

# Literal comma for $(call ...) arguments such as --features a$(comma)b.
comma := ,

# Line break. In a recipe, $(foreach x,$(LIST),<command>$(newline)) runs one
# command line per item, in order.
define newline


endef

empty :=
# Literal tab (field separator for the help parameters below).
tab := $(empty)	$(empty)

# Parameters shown by `make help` and `make help-<target>`. Register one next
# to the code that reads it (this file lists its own at the end):
#   HELP_PARAMS += NAME
#   help.NAME.usage   := NAME=<value>
#   help.NAME.targets := target ...          (extend with +=)
#   help.NAME.text     = One or two sentences.
#   help.NAME.values   = $(LIST)            (optional)
# `text` and `values` are expanded when help runs, so they can name variables.

# nextest_run(report-name, args[, env-prefix])
# Run `cargo nextest run` under $(NEXTEST_PROFILE). When JUNIT_DIR is set, copy
# this run's junit.xml to $(JUNIT_DIR)/<report-name>.xml -- also when tests
# fail -- so sequential runs in one job don't overwrite each other's report.
# The stale report is removed first: a run that fails to build must leave no
# report rather than the previous run's. The saved report is then given
# source locations by $(GEARS_JUNIT_ENRICH), if set ($(PYTHON_BOOTSTRAP), no venv --
# best effort, never fails the run); inside GitHub Actions it can also annotate
# the failing lines and write the job summary. The exit code is nextest's,
# except that a passing run which leaves no report fails: the report is the
# output CI depends on. After a failing run a missing report only warns.
define nextest_run
	@junit="$(NEXTEST_STORE_DIR)/$(NEXTEST_PROFILE)/junit.xml"; \
	rm -f "$$junit"; \
	(set -x; $(3) cargo nextest run --profile $(NEXTEST_PROFILE) $(2)); rc=$$?; \
	if [ -n "$(JUNIT_DIR)" ]; then \
		mkdir -p "$(JUNIT_DIR)"; \
		if [ -f "$$junit" ] && cp "$$junit" "$(JUNIT_DIR)/$(1).xml"; then \
			$(if $(GEARS_JUNIT_ENRICH),$(PYTHON_BOOTSTRAP) $(GEARS_JUNIT_ENRICH) $${GITHUB_ACTIONS:+--github} "$(JUNIT_DIR)/$(1).xml" \
				|| echo "::warning::could not add source locations to the JUnit report for '$(1)'",:); \
		elif [ $$rc -eq 0 ]; then \
			echo "::error::nextest passed but no JUnit report was saved for '$(1)'"; rc=1; \
		else echo "::warning::nextest produced no JUnit report for '$(1)' (build or setup failure?)"; fi; \
	fi; \
	exit $$rc
endef

help.NEXTEST_PROFILE.usage := NEXTEST_PROFILE=<profile>
help.NEXTEST_PROFILE.text = nextest profile for runs made with the nextest_run macro; CI uses ci. Default: $(NEXTEST_PROFILE).
help.JUNIT_DIR.usage := JUNIT_DIR=<dir>
help.JUNIT_DIR.text = Copy the JUnit report of each nextest_run call to <dir>/<name>.xml.

define check_rustup_component
    @command -v rustup >/dev/null || (echo "ERROR: rustup not installed. Install rustup or run 'make setup'." && exit 1)
	@rustup component list --installed | grep -q '^$(1)' || (echo "ERROR: $(1) component not installed. Run 'rustup component add $(1)' or 'make setup'." && exit 1)
endef

# Generic server start/stop with cleanup (cross-platform: Linux, Mac, Windows)
# Usage: $(call start_server_and_wait,<command>,<health_url>,<max_wait_seconds>)
# Args:
#   1. command - Full command to start the server (e.g., cargo run --bin server)
#   2. health_url - URL to poll for server readiness (e.g., http://localhost:8080/health)
#   3. max_wait_seconds - Maximum time to wait for server to be ready (e.g., 300)
# Returns: Sets $$SERVER_PID variable for use in the recipe
# Cleanup: Automatically kills server on EXIT/INT/TERM (normal or error)
# Features:
#   - Cross-platform: Works on Linux, Mac, and Windows (Git Bash/WSL/MSYS2)
#   - Logs server output to temp directory for debugging
#   - Exponential backoff polling (1s, 2s, 4s, 8s intervals)
#   - Detects if server dies unexpectedly during startup
#   - Graceful shutdown with SIGTERM, then SIGKILL if needed (or taskkill on Windows)
# Example:
#   @$(call start_server_and_wait,cargo run --bin my-server,http://localhost:8080/health,60); \
#   curl http://localhost:8080/api/data -o output.json
define start_server_and_wait
	TEMP_DIR=$$(if [ -n "$$TEMP" ]; then echo "$$TEMP"; elif [ -n "$$TMP" ]; then echo "$$TMP"; else echo "/tmp"; fi); \
	LOG_FILE="$$TEMP_DIR/server-$$$$.log"; \
	HEALTH_PORT=$$(echo "$(2)" | sed -E 's|.*://[^:]+:([0-9]+).*|\1|'); \
	if [ -n "$$HEALTH_PORT" ] && command -v lsof >/dev/null 2>&1; then \
		STALE_PIDS=$$(lsof -tiTCP:$$HEALTH_PORT -sTCP:LISTEN 2>/dev/null); \
		if [ -n "$$STALE_PIDS" ]; then \
			echo "Killing stale LISTEN processes on port $$HEALTH_PORT (PIDs: $$STALE_PIDS)"; \
			echo "$$STALE_PIDS" | xargs kill 2>/dev/null || true; \
			sleep 1; \
		fi; \
	fi; \
	$(1) > "$$LOG_FILE" 2>&1 & \
	SERVER_PID=$$!; \
	echo "Server started with PID: $$SERVER_PID (log: $$LOG_FILE)"; \
	is_process_running() { \
		if command -v kill >/dev/null 2>&1; then \
			kill -0 $$1 2>/dev/null; \
		elif command -v tasklist >/dev/null 2>&1; then \
			tasklist /FI "PID eq $$1" 2>NUL | grep -q "$$1"; \
		else \
			ps -p $$1 >/dev/null 2>&1; \
		fi; \
	}; \
	kill_process() { \
		PID_TO_KILL=$$1; \
		FORCE=$$2; \
		if command -v kill >/dev/null 2>&1; then \
			if [ "$$FORCE" = "force" ]; then \
				kill -9 $$PID_TO_KILL 2>/dev/null || true; \
			else \
				kill $$PID_TO_KILL 2>/dev/null || true; \
			fi; \
		elif command -v taskkill >/dev/null 2>&1; then \
			if [ "$$FORCE" = "force" ]; then \
				taskkill /PID $$PID_TO_KILL /F /T 2>NUL || true; \
			else \
				taskkill /PID $$PID_TO_KILL /T 2>NUL || true; \
			fi; \
		fi; \
	}; \
	cleanup_server() { \
		if is_process_running $$SERVER_PID; then \
			echo "Stopping server (PID $$SERVER_PID)..."; \
			kill_process $$SERVER_PID; \
			sleep 1; \
			if is_process_running $$SERVER_PID; then \
				echo "Server still running, forcing shutdown..."; \
				kill_process $$SERVER_PID force; \
				sleep 1; \
			fi; \
			wait $$SERVER_PID 2>/dev/null || true; \
			echo "Server stopped."; \
		fi; \
	}; \
	trap cleanup_server EXIT INT TERM; \
	echo "Waiting for $(2) to become ready..."; \
	ELAPSED=0; MAX_WAIT=$(3); SLEEP=1; \
	while ! curl -fsS "$(2)" -o /dev/null 2>/dev/null; do \
		if ! is_process_running $$SERVER_PID; then \
			echo "ERROR: Server process died unexpectedly. Check $$LOG_FILE"; \
			exit 1; \
		fi; \
		if [ $$ELAPSED -ge $$MAX_WAIT ]; then \
			echo "ERROR: Server did not become ready in time. Check $$LOG_FILE"; \
			exit 1; \
		fi; \
		echo "Waiting for server... ($$ELAPSED s)"; \
		sleep $$SLEEP; \
		ELAPSED=$$((ELAPSED + SLEEP)); \
		SLEEP=$$((SLEEP < 8 ? SLEEP*2 : 8)); \
	done; \
	echo "Server is ready!"
endef

# -------- Defaults --------

# Show the help message with list of commands and parameters (default target)
help:
	$(call print_target_banner)
	@$(PYTHON_BOOTSTRAP) -c "$$GEARS_HELP_PY" $(gears_help_files)

# Show one target's full description and parameters, e.g. make help-test
help-%:
	@$(PYTHON_BOOTSTRAP) -c "$$GEARS_HELP_PY" --target $* $(gears_help_files)

# This file first, then the project's make files.
gears_help_files = $(GEARS_MK_FILE) $(filter-out $(GEARS_MK_FILE),$(MAKEFILE_LIST))
# HELP_PARAMS for the help script: one line per parameter, tab-separated usage,
# targets, text and values. The script and the parameters go in the environment,
# so no shell quoting.
gears_help_params = $(foreach p,$(HELP_PARAMS),$(strip $(help.$(p).usage))$(tab)$(strip $(help.$(p).targets))$(tab)$(strip $(help.$(p).text))$(tab)$(strip $(help.$(p).values))$(newline))
help: export GEARS_HELP_PARAMS = $(gears_help_params)
help-%: export GEARS_HELP_PARAMS = $(gears_help_params)
help: export GEARS_HELP_PY = $(gears_help_py)
help-%: export GEARS_HELP_PY = $(gears_help_py)

# The help script (Python 3, standard library only).
define gears_help_py
#!/usr/bin/env python3
"""Grouped Makefile help generator.

This script scans Makefiles in include order and prints targets grouped by section
headers of the form `# -------- Section Name --------`.
The comment block right above a target (`#` lines for primary targets, `##` lines
for secondary ones) describes it; its first line is the summary.

Parameters come from the GEARS_HELP_PARAMS environment variable, which the
`help` targets of gears.mk fill from HELP_PARAMS: one line per parameter,
tab-separated usage, targets, text and values. Text between `define` and
`endef` (macros and this script) is not scanned.

    <script> gears.mk project.mk ...                 # grouped overview
    <script> --target <name> gears.mk project.mk ... # one target in full
"""

from __future__ import annotations

import os
import re
import sys
import textwrap
from dataclasses import dataclass, field
from pathlib import Path

# The part after `target:` in a target-specific variable assignment.
_TARGET_VARIABLE = re.compile(r"\s*(?:(?:override|export|private)\s+)*[A-Za-z_][\w.-]*\s*(?:[:?+!]|::)?=")

NAME_WIDTH = 22
PARAM_WIDTH = 24
TEXT_WIDTH = 100


@dataclass
class Target:
    level: int  # 1 = primary (`#`), 2 = secondary (`##`)
    name: str
    lines: list[str]
    source: Path

    @property
    def summary(self) -> str:
        return self.lines[0].strip()


@dataclass
class Param:
    usage: str
    targets: list[str]
    text: str
    values: str


@dataclass
class Colors:
    primary: str = ""
    secondary: str = ""
    reset: str = ""


@dataclass
class Parsed:
    section_order: list[str] = field(default_factory=list)
    entries: dict[str, list[Target]] = field(default_factory=dict)


def _color_support() -> bool:
    """Return True if it looks like the terminal supports ANSI colors."""

    if not sys.stdout.isatty():
        return False
    if os.environ.get("NO_COLOR") is not None:
        return False
    term = os.environ.get("TERM", "")
    if term.lower() == "dumb":
        return False
    return True


def _is_help_target(name: str) -> bool:
    return bool(name) and not name.startswith(".") and name == name.lower() and not any(
        char.isspace() for char in name
    )


def _display(name: str) -> str:
    # `help-%` is the pattern rule behind `make help-<target>`.
    return name.replace("%", "<target>")


def _comment_text(line: str) -> tuple[int, str] | None:
    """Return (level, text) for a comment line, or None for any other line."""
    if line.startswith("##"):
        return 2, line[3:] if line.startswith("## ") else line[2:]
    if line.startswith("#"):
        return 1, line[2:] if line.startswith("# ") else line[1:]
    return None


def parse(paths: list[Path]) -> Parsed:
    parsed = Parsed()
    section = ""
    # Comment block waiting for its target: (level of first line, lines).
    pending: tuple[int, list[str]] | None = None

    for path in dict.fromkeys(paths):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except OSError as exc:
            print(f"ERROR: cannot read {path}: {exc}", file=sys.stderr)
            sys.exit(1)
        # Sections do not carry over: targets before a file's first header are "Other targets".
        section = ""
        pending = None
        in_define = False

        for stripped in lines:
            # Skip define ... endef bodies: macro and script text, not targets.
            if in_define:
                in_define = stripped.strip() != "endef"
                continue
            if stripped.startswith("define "):
                in_define = True
                pending = None
                continue

            # A blank line ends a comment block: the comment described no target.
            if not stripped.strip():
                pending = None
                continue

            # Section headers: e.g. "# -------- Code coverage --------"
            if stripped.startswith("# ---"):
                section = stripped.strip("#- ").strip()
                if section not in parsed.entries:
                    parsed.entries[section] = []
                    parsed.section_order.append(section)
                pending = None
                continue

            comment = _comment_text(stripped)
            if comment is not None:
                level, text = comment
                if pending is None:
                    if text.strip():
                        pending = (level, [text])
                else:
                    pending[1].append(text)
                continue

            # Target lines: name:
            if pending and ":" in stripped and not stripped.startswith("\t"):
                name_part, rest = stripped.split(":", 1)
                name = name_part.strip()
                # `name := value` and `name ::= value` are assignments, not targets;
                # `target: VAR = value` sets a target-specific variable.
                if (rest.lstrip(":").startswith("=") or _TARGET_VARIABLE.match(rest)
                        or not _is_help_target(name)):
                    pending = None
                    continue
                if section not in parsed.entries:
                    parsed.entries[section] = []
                    parsed.section_order.append(section)
                level, block = pending
                parsed.entries[section].append(Target(level, name, block, path))
                pending = None
    return parsed


def parse_params(raw: str) -> list[Param]:
    params = []
    for line in raw.splitlines():
        if not line.strip():
            continue
        usage, targets, text, values = (line.split("\t") + ["", "", "", ""])[:4]
        if usage.strip():
            params.append(Param(usage.strip(), targets.split(), " ".join(text.split()), " ".join(values.split())))
    return params


def _wrapped(label: str, text: str, indent: int) -> list[str]:
    prefix = " " * indent
    return textwrap.wrap(f"{label}{text}", width=TEXT_WIDTH, initial_indent=prefix,
                         subsequent_indent=prefix + " " * len(label), break_on_hyphens=False, break_long_words=False)


def print_params(params: list[Param], c: Colors, show_targets: bool) -> None:
    indent = PARAM_WIDTH + 3
    for param in params:
        details = _wrapped("", param.text, indent) or [""]
        if len(param.usage) < PARAM_WIDTH:
            # Usage and the first text line share a line.
            print(f"  {c.primary}{param.usage:<{PARAM_WIDTH}}{c.reset} {c.secondary}{details[0].lstrip()}{c.reset}")
            details = details[1:]
        else:
            print(f"  {c.primary}{param.usage}{c.reset}")
        if show_targets and param.targets:
            details += _wrapped("Targets: ", " ".join(param.targets), indent)
        if param.values:
            details += _wrapped("Values: ", param.values, indent)
        for line in details:
            print(f"{c.secondary}{line}{c.reset}")


def print_overview(parsed: Parsed, params: list[Param], c: Colors) -> None:
    first_section_printed = False
    for section in parsed.section_order or [""]:
        targets = parsed.entries.get(section, [])
        if not targets:
            continue
        first_section_printed = True
        print()
        print(f"{c.primary}{section or 'Other targets'}:{c.reset}")
        for target in sorted(targets, key=lambda t: t.level):
            name = _display(target.name)
            if target.level == 1:
                print(f" {c.primary}* {name:<{NAME_WIDTH}}{c.reset} - {c.secondary}{target.summary}{c.reset}")
            else:
                print(f"   {c.secondary}{name:<{NAME_WIDTH}}{c.reset} - {c.secondary}{target.summary}{c.reset}")

    if params:
        print()
        print(f"{c.primary}Parameters{c.reset} {c.secondary}(set on the command line, e.g. make test GEAR=file-parser):{c.reset}")
        print_params(params, c, show_targets=True)

    if first_section_printed:
        print(
            f"\n{c.secondary}NOTE: '{c.primary}*{c.secondary}' marks primary commands in each section; "
            f"unmarked entries are secondary commands.{c.reset}"
        )
        print()
        print(f"{c.secondary}Run '{c.primary}make help-<target>{c.secondary}' for a target's full description and parameters.{c.reset}")
        listed = {t.name for targets in parsed.entries.values() for t in targets}
        if "build" in listed:
            print(f"{c.secondary}Run '{c.primary}make build{c.secondary}' to build the release binary{c.reset}")
        if "all" in listed:
            print(f"{c.secondary}Run '{c.primary}make all{c.secondary}' to run all necessary quality checks and tests and then build the release binary{c.reset}")


def print_target(name: str, parsed: Parsed, params: list[Param], c: Colors) -> int:
    targets = [t for ts in parsed.entries.values() for t in ts if t.name == name]
    # Parameters specific to this target first.
    own_params = sorted((p for p in params if name in p.targets), key=lambda p: len(p.targets))
    if not targets and not own_params:
        print(f"No help for '{name}'. Run 'make help' to list the documented targets.", file=sys.stderr)
        return 1
    print()
    print(f"{c.primary}make {name}{c.reset}")
    for target in targets:
        print(f"{c.secondary}  defined in {target.source.as_posix()}{c.reset}")
        print()
        block = list(target.lines)
        while block and not block[-1].strip():
            block.pop()
        for line in block:
            print(f"  {line}".rstrip())
    if own_params:
        print()
        print(f"{c.primary}Parameters:{c.reset}")
        print_params(own_params, c, show_targets=False)
    return 0


def generate_help(*paths: Path, target: str | None = None, params: list[Param] | None = None) -> int:
    c = Colors("\033[1;37m", "\033[0;90m", "\033[0m") if _color_support() else Colors()
    parsed = parse(list(paths))
    if params is None:
        params = parse_params(os.environ.get("GEARS_HELP_PARAMS", ""))
    if target is not None:
        return print_target(target, parsed, params, c)
    print_overview(parsed, params, c)
    return 0


def main(argv: list[str]) -> int:
    args = argv[1:]
    target = None
    if len(args) >= 2 and args[0] == "--target":
        target, args = args[1], args[2:]
    makefiles = [Path(name) for name in args] or [Path("Makefile")]
    return generate_help(*makefiles, target=target)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
endef

# -------- Code safety checks --------
#
# Tool Comparison - What Each Tool Checks:
# +-------------+----------------------------------------------------------------------+
# | Tool        | Checks Performed                                                     |
# +-------------+----------------------------------------------------------------------+
# | clippy      | - Idiomatic Rust patterns (e.g., use of .iter() vs into_iter())      |
# |             | - Common mistakes (e.g., unnecessary clones, redundant closures)     |
# |             | - Performance issues (e.g., inefficient string operations)           |
# |             | - Style violations (e.g., naming conventions, formatting)            |
# |             | - Suspicious constructs (e.g., comparison to NaN, unused results)    |
# |             | - Complexity warnings (e.g., too many arguments, cognitive load)     |
# +-------------+----------------------------------------------------------------------+
# | kani        | - Memory safety proofs (buffer overflows, null pointer dereferences) |
# |             | - Arithmetic overflow/underflow in all possible execution paths      |
# |             | - Assertion violations (panics, unwrap failures)                     |
# |             | - Undefined behavior detection                                       |
# |             | - Concurrency issues (data races, deadlocks) with #[kani::proof]     |
# |             | - Custom invariants and postconditions verification                  |
# +-------------+----------------------------------------------------------------------+
# | geiger      | - Unsafe blocks in your code and dependencies                        |
# |             | - FFI (Foreign Function Interface) calls                             |
# |             | - Raw pointer dereferences                                           |
# |             | - Mutable static variables access                                    |
# |             | - Inline assembly usage                                              |
# |             | - Dependency tree visualization of unsafe code usage                 |
# +-------------+----------------------------------------------------------------------+
# | lint        | - Compiler warnings treated as errors (unused variables, imports)    |
# |             | - Dead code detection                                                |
# |             | - Type inference failures                                            |
# |             | - Deprecated API usage                                               |
# |             | - Missing documentation warnings                                     |
# |             | - Ensures clean compilation across all targets and features          |
# +-------------+----------------------------------------------------------------------+

.PHONY: fmt clippy clippy-deep kani safety lint install-tools dylint shear

# Check code formatting
fmt:
	$(call print_target_banner)
	$(call check_rustup_component,rustfmt)
	$(if $(GEAR),cargo fmt $(GEAR_PKGS) --check,cargo fmt --all --check)

# Fast two-pass clippy used in PR CI.
#
# Pass 1 — one workspace-wide all-features run. Covers every crate, every
#   target and every additive feature combination.
#
# Pass 2 — cargo-hack --each-feature on CLIPPY_HACK_CRATES, the crates with
#   real #[cfg(feature = "...")] guards (e.g. mutually-exclusive backends).
#   Skipped when CLIPPY_HACK_CRATES is empty.
#
# Use `make clippy-deep` for the full per-feature matrix of every crate.
clippy:
	$(call print_target_banner)
	$(call check_rustup_component,clippy)
ifeq ($(GEAR),)
ifneq ($(strip $(CLIPPY_HACK_CRATES)),)
	$(call check_tool,cargo-hack)
endif
	cargo clippy --workspace --all-targets --all-features $(CLIPPY_FLAGS)
ifneq ($(strip $(CLIPPY_HACK_CRATES)),)
	cargo hack clippy $(CLIPPY_HACK_CRATES) --all-targets --each-feature $(CLIPPY_HACK_EXCLUDE) $(CLIPPY_FLAGS)
endif
else
	cargo clippy $(GEAR_PKGS) $(GEAR_CLIPPY_ARGS)
endif

## Full feature-matrix clippy: one pass per (crate × feature).
## ~182 runs — intended for nightly CI and pre-release validation, not PRs.
clippy-deep:
	$(call print_target_banner)
	$(call check_rustup_component,clippy)
	$(call check_tool,cargo-hack)
	cargo hack clippy --workspace --all-targets --each-feature $(CLIPPY_HACK_EXCLUDE) $(CLIPPY_FLAGS)

## The Kani Rust Verifier for checking safety of the code
kani:
	$(call print_target_banner)
	$(call check_tool,kani)
	cargo kani --workspace --all-features

## Check there are no compile time warnings
lint:
	$(call print_target_banner)
	RUSTFLAGS="-D warnings" cargo check $(if $(GEAR),$(GEAR_PKGS),--workspace) --all-targets --all-features

install-tools:
	$(call print_target_banner)
	@cargo gears tools check-version cargo-gears '>=$(CARGO_GEARS_VERSION)' >/dev/null 2>&1 \
	|| (echo "Installing cargo-gears >= $(CARGO_GEARS_VERSION)..." && cargo install --locked cargo-gears@$(CARGO_GEARS_VERSION))
	@cargo gears tools check-version cargo-nextest '>=$(NEXTEST_MIN_VERSION)' >/dev/null 2>&1 \
	|| (echo "Installing cargo-nextest >= $(NEXTEST_MIN_VERSION)..." && cargo install --locked cargo-nextest)
	@cargo gears tools check-version cargo-deny '>=$(DENY_MIN_VERSION)' >/dev/null 2>&1 \
	|| (echo "Installing cargo-deny >= $(DENY_MIN_VERSION)..." && cargo install --locked cargo-deny)

# Run architecture lints via cargo-gears (see Gears.toml for configuration).
dylint:
	$(call print_target_banner)
	$(call check_tool,cargo-gears)
	cargo gears lint --dylint

# Check for unused dependencies with cargo-shear.
shear:
	$(call print_target_banner)
	$(call check_tool,cargo-shear)
	cargo +$(RUST_NIGHTLY) shear --expand --deny-warnings

help.RUST_NIGHTLY.usage := RUST_NIGHTLY=<toolchain>
help.RUST_NIGHTLY.targets := shear
help.RUST_NIGHTLY.text = Nightly toolchain for shear. Default: $(RUST_NIGHTLY).

# Run all code safety checks
safety: clippy kani lint dylint # geiger
	$(call print_target_banner)
	@echo "OK. Rust Safety Pipeline complete"

# -------- Code security checks --------

.PHONY: deny fips-policy security

# Check licenses and dependencies
deny:
	$(call print_target_banner)
	$(call check_tool,cargo-deny)
	$(call check_tool_version,cargo-deny,>=$(DENY_MIN_VERSION))
	cargo deny check

## FIPS dependency-graph policy (DENY_FIPS_CONFIG, default deny-fips.toml; see ADR 0005).
## Refuses the build if any non-FIPS-validated crypto crate enters the
## --features fips dep graph. Build-time analogue of Go 1.25 fips140=only.
## Run on every PR that touches deps.
fips-policy:
	$(call print_target_banner)
	$(call check_tool,cargo-deny)
	$(call check_tool_version,cargo-deny,>=$(DENY_MIN_VERSION))
	cargo deny --config $(DENY_FIPS_CONFIG) check bans

# Run all security checks (fips-policy only when DENY_FIPS_CONFIG exists)
security: deny $(if $(wildcard $(DENY_FIPS_CONFIG)),fips-policy)
	$(call print_target_banner)

# -------- Studio --------

.PHONY: cfs-ensure cfs-repair cfs-validate cfs-validate-kits cfs-spec-coverage

# pipx installs the cfs CLI into ~/.local/bin.
export PATH := $(HOME)/.local/bin:$(PATH)

# Validate Constructor Studio artifacts (specs, code, templates).
cfs-validate: cfs-repair
	$(call print_target_banner)
	$(CFS) validate && echo "OK. Constructor Studio validation PASSED" || (echo "ERROR: Constructor Studio validation FAILED"; exit 1)

# Install the Constructor Studio CLI (cfs) with pipx if it is missing
# Works even when generated runtime files are ignored locally or absent in a
# clean checkout.
cfs-ensure:
	$(call print_target_banner)
	@if ! command -v $(CFS) >/dev/null 2>&1; then \
		echo "cfs not found; installing $(CFS_PIPX_SPEC) via pipx"; \
		if ! command -v pipx >/dev/null 2>&1; then \
			echo "ERROR: pipx is required before running this target"; \
			exit 1; \
		else \
			pipx install $(CFS_PIPX_SPEC); \
		fi; \
	fi
	@if ! command -v $(CFS) >/dev/null 2>&1; then \
		echo "ERROR: cfs was installed but is not on PATH"; \
		exit 1; \
	fi

## Repair ignored/generated Constructor Studio runtime files before validation.
cfs-repair: cfs-ensure
	$(call print_target_banner)
	$(CFS) init --yes

## Check Constructor Studio spec-to-code traceability coverage.
cfs-spec-coverage: cfs-repair
	$(call print_target_banner)
	$(CFS) spec-coverage --min-coverage 80

## Validate registered Constructor Studio kits.
cfs-validate-kits: cfs-repair
	$(call print_target_banner)
	$(CFS) validate-kits

# -------- Development and auto fix --------

.PHONY: dev dev-fmt dev-clippy dev-test

## Run tests in development mode
dev-test: install-tools
	$(call print_target_banner)
	cargo nextest run --workspace

## Auto-fix code formatting
dev-fmt:
	$(call print_target_banner)
	cargo fmt --all

## Auto-fix clippy warnings
dev-clippy:
	$(call print_target_banner)
	cargo clippy --workspace --all-targets --all-features --fix --allow-dirty

# Auto-fix formatting and clippy warnings
dev: dev-fmt dev-clippy dev-test
	$(call print_target_banner)

# -------- Tests --------

.PHONY: test

# Run all tests, or a single gear when GEAR=<gear> is set.
# When GEAR= is set, cargo gears ls packages finds matching crates + their
# transitive reverse deps (every workspace crate that depends on them).
test: install-tools
	$(call print_target_banner)
ifeq ($(GEAR),)
	cargo nextest run --workspace $(GEAR_FEATURE_ARGS) $(GEAR_TEST_ARGS)
else
	@GEAR_SCOPE=$$(cargo gears ls packages --dirs $(GEAR_PKG_DIRS) --filter '$(GEAR_NAME_REGEXP)' --include-rdeps -f cargo-flags) || exit 1; \
	echo "cargo nextest run $$GEAR_SCOPE $(GEAR_FEATURE_ARGS) $(GEAR_TEST_ARGS) $(GEAR_NO_TESTS_FLAG)"; \
	cargo nextest run $$GEAR_SCOPE $(GEAR_FEATURE_ARGS) $(GEAR_TEST_ARGS) $(GEAR_NO_TESTS_FLAG)
endif

# -------- Fuzzing --------

.PHONY: fuzz fuzz-build fuzz-list fuzz-run fuzz-clean fuzz-corpus

## Check cargo-fuzz is installed (required for fuzzing)
fuzz-install:
	$(call print_target_banner)
	$(call check_tool,cargo-fuzz)

## Build all fuzz targets
fuzz-build: fuzz-install
	$(call print_target_banner)
	cargo +nightly fuzz build --fuzz-dir $(FUZZ_DIR)

## List all available fuzz targets
fuzz-list: fuzz-install
	$(call print_target_banner)
	cargo +nightly fuzz list --fuzz-dir $(FUZZ_DIR)

## Run a specific fuzz target (use FUZZ_TARGET=name)
## Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter FUZZ_SECONDS=60
fuzz-run: fuzz-install
	$(call print_target_banner)
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-run FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz run --fuzz-dir $(FUZZ_DIR) $(FUZZ_TARGET) -- -max_total_time=$(or $(FUZZ_SECONDS),60)

# Run all fuzz targets for a short time (smoke test)
fuzz: fuzz-build
	$(call print_target_banner)
	@echo "Running all fuzz targets for 30 seconds each..."
	@FAILED=0; \
	for target in $$(cargo +nightly fuzz list --fuzz-dir $(FUZZ_DIR)); do \
		echo "=== Fuzzing $$target ==="; \
		cargo +nightly fuzz run --fuzz-dir $(FUZZ_DIR) $$target -- -max_total_time=30 || FAILED=1; \
	done; \
	if [ $$FAILED -ne 0 ]; then \
		echo "Fuzzing found crashes. Check $(FUZZ_DIR)/artifacts/ for details."; \
		exit 1; \
	fi
	@echo "Fuzzing complete. No crashes found."

## Clean fuzzing artifacts and corpus
fuzz-clean:
	$(call print_target_banner)
	rm -rf $(FUZZ_DIR)/artifacts/
	rm -rf $(FUZZ_DIR)/corpus/*/
	rm -rf $(FUZZ_DIR)/target/

## Minimize corpus for a specific target
fuzz-corpus: fuzz-install
	$(call print_target_banner)
	@if [ -z "$(FUZZ_TARGET)" ]; then \
		echo "ERROR: FUZZ_TARGET is required. Example: make fuzz-corpus FUZZ_TARGET=fuzz_odata_filter"; \
		exit 1; \
	fi
	cargo +nightly fuzz cmin --fuzz-dir $(FUZZ_DIR) $(FUZZ_TARGET)

help.FUZZ_TARGET.usage := FUZZ_TARGET=<name>
help.FUZZ_TARGET.targets := fuzz-run fuzz-corpus
help.FUZZ_TARGET.text = Fuzz target to use; make fuzz-list prints them.
help.FUZZ_SECONDS.usage := FUZZ_SECONDS=<n>
help.FUZZ_SECONDS.targets := fuzz-run
help.FUZZ_SECONDS.text = How long to fuzz, in seconds. Default: 60.

# -------- CI --------

.PHONY: ci

# Run the CI pipeline locally (targets in GEARS_CI_TARGETS)
ci: $(GEARS_CI_TARGETS)
	$(call print_target_banner)

# Library parameters first in `make help`, then the project's.
HELP_PARAMS := GEAR GEAR_FEATURES GEAR_TEST_ARGS GEAR_CLIPPY_ARGS NEXTEST_PROFILE JUNIT_DIR \
	RUST_NIGHTLY FUZZ_TARGET FUZZ_SECONDS $(HELP_PARAMS)
endif
