#!/usr/bin/env python3
"""Unit tests for release_pending.py (release gate pending check).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import release_pending  # noqa: E402

PKGS = [
    {"name": "lib", "version": "1.0.0", "publish": None},
    {"name": "hello", "version": "0.1.6", "publish": []},
    {"name": "internal", "version": "0.1.0", "publish": []},
]
GIT_ONLY = {"hello"}


def on_crates_io(name, version):
    return True


class GitOnlyNamesTest(unittest.TestCase):
    def test_only_git_only_true(self) -> None:
        text = '[[package]]\nname = "hello"\ngit_only = true\n[[package]]\nname = "x"\ngit_only = false\n[[package]]\nname = "y"\n'
        self.assertEqual(release_pending.git_only_names(text), {"hello"})

    def test_invalid_toml_raises(self) -> None:
        with self.assertRaises(Exception):
            release_pending.git_only_names("[[package")


class RemoteTagsTest(unittest.TestCase):
    def test_parses_and_folds_peeled(self) -> None:
        out = "abc\trefs/tags/hello-v0.1.6\ndef\trefs/tags/hello-v0.1.6^{}\n123\trefs/tags/v1\n"
        self.assertEqual(release_pending.remote_tags(out), {"hello-v0.1.6", "v1"})


class PendingCratesTest(unittest.TestCase):
    def test_tag_present_not_pending(self) -> None:
        self.assertEqual(release_pending.pending_crates(PKGS, on_crates_io, GIT_ONLY, {"hello-v0.1.6"}), [])

    def test_tag_absent_pending(self) -> None:
        self.assertEqual(release_pending.pending_crates(PKGS, on_crates_io, GIT_ONLY, {"hello-v0.1.5"}), ["hello 0.1.6 (no tag)"])

    def test_ls_remote_failure_pending(self) -> None:
        self.assertEqual(release_pending.pending_crates(PKGS, on_crates_io, GIT_ONLY, None), ["hello 0.1.6 (no tag)"])

    def test_non_git_only_publish_false_skipped(self) -> None:
        pending = release_pending.pending_crates(PKGS, on_crates_io, GIT_ONLY, None)
        self.assertNotIn("internal 0.1.0 (no tag)", pending)

    def test_unpublished_crate_pending(self) -> None:
        self.assertEqual(release_pending.pending_crates(PKGS, lambda n, v: False, GIT_ONLY, {"hello-v0.1.6"}), ["lib 1.0.0"])


if __name__ == "__main__":
    unittest.main()
