#!/usr/bin/env python3
"""Unit tests for rustc_annotate.py (titled annotations for compiler errors).

    python3 -m unittest discover -s tools/scripts/tests
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import rustc_annotate  # noqa: E402

# Trimmed from real CI logs, colour codes included where the log had them.
DYLINT = (
    "\x1b[1m\x1b[91merror\x1b[0m\x1b[1m: hard-coded GTS ID prefix; use gts_id!(\"<suffix>\") instead (DE0904)\x1b[0m\n"
    "    \x1b[1m\x1b[94m--> \x1b[0mlibs/toolkit-security/src/access_scope.rs:1600:31\n"
    "     |\n"
    "1600 |     const MEMBER_TYPE: &str = \"gts.cf.core.rg.type.v1~\";\n"
    "     |\n"
    "     = help: for example: gts_id!(\"cf.core.users.user.v1~\")\n"
    "     = note: `#[deny(de0904_no_hardcoded_gts_prefix)]` on by default\n"
    "\n"
    "error: could not compile `cf-gears-toolkit-security` (lib) due to 1 previous error\n"
)

CLIPPY = (
    "error: item in documentation is missing backticks\n"
    " --> libs/x/tests/probe.rs:1:29\n"
    "  |\n"
    "  = help: for further information visit https://rust-lang.github.io/rust-clippy/\n"
    "  = note: requested on the command line with `-D clippy::doc-markdown`\n"
    "help: try\n"
    "  |\n"
    "\n"
)

RUSTC = (
    "error[E0308]: mismatched types\n"
    "  --> libs\\x\\src\\lib.rs:10:5\n"
    "   |\n"
    "\n"
)


class ParseTest(unittest.TestCase):
    def test_trailing_lint_code_becomes_the_title(self) -> None:
        [e] = rustc_annotate.parse(DYLINT)
        self.assertEqual(e["title"], "DE0904")
        self.assertEqual((e["file"], e["line"], e["col"]), ("libs/toolkit-security/src/access_scope.rs", "1600", "31"))
        self.assertEqual(
            e["message"],
            'hard-coded GTS ID prefix; use gts_id!("<suffix>") instead\nhelp: for example: gts_id!("cf.core.users.user.v1~")',
        )

    def test_clippy_lint_name_and_useless_help_dropped(self) -> None:
        [e] = rustc_annotate.parse(CLIPPY)
        self.assertEqual(e["title"], "clippy::doc_markdown")
        self.assertEqual(e["message"], "item in documentation is missing backticks")

    def test_rustc_code_and_windows_path(self) -> None:
        [e] = rustc_annotate.parse(RUSTC)
        self.assertEqual((e["title"], e["file"]), ("E0308", "libs/x/src/lib.rs"))

    def test_annotation_escapes_title(self) -> None:
        [e] = rustc_annotate.parse(CLIPPY)
        self.assertTrue(rustc_annotate.annotation(e).startswith(
            "::error file=libs/x/tests/probe.rs,line=1,col=29,title=clippy%3A%3Adoc_markdown::"
        ))

    def test_same_line_different_columns_are_kept(self) -> None:
        other_col = RUSTC.replace("lib.rs:10:5", "lib.rs:10:9")
        self.assertEqual(len(rustc_annotate.parse(RUSTC + other_col)), 2)

    def test_duplicates_and_locationless_errors_are_skipped(self) -> None:
        self.assertEqual(len(rustc_annotate.parse(DYLINT + DYLINT)), 1)
        self.assertEqual(rustc_annotate.parse("error: test run failed\n"), [])


if __name__ == "__main__":
    unittest.main()
