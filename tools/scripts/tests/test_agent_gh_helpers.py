#!/usr/bin/env python3
"""Unit tests for gh_pr_comments.py, gh_ci_failures.py and worktree_gc.py.

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import gh_ci_failures  # noqa: E402
import gh_pr_comments  # noqa: E402
import worktree_gc  # noqa: E402


def thread(login="coderabbitai", resolved=False, outdated=False, body="b", replies=()):
    comments = [{"databaseId": 1, "author": {"login": login}, "body": body}]
    comments += [{"databaseId": 2, "author": {"login": r}, "body": "ok"} for r in replies]
    return {
        "isResolved": resolved, "isOutdated": outdated, "path": "src/a.rs",
        "line": None, "originalLine": 7, "comments": {"nodes": comments},
    }


class CleanBodyTest(unittest.TestCase):
    def test_strips_nested_details_comments_and_tags(self):
        body = (
            "<!-- meta -->**Fix the bound.**\n\n\n<details><summary>Patch</summary>"
            "<details>inner</details>diff</details>\nKeep <b>this</b>."
        )
        self.assertEqual(gh_pr_comments.clean_body(body), "**Fix the bound.**\nKeep this.")

    def test_keep_details_keeps_text(self):
        body = "a <details><summary>s</summary>prompt</details>"
        self.assertEqual(gh_pr_comments.clean_body(body, keep_details=True), "a sprompt")

    def test_drops_bot_footer_and_raw_links(self):
        body = (
            "Real issue. ([raw.githubusercontent.com](https://raw.githubusercontent.com/x))"
            "\n---\nIf you found this review helpful, share it."
        )
        self.assertEqual(gh_pr_comments.clean_body(body), "Real issue.")

    def test_caps_length(self):
        self.assertEqual(gh_pr_comments.clean_body("x" * 20, max_chars=5), "xxxxx […]")


class SelectThreadsTest(unittest.TestCase):
    def test_default_drops_resolved_and_outdated(self):
        threads = [thread(), thread(resolved=True), thread(outdated=True)]
        self.assertEqual(len(gh_pr_comments.select(threads)), 1)

    def test_author_filter_uses_first_comment(self):
        threads = [thread(login="coderabbitai[bot]", replies=["me"]), thread(login="me")]
        picked = gh_pr_comments.select(threads, author="coderabbit")
        self.assertEqual(len(picked), 1)

    def test_summary_falls_back_to_original_line(self):
        item = gh_pr_comments.summarize(thread(replies=["me"]), False, 100)
        self.assertEqual((item["line"], len(item["replies"])), (7, 1))


LOG = "\n".join([
    "job\tUNKNOWN STEP\t﻿2026-11-12T17:19:17.6961899Z Runner name: 'r1'",
    "job\tUNKNOWN STEP\t2026-11-12T17:56:18.1Z ^[[31;1mthread 't' panicked at src/a.rs:1:5:^[[0m",
    "job\tUNKNOWN STEP\t2026-11-12T17:57:20.0Z         FAIL [   0.277s] crate t",
    "job\tUNKNOWN STEP\t2026-11-12T17:57:21.0Z ##[error]assertion failed",
    "job\tUNKNOWN STEP\t2026-11-12T17:57:21.1Z make: *** [gears-rust.mk:429: test] Error 100",
    "job\tUNKNOWN STEP\t2026-11-12T17:57:21.2Z ##[error]Process completed with exit code 2.",
    "job\tUNKNOWN STEP\t2026-11-12T17:57:22.0Z Post job cleanup.",
    "job\tUNKNOWN STEP\t2026-11-12T17:57:23.0Z Cleaning up orphan processes",
])


class SummarizeLogTest(unittest.TestCase):
    def test_highlights_errors_without_noise(self):
        highlights, _ = gh_ci_failures.summarize_log(LOG, 10)
        self.assertEqual(highlights, [
            "thread 't' panicked at src/a.rs:1:5:",
            "        FAIL [   0.277s] crate t",
            "##[error]assertion failed",
            "make: *** [gears-rust.mk:429: test] Error 100",
            "##[error]Process completed with exit code 2.",
        ])

    def test_context_ends_at_last_error_not_cleanup(self):
        _, context = gh_ci_failures.summarize_log(LOG, 2)
        self.assertEqual(context, [
            "make: *** [gears-rust.mk:429: test] Error 100",
            "##[error]Process completed with exit code 2.",
        ])

    def test_job_ids_only_for_actions_links(self):
        link = "https://github.com/o/r/actions/runs/43010140914/job/130487703943"
        self.assertEqual(gh_ci_failures.job_ids(link), ("43010140914", "130487703943"))
        self.assertIsNone(gh_ci_failures.job_ids("https://github.com/o/r/runs/130529581342"))


PORCELAIN = """worktree /repo
HEAD aaa
branch refs/heads/main

worktree /repo-feat
HEAD bbb
branch refs/heads/me/feat

worktree /tmp/scratch
HEAD ccc
detached

worktree /gone
HEAD ddd
branch refs/heads/me/gone
prunable gitdir file points to non-existent location
"""


class WorktreeGcTest(unittest.TestCase):
    def test_parse_porcelain(self):
        trees = worktree_gc.parse_porcelain(PORCELAIN)
        self.assertEqual([t.path for t in trees], ["/repo", "/repo-feat", "/tmp/scratch", "/gone"])
        self.assertTrue(trees[0].main)
        self.assertEqual(trees[1].branch, "me/feat")
        self.assertIsNone(trees[2].branch)
        self.assertTrue(trees[3].prunable)

    def verdict(self, **kw):
        wt = worktree_gc.Worktree(path="/w", head="h", branch=kw.pop("branch", "me/b"), **kw)
        worktree_gc.decide(wt)
        return wt.verdict

    def test_merged_pr_with_same_head_is_removed(self):
        pr = {"number": 1, "state": "closed", "merged": True, "head": "h"}
        self.assertEqual(self.verdict(pr=pr), "remove")

    def test_local_commits_after_merge_are_kept(self):
        pr = {"number": 1, "state": "closed", "merged": True, "head": "other"}
        self.assertEqual(self.verdict(pr=pr), "keep")

    def test_dirty_tree_is_kept(self):
        pr = {"number": 1, "state": "closed", "merged": True, "head": "h"}
        self.assertEqual(self.verdict(pr=pr, dirty=True), "keep")

    def test_fresh_branch_in_base_is_kept(self):
        self.assertEqual(self.verdict(in_base=True), "keep")

    def test_detached_head_in_base_is_removed(self):
        self.assertEqual(self.verdict(in_base=True, branch=None), "remove")

    def test_open_pr_is_kept(self):
        pr = {"number": 1, "state": "open", "merged": False, "head": "h"}
        self.assertEqual(self.verdict(pr=pr), "keep")


if __name__ == "__main__":
    unittest.main()
