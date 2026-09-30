#!/usr/bin/env python3
"""File classification for toolkit-pr-review.

Decides what each changed file is: reviewable Rust, a dependency manifest, or out of
scope; and which module it groups with for sharding.

Classification works from the path alone.
"""

from __future__ import annotations


# The complete set of non-.rs files this review looks at. Anything else in the diff is
# out of scope: there are no rules for YAML, SQL, proto or Dockerfiles, so routing them
# to a shard holding only Rust rules would spend the finding cap on generic remarks.
MANIFEST_NAMES = frozenset({
    "Cargo.toml",
    "Cargo.lock",
    "deny.toml",
    "audit.toml",        # .cargo/audit.toml
    "config.toml",       # .cargo/config.toml
    "clippy.toml",
    "rust-toolchain.toml",
})

# Cargo.lock is tracked so a RUST-DEP-001 finding can anchor on a changed line, but its
# content is never snapshotted: it is generated, routinely over 300 KB, and everything
# the rule needs from it is visible in the diff.
NEVER_SNAPSHOT = frozenset({"Cargo.lock"})


def basename(path: str) -> str:
    return path.rsplit("/", 1)[-1]


def is_rust(path: str) -> bool:
    return path.endswith(".rs")


def is_manifest(path: str) -> bool:
    name = basename(path)
    if name not in MANIFEST_NAMES:
        return False
    # `config.toml` and `audit.toml` are only interesting under .cargo/; any other
    # config.toml in the tree is somebody else's.
    if name in {"config.toml", "audit.toml"}:
        return path.endswith(f".cargo/{name}")
    return True


def in_scope(path: str) -> bool:
    return is_rust(path) or is_manifest(path)


def snapshotable(path: str) -> bool:
    return basename(path) not in NEVER_SNAPSHOT


def group_key(path: str) -> str:
    """The module a file groups with, so a shard sees code that belongs together.

    First two path segments when there are three or more, else the first segment:
    `gears/mini-chat/src/lib.rs` -> `gears/mini-chat`, `deny.toml` -> `deny.toml`.
    """
    parts = path.split("/")
    return "/".join(parts[:2]) if len(parts) >= 3 else parts[0]


def test_sibling_source(path: str) -> str | None:
    """The source file a `<name>_tests.rs` belongs to, or None.

    Only the `<name>_tests.rs`-beside-the-source convention is recognised, because it is
    the only one that names a source file. A survey of this repository found 625 files
    following it, against 551 under `tests/` and 122 named `<name>_test.rs` — those are
    named after scenarios (`handshake_smoke.rs`, `integration_test.rs`) and matching them
    to a source file by basename pairs the wrong files as often as the right ones.

    Kept for the `tests` agent's own use, not for packing: every agent now sees every
    file, so a source and its test are never separated in the first place.
    """
    if path.endswith("_tests.rs"):
        return path[: -len("_tests.rs")] + ".rs"
    return None
