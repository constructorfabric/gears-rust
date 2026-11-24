import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from results import collect, render


class ResultTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.work = Path(tmp.name)
        (self.work / "out").mkdir()
        self.ctx = {"agents": [{"name": "errors"}], "mode": "pr", "head_sha": "abc",
                    "files": {"a.rs": {"status": "modified",
                                       "ranges": {"left": [[7, 7]], "right": [[2, 2]]}}},
                    "skipped_files": []}
        self.path = self.work / "out/errors.json"
        self.finding = dict(id="RUST-ERR-001", file="a.rs", line=7, side="LEFT",
                            severity="HIGH", issue="Issue", comment="Comment", fix="Fix")

    def finish(self, excluded=None):
        rows, warnings = collect(self.work, self.ctx)
        render(self.work, self.ctx, rows, warnings, excluded or {})
        plan = json.loads((self.work / "publication.json").read_text())
        return warnings, plan["review"]

    def write_pass(self, findings, status="complete"):
        self.path.write_text(json.dumps(dict(status=status, findings=findings,
                                             unread_files=[], notes=[])))

    def test_corruption_and_missing_pass_never_publish_clean(self):
        for raw in (None, "[", "[]"):
            if raw is not None:
                self.path.write_text(raw)
            warnings, review = self.finish()
            self.assertTrue(warnings)
            self.assertIsNone(review)

    def test_partial_keeps_valid_candidates_and_checks_side(self):
        self.write_pass([self.finding, dict(self.finding, side="RIGHT")], "partial")
        warnings, review = self.finish()
        self.assertEqual(len(warnings), 2)
        self.assertEqual([c["side"] for c in review["comments"]], ["LEFT"])

    def test_distinct_findings_survive_shared_anchor_and_id(self):
        self.write_pass([self.finding, dict(self.finding, comment="Other")])
        warnings, review = self.finish()
        self.assertFalse(warnings)
        self.assertEqual(len(review["comments"]), 2)
        _, review = self.finish({"errors:0": "Already reported in comment 123"})
        self.assertEqual(review["comments"][0]["body"], "**HIGH**\n\nOther")

    def test_local_report_cannot_publish(self):
        self.ctx["mode"] = "local"
        self.write_pass([self.finding])
        _, review = self.finish()
        self.assertIsNone(review)
        self.assertIn("a.rs:7 (LEFT)", (self.work / "report.md").read_text())
