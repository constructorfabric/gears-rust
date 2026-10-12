#!/usr/bin/env python3
"""
Make JUnit failures point at the source that failed (nextest and pytest).

nextest records a failing test as

    <failure message="thread 'x' panicked at libs/foo/tests/bar.rs:7:5" ...>
    thread 'x' panicked at libs/foo/tests/bar.rs:7:5:
    assertion `left == right` failed: math is broken
      left: 2
     right: 3
    note: run with `RUST_BACKTRACE=1` ...
    </failure>

so the one line a report shows (`message`) carries the location and drops the
reason, and nothing machine-readable says which file or line it was. This
rewrites, in place, each failing <testcase> whose output has a panic location:

- `message` becomes the panic message -- the assertion text, `left`/`right`
  included -- with the location in front of it;
- `file` and `line` are set on the <testcase>, the attributes test reporters
  use for source annotations.

pytest's reports already carry the assertion in `message`, but no location;
for those only `file`/`line` are set, from the traceback in the <failure>
body: the frame running the test function itself, else the deepest frame in
a test file (test_*.py / *_test.py), else the deepest frame inside the
repository. Paths come out as pytest printed them,
relative to its working directory -- the repository root in CI.

Anything it cannot recognise (a timeout, a crash without a panic) is left
exactly as the runner wrote it. Called by the Makefile's `nextest_run` and by
the e2e workflow after a report is saved; a failure here must never change
the test outcome, so callers treat a non-zero exit as a warning.

With --github (set by callers inside GitHub Actions), the failures are also
surfaced in the job that ran the tests, without waiting for test-report.yml:

- one `::error file=...,line=...` workflow command per failing test (GitHub
  shows at most 10 per step), visible at the top of the job page and on the
  PR diff -- workflow commands need no token, so fork PRs get them too;
- a block in $GITHUB_STEP_SUMMARY: pass/fail/skip counts and every failing
  test with a link to its source line and its message;
- the same list as plain text at the end of the step's log, which is where
  GitHub scrolls a failed step to when the job is opened.

Usage: junit_enrich.py [--github] REPORT.xml [REPORT.xml ...]
"""

import os
import re
import sys
import xml.etree.ElementTree as ET

# `thread 'name' (tid) panicked at path/to/file.rs:LINE:COL:` -- the thread id
# is printed by newer toolchains only.
PANIC_RE = re.compile(
    r"panicked at (?P<file>[^\s:][^:\n]*\.rs):(?P<line>\d+):\d+:\n(?P<msg>.*?)(?:\nnote: |\nstack backtrace:|\Z)",
    re.S,
)

# `path/to/file.py:LINE: ...` -- a frame in pytest's --tb=short/long output.
PY_FRAME_RE = re.compile(r"^(?P<file>[^\s:]+\.py):(?P<line>\d+): (?:in (?P<func>\w+))?", re.M)

# Keeps the one-line summaries in the reporter readable; the full text stays
# in the <failure> body.
MAX_MESSAGE = 1000


def python_location(text: str, test_name: str = "") -> tuple[str, str] | None:
    # Parametrized ids (`test_x[a-b]`) run the plain function `test_x`.
    func = test_name.split("[", 1)[0]
    frames = [
        (m.group("file"), m.group("line"), m.group("func"))
        for m in PY_FRAME_RE.finditer(text)
        # Outside the repository (site-packages, stdlib): nothing to link to.
        if not m.group("file").startswith(("/", "..")) and "site-packages" not in m.group("file")
    ]
    if not frames:
        return None
    own = [f for f in frames if func and f[2] == func]
    if own:
        return own[-1][:2]
    in_tests = [
        f for f in frames
        if f[0].rsplit("/", 1)[-1].startswith("test_") or f[0].endswith("_test.py")
    ]
    return (in_tests or frames)[-1][:2]


def enrich(path: str) -> int:
    tree = ET.parse(path)
    changed = 0
    for case in tree.iter("testcase"):
        failure = case.find("failure")
        if failure is None:
            failure = case.find("error")
        if failure is None or not failure.text:
            continue
        m = PANIC_RE.search(failure.text)
        if m is None:
            loc = python_location(failure.text, case.get("name", ""))
            if loc is not None:
                case.set("file", loc[0])
                case.set("line", loc[1])
                changed += 1
            continue
        file = m.group("file").replace("\\", "/")
        line = m.group("line")
        msg = m.group("msg").strip() or failure.get("message", "")
        if len(msg) > MAX_MESSAGE:
            msg = msg[:MAX_MESSAGE] + "…"
        case.set("file", file)
        case.set("line", line)
        failure.set("message", f"{file}:{line}: {msg}")
        changed += 1
    if changed:
        tree.write(path, encoding="UTF-8", xml_declaration=True)
    return changed


# GitHub stops rendering annotations past this many per step anyway.
MAX_ANNOTATIONS = 10


def collect(path: str) -> tuple[dict[str, int], list[dict[str, str]]]:
    """Counts by outcome and the failing test cases of one report."""
    counts = {"passed": 0, "failed": 0, "skipped": 0, "flaky": 0}
    failures = []
    for case in ET.parse(path).iter("testcase"):
        failure = case.find("failure")
        if failure is None:
            failure = case.find("error")
        if failure is not None:
            counts["failed"] += 1
            file, line = case.get("file", ""), case.get("line", "")
            message = failure.get("message") or (failure.text or "").strip() or "Test failed"
            # The location is carried separately; don't repeat it in the text.
            if file and message.startswith(f"{file}:{line}: "):
                message = message[len(f"{file}:{line}: "):]
            classname = case.get("classname", "")
            name = case.get("name", "")
            failures.append({
                "test": f"{classname} › {name}" if classname else name,
                "file": file,
                "line": line,
                "message": message,
            })
        elif case.find("skipped") is not None:
            counts["skipped"] += 1
        else:
            counts["passed"] += 1
            # nextest: passed on retry, with the failed attempts recorded.
            if case.find("flakyFailure") is not None or case.find("rerunFailure") is not None:
                counts["flaky"] += 1
    return counts, failures


def escape_data(text: str) -> str:
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def escape_property(text: str) -> str:
    return escape_data(text).replace(":", "%3A").replace(",", "%2C")


def annotation(failure: dict[str, str]) -> str:
    props = []
    if failure["file"]:
        props.append(f"file={escape_property(failure['file'])}")
        if failure["line"]:
            props.append(f"line={escape_property(failure['line'])}")
    props.append(f"title={escape_property(failure['test'])}")
    return f"::error {','.join(props)}::{escape_data(failure['message'])}"


def summary(report: str, counts: dict[str, int], failures: list[dict[str, str]]) -> str:
    stats = f"{counts['failed']} failed, {counts['passed']} passed, {counts['skipped']} skipped"
    if counts["flaky"]:
        stats += f", {counts['flaky']} flaky"
    if not failures:
        return f"#### ✅ {report} — {stats}\n\n"
    server = os.environ.get("GITHUB_SERVER_URL", "https://github.com")
    repo = os.environ.get("GITHUB_REPOSITORY")
    sha = os.environ.get("GITHUB_SHA")
    out = [f"#### ❌ {report} — {stats}", ""]
    for f in failures:
        where = ""
        if f["file"]:
            loc = f"{f['file']}:{f['line']}" if f["line"] else f["file"]
            if repo and sha:
                anchor = f"#L{f['line']}" if f["line"] else ""
                where = f" — [{loc}]({server}/{repo}/blob/{sha}/{f['file']}{anchor})"
            else:
                where = f" — `{loc}`"
        # GitHub caps a step summary at 1 MiB; the full text stays in the
        # report and the log.
        message = f["message"]
        if len(message) > MAX_MESSAGE:
            message = message[:MAX_MESSAGE] + "…"
        out += [f"- **{f['test']}**{where}", "", "  ````", *(f"  {l}" for l in message.splitlines()), "  ````", ""]
    return "\n".join(out) + "\n"


def log_block(report: str, counts: dict[str, int], failures: list[dict[str, str]]) -> str:
    total = counts["passed"] + counts["failed"]
    out = ["", f"──── Failed tests in {report}: {counts['failed']} of {total} ────"]
    for f in failures:
        out.append(f"✗ {f['test']}")
        if f["file"]:
            out.append(f"    at {f['file']}:{f['line']}" if f["line"] else f"    at {f['file']}")
        out += [f"    {l}" for l in f["message"].splitlines()]
    return "\n".join(out) + "\n"


def publish(path: str) -> None:
    """Annotations, the log block and the step summary for one report."""
    counts, failures = collect(path)
    report = os.path.splitext(os.path.basename(path))[0]
    for failure in failures[:MAX_ANNOTATIONS]:
        print(annotation(failure))
    if len(failures) > MAX_ANNOTATIONS:
        print(f"::notice::{len(failures) - MAX_ANNOTATIONS} more failing test(s) in {path}; see the job summary")
    if failures:
        print(log_block(report, counts, failures), end="")
    summary_file = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_file:
        with open(summary_file, "a", encoding="utf-8") as fh:
            fh.write(summary(report, counts, failures))


def main(argv: list[str]) -> int:
    # Windows runners default to a legacy code page; workflow commands are
    # read back as UTF-8 (test names carry "›").
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    github = "--github" in argv
    paths = [a for a in argv if a != "--github"]
    if not paths:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    for path in paths:
        print(f"junit_enrich: {path}: {enrich(path)} failure(s) annotated")
        if github:
            publish(path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
