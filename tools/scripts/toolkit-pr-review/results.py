"""Validate individual review passes and render their accepted findings."""
import json
from pathlib import Path

from lint import parse_rules, SEVERITIES


def validate(f, module, ctx, rules):
    if not isinstance(f, dict) or any(
        not isinstance(f.get(k), str) or not f[k].strip()
        for k in ("file", "id", "issue", "fix", "comment", "severity")
    ):
        raise ValueError("missing finding fields")
    if f["severity"] not in SEVERITIES or rules.get(f["id"], {}).get("module") != module:
        raise ValueError("invalid severity or rule for pass")
    rec = ctx["files"].get(f["file"])
    if rec is None:
        raise ValueError("unknown file")
    if rec["status"] == "deleted" or (module == "architecture" and "line" not in f):
        if "line" in f or "side" in f:
            raise ValueError("file finding has line coordinates")
        return
    side = f.get("side", "RIGHT")
    line = f.get("line")
    if side not in ("RIGHT", "LEFT") or type(line) is not int or not any(
        lo <= line <= hi for lo, hi in rec["ranges"][side.lower()]
    ):
        raise ValueError("invalid changed-line anchor")


def collect(work, ctx):
    rules = parse_rules()
    rows, warnings = {}, []
    for agent in ctx["agents"]:
        name = agent["name"]
        try:
            data = json.loads((work / "out" / (name + ".json")).read_text())
            if not isinstance(data, dict) or not isinstance(data.get("findings"), list):
                raise ValueError("expected pass object with findings")
            status = data.get("status")
            unread = data.get("unread_files")
            notes = data.get("notes")
            if not isinstance(notes, list) or not all(isinstance(n, str) for n in notes):
                warnings.append(f"{name}: invalid or missing notes")
            if status != "complete" or unread != []:
                warnings.append(f"{name}: {status}, unread={unread}, {data.get('notes', [])}")
            for i, finding in enumerate(data["findings"]):
                key = f"{name}:{i}"
                try:
                    validate(finding, name, ctx, rules)
                    rows[key] = finding
                except ValueError as exc:
                    warnings.append(f"{key}: {exc}")
        except (OSError, ValueError) as exc:
            warnings.append(f"{name}: {exc}")
    return rows, warnings


def render(work, ctx, rows, warnings, excluded):
    if not isinstance(excluded, dict) or any(
        key not in rows or not isinstance(reason, str) or not reason.strip()
        for key, reason in excluded.items()
    ):
        raise ValueError("exclusions must map known candidate IDs to reasons")
    accepted = sorted((f for key, f in rows.items() if key not in excluded),
                      key=lambda f: SEVERITIES.index(f["severity"]))
    report = [f"# Review of {ctx['head_sha']}", ""]
    if warnings:
        report += ["Incomplete review", *warnings, ""]
    if ctx["skipped_files"]:
        report += ["Skipped files: " + ", ".join(ctx["skipped_files"]), ""]
    if not ctx["agents"]:
        report.append("No .rs files changed; nothing was reviewed.")
    elif not accepted and not warnings:
        report.append("No new findings.")
    lines, files = [], []
    for f in accepted:
        sev, path = f["severity"], f["file"]
        location = path + (f":{f['line']} ({f.get('side', 'RIGHT')})" if "line" in f else "")
        report.extend([f"### {sev}: {f['issue']}", f"`{location}` ({f['id']})",
                       "", f["comment"], "", "Fix: " + f["fix"], ""])
        if sev == "LOW":
            continue
        body = (f"**{sev}**\n\n" if sev in ("HIGH", "CRITICAL") else "") + f["comment"]
        if "line" in f:
            lines.append(dict(path=path, line=f["line"], side=f.get("side", "RIGHT"), body=body))
        else:
            files.append(dict(commit_id=ctx["head_sha"], path=path, subject_type="file", body=body))
    (work / "report.md").write_text("\n".join(report) + "\n")
    # A positive clean statement is valid only after every scheduled pass completed.
    review = None
    if ctx["mode"] == "pr" and ctx["agents"]:
        if lines:
            review = dict(commit_id=ctx["head_sha"], event="COMMENT", body="", comments=lines)
        elif not files and not warnings and not rows:
            review = dict(commit_id=ctx["head_sha"], event="COMMENT", body="No issues found.")
    plan = dict(review=review, file_comments=files if ctx["mode"] == "pr" else [])
    (work / "publication.json").write_text(json.dumps(plan, indent=2))


def finish(args):
    work = Path(args.work_dir)
    ctx = json.loads((work / "context.json").read_text())
    rows, warnings = collect(work, ctx)
    excluded = json.loads(Path(args.exclusions).read_text()) if args.exclusions else {}
    (work / "candidates.json").write_text(json.dumps(rows, indent=2))
    render(work, ctx, rows, warnings, excluded)
    for warning in warnings:
        print(warning)
    return 3 if warnings else 0
