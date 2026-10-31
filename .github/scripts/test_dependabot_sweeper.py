import unittest
from datetime import datetime, timezone

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
    base = dict(human_commits=False, eligible=True, checks="green", mergeable="MERGEABLE",
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


class Decide(unittest.TestCase):
    def test_human(self):
        self.assertEqual(act(human_commits=True), "skip: human commits")

    def test_ineligible_label(self):
        self.assertEqual(act(eligible=False), "label needs-migration")

    def test_ineligible_already_labelled(self):
        self.assertEqual(act(eligible=False, labels={s.LABEL_MIGRATION}), "noop (labelled)")

    def test_merge(self):
        self.assertEqual(act(), "merge")

    def test_conflicting(self):
        self.assertEqual(act(mergeable="CONFLICTING"), "skip: conflicting")

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


class HumanEdits(unittest.TestCase):
    def commit(self, login, headline):
        return {"authors": [{"login": login}], "messageHeadline": headline}

    def test_dependabot_commit_is_not_human(self):
        self.assertFalse(s.is_human_edit(self.commit("dependabot[bot]", "chore(deps): bump x")))

    def test_base_sync_merge_is_not_human_edit(self):
        c = self.commit("MikeFalcon77", "Merge branch 'main' into dependabot/cargo/rusty-s3-0.10.2")
        self.assertTrue(s.is_foreign(c))
        self.assertFalse(s.is_human_edit(c))

    def test_real_human_commit_is_human_edit(self):
        self.assertTrue(s.is_human_edit(self.commit("someone", "fix: adapt call sites")))


if __name__ == "__main__":
    unittest.main()
