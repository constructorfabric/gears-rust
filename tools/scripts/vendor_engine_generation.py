#!/usr/bin/env python3
"""
Vendor and check the frozen evaluation code of a BSS Rating engine generation.

An engine generation (gears/bss/rating/docs/ADR/0006) must give the same result
for the same input years later, so every external crate whose code runs inside
evaluation is part of the generation's frozen tree. A generation directory holds
a hand-written `vendor.toml` naming

  - first-party sources (`[[copy]]`) copied byte for byte into `vendor/<to>/`;
  - the registry crates of the numeric closure (`[registry] crates`), which stay
    pinned by version, `Cargo.lock` checksum and resolved features until the
    generation is frozen.

Two commands:

    vendor_engine_generation.py vendor <generation-dir>
        Copy the sources and write `vendor/LOCK.toml`: the SHA-256 of every
        copied file and the version, checksum and features of every registry
        crate, as `cargo metadata` resolves them for the generation's package.

    vendor_engine_generation.py check <generation-dir> [<generation-dir> ...]
        Fail when a vendored file no longer matches `LOCK.toml` (a hand edit),
        or when the workspace now resolves a registry crate of the closure to
        another version, checksum or feature set (a `cargo update`, or a feature
        such as `rust_decimal/legacy-ops` enabled elsewhere, would change the
        generation's arithmetic: Cargo unifies features across the workspace). A first-party source that moved on upstream is
        reported, not failed: a generation keeps the copy it was released with.

The generation's `engine_digest` (its build script) hashes the same tree, so a
vendored edit also changes the digest.

Exit codes:
  0 - Everything matches
  1 - A vendored file or a registry pin drifted, or cargo failed
"""

import hashlib
import json
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Dict, List, Tuple

ROOT = Path(__file__).resolve().parents[2]
LOCK_NAME = "LOCK.toml"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def load_spec(generation_dir: Path) -> Dict:
    with (generation_dir / "vendor.toml").open("rb") as handle:
        return tomllib.load(handle)


def copied_files(spec: Dict) -> List[Tuple[str, Path]]:
    """`(vendored path relative to vendor/, upstream source path)` for every copied file."""
    files = []
    for copy in spec.get("copy", []):
        for file in copy["files"]:
            files.append((f"{copy['to']}/{file}", ROOT / copy["from"] / file))
    return files


def lock_checksums() -> Dict[Tuple[str, str], str]:
    """`(name, version) -> checksum` for every registry package of the workspace lockfile."""
    with (ROOT / "Cargo.lock").open("rb") as handle:
        lock = tomllib.load(handle)
    return {
        (package["name"], package["version"]): package["checksum"]
        for package in lock.get("package", [])
        if "checksum" in package
    }


def resolve_registry(spec: Dict) -> List[Dict[str, str]]:
    """The registry crates of the closure as the generation's package resolves them."""
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=ROOT,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    roots = [id_ for id_, package in packages.items() if package["name"] == spec["package"]]
    if len(roots) != 1:
        raise SystemExit(f"error: package {spec['package']} is not in the workspace")
    # Walk normal edges only: dev and build dependencies do not run inside evaluation.
    reachable, stack = set(), list(roots)
    while stack:
        current = stack.pop()
        if current in reachable:
            continue
        reachable.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    checksums = lock_checksums()
    wanted = set(spec["registry"]["crates"])
    pins = []
    for id_ in sorted(reachable):
        package = packages[id_]
        if package["name"] not in wanted:
            continue
        key = (package["name"], package["version"])
        if key not in checksums:
            raise SystemExit(f"error: {key[0]} {key[1]} has no checksum in Cargo.lock")
        pins.append(
            {
                "name": key[0],
                "version": key[1],
                "checksum": checksums[key],
                "features": sorted(nodes[id_].get("features", [])),
            }
        )
    missing = wanted - {pin["name"] for pin in pins}
    if missing:
        raise SystemExit(
            f"error: {spec['package']} no longer reaches {', '.join(sorted(missing))}; "
            "update [registry] crates in vendor.toml"
        )
    return sorted(pins, key=lambda pin: (pin["name"], pin["version"]))


def render_lock(spec: Dict, files: List[Dict[str, str]], pins: List[Dict[str, str]]) -> str:
    lines = [
        "# Generated by tools/scripts/vendor_engine_generation.py from vendor.toml; do not edit.",
        f"generation = {spec['generation']}",
        "",
    ]
    for file in files:
        lines += [
            "[[file]]",
            f'path = "{file["path"]}"',
            f'source = "{file["source"]}"',
            f'sha256 = "{file["sha256"]}"',
            "",
        ]
    for pin in pins:
        lines += [
            "[[registry]]",
            f'name = "{pin["name"]}"',
            f'version = "{pin["version"]}"',
            f'checksum = "{pin["checksum"]}"',
            "features = [" + ", ".join(f'"{feature}"' for feature in pin["features"]) + "]",
            "",
        ]
    return "\n".join(lines)


def vendor(generation_dir: Path) -> int:
    spec = load_spec(generation_dir)
    vendor_dir = generation_dir / "vendor"
    files = []
    for vendored, source in copied_files(spec):
        target = vendor_dir / vendored
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        files.append(
            {
                "path": vendored,
                "source": source.relative_to(ROOT).as_posix(),
                "sha256": sha256(target),
            }
        )
    pins = resolve_registry(spec)
    (vendor_dir / LOCK_NAME).write_text(render_lock(spec, files, pins), encoding="utf-8")
    print(f"vendored {len(files)} files and pinned {len(pins)} registry crates in {vendor_dir}")
    return 0


def check(generation_dir: Path) -> List[str]:
    spec = load_spec(generation_dir)
    vendor_dir = generation_dir / "vendor"
    with (vendor_dir / LOCK_NAME).open("rb") as handle:
        lock = tomllib.load(handle)
    failures = []
    recorded = {file["path"]: file for file in lock.get("file", [])}
    expected = {vendored for vendored, _ in copied_files(spec)}
    if set(recorded) != expected:
        failures.append(f"{generation_dir}: {LOCK_NAME} does not list the files vendor.toml copies")
    for path, file in sorted(recorded.items()):
        vendored = vendor_dir / path
        if not vendored.is_file() or sha256(vendored) != file["sha256"]:
            failures.append(f"{vendored}: differs from the copy recorded in {LOCK_NAME}")
            continue
        upstream = ROOT / file["source"]
        if upstream.is_file() and sha256(upstream) != file["sha256"]:
            print(f"note: {file['source']} moved on upstream; generation keeps its copy")
    pins = resolve_registry(spec)
    if pins != lock.get("registry", []):
        failures.append(
            f"{generation_dir}: the workspace resolves the numeric closure to "
            f"{[(p['name'], p['version'], p['features']) for p in pins]}, not the pins in "
            f"{LOCK_NAME}; "
            "a generation's arithmetic may not change (ADR-0006)"
        )
    if not failures:
        print(f"ok: {generation_dir.relative_to(ROOT)} matches {LOCK_NAME}")
    return failures


def main(argv: List[str]) -> int:
    if len(argv) < 3 or argv[1] not in ("vendor", "check"):
        print(__doc__, file=sys.stderr)
        return 1
    directories = [Path(arg).resolve() for arg in argv[2:]]
    try:
        if argv[1] == "vendor":
            return max(vendor(directory) for directory in directories)
        failures = [failure for directory in directories for failure in check(directory)]
    except subprocess.CalledProcessError as error:
        print(f"error: cargo failed: {error.stderr.strip()}", file=sys.stderr)
        return 1
    for failure in failures:
        print(f"error: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
