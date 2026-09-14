#!/usr/bin/env python
"""Local E2E entry point (shared, per-suite, and per-gear runs).

Single entry point behind `make e2e-local [SUITE=<suite>] [GEAR=<gear>]`. A
*suite* is one named E2E scenario (`file-parser`, `scope-enforcement`,
`tr-authz`) — often, but not always, named after a gear crate. `--gear <gear>`
discovers and runs every e2e-launcher suite whose resolved Cargo features
(`features` + `features_file` + `extra_features`) include that gear. The runner
resolves *what* to build and run from config (global
`config/e2e-launcher.yaml` + per-suite `testing/e2e/suites/<suite>/e2e.yaml`),
builds the server with the right Cargo features (plus any sidecars), then
dispatches by the suite's `launcher`:

  - `launcher: e2e-launcher` -> one server started by tools/scripts/ci.py
    against a generated `config/e2e-local.yaml`; tests are HTTP clients. (default)
  - `launcher: pytest`       -> `python -m pytest <test_path>`; the suite's own
    conftest starts/stops the server (+ sidecars). The built binary path is
    auto-exported as `E2E_BINARY`.

Conventions that keep manifests short:
  - `test_path` defaults to the manifest's own directory.
  - `E2E_BINARY` is injected automatically for `launcher: pytest` suites.
  - The server binary defaults to `cf-gears-example-server`; a suite may pick a
    different one with `binary: <cargo-bin-name>`.
  - Variants of a suite live under `profiles:` in the same `e2e.yaml`, selected
    with `--profile <name>` (deep-merged over the base manifest).

No suite/gear names or GTS ids are hardcoded here; all such knowledge lives in
the YAML config. See `testing/e2e/README.md` ("How E2E Is Executed" and
"Configuration Format") for details.
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
from pathlib import Path
from typing import Any

import yaml


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_GLOBAL_CONFIG = ROOT / "config" / "e2e-launcher.yaml"
DEFAULT_BINARY = ROOT / "target" / "debug" / "cf-gears-example-server"
DEFAULT_BIN_NAME = "cf-gears-example-server"


def load_yaml(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as fh:
        data = yaml.safe_load(fh) or {}
    if not isinstance(data, dict):
        raise ValueError(f"{path} must contain a YAML mapping")
    return data


def suite_key(name: str) -> str:
    return name.replace("-", "_")


def suite_manifest_path(suite_name: str, explicit: str | None) -> Path | None:
    if explicit:
        path = (ROOT / explicit).resolve()
        return path if path.exists() else None

    default = ROOT / "testing" / "e2e" / "suites" / suite_key(suite_name) / "e2e.yaml"
    if default.exists():
        return default

    for path in (ROOT / "testing" / "e2e" / "suites").glob("**/e2e.yaml"):
        data = load_yaml(path)
        if data.get("suite") == suite_name:
            return path
    return None


def as_list(value: Any) -> list[str]:
    if value is None:
        return []
    if isinstance(value, str):
        return [item.strip() for item in value.split(",") if item.strip()]
    if isinstance(value, list):
        return [str(item) for item in value]
    raise ValueError(f"expected list or comma-separated string, got {type(value).__name__}")


def default_suite(suite_name: str, global_cfg: dict[str, Any]) -> dict[str, Any]:
    return {
        "suite": suite_name,
        "features": [suite_name, *as_list(global_cfg.get("base_features"))],
        "launcher": "e2e-launcher",
        "config_gears": [suite_name],
    }


def merged_suite(
    suite_name: str,
    global_cfg: dict[str, Any],
    manifest_path: Path | None,
    profile: str | None = None,
) -> dict[str, Any]:
    suite = default_suite(suite_name, global_cfg)
    features_explicit = False
    if manifest_path is not None:
        data = load_yaml(manifest_path)
        profiles = data.pop("profiles", {}) or {}
        features_explicit = "features" in data
        suite.update(data)
        if profile:
            variant = profiles.get(profile)
            if variant is None:
                raise ValueError(f"profile '{profile}' not found in {manifest_path}")
            features_explicit = features_explicit or "features" in variant
            deep_merge(suite, variant)
    elif profile:
        raise ValueError(f"--profile {profile} given but no manifest found for suite '{suite_name}'")

    # A suite that pulls its Cargo features from a `features_file` (a full-config
    # profile like scope-enforcement / tr-authz) is not necessarily named after a
    # real Cargo feature, so drop the suite-name default unless it was set
    # explicitly. Otherwise the build would fail on an unknown feature.
    if suite.get("features_file") and not features_explicit:
        suite["features"] = list(as_list(global_cfg.get("base_features")))

    suite["suite"] = suite_name
    # `test_path` defaults to the manifest's own directory, so most suites never
    # need to state it.
    if not suite.get("test_path"):
        if manifest_path is not None:
            suite["test_path"] = str(manifest_path.parent.relative_to(ROOT))
        else:
            suite["test_path"] = f"testing/e2e/suites/{suite_key(suite_name)}"
    return suite


def focused_config_path(suite_name: str, profile: str | None, global_cfg: dict[str, Any]) -> Path:
    generated_dir = ROOT / str(global_cfg.get("generated_config_dir", "target"))
    suffix = f"{suite_name}-{profile}" if profile else suite_name
    return generated_dir / f"e2e-local-{suffix}.yaml"


def _dig(data: Any, path: list[str]) -> Any:
    node = data
    for key in path:
        if not isinstance(node, dict) or key not in node:
            return None
        node = node[key]
    return node


def _entry_matches(entry: Any, match: dict[str, Any]) -> bool:
    """Generic list-entry matcher driven by declarative `match` clauses.

    Supported keys: `id_contains` (substring test on the entry's `$id`).
    """
    if "id_contains" in match:
        return str(match["id_contains"]) in str(_dig(entry, ["$id"]) or "")
    return False


def apply_config_prune(data: dict[str, Any], rules: list[dict[str, Any]], keep_gears: set[str]) -> None:
    """Drop list entries described by declarative prune rules from config.

    Each rule names a dotted `path` to a list, a `match` clause, and an
    optional `keep_when_config_gears` allowlist that skips the rule when any of
    those gears is part of the focused run. This keeps all gear-specific literals
    in `config/e2e-launcher.yaml`, not in this script.
    """
    for rule in rules or []:
        if keep_gears.intersection(as_list(rule.get("keep_when_config_gears"))):
            continue
        path = str(rule.get("path", "")).split(".")
        if not path or not path[0]:
            continue
        parent = _dig(data, path[:-1])
        leaf = path[-1]
        if not isinstance(parent, dict) or not isinstance(parent.get(leaf), list):
            continue
        match = rule.get("match") or {}
        kept = [entry for entry in parent[leaf] if not _entry_matches(entry, match)]
        if kept:
            parent[leaf] = kept
        else:
            parent.pop(leaf, None)


def deep_merge(base: dict[str, Any], override: dict[str, Any]) -> dict[str, Any]:
    """Recursively merge `override` into `base` (dicts merge, everything else replaces).

    Lists and scalars replace wholesale — e.g. an override's `tokens:` list
    fully supersedes the base list rather than concatenating.
    """
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            deep_merge(base[key], value)
        else:
            base[key] = value
    return base


def write_config(
    output_path: Path,
    base_config: Path,
    config_gears: Any,
    overlay_file: Any,
    overrides: Any,
    global_cfg: dict[str, Any],
) -> Path:
    """Prune the base config to the requested gears, apply overlays, write it out."""
    wanted_gears = as_list(config_gears)
    keep_all = "*" in wanted_gears
    keep_gears = set(as_list(global_cfg.get("core_config_gears")))
    keep_gears.update(g for g in wanted_gears if g != "*")

    data = load_yaml(base_config)
    gears = data.get("gears")
    if isinstance(gears, dict):
        # `config_gears: ['*']` keeps every base gear block (used by full-config
        # profiles like tr-authz); otherwise keep only core + requested gears.
        if not keep_all:
            data["gears"] = {name: value for name, value in gears.items() if name in keep_gears}

    prune_keep = set((data.get("gears") or {}).keys()) if keep_all else keep_gears
    apply_config_prune(data, global_cfg.get("config_prune") or [], prune_keep)

    # Overlays: an external file first, then inline `config_overrides` — both
    # deep-merged. This is how profile variants (tr-authz priorities,
    # scope-enforcement route_policies/tokens) avoid cloning the base config.
    if overlay_file:
        deep_merge(data, load_yaml(ROOT / str(overlay_file)))
    if isinstance(overrides, dict):
        deep_merge(data, overrides)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as fh:
        yaml.safe_dump(data, fh, sort_keys=False)
    return output_path


def write_focused_config(suite_name: str, profile: str | None, suite: dict[str, Any], global_cfg: dict[str, Any]) -> Path:
    base_config = ROOT / str(suite.get("base_config", global_cfg.get("base_config", "config/e2e-local.yaml")))
    output_path = focused_config_path(suite_name, profile, global_cfg)
    return write_config(
        output_path,
        base_config,
        suite.get("config_gears"),
        suite.get("config_overlay_file"),
        suite.get("config_overrides"),
        global_cfg,
    )


def expand(value: str, binary: Path, suite_name: str) -> str:
    # `{gear}`/`{gear_key}` are kept as back-compat aliases of `{suite}`.
    return value.format(
        binary=str(binary),
        suite=suite_name,
        suite_key=suite_key(suite_name),
        gear=suite_name,
        gear_key=suite_key(suite_name),
        root=str(ROOT),
    )


def _free_port() -> int:
    """Grab an ephemeral TCP port the OS is willing to hand out right now."""
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def run(cmd: list[str], env: dict[str, str] | None = None, dry_run: bool = False) -> int:
    print(" ".join(cmd))
    if dry_run:
        return 0
    return subprocess.run(cmd, cwd=ROOT, env=env, check=False).returncode


def build_server(bin_name: str, features: list[str], default_features: bool, dry_run: bool) -> int:
    if not features:
        raise ValueError("E2E build requires at least one Cargo feature")
    cmd = ["cargo", "build", "--bin", bin_name]
    if not default_features:
        cmd.append("--no-default-features")
    cmd += ["--features", ",".join(features)]
    return run(cmd, dry_run=dry_run)


def build_sidecars(names: list[str], global_cfg: dict[str, Any], dry_run: bool) -> dict[str, str]:
    """Build each named sidecar and return the env vars it contributes.

    Sidecar build commands and env are declared centrally under `sidecars:` in
    `config/e2e-launcher.yaml`, so neither this script nor the Makefile hardcodes
    sidecar-specific knowledge.
    """
    defs = global_cfg.get("sidecars") or {}
    env: dict[str, str] = {}
    for name in names:
        spec = defs.get(name)
        if not spec:
            raise ValueError(f"sidecar '{name}' is not defined under sidecars: in the global config")
        build_cmd = as_list(spec.get("build"))
        if build_cmd:
            rc = run(build_cmd, dry_run=dry_run)
            if rc != 0:
                raise SystemExit(rc)
        for key, value in (spec.get("env") or {}).items():
            env[str(key)] = str(value)
    return env


def run_pytest(python: str, suite: dict[str, Any], binary: Path, suite_name: str, sidecar_env: dict[str, str], extra_args: list[str], dry_run: bool) -> int:
    """`launcher: pytest` — the suite's own conftest owns the server lifecycle."""
    env = os.environ.copy()
    env.update(sidecar_env)
    # Self-managed suites read the built binary path from E2E_BINARY; inject it
    # automatically so manifests don't have to.
    env["E2E_BINARY"] = str(binary)
    for key, value in (suite.get("env") or {}).items():
        env[str(key)] = expand(str(value), binary, suite_name)
    cmd = [python, "-m", "pytest", *as_list(suite.get("test_path")), *as_list(suite.get("pytest_args")), *extra_args]
    return run(cmd, env=env, dry_run=dry_run)


def run_launcher(python: str, suite: dict[str, Any], binary: Path, suite_name: str, profile: str | None, global_cfg: dict[str, Any], sidecar_env: dict[str, str], extra_args: list[str], dry_run: bool) -> int:
    """`launcher: e2e-launcher` — one server started by ci.py; tests are HTTP clients."""
    env = os.environ.copy()
    env["E2E_SERVER_BINARY"] = str(binary)
    env.update(sidecar_env)
    for key, value in (suite.get("env") or {}).items():
        env[str(key)] = expand(str(value), binary, suite_name)

    cmd = [python, "tools/scripts/ci.py", "e2e-local"]
    # A focused run rewrites config/e2e-local.yaml down to the gears it built; a
    # shared run (no suite) uses ci.py's default config unchanged.
    if suite_name:
        config_path = write_focused_config(suite_name, profile, suite, global_cfg)
        cmd += ["--config", str(config_path)]
    cmd += ["--", *as_list(suite.get("test_path")), *as_list(suite.get("pytest_args")), *extra_args]
    return run(cmd, env=env, dry_run=dry_run)


def read_features_file(rel_path: str) -> list[str]:
    text = (ROOT / rel_path).read_text(encoding="utf-8")
    features: list[str] = []
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            features.extend(part.strip() for part in line.split(",") if part.strip())
    return features


def shared_suite(global_cfg: dict[str, Any]) -> dict[str, Any]:
    """Build the unscoped `make e2e-local` suite from the global `shared:` block."""
    shared = global_cfg.get("shared") or {}
    suite: dict[str, Any] = {
        "suite": "",
        "default_features": True,
        "launcher": shared.get("launcher", "e2e-launcher"),
        "sidecars": as_list(shared.get("sidecars")),
        "test_path": [],
    }
    suite.update(shared)
    return suite


def discover_launcher_test_paths(global_cfg: dict[str, Any]) -> list[str]:
    """Test paths of every suite whose launcher is `e2e-launcher`.

    The unscoped `make e2e-local` (no SUITE) runs the tests against a single
    shared server, so it must only collect suites that fit that model. Passing
    these explicit paths to pytest excludes self-managed (`launcher: pytest`)
    suites like `mini-chat` / `usage-collector` \u2014 which own their own server and
    would otherwise just be collected and skipped \u2014 rather than relying on their
    conftest to opt out.
    """
    paths: list[str] = []
    seen: set[str] = set()
    for manifest in sorted((ROOT / "testing" / "e2e" / "suites").glob("**/e2e.yaml")):
        data = load_yaml(manifest)
        if data.get("launcher", "e2e-launcher") != "e2e-launcher":
            continue
        test_path = data.get("test_path") or str(manifest.parent.relative_to(ROOT))
        for path in as_list(test_path):
            if path not in seen:
                seen.add(path)
                paths.append(path)
    return paths


def resolve_features(suite: dict[str, Any]) -> list[str]:
    """Combine `features`, an optional shared `features_file`, and `extra_features`.

    Lets full-config profiles reuse `config/e2e-features.txt` and only add the
    couple of extra Cargo features they need, instead of restating the list.
    """
    features = list(as_list(suite.get("features")))
    if suite.get("features_file"):
        features += read_features_file(str(suite["features_file"]))
    features += as_list(suite.get("extra_features"))
    seen: set[str] = set()
    return [f for f in features if not (f in seen or seen.add(f))]


def discover_suites_for_gear(gear: str) -> list[tuple[str, Path]]:
    """Suites (name, manifest) that exercise `gear` and use `launcher: e2e-launcher`.

    A gear is matched against a suite's resolved Cargo feature set: the manifest's
    `features`, any features pulled in via `features_file`, and `extra_features`.
    A `make e2e-local GEAR=<gear>` run collects every e2e-launcher suite whose
    features include `<gear>` and executes them one after another (each as its own
    focused build+run). Self-managed (`launcher: pytest`) suites are excluded
    because they own their server lifecycle and are not part of the gear-scoped
    shared-server model.
    """
    matches: list[tuple[str, Path]] = []
    for manifest in sorted((ROOT / "testing" / "e2e" / "suites").glob("**/e2e.yaml")):
        data = load_yaml(manifest)
        if data.get("launcher", "e2e-launcher") != "e2e-launcher":
            continue
        if gear in resolve_features(data):
            name = str(data.get("suite") or suite_key(manifest.parent.name))
            matches.append((name, manifest))
    return matches


def run_suite(
    suite: dict[str, Any],
    suite_name: str,
    profile: str | None,
    args: argparse.Namespace,
    global_cfg: dict[str, Any],
    extra_args: list[str],
) -> int:
    """Build the server for one suite (focused features) and run it."""
    # A suite may build a non-default server binary (`binary: <cargo-bin-name>`);
    # keep it alongside the default under target/debug/.
    bin_name = str(suite.get("binary") or args.bin_name)
    binary = (ROOT / args.binary).resolve()
    if suite.get("binary"):
        binary = binary.parent / bin_name

    features = resolve_features(suite)
    default_features = bool(suite.get("default_features", False))
    result = build_server(bin_name, features, default_features, args.dry_run)
    if result != 0:
        return result

    sidecar_env = build_sidecars(as_list(suite.get("sidecars")), global_cfg, args.dry_run)

    launcher = suite.get("launcher", "e2e-launcher")
    if launcher == "pytest":
        return run_pytest(args.python, suite, binary, suite_name, sidecar_env, extra_args, args.dry_run)
    if launcher == "e2e-launcher":
        return run_launcher(args.python, suite, binary, suite_name, profile, global_cfg, sidecar_env, extra_args, args.dry_run)
    raise ValueError(f"unsupported launcher: {launcher!r} (expected 'e2e-launcher' or 'pytest')")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--suite", default="")
    parser.add_argument("--gear", default="", help="run every e2e-launcher suite whose resolved Cargo features include this gear")
    parser.add_argument("--profile", default="", help="select a variant under the manifest's profiles:")
    parser.add_argument("--global-config", default=str(DEFAULT_GLOBAL_CONFIG))
    parser.add_argument("--manifest")
    parser.add_argument("--binary", default=str(DEFAULT_BINARY))
    parser.add_argument("--bin-name", default=DEFAULT_BIN_NAME)
    parser.add_argument("--python", default=sys.executable)
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("pytest_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()

    suite_name = args.suite.strip()
    gear = args.gear.strip()
    profile = args.profile.strip() or None
    extra_args = args.pytest_args
    if extra_args and extra_args[0] == "--":
        extra_args = extra_args[1:]
    if args.smoke:
        extra_args = ["-m", "smoke", *extra_args]

    if suite_name and gear:
        raise SystemExit("choose either --suite or --gear, not both")
    if gear and profile:
        raise SystemExit("--profile is only valid with --suite, not --gear")

    global_cfg = load_yaml((ROOT / args.global_config).resolve())

    # GEAR mode: discover and execute every e2e-launcher suite that exercises the
    # gear, each as its own focused build+run. Runs all of them and reports the
    # first non-zero exit code.
    if gear:
        matches = discover_suites_for_gear(gear)
        if not matches:
            # No e2e-launcher suite exercises this gear (its features don't appear
            # in any suite's features/features_file). This is a valid state — many
            # gears have no E2E coverage — so a gear-scoped run (e.g. `make all
            # GEAR=<gear>`) treats it as a no-op rather than a failure.
            print(f"gear '{gear}' -> no e2e-launcher suites match its features; nothing to run")
            return 0
        print(f"gear '{gear}' -> suites: {', '.join(name for name, _ in matches)}")
        overall = 0
        for name, manifest_path in matches:
            print(f"\n=== e2e suite: {name} (gear={gear}) ===")
            suite = merged_suite(name, global_cfg, manifest_path, None)
            rc = run_suite(suite, name, None, args, global_cfg, extra_args)
            if rc != 0 and overall == 0:
                overall = rc
        return overall

    if suite_name:
        manifest_path = suite_manifest_path(suite_name, args.manifest)
        suite = merged_suite(suite_name, global_cfg, manifest_path, profile)
    else:
        suite = shared_suite(global_cfg)
        # No SUITE: run only the suites that fit the shared server, i.e. those
        # with `launcher: e2e-launcher`. (A `shared.test_path` in the global
        # config still wins if explicitly set.)
        if not as_list(suite.get("test_path")):
            suite["test_path"] = discover_launcher_test_paths(global_cfg)

    return run_suite(suite, suite_name, profile, args, global_cfg, extra_args)


if __name__ == "__main__":
    raise SystemExit(main())
