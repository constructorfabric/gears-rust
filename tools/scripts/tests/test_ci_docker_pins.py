#!/usr/bin/env python3
"""Unit tests for the `docker-pins` check in ci.py (see `cmd_docker_pins`).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPTS = HERE.parent
sys.path.insert(0, str(SCRIPTS))

import ci  # noqa: E402

SHA = "a" * 64


class TestCheckDockerfileFromLines(unittest.TestCase):
    """Exercises the pure parser directly - no filesystem, no PROJECT_ROOT."""

    def test_missing_digest_is_an_error(self):
        errors, warnings = ci._check_dockerfile_from_lines("FROM debian:bookworm\n", "1.97.0")
        self.assertEqual(errors, [(1, "base image is not digest-pinned: debian:bookworm")])
        self.assertEqual(warnings, [])

    def test_rust_version_mismatch_is_a_warning_not_an_error(self):
        text = f"FROM rust:1.98.0-bookworm@sha256:{SHA}\n"
        errors, warnings = ci._check_dockerfile_from_lines(text, "1.97.0")
        self.assertEqual(errors, [])
        self.assertEqual(len(warnings), 1)
        self.assertIn("rust 1.98.0 != rust-toolchain.toml 1.97.0", warnings[0][1])

    def test_stage_reference_is_not_flagged(self):
        text = (
            f"FROM rust:1.97.0-bookworm@sha256:{SHA} AS builder\n"
            "FROM builder AS runtime\n"
        )
        errors, warnings = ci._check_dockerfile_from_lines(text, "1.97.0")
        self.assertEqual(errors, [])
        self.assertEqual(warnings, [])

    def test_scratch_is_not_flagged(self):
        errors, warnings = ci._check_dockerfile_from_lines("FROM scratch\n", "1.97.0")
        self.assertEqual(errors, [])
        self.assertEqual(warnings, [])

    def test_leading_flags_are_skipped_to_find_the_ref(self):
        text = "FROM --platform=$BUILDPLATFORM debian:bookworm\n"
        errors, warnings = ci._check_dockerfile_from_lines(text, "1.97.0")
        self.assertEqual(errors, [(1, "base image is not digest-pinned: debian:bookworm")])

    def test_variable_ref_is_a_warning_not_an_error(self):
        errors, warnings = ci._check_dockerfile_from_lines("FROM $BASE_IMAGE\n", "1.97.0")
        self.assertEqual(errors, [])
        self.assertEqual(len(warnings), 1)
        self.assertIn("could not be verified", warnings[0][1])


class TestCmdDockerPins(unittest.TestCase):
    """Exercises cmd_docker_pins end-to-end against a scratch PROJECT_ROOT."""

    def setUp(self):
        self._tmpdir = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmpdir.cleanup)
        self._orig_root = ci.PROJECT_ROOT
        ci.PROJECT_ROOT = self._tmpdir.name
        self.addCleanup(setattr, ci, "PROJECT_ROOT", self._orig_root)
        self._write("rust-toolchain.toml", '[toolchain]\nchannel = "1.97.0"\n')

    def _write(self, rel, content):
        path = Path(ci.PROJECT_ROOT) / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def _run(self):
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            try:
                ci.cmd_docker_pins(None)
            except SystemExit as exc:
                return exc.code or 0, stdout.getvalue()
        return 0, stdout.getvalue()

    def test_missing_digest_fails_the_command(self):
        self._write("a.Dockerfile", "FROM debian:bookworm\n")
        code, output = self._run()
        self.assertEqual(code, 1)
        self.assertIn("not digest-pinned", output)

    def test_rust_mismatch_warns_but_exits_zero(self):
        self._write("a.Dockerfile", f"FROM rust:1.98.0-bookworm@sha256:{SHA}\n")
        code, output = self._run()
        self.assertEqual(code, 0)
        self.assertIn("WARNING", output)

    def test_github_actions_env_emits_a_workflow_annotation(self):
        self._write("a.Dockerfile", f"FROM rust:1.98.0-bookworm@sha256:{SHA}\n")
        old = os.environ.get("GITHUB_ACTIONS")
        os.environ["GITHUB_ACTIONS"] = "true"
        try:
            code, output = self._run()
        finally:
            if old is None:
                os.environ.pop("GITHUB_ACTIONS", None)
            else:
                os.environ["GITHUB_ACTIONS"] = old
        self.assertEqual(code, 0)
        self.assertIn("::warning file=a.Dockerfile,line=1::", output)


if __name__ == "__main__":
    unittest.main()
