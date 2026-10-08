#!/usr/bin/env python3
"""Unit tests for junit_enrich.py (source locations for nextest/pytest JUnit).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import sys
import tempfile
import unittest
import unittest.mock
import xml.etree.ElementTree as ET
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))

import junit_enrich  # noqa: E402

# Shape of a real nextest 0.9.148 report (trimmed).
REPORT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="2">
  <testsuite name="crate::probe" tests="3" failures="2">
    <testcase name="passes" classname="crate::probe" time="0.002"/>
    <testcase name="fails_assert_eq" classname="crate::probe" time="0.003">
      <failure message="thread &apos;fails_assert_eq&apos; (1) panicked at libs/x/tests/probe.rs:7:5" type="test failure">thread &apos;fails_assert_eq&apos; (1) panicked at libs/x/tests/probe.rs:7:5:
assertion `left == right` failed: math is broken
  left: 2
 right: 3
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace</failure>
    </testcase>
    <testcase name="times_out" classname="crate::probe" time="60.0">
      <failure message="test timed out" type="timeout">test timed out after 60s</failure>
    </testcase>
  </testsuite>
</testsuites>
"""


class EnrichTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False, encoding="utf-8")
        tmp.write(REPORT)
        tmp.close()
        self.path = tmp.name
        self.addCleanup(Path(self.path).unlink)

    def cases(self) -> dict[str, ET.Element]:
        return {c.get("name"): c for c in ET.parse(self.path).iter("testcase")}

    def test_panic_gets_location_and_assertion_text(self) -> None:
        self.assertEqual(junit_enrich.enrich(self.path), 1)
        case = self.cases()["fails_assert_eq"]
        self.assertEqual(case.get("file"), "libs/x/tests/probe.rs")
        self.assertEqual(case.get("line"), "7")
        message = case.find("failure").get("message")
        self.assertTrue(message.startswith("libs/x/tests/probe.rs:7: assertion `left == right` failed: math is broken"))
        self.assertIn("right: 3", message)
        self.assertNotIn("RUST_BACKTRACE", message)

    def test_unrecognised_failures_and_passes_are_untouched(self) -> None:
        junit_enrich.enrich(self.path)
        cases = self.cases()
        self.assertIsNone(cases["times_out"].get("file"))
        self.assertEqual(cases["times_out"].find("failure").get("message"), "test timed out")
        self.assertIsNone(cases["passes"].get("file"))

    def test_idempotent(self) -> None:
        junit_enrich.enrich(self.path)
        first = Path(self.path).read_text(encoding="utf-8")
        junit_enrich.enrich(self.path)
        self.assertEqual(Path(self.path).read_text(encoding="utf-8"), first)


# Shape of a pytest --tb=short report, run from the repository root.
PYTEST_REPORT = """<?xml version="1.0" encoding="utf-8"?>
<testsuites><testsuite name="e2e-probe" tests="2" failures="2">
  <testcase classname="suites.x.test_probe" name="test_direct" time="0.1">
    <failure message="AssertionError: health endpoint not ready&#10;assert 503 == 200">testing/e2e/suites/x/test_probe.py:9: in test_direct
    assert status == 200, "health endpoint not ready"
E   AssertionError: health endpoint not ready</failure>
  </testcase>
  <testcase classname="suites.x.test_probe" name="test_via_helper" time="0.1">
    <failure message="AssertionError: bad status 500">testing/e2e/suites/x/test_probe.py:17: in test_via_helper
    check(500)
testing/e2e/helpers/http.py:2: in check
    assert v == 200, f"bad status {v}"
/usr/lib/python3/site-packages/requests/api.py:59: in request
E   AssertionError: bad status 500</failure>
  </testcase>
</testsuite></testsuites>
"""


class EnrichPytestTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False, encoding="utf-8")
        tmp.write(PYTEST_REPORT)
        tmp.close()
        self.path = tmp.name
        self.addCleanup(Path(self.path).unlink)

    def test_helper_in_the_test_file_resolves_to_the_call_in_the_test(self) -> None:
        text = (
            "testing/e2e/suites/x/test_probe.py:13: in test_fails\n"
            "    _expect_ok(503)\n"
            "testing/e2e/suites/x/test_probe.py:5: in _expect_ok\n"
            "    assert status == 200\n"
            "E   AssertionError\n"
        )
        self.assertEqual(
            junit_enrich.python_location(text, "test_fails[param-1]"),
            ("testing/e2e/suites/x/test_probe.py", "13"),
        )

    def test_location_is_the_test_file_line_and_message_is_kept(self) -> None:
        self.assertEqual(junit_enrich.enrich(self.path), 2)
        cases = {c.get("name"): c for c in ET.parse(self.path).iter("testcase")}
        direct, helper = cases["test_direct"], cases["test_via_helper"]
        self.assertEqual((direct.get("file"), direct.get("line")), ("testing/e2e/suites/x/test_probe.py", "9"))
        # The call site in the test, not the helper or a third-party frame.
        self.assertEqual((helper.get("file"), helper.get("line")), ("testing/e2e/suites/x/test_probe.py", "17"))
        self.assertEqual(helper.find("failure").get("message"), "AssertionError: bad status 500")


class GithubOutputTest(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.NamedTemporaryFile("w", suffix=".xml", delete=False, encoding="utf-8")
        tmp.write(REPORT)
        tmp.close()
        self.path = tmp.name
        self.addCleanup(Path(self.path).unlink)
        junit_enrich.enrich(self.path)

    def test_counts_and_failures(self) -> None:
        counts, failures = junit_enrich.collect(self.path)
        self.assertEqual(counts, {"passed": 1, "failed": 2, "skipped": 0, "flaky": 0})
        located = [f for f in failures if f["file"]][0]
        self.assertEqual(located["test"], "crate::probe › fails_assert_eq")
        # The location is not repeated inside the message.
        self.assertTrue(located["message"].startswith("assertion `left == right` failed"))

    def test_annotation_is_escaped(self) -> None:
        _, failures = junit_enrich.collect(self.path)
        line = junit_enrich.annotation([f for f in failures if f["file"]][0])
        self.assertTrue(line.startswith("::error file=libs/x/tests/probe.rs,line=7,title=crate%3A%3Aprobe › fails_assert_eq::"))
        self.assertNotIn("\n", line)
        self.assertIn("math is broken%0A  left: 2", line)

    def test_summary_truncates_long_messages(self) -> None:
        failure = {"test": "t", "file": "", "line": "", "message": "x" * 5000}
        counts = {"passed": 0, "failed": 1, "skipped": 0, "flaky": 0}
        text = junit_enrich.summary("unit", counts, [failure])
        self.assertIn("x" * junit_enrich.MAX_MESSAGE + "…", text)
        self.assertNotIn("x" * (junit_enrich.MAX_MESSAGE + 1), text)

    def test_log_block_names_test_location_and_message(self) -> None:
        counts, failures = junit_enrich.collect(self.path)
        text = junit_enrich.log_block("unit", counts, failures)
        self.assertIn("Failed tests in unit: 2 of 3", text)
        self.assertIn("✗ crate::probe › fails_assert_eq\n    at libs/x/tests/probe.rs:7\n    assertion", text)

    def test_summary_links_source(self) -> None:
        counts, failures = junit_enrich.collect(self.path)
        env = {"GITHUB_REPOSITORY": "o/r", "GITHUB_SHA": "abc", "GITHUB_SERVER_URL": "https://github.com"}
        with unittest.mock.patch.dict("os.environ", env):
            text = junit_enrich.summary("unit", counts, failures)
        self.assertIn("#### ❌ unit — 2 failed, 1 passed, 0 skipped", text)
        self.assertIn("[libs/x/tests/probe.rs:7](https://github.com/o/r/blob/abc/libs/x/tests/probe.rs#L7)", text)
        self.assertIn("**crate::probe › times_out**", text)


if __name__ == "__main__":
    unittest.main()
