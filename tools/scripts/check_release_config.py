#!/usr/bin/env python3
"""
Validate the release configuration: who may be published, and who gets a
changelog section and a GitHub Release.

Three invariants, all of which have broken a real release before:

1. Nothing under `examples/`, `apps/` or `tools/` may be publishable. Those are
   demos, the dev server and repo tooling; a missing `publish = false` in a new
   example's Cargo.toml is invisible until release-plz pushes it to crates.io,
   where a publish cannot be undone.

2. `release-plz.toml` must agree with the workspace, per the path rule
   documented at the top of that file:
     - `libs/**` (the framework) is folded into the single `cf-gears-toolkit`
       release, so every publishable `libs/**` crate needs an entry with
       `changelog_update = false` / `git_release_enable = false`. An omission is
       not inert: the crate inherits the `[workspace]` defaults and silently
       starts writing its own changelog section and its own Release page.
     - `gears/**` gets its own changelog section and Release from those same
       defaults, so it must have no entry at all.
     - A crate with `publish = false` in its manifest is skipped by release-plz
       entirely, so an entry for it is dead config.

3. A dev-dependency that closes a cycle must be path-only, with no version.
   Cargo allows the cycle (`A` regular-depends on `B`, `B` dev-depends on `A`)
   and publishes `B` first, but `cargo package` still resolves `B`'s
   dev-dependencies against crates.io — so a versioned dev-dep on `A` demands a
   release of `A` that does not exist yet, and the release dies with
   "failed to select a version for the requirement". Path-only dev-deps are
   dropped from the published manifest, so the cycle never reaches crates.io.
   Note that `workspace = true` is not path-only: it carries the version from
   `[workspace.dependencies]`.

All checks are static — no cargo invocation — so this is cheap enough to run on
every CI build.

Exit codes:
  0 - Configuration is consistent
  1 - One or more violations
"""

import sys
import tomllib
from pathlib import Path
from typing import Dict, List, Tuple

sys.path.insert(0, str(Path(__file__).parent))

from check_packaging_metadata import find_manifests, is_publishable, parse_manifest

# Top-level directories whose crates must never be publishable.
NEVER_PUBLISHABLE_DIRS = ("examples", "apps", "tools")

# The one crate that carries the framework's changelog and GitHub Release; the
# rest of `libs/**` is folded into it.
FRAMEWORK_RELEASE_CRATE = "cf-gears-toolkit"

# Flags a folded-in `libs/**` crate must set, and the values it must set them to.
FOLDED_IN_FLAGS = {"changelog_update": False, "git_release_enable": False}


def is_release_target(package: dict) -> bool:
    """True if cargo would let this crate be published.

    Two independent ways to be unpublishable, both of which occur in this repo:
    an explicit `publish = false`, and an omitted `version` — cargo refuses to
    publish a versionless package, and `cargo metadata` reports it as
    `publish: []` just like the explicit form. `gears/bss/**` relies on the
    latter today, so checking only the `publish` field would misjudge it.
    """
    return is_publishable(package) and "version" in package


def collect_crates(workspace_root: Path) -> Tuple[Dict[str, dict], List[Tuple[Path, str]]]:
    """Map crate name -> {area, publishable, rel_path} for every manifest found.

    Returns the map plus a list of `(manifest_path, message)` parse errors.
    """
    crates: Dict[str, dict] = {}
    parse_errors: List[Tuple[Path, str]] = []
    for manifest_path in find_manifests(workspace_root):
        try:
            data = parse_manifest(manifest_path.read_text(encoding="utf-8"))
        except tomllib.TOMLDecodeError as exc:
            parse_errors.append((manifest_path, str(exc)))
            continue
        package = data.get("package")
        if package is None:
            continue  # virtual manifest (the workspace root)
        name = package.get("name")
        if not isinstance(name, str):
            continue
        rel_path = manifest_path.relative_to(workspace_root)
        crates[name] = {
            "area": rel_path.parts[0],
            "publishable": is_release_target(package),
            "rel_path": rel_path,
            "data": data,
        }
    return crates, parse_errors


def resolve_dep(alias: str, spec, ws_deps: dict) -> Tuple[str, bool]:
    """Resolve a dependency entry to `(real crate name, carries a version)`.

    The table key is an alias when the entry renames the crate (`package = "..."`),
    and `workspace = true` defers both the name and the version to the shared
    `[workspace.dependencies]` table.
    """
    if isinstance(spec, dict) and spec.get("workspace") is True:
        shared = ws_deps.get(alias)
        if isinstance(shared, dict):
            return shared.get("package", alias), "version" in shared
        return alias, True  # bare `dep = "1.2"` in the shared table
    if isinstance(spec, dict):
        return spec.get("package", alias), "version" in spec
    return alias, True  # bare `dep = "1.2"`


def iter_deps(data: dict, table_names: Tuple[str, ...]):
    """Yield `(table_label, alias, spec)` from the given dependency tables,
    including their target-specific variants (`[target.'cfg(...)'.<table>]`)."""
    containers = [("", data)]
    targets = data.get("target")
    if isinstance(targets, dict):
        containers += [
            (f"target.{cfg}.", tables) for cfg, tables in targets.items() if isinstance(tables, dict)
        ]
    for prefix, container in containers:
        for table_name in table_names:
            table = container.get(table_name)
            if isinstance(table, dict):
                for alias, spec in table.items():
                    yield f"{prefix}{table_name}", alias, spec


def normal_dep_graph(crates: Dict[str, dict], ws_deps: dict) -> Dict[str, set]:
    """Direct normal/build dependency edges between publishable crates.

    Dev-dependencies are excluded on purpose: they are what this graph is used to
    judge, and Cargo permits cycles through them.
    """
    nodes = {name for name, info in crates.items() if info["publishable"]}
    edges = {name: set() for name in nodes}
    for name in nodes:
        for _, alias, spec in iter_deps(crates[name]["data"], ("dependencies", "build-dependencies")):
            dep, _ = resolve_dep(alias, spec, ws_deps)
            if dep in nodes and dep != name:
                edges[name].add(dep)
    return edges


def check_dev_dep_cycles(crates: Dict[str, dict], ws_deps: dict) -> List[str]:
    """Versioned dev-deps on a workspace crate that may not be published yet.

    release-plz publishes in dependency order, so a crate's own (transitive)
    normal dependencies are guaranteed to be on crates.io by the time it is
    published. Nothing guarantees that for anything else — least of all for a
    crate that depends on *us*, which is published strictly later. A dev-dep
    carrying a version therefore has to point at a real normal dependency;
    anything else must be path-only so cargo strips it before upload.
    """
    edges = normal_dep_graph(crates, ws_deps)
    problems: List[str] = []
    for name in sorted(edges):
        # Everything guaranteed to be published before `name`.
        published_first, stack = set(), [name]
        while stack:
            for dep in edges[stack.pop()]:
                if dep not in published_first:
                    published_first.add(dep)
                    stack.append(dep)

        info = crates[name]
        for label, alias, spec in iter_deps(info["data"], ("dev-dependencies",)):
            dep, has_version = resolve_dep(alias, spec, ws_deps)
            if dep not in edges or not has_version or dep in published_first:
                continue
            reason = (
                "this very crate"
                if dep == name
                else f"`{dep}`, which is not a normal dependency of it"
            )
            problems.append(
                f"{info['rel_path']}: [{label}] `{alias}` carries a version and points at "
                f"{reason}, so that release need not be on crates.io yet when this crate is "
                f"published -> declare it path-only: no version, and not `workspace = true`"
            )
    return problems


def check_never_publishable(crates: Dict[str, dict]) -> List[str]:
    """Crates under examples/apps/tools that are missing `publish = false`."""
    return [
        f"{info['rel_path']}: crate `{name}` is publishable -> add `publish = false`"
        for name, info in sorted(crates.items())
        if info["area"] in NEVER_PUBLISHABLE_DIRS and info["publishable"]
    ]


def check_release_plz(config: dict, crates: Dict[str, dict]) -> List[str]:
    """`release-plz.toml` entries vs the workspace, per the documented path rule."""
    problems: List[str] = []
    entries = config.get("package")
    entries = entries if isinstance(entries, list) else []

    seen: Dict[str, int] = {}
    for entry in entries:
        name = entry.get("name")
        if not isinstance(name, str):
            problems.append("release-plz.toml: a [[package]] entry has no `name`")
            continue
        seen[name] = seen.get(name, 0) + 1
        if seen[name] == 2:
            problems.append(f"release-plz.toml: duplicate entry for `{name}`")

        info = crates.get(name)
        if info is None:
            problems.append(
                f"release-plz.toml: entry for `{name}`, which is not a crate in this "
                f"workspace -> stale, remove it"
            )
            continue
        if not info["publishable"]:
            problems.append(
                f"release-plz.toml: entry for `{name}`, which has `publish = false` in "
                f"{info['rel_path']} -> release-plz skips it anyway, remove the entry"
            )
            continue
        if "publish" in entry:
            problems.append(
                f"release-plz.toml: entry for `{name}` sets `publish` -> the Cargo "
                f"manifest is the only source of truth for that, remove the key"
            )
        if info["area"] == "gears":
            problems.append(
                f"release-plz.toml: entry for `{name}` ({info['rel_path']}) -> crates "
                f"under gears/** inherit the [workspace] defaults, remove the entry"
            )

    for name, info in sorted(crates.items()):
        if info["area"] != "libs" or not info["publishable"]:
            continue
        entry = next((e for e in entries if e.get("name") == name), None)
        if entry is None:
            problems.append(
                f"release-plz.toml: no entry for `{name}` ({info['rel_path']}) -> it "
                f"would inherit the [workspace] defaults and get its own changelog "
                f"section and GitHub Release"
            )
            continue
        expected = (
            {"changelog_update": True, "git_release_enable": True}
            if name == FRAMEWORK_RELEASE_CRATE
            else FOLDED_IN_FLAGS
        )
        for flag, want in expected.items():
            got = entry.get(flag)
            if got is not want:
                problems.append(
                    f"release-plz.toml: `{name}` has {flag} = "
                    f"{'unset' if got is None else str(got).lower()}, expected "
                    f"{str(want).lower()}"
                )
    return problems


def main() -> int:
    workspace_root = Path(__file__).parent.parent.parent

    crates, parse_errors = collect_crates(workspace_root)
    if not crates:
        print(f"Error: no crates found under {workspace_root}", file=sys.stderr)
        return 1

    config_path = workspace_root / "release-plz.toml"
    try:
        config = tomllib.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        print(f"Error: cannot read {config_path.name}: {exc}", file=sys.stderr)
        return 1

    try:
        root_manifest = parse_manifest((workspace_root / "Cargo.toml").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        print(f"Error: cannot read the root Cargo.toml: {exc}", file=sys.stderr)
        return 1
    ws_deps = root_manifest.get("workspace", {}).get("dependencies")
    ws_deps = ws_deps if isinstance(ws_deps, dict) else {}

    groups = [
        ("Crates that must never be published", check_never_publishable(crates)),
        ("release-plz.toml disagrees with the workspace", check_release_plz(config, crates)),
        ("Dev-dependencies that break `cargo publish`", check_dev_dep_cycles(crates, ws_deps)),
    ]
    if parse_errors:
        groups.append(
            (
                "Manifests that could not be parsed",
                [f"{path.relative_to(workspace_root)}: {message}" for path, message in parse_errors],
            )
        )

    total = sum(len(problems) for _, problems in groups)
    if total:
        print("=" * 80, file=sys.stderr)
        print("RELEASE CONFIGURATION VIOLATIONS DETECTED", file=sys.stderr)
        print("=" * 80, file=sys.stderr)
        for header, problems in groups:
            if not problems:
                continue
            print(file=sys.stderr)
            print(f"{header}:", file=sys.stderr)
            print(file=sys.stderr)
            for problem in problems:
                print(f"  [X] {problem}", file=sys.stderr)
        print(file=sys.stderr)
        print("=" * 80, file=sys.stderr)
        print(f"Summary: {len(crates)} crates checked, {total} violation(s)", file=sys.stderr)
        print("The rules live at the top of release-plz.toml.", file=sys.stderr)
        print("=" * 80, file=sys.stderr)
        return 1

    publishable = sum(1 for info in crates.values() if info["publishable"])
    print(
        f"OK: {len(crates)} crates checked "
        f"({publishable} publishable), release-plz.toml is consistent"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
