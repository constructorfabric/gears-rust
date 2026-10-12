#!/usr/bin/env python3
"""
Turn rustc / clippy / dylint errors in a saved build log into annotations.

A problem matcher could find `error: ... --> file:line:col`, but it cannot
set an annotation title, so GitHub falls back to the step name -- the same
header as its own "Process completed with exit code N" annotation. This
reads the log instead and, for every error diagnostic with a location,
prints

    ::error file=...,line=...,col=...,title=<code>::<message + help>

where <code> is the rustc code (`E0308`), a lint code at the end of the
message (`... (DE0904)`), or the lint's name (`clippy::doc_markdown`). The
same list is printed as plain text at the end, which is where GitHub scrolls
a failed step to. Colour codes in the log are ignored.

Never fails: it reports on a step whose outcome the caller already has.

Usage: rustc_annotate.py BUILD.log
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from junit_enrich import MAX_ANNOTATIONS, escape_data, escape_property  # noqa: E402

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
HEAD_RE = re.compile(r"^error(?:\[(?P<code>[^\]]+)\])?: (?P<msg>.+)$")
LOC_RE = re.compile(r"^\s*--> (?P<file>[^\s:]+):(?P<line>\d+):(?P<col>\d+)")
TRAILING_CODE_RE = re.compile(r"\s*\((?P<code>[A-Z]{1,4}\d{2,5})\)$")
LINT_RE = re.compile(r"#\[deny\((?P<a>[\w:]+)\)\]|-D (?P<b>[\w:-]+)")
HELP_RE = re.compile(r"^\s*(?:= )?help: (?P<text>.+)$")


def parse(text: str) -> list[dict[str, str]]:
    lines = [ANSI_RE.sub("", l).rstrip() for l in text.splitlines()]
    found, seen = [], set()
    for i, line in enumerate(lines[:-1]):
        head = HEAD_RE.match(line)
        loc = LOC_RE.match(lines[i + 1]) if head else None
        if not loc:
            continue
        # The diagnostic runs to the next blank line.
        block = []
        for l in lines[i + 2:]:
            if not l.strip():
                break
            block.append(l)
        msg = head.group("msg")
        title = head.group("code")
        if not title:
            m = TRAILING_CODE_RE.search(msg)
            if m:
                title, msg = m.group("code"), msg[: m.start()]
        if not title:
            for l in block:
                m = LINT_RE.search(l)
                if m:
                    title = (m.group("a") or m.group("b")).replace("-", "_")
                    break
        helps = [
            "help: " + h.group("text")
            for h in map(HELP_RE.match, block)
            # Skip links, and headers of a suggested diff ("help: try") that
            # only make sense next to the diff itself.
            if h and not h.group("text").startswith("for further information")
            and not h.group("text").rstrip().endswith((":", "try"))
        ]
        item = {
            "title": title or "error",
            "file": loc.group("file").replace("\\", "/"),
            "line": loc.group("line"),
            "col": loc.group("col"),
            "message": "\n".join([msg, *helps]),
        }
        key = (item["file"], item["line"], item["col"], item["title"], item["message"])
        if key not in seen:
            seen.add(key)
            found.append(item)
    return found


def annotation(e: dict[str, str]) -> str:
    props = ",".join([
        f"file={escape_property(e['file'])}",
        f"line={e['line']}",
        f"col={e['col']}",
        f"title={escape_property(e['title'])}",
    ])
    return f"::error {props}::{escape_data(e['message'])}"


def log_block(errors: list[dict[str, str]]) -> str:
    out = ["", f"──── Rust errors: {len(errors)} ────"]
    for e in errors:
        out += [f"✗ {e['title']}", f"    at {e['file']}:{e['line']}:{e['col']}"]
        out += [f"    {l}" for l in e["message"].splitlines()]
    return "\n".join(out) + "\n"


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    try:
        with open(argv[0], encoding="utf-8", errors="replace") as fh:
            errors = parse(fh.read())
    except OSError as exc:
        print(f"rustc_annotate: {exc}")
        return 0
    for e in errors[:MAX_ANNOTATIONS]:
        print(annotation(e))
    if len(errors) > MAX_ANNOTATIONS:
        print(f"::notice::{len(errors) - MAX_ANNOTATIONS} more error(s); see the end of the log")
    if errors:
        print(log_block(errors), end="")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
