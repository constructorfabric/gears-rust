#!/usr/bin/env python3
"""Unit tests for backfill_release_tags.py (release tag backfill).

    python3 -m unittest discover -s tools/scripts/tests -p test_backfill_release_tags.py
"""

from __future__ import annotations

import base64
import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import backfill_release_tags as bt  # noqa: E402

NOW = datetime(2026, 11, 12, 12, 0, tzinfo=timezone.utc)
OLD = NOW - timedelta(hours=2)


def pkg(name, version, manifest="Cargo.toml"):
    return {"name": name, "version": version, "manifest_path": os.path.abspath(manifest)}


def manifest(version, dep_version=None):
    text = f'[package]\nname = "foo"\nversion = "{version}"\n'
    if dep_version:
        text += f'\n[dependencies]\nbar = {{ version = "{dep_version}", path = "../bar" }}\n'
    return text


class IndexPathTest(unittest.TestCase):
    def test_one_char(self):
        self.assertEqual(bt.index_path("a"), "1/a")

    def test_two_chars(self):
        self.assertEqual(bt.index_path("ab"), "2/ab")

    def test_three_chars(self):
        self.assertEqual(bt.index_path("abc"), "3/a/abc")

    def test_four_chars(self):
        self.assertEqual(bt.index_path("abcd"), "ab/cd/abcd")

    def test_long_name_is_lowercased(self):
        self.assertEqual(bt.index_path("Cf-Gears"), "cf/-g/cf-gears")


class IntroducingCommitTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.old_cwd = os.getcwd()
        self.addCleanup(os.chdir, self.old_cwd)
        os.chdir(self.tmp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        self.git("config", "commit.gpgsign", "false")

    def git(self, *args):
        return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout.strip()

    def commit(self, text, msg):
        Path("Cargo.toml").write_text(text)
        self.git("add", "Cargo.toml")
        self.git("commit", "-q", "-m", msg)
        return self.git("rev-parse", "HEAD")

    def test_decoy_dependency_line_with_same_version_is_ignored(self):
        self.commit(manifest("0.1.0", dep_version="0.2.3"), "decoy first")
        real = self.commit(manifest("0.2.3", dep_version="0.2.3"), "bump")
        self.assertEqual(bt.introducing_commit("0.2.3", "Cargo.toml"), real)

    def test_bump_returns_the_bump_commit_not_an_earlier_version(self):
        self.commit(manifest("0.1.0"), "v1")
        bump = self.commit(manifest("0.2.0"), "v2")
        self.commit(manifest("0.2.0", dep_version="9.9.9"), "unrelated edit")
        self.assertEqual(bt.introducing_commit("0.2.0", "Cargo.toml"), bump)

    def test_revert_and_reintroduce_returns_oldest_of_run_reaching_head(self):
        self.commit(manifest("0.2.0"), "first 0.2.0")
        self.commit(manifest("0.1.0"), "revert")
        again = self.commit(manifest("0.2.0"), "reintroduce")
        self.commit(manifest("0.2.0", dep_version="1.0.0"), "edit")
        self.assertEqual(bt.introducing_commit("0.2.0", "Cargo.toml"), again)

    def test_version_not_in_history_returns_none(self):
        self.commit(manifest("0.1.0"), "v1")
        self.assertIsNone(bt.introducing_commit("0.9.9", "Cargo.toml"))

    def test_toml_1_1_manifest_falls_back_to_package_scan(self):
        text = '[package]\nname = "foo"\nversion = "0.4.0"\n\n[dependencies]\nbar = {\n    path = "../bar",\n}\n'
        sha = self.commit(text, "toml 1.1")
        self.assertEqual(bt.introducing_commit("0.4.0", "Cargo.toml"), sha)

    def test_workspace_inherited_version_returns_none(self):
        self.commit('[package]\nname = "foo"\nversion.workspace = true\n', "ws")
        self.assertIsNone(bt.introducing_commit("0.1.0", "Cargo.toml"))


class BuildPlanTest(unittest.TestCase):
    def plan(self, packages, existing=(), published=True, created=OLD, sha="abc"):
        def created_at(name, version):
            if isinstance(created, Exception):
                raise created
            return created

        with contextlib.redirect_stdout(io.StringIO()):
            return bt.build_plan(
                packages, set(existing), now=NOW,
                on_crates_io=lambda n, v: published,
                introducing_commit=lambda v, m: sha,
                created_at=created_at,
            )

    def test_missing_published_tag_is_planned(self):
        self.assertEqual(self.plan([pkg("foo", "1.0.0")]), [("foo", "1.0.0", "foo-v1.0.0", "abc")])

    def test_existing_tag_is_skipped(self):
        self.assertEqual(self.plan([pkg("foo", "1.0.0")], existing={"foo-v1.0.0"}), [])

    def test_unpublished_version_is_skipped(self):
        self.assertEqual(self.plan([pkg("foo", "1.0.0")], published=False), [])

    def test_too_fresh_version_is_skipped(self):
        self.assertEqual(self.plan([pkg("foo", "1.0.0")], created=NOW - timedelta(minutes=5)), [])

    def test_age_api_error_skips_candidate_with_warning(self):
        self.assertEqual(self.plan([pkg("foo", "1.0.0")], created=OSError("boom")), [])

    def test_no_introducing_commit_is_skipped(self):
        self.assertEqual(self.plan([pkg("foo", "1.0.0")], sha=None), [])


PLAN = [("foo", "1.0.0", "foo-v1.0.0", "abc123")]


class ApplyPlanTest(unittest.TestCase):
    def apply(self, run, remote_tags=lambda: set(), dry_run=False):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            result = bt.apply_plan(PLAN, dry_run, run=run, remote_tags=remote_tags,
                                   token="s3cret", repo="org/repo")
        return result, out.getvalue()

    @staticmethod
    def failing_push(args, **kwargs):
        if args[:2] == ["git", "push"]:
            raise subprocess.CalledProcessError(1, args, stderr="rejected")
        return ""

    def test_dry_run_runs_nothing(self):
        run = mock.Mock()
        (done, failed), out = self.apply(run, dry_run=True)
        self.assertEqual((run.call_count, done, failed, "[dry-run]" in out), (0, [], [], True))

    def test_success_is_reported_done(self):
        (done, failed), _ = self.apply(mock.Mock(return_value=""))
        self.assertEqual((done, failed), (["foo-v1.0.0"], []))

    def test_push_env_has_header_and_token_not_in_argv(self):
        run = mock.Mock(return_value="")
        _, out = self.apply(run)
        push = [c for c in run.call_args_list if c.args[0][:2] == ["git", "push"]][0]
        b64 = base64.b64encode(b"x-access-token:s3cret").decode()
        env = push.kwargs["env"]
        self.assertEqual(
            (env["GIT_CONFIG_VALUE_0"], "s3cret" in " ".join(push.args[0]), f"::add-mask::{b64}" in out),
            (f"AUTHORIZATION: basic {b64}", False, True),
        )

    def test_push_env_extraheader_key(self):
        run = mock.Mock(return_value="")
        self.apply(run)
        push = [c for c in run.call_args_list if c.args[0][:2] == ["git", "push"]][0]
        self.assertEqual(push.kwargs["env"]["GIT_CONFIG_KEY_0"], "http.https://github.com/.extraheader")

    def test_tag_message_matches_release_plz_format(self):
        run = mock.Mock(return_value="")
        self.apply(run)
        tag = run.call_args_list[0].args[0]
        self.assertEqual(tag[-1], "chore: Release package foo version 1.0.0")

    def test_push_failure_with_tag_on_origin_is_not_a_failure(self):
        (done, failed), out = self.apply(self.failing_push, remote_tags=lambda: {"foo-v1.0.0"})
        self.assertEqual((done, failed, "concurrent run" in out), ([], [], True))

    def test_other_push_failure_is_an_error(self):
        (done, failed), out = self.apply(self.failing_push)
        self.assertEqual((failed, "::error::" in out, "s3cret" in out), (["foo-v1.0.0"], True, False))


class MainTest(unittest.TestCase):
    def run_main(self, argv, plan, apply_result=((), ())):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), \
                mock.patch.object(bt, "publishable_packages", return_value=[]), \
                mock.patch.object(bt, "remote_tags", return_value=set()), \
                mock.patch.object(bt, "build_plan", return_value=plan), \
                mock.patch.object(bt, "apply_plan", return_value=apply_result), \
                mock.patch.object(bt, "write_summary"):
            return bt.main(argv), out.getvalue()

    def test_no_plan_exits_zero(self):
        self.assertEqual(self.run_main([], [])[0], 0)

    def test_failed_tag_exits_one(self):
        self.assertEqual(self.run_main([], PLAN, ([], ["foo-v1.0.0"]))[0], 1)

    def test_network_error_exits_one_with_annotation(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), \
                mock.patch.object(bt, "publishable_packages", return_value=[]), \
                mock.patch.object(bt, "remote_tags", return_value=set()), \
                mock.patch.object(bt, "build_plan", side_effect=bt.urllib.error.URLError("down")):
            code = bt.main([])
        self.assertEqual((code, "::error::" in out.getvalue()), (1, True))


if __name__ == "__main__":
    unittest.main()
