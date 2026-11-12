#!/usr/bin/env python3
"""Unit tests for codeql_gate.py (release gate on open CodeQL alerts).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import contextlib
import io
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import codeql_gate  # noqa: E402


def alert(number: int, path: str, sev: str = "high", rule: str = "rust/cleartext-logging") -> dict:
    return {
        "number": number,
        "rule": {"id": rule, "security_severity_level": sev},
        "most_recent_instance": {"location": {"path": path}},
    }


def entry(**kw) -> dict:
    return {"rule": "rust/cleartext-logging", "path": "a/b.rs", "reason": "test", **kw}


def run(alerts, entries, today="2026-11-12") -> int:
    with contextlib.redirect_stdout(io.StringIO()):
        return codeql_gate.evaluate(alerts, entries, today)


class EvaluateTest(unittest.TestCase):
    def test_unlisted_high_alert_blocks(self) -> None:
        self.assertEqual(run([alert(1, "a/b.rs")], []), 1)

    def test_listed_alert_passes(self) -> None:
        self.assertEqual(run([alert(1, "a/b.rs")], [entry()]), 0)

    def test_low_severity_ignored(self) -> None:
        self.assertEqual(run([alert(1, "a/b.rs", sev="medium")], []), 0)

    def test_alert_number_must_match(self) -> None:
        self.assertEqual(run([alert(2, "a/b.rs")], [entry(alert=1)]), 1)
        self.assertEqual(run([alert(1, "a/b.rs")], [entry(alert=1)]), 0)

    def test_expired_entry_does_not_suppress(self) -> None:
        self.assertEqual(run([alert(1, "a/b.rs")], [entry(expires="2026-11-11")]), 1)
        self.assertEqual(run([alert(1, "a/b.rs")], [entry(expires="2026-11-12")]), 0)


class AllowlistValidationTest(unittest.TestCase):
    def _load(self, text: str):
        import tempfile

        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "allow.toml"
            p.write_text(text)
            return codeql_gate.load_allowlist(str(p))

    def test_bad_expiry_exits(self) -> None:
        with self.assertRaises(SystemExit):
            self._load('[[ignore]]\nrule="r"\npath="p"\nreason="x"\nexpires="never"\n')

    def test_missing_reason_exits(self) -> None:
        with self.assertRaises(SystemExit):
            self._load('[[ignore]]\nrule="r"\npath="p"\n')

    def test_valid_entry_loads(self) -> None:
        self.assertEqual(len(self._load('[[ignore]]\nrule="r"\npath="p"\nreason="x"\nalert=3\n')), 1)


if __name__ == "__main__":
    unittest.main()
