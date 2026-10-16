#!/usr/bin/env python3
"""Resolve what to publish, from deploy/components.yaml.

Two modes:
  (default)  Resolve ONE component and emit its build spec for the image/chart jobs.
             The component comes from explicit inputs (INPUT_COMPONENT + INPUT_VERSION,
             set by workflow_dispatch / workflow_call) or else the GITHUB_REF_NAME tag.
  plan       Read release-plz's `releases` JSON from $RELEASES and emit a publish
             matrix of the released components that are publishable.

Manifest (deploy/components.yaml): a map of components keyed by artifact name. Each
has a `chart`, an optional `image`, and an optional `crate` (tag prefix `<crate>-v`,
used to map a released tag back to the component).

Outputs go to $GITHUB_OUTPUT (else stdout):
  default  component, version, ref, has_image, dockerfile, build_args, has_chart, chart_path
  plan     matrix (JSON array of {component, version}), any ("true"/"false")

Exits 1 on an invalid manifest, unknown dispatch component, or bad version.
"""

import json
import os
import re
import sys
from pathlib import Path
from typing import NoReturn

import yaml

SEMVER = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$")

MANIFEST = Path(__file__).resolve().parents[2] / "deploy" / "components.yaml"


def fail(message: str) -> NoReturn:
    """Print an error to stderr and exit 1."""
    print(f"resolve_release: {message}", file=sys.stderr)
    sys.exit(1)


def load_components() -> list[dict]:
    """Parse the manifest into normalized units: {name, chart, tagPrefix?, image?}."""
    data = yaml.safe_load(MANIFEST.read_text()) or {}
    components = data.get("components") or {}
    if not isinstance(components, dict):
        fail("`components` must be a map keyed by name")
    if not components:
        fail(f"{MANIFEST} defines no components")

    units: list[dict] = []
    for name, spec in components.items():
        spec = spec or {}
        if "chart" not in spec:
            fail(f"component {name} needs a chart")
        unit = {"name": name, "chart": spec["chart"]}
        if "crate" in spec:  # a crate makes the component auto-publishable by release-plz
            unit["tagPrefix"] = f"{spec['crate']}-v"
        if (image := spec.get("image")) is not None:
            if not isinstance(image, dict) or "dockerfile" not in image:
                fail(f"component {name} image needs a dockerfile")
            unit["image"] = image
        units.append(unit)

    return units


def normalize_version(version: str) -> str:
    """Strip an optional leading `v` and validate SemVer."""
    version = version.strip().removeprefix("v")
    if not SEMVER.match(version):
        fail(f"invalid SemVer: {version!r}")
    return version


def match_tag(units: list[dict], ref: str) -> tuple[dict | None, str]:
    """Match a `<prefix><version>` tag to its unit, or (None, '') for no match.

    Longest prefix wins, so e.g. `foo-bar-v` beats a hypothetical `foo-v`.
    """
    best: tuple[dict, str] | None = None
    for unit in units:
        prefix = unit.get("tagPrefix")  # components with no crate can't match a tag
        if prefix and ref.startswith(prefix):
            suffix = ref[len(prefix):]
            if SEMVER.match(suffix) and (best is None or len(prefix) > len(best[0]["tagPrefix"])):
                best = (unit, suffix)
    return best or (None, "")


def resolve(units: list[dict]) -> tuple[dict | None, str]:
    """Resolve one unit + version, or (None, '') for no match.

    Explicit inputs (INPUT_COMPONENT + INPUT_VERSION, from workflow_dispatch /
    workflow_call) take precedence; otherwise fall back to the GITHUB_REF_NAME tag.
    The event name is never consulted, so this behaves the same whether run directly
    or as a reusable-workflow call (where GITHUB_EVENT_NAME is the caller's event).
    """
    name = (os.environ.get("INPUT_COMPONENT") or "").strip()
    version = (os.environ.get("INPUT_VERSION") or "").strip()
    if name or version:
        if not (name and version):
            fail("both INPUT_COMPONENT and INPUT_VERSION are required")
        for unit in units:
            if unit["name"] == name:
                return unit, normalize_version(version)
        fail(f"unknown component {name!r}; not in {MANIFEST.name}")

    ref = (os.environ.get("GITHUB_REF_NAME") or "").strip()
    if not ref:
        fail("no INPUT_COMPONENT/INPUT_VERSION and no GITHUB_REF_NAME to resolve")
    return match_tag(units, ref)


def plan(units: list[dict], releases: str) -> list[dict]:
    """Map release-plz's `releases` JSON to a publish matrix of {component, version}.

    Each released git tag that matches a manifest unit becomes an entry; released
    crates that aren't publishable components resolve to nothing and are skipped.
    """
    matrix: list[dict] = []
    for release in json.loads(releases or "[]"):
        unit, version = match_tag(units, release.get("tag", ""))
        if unit:
            matrix.append({"component": unit["name"], "version": version})
    return matrix


def outputs(unit: dict | None, version: str) -> dict[str, str]:
    """Flat key -> value build spec for one component (empty when nothing matched)."""
    if unit is None:
        return {"component": "", "version": "", "ref": "", "has_image": "", "has_chart": ""}

    ref = f"{unit['tagPrefix']}{version}" if "tagPrefix" in unit else ""
    out = {"component": unit["name"], "version": version, "ref": ref,
           "has_image": "", "has_chart": ""}
    if image := unit.get("image"):
        out.update(has_image="true", dockerfile=image["dockerfile"],
                   build_args="\n".join(image.get("buildArgs", [])))
    if "chart" in unit:
        out.update(has_chart="true", chart_path=unit["chart"])
    return out


def write_outputs(values: dict[str, str]) -> None:
    """Write key=value pairs to $GITHUB_OUTPUT (heredoc for multi-line), else stdout."""
    out_path = os.environ.get("GITHUB_OUTPUT")
    sink = open(out_path, "a") if out_path else sys.stdout
    try:
        for key, value in values.items():
            if "\n" in value:
                sink.write(f"{key}<<__EOF__\n{value}\n__EOF__\n")
            else:
                sink.write(f"{key}={value}\n")
    finally:
        if out_path:
            sink.close()


def main() -> None:
    units = load_components()
    if sys.argv[1:2] == ["plan"]:
        matrix = plan(units, os.environ.get("RELEASES", ""))
        write_outputs({"matrix": json.dumps(matrix), "any": "true" if matrix else "false"})
        for entry in matrix:
            print(f"will publish: {entry['component']}@{entry['version']}", file=sys.stderr)
    else:
        unit, version = resolve(units)
        write_outputs(outputs(unit, version))
        print(f"resolved: {unit['name'] if unit else '(no match)'} {version or '-'}", file=sys.stderr)


if __name__ == "__main__":
    main()
