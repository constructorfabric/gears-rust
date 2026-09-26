#!/usr/bin/env python3
"""
Validate publishable crates' packaging metadata statically.

`cargo publish`/`cargo package` enforce two rules that are otherwise only
caught at release time (which is expensive — it compiles the crate — and only
runs in the release job):

1. A declared `readme`/`license-file` path must exist relative to that
   crate's Cargo.toml.
2. Every normal/build dependency must carry a version requirement. A
   `path`-only dependency (no `version`) publishes fine locally but makes
   `cargo publish` fail with:

       all dependencies must have a version requirement specified when
       publishing. dependency `X` does not specify a version

   because the `path` is stripped on publish and crates.io needs a version
   to resolve the dependency.

And one rule that is ours rather than cargo's:

3. Every crate spells out its target section: a crate with `src/lib.rs`
   declares `[lib] name`, a crate with `src/main.rs` declares a `[[bin]]`
   covering it. Cargo is perfectly happy without either — it derives the
   name from the *package* name, `-` becoming `_` for a library and verbatim
   for a binary — and that is exactly the problem. The derived name moves
   whenever the package is renamed, silently, and a gear description restates
   the library name:

       package = cargo(crate_name = "cf-gears-api-gateway",
                       lib = "api_gateway", path = ".")

   (`api_gateway` is not derivable from that package name; it is what `[lib]`
   says.) So a silent rename is a wrong `gear.gdl` and a wrong
   `use <lib> as _;` in the code Gearbox generates from it. Declared, the name
   is a fact somebody has to change on purpose.

   Unlike the two above this has nothing to do with `cargo publish`, so it
   applies to every crate in the workspace, published or not.

This script checks all three rules with no cargo invocation at all, so it's
cheap enough to run on every CI build.

Exit codes:
  0 - All crates pass every applicable check
  1 - One or more crates violate a rule
"""

import sys
import tomllib
from pathlib import Path
from typing import List, Tuple

# Directories to skip entirely while walking for Cargo.toml files.
SKIP_DIR_NAMES = {"target", ".git"}

# Relative to the workspace root, directories excluded from the workspace
# (mirrors `exclude` in the root Cargo.toml) — not real workspace members,
# so their packaging metadata is irrelevant here.
EXCLUDED_DIRS = {"tools/fuzz"}

# Relative to the workspace root, directories exempt from rule 3 only (they
# are still checked for packaging metadata). `examples/oop-gears/` is being
# rewritten in an open PR (#4663) and its four crates predate the rule; they
# are left to that PR rather than edited underneath it. Delete this once the
# PR merges — the rule is then unconditional.
TARGET_RULE_EXEMPT_DIRS = {"examples/oop-gears"}


def is_under(rel_dir: str, directories) -> bool:
    """True if `rel_dir` is one of `directories` or sits inside one."""
    return any(rel_dir == d or rel_dir.startswith(d + "/") for d in directories)


def find_manifests(workspace_root: Path) -> List[Path]:
    """Find all Cargo.toml files in the workspace, skipping excluded dirs."""
    manifests = []
    for path in workspace_root.rglob("Cargo.toml"):
        rel_parts = path.relative_to(workspace_root).parts
        if any(part in SKIP_DIR_NAMES for part in rel_parts):
            continue
        if is_under("/".join(rel_parts[:-1]), EXCLUDED_DIRS):
            continue
        manifests.append(path)
    return sorted(manifests)


def is_publishable(package: dict) -> bool:
    """Mirror cargo's `publish` field semantics: only `false`/`[]` disable publishing."""
    publish = package.get("publish")
    if publish is False:
        return False
    if isinstance(publish, list) and len(publish) == 0:
        return False
    return True


def collapse_inline_tables(manifest_text: str) -> str:
    """Join multi-line inline tables onto a single line.

    Cargo's TOML parser tolerates inline tables that wrap across lines
    (commonly used for local path dependencies), but Python's strict
    `tomllib` rejects them. Collapsing newlines that fall inside `{ ... }`
    (while respecting quoted strings) makes the manifest parseable by
    `tomllib` without changing its meaning for our purposes.
    """
    out = []
    depth = 0
    in_str = False
    str_ch = ""
    escaped = False
    for ch in manifest_text:
        if in_str:
            out.append(ch)
            if escaped:
                escaped = False
            elif ch == "\\" and str_ch == '"':
                escaped = True
            elif ch == str_ch:
                in_str = False
            continue
        if ch in ('"', "'"):
            in_str = True
            str_ch = ch
            out.append(ch)
            continue
        if ch == "{":
            depth += 1
            out.append(ch)
        elif ch == "}":
            depth = max(0, depth - 1)
            out.append(ch)
        elif ch == "\n" and depth > 0:
            out.append(" ")
        else:
            out.append(ch)
    return "".join(out)


def parse_manifest(manifest_text: str) -> dict:
    """Parse a full Cargo.toml, tolerating multi-line inline tables."""
    return tomllib.loads(collapse_inline_tables(manifest_text))


def iter_publish_deps(data: dict):
    """Yield `(table_label, name, spec)` for every normal/build dependency,
    including target-specific ones (`[target.'cfg(...)'.dependencies]`).

    Dev-dependencies are excluded because cargo strips them on publish.
    """
    containers = [("", data)]
    targets = data.get("target")
    if isinstance(targets, dict):
        containers += [
            (f"target.{cfg}.", tables)
            for cfg, tables in targets.items()
            if isinstance(tables, dict)
        ]

    for prefix, container in containers:
        for table_name in ("dependencies", "build-dependencies"):
            table = container.get(table_name)
            if isinstance(table, dict):
                for name, spec in table.items():
                    yield f"{prefix}{table_name}", name, spec


def _has_pathless_version(spec) -> bool:
    """True if `spec` is a `path` dependency with no `version` requirement."""
    return isinstance(spec, dict) and "path" in spec and "version" not in spec


def find_dep_violations(data: dict, ws_deps: dict) -> List[Tuple[str, str]]:
    """Return `(table_label, dep_name)` for deps that break `cargo publish`
    because they resolve to a `path` with no `version`.

    Inline path deps are checked directly; `workspace = true` deps are resolved
    against the shared `[workspace.dependencies]` table (`ws_deps`).
    """
    violations: List[Tuple[str, str]] = []
    for label, name, spec in iter_publish_deps(data):
        if isinstance(spec, dict) and spec.get("workspace") is True:
            if _has_pathless_version(ws_deps.get(name)):
                violations.append((f"{label} (workspace)", name))
        elif _has_pathless_version(spec):
            violations.append((label, name))
    return violations


def check_file_metadata(
    package: dict, crate_dir: Path, workspace_root: Path
) -> List[Tuple[str, str, str]]:
    """Return (field, declared_value, expected_path) for missing readme/license files."""
    violations = []
    for field in ("readme", "license-file"):
        value = package.get(field)
        if not isinstance(value, str):
            continue  # absent, `false`, or workspace-inherited (never used as such today)
        if not (crate_dir / value).is_file():
            violations.append((field, value, str((crate_dir / value).relative_to(workspace_root))))
    return violations


def check_declared_targets(
    data: dict, package: dict, crate_dir: Path
) -> List[Tuple[str, str]]:
    """Return `(section, name_cargo_would_derive)` for each target left implicit.

    Presence of the source file is the trigger, because that is what makes
    cargo auto-discover a target at all. A crate with neither `src/lib.rs` nor
    `src/main.rs` has no target to name and is therefore silently in the clear
    — which is the right answer for the macro-test crates, whose whole
    contents are `tests/`.

    `src/bin/*.rs` is deliberately *not* required to be declared: those target
    names come from the file name, not from the package name, so renaming the
    package cannot move them. They are already explicit, which is all rule 3
    asks for.
    """
    violations: List[Tuple[str, str]] = []
    package_name = package.get("name")
    if not isinstance(package_name, str):
        return violations  # nothing to derive a name from; cargo will complain first

    if (crate_dir / "src" / "lib.rs").is_file():
        lib = data.get("lib")
        if not (isinstance(lib, dict) and isinstance(lib.get("name"), str)):
            violations.append(("[lib]", package_name.replace("-", "_")))

    if (crate_dir / "src" / "main.rs").is_file():
        declared = data.get("bin")
        declared = declared if isinstance(declared, list) else []
        # A `[[bin]]` for some `src/bin/*.rs` does not cover `src/main.rs`:
        # auto-discovery still applies to it, under the package's name. So the
        # entry has to be the one that claims `src/main.rs` — by path, or by
        # carrying the name cargo infers that path from.
        covers_main = any(
            isinstance(entry, dict)
            and (
                entry.get("path") in ("src/main.rs", "./src/main.rs")
                or entry.get("name") == package_name
            )
            for entry in declared
        )
        if not covers_main:
            violations.append(("[[bin]]", package_name))

    return violations


def main() -> int:
    script_dir = Path(__file__).parent
    workspace_root = script_dir.parent.parent

    manifests = find_manifests(workspace_root)
    if not manifests:
        print(f"Error: no Cargo.toml files found under {workspace_root}", file=sys.stderr)
        return 1

    checked = 0
    targets_checked = 0
    file_violations = []   # (manifest_path, [(field, declared, expected_rel), ...])
    dep_violations = []    # (manifest_path, [(table_path, dep_name), ...])
    target_violations = []  # (manifest_path, [(section, derived_name), ...])
    parse_errors = []      # (manifest_path, message)

    # First pass: parse every manifest once and record workspace roots so that
    # `workspace = true` dependencies can be resolved to their shared spec.
    parsed = {}                 # manifest_path -> data
    ws_deps_by_root = {}        # workspace-root dir -> [workspace.dependencies] table
    for manifest_path in manifests:
        try:
            data = parse_manifest(manifest_path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError as exc:
            parse_errors.append((manifest_path, str(exc)))
            continue
        parsed[manifest_path] = data
        workspace = data.get("workspace")
        if isinstance(workspace, dict):
            ws_deps = workspace.get("dependencies")
            ws_deps_by_root[manifest_path.parent] = ws_deps if isinstance(ws_deps, dict) else {}

    def governing_root(crate_dir: Path) -> Path | None:
        """The nearest ancestor directory that is a Cargo workspace root."""
        for ancestor in (crate_dir, *crate_dir.parents):
            if ancestor in ws_deps_by_root:
                return ancestor
        return None

    # Second pass: validate each crate.
    #
    # Rule 3 covers every crate this workspace builds, while rules 1 and 2 only
    # concern the ones `cargo publish` runs on — so the publish-only filters sit
    # *below* the target check rather than above it. A `publish = false` crate
    # still has to name its target; that is where most of the offenders were.
    for manifest_path, data in parsed.items():
        package = data.get("package")
        if package is None:
            continue

        # Crates in a separate nested workspace (e.g. `tools/dylint_lints`,
        # test fixtures) are never published alongside the root workspace, so
        # `cargo publish` never runs on them — and nothing here links them
        # either, so neither rule has anything to say. Skip to avoid false
        # positives.
        root = governing_root(manifest_path.parent)
        if root is not None and root != workspace_root:
            continue

        rel_dir = "/".join(manifest_path.relative_to(workspace_root).parts[:-1])
        if not is_under(rel_dir, TARGET_RULE_EXEMPT_DIRS):
            targets_checked += 1
            targets_missing = check_declared_targets(data, package, manifest_path.parent)
            if targets_missing:
                target_violations.append((manifest_path, targets_missing))

        if not is_publishable(package):
            continue

        # Per repo convention (see release-plz.toml) every crate that is
        # actually released declares an explicit `version`. A crate without
        # one is not a release target yet (WIP), so `cargo publish` never runs
        # on it — skip rather than flag its dependencies.
        if "version" not in package:
            continue

        checked += 1

        files_missing = check_file_metadata(package, manifest_path.parent, workspace_root)
        if files_missing:
            file_violations.append((manifest_path, files_missing))

        ws_deps = ws_deps_by_root.get(root, {}) if root is not None else {}
        deps_missing = find_dep_violations(data, ws_deps)
        if deps_missing:
            dep_violations.append((manifest_path, deps_missing))

    has_problems = bool(file_violations or dep_violations or target_violations or parse_errors)
    if has_problems:
        print("=" * 80, file=sys.stderr)
        print("PACKAGING METADATA VIOLATIONS DETECTED", file=sys.stderr)
        print("=" * 80, file=sys.stderr)

    if parse_errors:
        print(file=sys.stderr)
        print("The following manifests could not be parsed:", file=sys.stderr)
        print(file=sys.stderr)
        for manifest_path, message in parse_errors:
            print(f"  [X] {manifest_path.relative_to(workspace_root)}: {message}", file=sys.stderr)

    def report_group(header, groups, format_item):
        if not groups:
            return
        print(file=sys.stderr)
        print(header, file=sys.stderr)
        print(file=sys.stderr)
        for manifest_path, items in groups:
            print(f"  [X] {manifest_path.relative_to(workspace_root)}", file=sys.stderr)
            for item in items:
                print(f"      {format_item(item)}", file=sys.stderr)
            print(file=sys.stderr)

    report_group(
        "The following crates declare a `readme`/`license-file` path "
        "that does not exist. `cargo publish` will fail on these:",
        file_violations,
        lambda v: f'{v[0]} = "{v[1]}" -> missing {v[2]}',
    )
    report_group(
        "The following crates have `path` dependencies without a `version` "
        "requirement. `cargo publish` strips the path and requires a version:",
        dep_violations,
        lambda v: f'[{v[0]}] {v[1]} -> add a `version = "..."`',
    )
    report_group(
        "The following crates leave a target name for cargo to derive from the "
        "package name, so renaming the package renames the target silently. "
        "Declare it (the name below is the one in effect today, so adding it "
        "changes nothing):",
        target_violations,
        lambda v: f'{v[0]} -> add `name = "{v[1]}"`',
    )

    if has_problems:
        invalid = len(
            {p for p, _ in file_violations}
            | {p for p, _ in dep_violations}
            | {p for p, _ in target_violations}
        )
        print("=" * 80, file=sys.stderr)
        print(
            f"Summary: {checked} publishable crates checked for packaging metadata, "
            f"{targets_checked} crates checked for declared targets, "
            f"{invalid} with violations, {len(parse_errors)} unparseable",
            file=sys.stderr,
        )
        print("=" * 80, file=sys.stderr)
        return 1

    print(
        f"OK: {checked} publishable crates checked for packaging metadata, "
        f"{targets_checked} crates declare their targets"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
