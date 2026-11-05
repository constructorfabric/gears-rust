import io
import os
import subprocess
import unittest
from contextlib import redirect_stderr, redirect_stdout
from datetime import datetime, timezone
import unittest.mock as mock

import dependabot_sweeper as s


def msg(*entries):
    out = "updated-dependencies:\n"
    for name, utype in entries:
        out += f"- dependency-name: {name}\n  dependency-version: 9.9.9\n  dependency-type: direct:production\n"
        if utype:
            out += f"  update-type: version-update:semver-{utype}\n"
    return out + "...\n"


def elig(eco, entries, title="Bump x from 1.0.0 to 1.0.1", body=""):
    return s.eligibility(eco, s.parse_metadata(msg(*entries)), s.parse_from_versions(title, body))[0]


def act(**kw):
    base = dict(untrusted_commits=False, eligible=True, checks="green", mergeable="MERGEABLE",
                labels=set(), age_h=1, budget=s.Budget())
    base.update(kw)
    return s.decide(**base)


class Eligibility(unittest.TestCase):
    def test_patch(self):
        self.assertTrue(elig("cargo", [("x", "patch")]))

    def test_minor_major_nonzero(self):
        self.assertTrue(elig("cargo", [("x", "minor")], title="Bump x from 1.2.0 to 1.3.0"))

    def test_major(self):
        self.assertFalse(elig("cargo", [("x", "major")]))

    def test_cargo_zero_minor(self):
        self.assertFalse(elig("cargo", [("base64", "minor")], title="Bump base64 from 0.22.1 to 0.23.0"))

    def test_cargo_minor_unknown_from(self):
        self.assertFalse(elig("cargo", [("x", "minor")], title="Bump the group"))

    def test_group_mixed(self):
        body = "Updates `a` from 1.0.0 to 1.1.0\nUpdates `b` from 0.3.0 to 0.4.0\n"
        self.assertFalse(elig("cargo", [("a", "minor"), ("b", "minor")], title="Bump the group", body=body))

    def test_group_all_ok(self):
        body = "Updates `a` from 1.0.0 to 1.1.0\nUpdates `b` from 2.3.0 to 2.3.1\n"
        self.assertTrue(elig("cargo", [("a", "minor"), ("b", "patch")], title="Bump the group", body=body))

    def test_cargo_zero_minor_ignores_pr_text(self):
        c = commit(headline="chore(deps): Bump x from 0.22.0 to 0.23.0",
                   body="Bumps [x](https://e) from 0.22.0 to 0.23.0.\n" + msg(("x", "minor")))
        fv = s.parse_from_versions(c["messageHeadline"], c["messageBody"])
        pr_fv = s.parse_from_versions("Bump x from 1.0.0 to 1.1.0", "Bumps x from 1.0.0 to 1.1.0.")
        self.assertEqual((s.eligibility("cargo", s.parse_metadata(c["messageBody"]), fv)[0], pr_fv[None]),
                         (False, "1.0.0"))

    def test_group_commit_body_per_dependency(self):
        body = "Updates `a` from 0.3.0 to 0.4.0\nUpdates `b` from 2.3.0 to 2.3.1\n"
        self.assertEqual(s.parse_from_versions("Bump the g group with 2 updates", body),
                         {"a": "0.3.0", "b": "2.3.0"})

    def test_docker_digest(self):
        self.assertTrue(elig("docker", [("img", None)]))

    def test_actions_digest(self):
        self.assertTrue(elig("github_actions", [("a/b", None)]))

    def test_cargo_no_type(self):
        self.assertFalse(elig("cargo", [("x", None)]))

    def test_unparseable(self):
        self.assertFalse(s.eligibility("cargo", s.parse_metadata("nothing"), {})[0])

    def test_unknown_ecosystem(self):
        self.assertFalse(elig("npm", [("x", "patch")]))

    def test_ecosystem_branch(self):
        self.assertEqual(s.ecosystem_from_branch("dependabot/github_actions/foo-1"), "github_actions")


class Checks(unittest.TestCase):
    def cr(self, status, conclusion=""):
        return {"__typename": "CheckRun", "status": status, "conclusion": conclusion}

    def test_none(self):
        self.assertEqual(s.classify_checks([]), "wait")

    def test_green(self):
        r = [self.cr("COMPLETED", "SUCCESS"), self.cr("COMPLETED", "SKIPPED"),
             {"__typename": "StatusContext", "state": "SUCCESS"}]
        self.assertEqual(s.classify_checks(r), "green")

    def test_in_progress(self):
        self.assertEqual(s.classify_checks([self.cr("IN_PROGRESS")]), "wait")

    def test_status_pending(self):
        self.assertEqual(s.classify_checks([{"__typename": "StatusContext", "state": "PENDING"}]), "wait")

    def test_red(self):
        self.assertEqual(s.classify_checks([self.cr("COMPLETED", "FAILURE"), self.cr("COMPLETED", "SUCCESS")]), "red")

    def test_wait_beats_red(self):
        self.assertEqual(s.classify_checks([self.cr("COMPLETED", "FAILURE"), self.cr("QUEUED")]), "wait")

    def test_unknown_is_wait(self):
        self.assertEqual(s.classify_checks([self.cr("COMPLETED", "STALE")]), "wait")


def verdict(rollup=None, runs=None, total=None, age_min=60):
    rollup = ok_rollup() if rollup is None else rollup
    runs = RUNS if runs is None else runs
    return s.ci_verdict(rollup, runs, len(runs) if total is None else total, age_min)


class CiVerdict(unittest.TestCase):
    def test_all_good(self):
        self.assertEqual(verdict(), "green")

    def test_zero_runs(self):
        self.assertEqual(s.ci_verdict(ok_rollup(), [], 0, 60), "wait")

    def test_run_in_progress(self):
        self.assertEqual(verdict(runs=[{"status": "in_progress", "conclusion": None}]), "wait")

    def test_run_failure(self):
        self.assertEqual(verdict(runs=[{"status": "completed", "conclusion": "failure"}]), "red")

    def test_run_cancelled(self):
        self.assertEqual(verdict(runs=[{"status": "completed", "conclusion": "cancelled"}]), "red")

    def test_run_action_required(self):
        self.assertEqual(verdict(runs=[{"status": "completed", "conclusion": "action_required"}]), "red")

    def test_run_skipped_ok(self):
        runs = RUNS + [{"status": "completed", "conclusion": "skipped"}]
        self.assertEqual(verdict(runs=runs), "green")

    def test_partial_run_list(self):
        self.assertEqual(verdict(total=101), "wait")

    def test_dco_missing(self):
        self.assertEqual(verdict(rollup=ok_rollup()[:1] + ok_rollup()[2:]), "wait")

    def test_sonar_missing(self):
        self.assertEqual(verdict(rollup=ok_rollup()[:2]), "wait")

    def test_sonar_failure(self):
        r = ok_rollup()
        r[2]["conclusion"] = "FAILURE"
        self.assertEqual(verdict(rollup=r), "red")

    def test_dco_pending(self):
        r = ok_rollup()
        r[1]["state"] = "PENDING"
        self.assertEqual(verdict(rollup=r), "wait")

    def test_young_head(self):
        self.assertEqual(verdict(age_min=29), "wait")

    def test_rollup_red(self):
        r = ok_rollup() + [{"__typename": "CheckRun", "name": "x", "status": "COMPLETED", "conclusion": "FAILURE"}]
        self.assertEqual(verdict(rollup=r), "red")


class Decide(unittest.TestCase):
    def test_untrusted(self):
        self.assertEqual(act(untrusted_commits=True), "skip: non-Dependabot commit")

    def test_ineligible_label(self):
        self.assertEqual(act(eligible=False), "label needs-migration")

    def test_ineligible_already_labelled(self):
        self.assertEqual(act(eligible=False, labels={s.LABEL_MIGRATION}), "noop (labelled)")

    def test_merge(self):
        self.assertEqual(act(), "merge")

    def test_conflicting(self):
        self.assertEqual(act(mergeable="CONFLICTING"), "skip: conflicting")

    def test_mergeable_unknown(self):
        self.assertEqual(act(mergeable="UNKNOWN"), "wait: mergeability unknown")

    def test_mergeable_none(self):
        self.assertEqual(act(mergeable=None), "wait: mergeability unknown")

    def test_mergeable_unknown_keeps_budget(self):
        b = s.Budget()
        act(mergeable="UNKNOWN", budget=b)
        self.assertEqual(b.merges, 0)

    def test_wait(self):
        self.assertEqual(act(checks="wait"), "wait")

    def test_red_young(self):
        self.assertEqual(act(checks="red", age_h=10), "wait (red <72h)")

    def test_red_old_recreate(self):
        self.assertEqual(act(checks="red", age_h=100), "recreate")

    def test_red_old_recreated(self):
        self.assertEqual(act(checks="red", age_h=100, labels={s.LABEL_RECREATED}), "label needs-migration")

    def test_merge_cap(self):
        b = s.Budget()
        r = [act(budget=b) for _ in range(5)]
        self.assertEqual(r, ["merge"] * 3 + ["defer: merge cap"] * 2)

    def test_hold(self):
        self.assertEqual(act(labels={s.LABEL_HOLD}), "skip: hold")

    def test_hold_beats_ineligible(self):
        self.assertEqual(act(labels={s.LABEL_HOLD}, eligible=False), "skip: hold")

    def test_draft(self):
        self.assertEqual(act(draft=True), "skip: draft")

    def test_green_needs_migration_label(self):
        self.assertEqual(act(labels={s.LABEL_MIGRATION}), "skip: needs-migration label")

    def test_changes_requested(self):
        self.assertEqual(act(review_decision="CHANGES_REQUESTED"), "skip: changes requested")

    def test_unresolved_threads(self):
        self.assertEqual(act(unresolved_threads=True), "skip: unresolved threads")

    def test_skips_keep_budget(self):
        b = s.Budget()
        act(unresolved_threads=True, budget=b)
        self.assertEqual(b.merges, 0)

    def test_recreate_cap(self):
        b = s.Budget()
        r = [act(checks="red", age_h=100, budget=b) for _ in range(4)]
        self.assertEqual(r, ["recreate"] * 3 + ["defer: recreate cap"])


class Misc(unittest.TestCase):
    def test_age(self):
        now = datetime(2026, 1, 2, tzinfo=timezone.utc)
        self.assertEqual(s.age_hours("2026-01-01T00:00:00Z", now), 24)

    def test_approval_on_head(self):
        rv = [{"state": "APPROVED", "commit": {"oid": "a"}}]
        self.assertEqual((s.has_head_approval(rv, "a"), s.has_head_approval(rv, "b")), (True, False))

    def test_write_refuses_dry_run(self):
        with self.assertRaises(RuntimeError):
            s.Gh("o/r", False).write("pr", "merge", "1")


def commit(login="dependabot[bot]", headline="chore(deps): bump x", parents=1,
           sig=None, authors=None, body=""):
    if sig is None:
        sig = {"isValid": True, "wasSignedByGitHub": True}
    if authors is None:
        authors = [{"user": {"login": login}}]
    return {"oid": "a", "messageHeadline": headline, "messageBody": body,
            "committedDate": "2026-01-01T00:00:00Z", "parents": {"totalCount": parents},
            "authors": {"nodes": authors}, "signature": sig}


class Trust(unittest.TestCase):
    def test_genuine(self):
        self.assertTrue(s.is_trusted_commit(commit()))

    def test_foreign_author(self):
        self.assertFalse(s.is_trusted_commit(commit(login="someone")))

    def test_spoofed_base_sync(self):
        c = commit(login="MikeFalcon77", headline="Merge branch 'main' into dependabot/cargo/x", parents=2)
        self.assertFalse(s.is_trusted_commit(c))

    def test_dependabot_merge_commit(self):
        self.assertFalse(s.is_trusted_commit(commit(parents=2)))

    def test_unsigned(self):
        c = commit()
        c["signature"] = None
        self.assertFalse(s.is_trusted_commit(c))

    def test_not_signed_by_github(self):
        self.assertFalse(s.is_trusted_commit(commit(sig={"isValid": True, "wasSignedByGitHub": False})))

    def test_invalid_signature(self):
        self.assertFalse(s.is_trusted_commit(commit(sig={"isValid": False, "wasSignedByGitHub": True})))

    def test_author_without_user(self):
        self.assertFalse(s.is_trusted_commit(commit(authors=[{"user": None}])))

    def test_mixed_authors(self):
        authors = [{"user": {"login": "dependabot[bot]"}}, {"user": {"login": "someone"}}]
        self.assertFalse(s.is_trusted_commit(commit(authors=authors)))

    def test_empty_authors(self):
        self.assertFalse(s.is_trusted_commit(commit(authors=[])))


PR = {"number": 7, "title": "Bump x from 1.0.0 to 1.0.1", "headRefName": "dependabot/cargo/x", "body": ""}
def ok_rollup():
    return [{"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"__typename": "StatusContext", "context": "DCO", "state": "SUCCESS"},
            {"__typename": "CheckRun", "name": "SonarCloud Code Analysis", "status": "COMPLETED",
             "conclusion": "SUCCESS"}]


RUNS = [{"status": "completed", "conclusion": "success"}]
VIEW = {"statusCheckRollup": ok_rollup(), "mergeable": "MERGEABLE", "reviews": [], "labels": [],
        "headRefOid": "a", "isDraft": False, "reviewDecision": None}
NOW = datetime(2026, 1, 2, tzinfo=timezone.utc)


def graphql(commits, total=None):
    nodes = [{"commit": c} for c in commits]
    return {"data": {"repository": {"pullRequest": {"commits": {
        "totalCount": len(nodes) if total is None else total, "nodes": nodes}}}}}


def threads(resolved=(True,), total=None):
    return {"data": {"repository": {"pullRequest": {"reviewThreads": {
        "totalCount": len(resolved) if total is None else total,
        "nodes": [{"isResolved": r} for r in resolved]}}}}}


def fake_read(commits, total=None, view=None, runs=RUNS, runs_total=None, thr=None):
    def read(self, *args):
        if args[0] != "api":
            return VIEW if view is None else view
        if "actions/runs" in args[1]:
            return {"total_count": len(runs) if runs_total is None else runs_total,
                    "workflow_runs": runs}
        if any("reviewThreads" in a for a in args):
            return thr or threads()
        return graphql(commits, total)
    return read


GOOD = commit(body=msg(("x", "patch")))
TIMEOUT = subprocess.TimeoutExpired(["gh"], 60)


class Failures(unittest.TestCase):
    def test_read_timeout(self):
        with mock.patch("subprocess.run", side_effect=TIMEOUT):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "read FAILED (timeout)")

    def test_merge_write_timeout(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD])), \
                mock.patch("subprocess.run", side_effect=TIMEOUT):
            row = s.process(s.Gh("o/r", True), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "merge FAILED (timeout)")

    def live_run(self, fail_on, reviews=None):
        """Run a live merge; subprocess calls whose args contain a fail_on marker time out."""
        calls = []

        def fake_run(args, **kw):
            calls.append(args)
            if any(m in args for m in fail_on):
                raise TIMEOUT
            return mock.Mock(stdout='{"id": 99}')

        view = dict(VIEW, reviews=reviews or [])
        with mock.patch.object(s.Gh, "read_json",
                               fake_read([GOOD], view=view)), \
                mock.patch("subprocess.run", side_effect=fake_run):
            row = s.process(s.Gh("o/r", True), PR, s.Budget(), NOW)
        return row, calls

    def test_merge_failure_dismisses_approval(self):
        row, calls = self.live_run(["merge"])
        self.assertEqual(row[-1], "merge FAILED (timeout); approval dismissed")

    def test_dismissal_call_shape(self):
        _, calls = self.live_run(["merge"])
        self.assertEqual(calls[-1][1:5], ["api", "-X", "PUT", "repos/o/r/pulls/7/reviews/99/dismissals"])

    def test_dismissal_failure_reported(self):
        row, _ = self.live_run(["merge", "-X"])
        self.assertEqual(row[-1], "merge FAILED (timeout); approval dismiss FAILED (timeout)")

    def test_existing_approval_not_dismissed(self):
        rv = [{"state": "APPROVED", "commit": {"oid": "a"}}]
        row, calls = self.live_run(["merge"], reviews=rv)
        self.assertEqual((row[-1], len(calls)), ("merge FAILED (timeout)", 1))

    def test_approve_failure_skips_merge(self):
        row, calls = self.live_run(["event=APPROVE"])
        self.assertEqual((row[-1], len(calls)), ("merge FAILED (timeout)", 1))

    def test_eligible_dry_run_merge(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD])):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "merge (dry-run)")

    def test_more_than_100_commits_skipped(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], total=101)):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "skip: non-Dependabot commit")

    def test_head_moved_between_reads_skipped(self):
        moved = dict(GOOD, oid="b")
        with mock.patch.object(s.Gh, "read_json", fake_read([moved])):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "skip: non-Dependabot commit")

    def test_live_merge_pins_approval_to_checked_sha(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD])), \
                mock.patch("subprocess.run", return_value=mock.Mock(stdout='{"id": 99}')) as run:
            row = s.process(s.Gh("o/r", True), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "merge")
        approve, merge = (c.args[0] for c in run.call_args_list)
        self.assertEqual(approve[:3], ["gh", "api", "repos/o/r/pulls/7/reviews"])
        self.assertIn("commit_id=a", approve)
        self.assertIn("event=APPROVE", approve)
        self.assertNotIn("--silent", approve)
        self.assertEqual(merge[-2:], ["--match-head-commit", "a"])

    def test_pr_title_cannot_make_zero_x_minor_eligible(self):
        # Anyone with write access can edit the PR title/body; only the signed
        # commit message counts.
        pr = dict(PR, title="Bump x from 1.0.0 to 1.1.0", body="Updates `x` from 1.0.0 to 1.1.0")
        c = commit(headline="chore(deps): Bump x from 0.22.0 to 0.23.0",
                   body="Bumps [x](https://example) from 0.22.0 to 0.23.0.\n\n---\n" + msg(("x", "minor")))
        with mock.patch.object(s.Gh, "read_json", fake_read([c])):
            row = s.process(s.Gh("o/r", False), pr, s.Budget(), NOW)
        self.assertTrue(row[3].startswith("no: "), row)
        self.assertEqual(row[-1], "label needs-migration (dry-run)")

    def test_no_commits_skipped(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([])):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "skip: non-Dependabot commit")

    def test_fetch_commits_total(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], total=150)):
            self.assertEqual(s.fetch_commits(s.Gh("o/r", False), 7), (150, [GOOD]))

    def test_hold_live_no_writes(self):
        view = dict(VIEW, labels=[{"name": s.LABEL_HOLD}])
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], view=view)), \
                mock.patch("subprocess.run") as run:
            row = s.process(s.Gh("o/r", True), PR, s.Budget(), NOW)
        self.assertEqual((row[-1], run.call_count), ("skip: hold", 0))

    def test_draft_live_no_writes(self):
        view = dict(VIEW, isDraft=True)
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], view=view)), \
                mock.patch("subprocess.run") as run:
            row = s.process(s.Gh("o/r", True), PR, s.Budget(), NOW)
        self.assertEqual((row[-1], run.call_count), ("skip: draft", 0))

    def test_unresolved_thread_row(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], thr=threads((True, False)))):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "skip: unresolved threads")

    def test_over_100_threads_unresolved(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], thr=threads((True,), total=101))):
            self.assertTrue(s.fetch_unresolved_threads(s.Gh("o/r", False), 7))

    def test_no_workflow_runs_waits(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD], runs=[])):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "wait")

    def test_young_head_waits(self):
        with mock.patch.object(s.Gh, "read_json", fake_read([GOOD])):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), datetime(2026, 1, 1, 0, 10, tzinfo=timezone.utc))
        self.assertEqual(row[-1], "wait")

    def test_runs_read_failure(self):
        def read(self, *args):
            if args[0] == "api" and "actions/runs" in args[1]:
                raise TIMEOUT
            return fake_read([GOOD])(self, *args)
        with mock.patch.object(s.Gh, "read_json", read):
            row = s.process(s.Gh("o/r", False), PR, s.Budget(), NOW)
        self.assertEqual(row[-1], "read FAILED (timeout)")


class Helpers(unittest.TestCase):
    def test_title_pipe(self):
        self.assertEqual(s.md_title("a | b"), "a \\| b")

    def test_title_mention(self):
        self.assertEqual(s.md_title("@user"), "@\u200buser")

    def test_title_link(self):
        self.assertEqual(s.md_title("[x](y)"), "\\[x\\](y)")

    def test_title_markup(self):
        self.assertEqual(s.md_title("<b>*a*_`c`\\"), "\\<b\\>\\*a\\*\\_\\`c\\`\\\\")

    def test_title_long(self):
        out = s.md_title("x" * 300)
        self.assertEqual((len(out), out[-1]), (100, "…"))

    def test_add_label_rest(self):
        gh = mock.Mock()
        s.add_label(gh, 7, "deps:hold")
        gh.write.assert_called_once_with(
            "api", "-X", "POST", f"repos/{gh.repo}/issues/7/labels", "-f", "labels[]=deps:hold")


class Main(unittest.TestCase):
    def run_argv(self, argv, env):
        out, err = io.StringIO(), io.StringIO()
        with mock.patch("subprocess.run") as run, \
                mock.patch.dict(os.environ, env, clear=True), \
                mock.patch("sys.argv", ["dependabot_sweeper.py", *argv]), \
                redirect_stdout(out), redirect_stderr(err):
            rc = s.main()
        return rc, run.call_count

    def test_invalid_repo(self):
        self.assertEqual(self.run_argv(["--repo", "o/r; rm -rf"], {}), (2, 0))

    def test_repo_without_slash(self):
        self.assertEqual(self.run_argv(["--repo", "orr"], {}), (2, 0))

    def test_live_off_main(self):
        env = {"GITHUB_REPOSITORY": "o/r", "GITHUB_REF": "refs/heads/feature"}
        self.assertEqual(self.run_argv(["--live"], env), (2, 0))

    def test_live_without_ref(self):
        self.assertEqual(self.run_argv(["--live"], {"GITHUB_REPOSITORY": "o/r"}), (2, 0))

    def test_dry_run_makes_no_writes(self):
        prs = [{"number": i, "title": "t", "headRefName": "dependabot/cargo/x"} for i in (1, 2, 3)]
        inner = fake_read([GOOD])

        def read(self, *args):
            if args[:2] == ("label", "list"):
                return []
            return prs if args[:2] == ("pr", "list") else inner(self, *args)
        with mock.patch.object(s.Gh, "read_json", read), \
                mock.patch.object(s.Gh, "write") as write:
            rc, _ = self.run_argv([], {"GITHUB_REPOSITORY": "o/r"})
        self.assertEqual(write.call_count, 0)

    def test_deadline_defers_rest(self):
        prs = [{"number": i, "title": "t", "headRefName": "b"} for i in (1, 2, 3)]
        out = io.StringIO()
        clock = iter([0, 0, 1000, 1000, 1000])
        with mock.patch.object(s.Gh, "read_json", lambda self, *a: prs if a[:2] == ("pr", "list") else []), \
                mock.patch.object(s, "process", return_value=["#1", "t", "?", "no", "wait", "x"]) as proc, \
                mock.patch.object(s.time, "monotonic", side_effect=lambda: next(clock)), \
                mock.patch.dict(os.environ, {"GITHUB_REPOSITORY": "o/r"}, clear=True), \
                mock.patch("sys.argv", ["dependabot_sweeper.py"]), \
                redirect_stdout(out), redirect_stderr(io.StringIO()):
            rc = s.main()
        self.assertEqual((rc, proc.call_count, "deferred: run deadline reached, 2 PRs not processed" in out.getvalue()),
                         (0, 1, True))

    def run_main(self, read):
        env = {k: v for k, v in os.environ.items() if k != "GITHUB_STEP_SUMMARY"}
        env["GITHUB_REPOSITORY"] = "o/r"
        with mock.patch.object(s.Gh, "read_json", read), \
                mock.patch.dict(os.environ, env, clear=True), \
                mock.patch("sys.argv", ["dependabot_sweeper.py"]), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            return s.main()

    def test_list_failure(self):
        def read(self, *args):
            if args[0] == "pr" and args[1] == "list":
                raise subprocess.CalledProcessError(1, "gh", stderr="boom")
            return []
        self.assertEqual(self.run_main(read), 1)

    def test_list_limit_warns_and_fails(self):
        prs = [{"number": i, "title": "t", "headRefName": "b"} for i in range(s.PR_LIST_LIMIT)]
        out = io.StringIO()
        env = {"GITHUB_REPOSITORY": "o/r"}
        with mock.patch.object(s.Gh, "read_json", lambda self, *a: prs if a[:2] == ("pr", "list") else []), \
                mock.patch.object(s, "process", return_value=["#1", "t", "?", "no", "wait", "x"]), \
                mock.patch.dict(os.environ, env, clear=True), \
                mock.patch("sys.argv", ["dependabot_sweeper.py"]), \
                redirect_stdout(out), redirect_stderr(io.StringIO()):
            rc = s.main()
        self.assertEqual((rc, f"PR list hit the limit of {s.PR_LIST_LIMIT}" in out.getvalue()), (1, True))

    def test_empty_list(self):
        self.assertEqual(self.run_main(lambda self, *args: []), 0)


if __name__ == "__main__":
    unittest.main()
