# md-fabric.py
from __future__ import annotations

import argparse
import fnmatch
import functools
import html
import json
import math
import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import tomllib

_SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(_SCRIPT_DIR / "lib"))
import rectpack_local as rectpack

DEFAULT_SKIP_DIRS = (".cargo", "target")

CATEGORY_GAP = 60
BUCKET_GAP = 80
CATEGORY_HEADER_H = 68
CAT_PAD_TOP = 20
CAT_PAD_BOTTOM = 40
CAT_PAD_SIDE = 30
CATEGORY_REPACK_GAP = 40
MAX_ROW_W = 12000
TARGET_ASPECT = 16 / 9

_RECTPACK_ALGOS: tuple[Any, ...] = (
    rectpack.MaxRectsBssf,
    rectpack.MaxRectsBaf,
    rectpack.MaxRectsBl,
    rectpack.MaxRectsBlsf,
)

_CATEGORY_STYLE: dict[str, tuple[str, str, str]] = {
    "cf-studio": ("rgba(110,60,210,0.06)",  "rgba(110,60,210,0.30)",  "rgba(90,45,190,0.85)"),
    "toolkit":  ("rgba(200,100,10,0.06)",  "rgba(200,100,10,0.30)",  "rgba(165,80,5,0.85)"),
    "gears": ("rgba(20,145,60,0.06)",   "rgba(20,145,60,0.30)",   "rgba(10,115,40,0.85)"),
    "other":   ("rgba(100,100,100,0.04)", "rgba(100,100,100,0.20)", "rgba(60,60,60,0.75)"),
}

_NODE_COLORS: dict[str, dict[str, str]] = {
    "cf-studio": {"background": "#f4eeff", "border": "#7744cc"},
    "toolkit":  {"background": "#fff5e6", "border": "#c07000"},
    "gears": {"background": "#edfff4", "border": "#28a060"},
    "other":   {"background": "#f4f4f4", "border": "#888888"},
}


@dataclass(frozen=True)
class Group:
    id: str
    label: str
    row: int
    col: int
    colspan: int = 1
    rowspan: int = 1


@dataclass(frozen=True)
class Rule:
    group: str
    path: tuple[str, ...] = ()
    content_regex: tuple[str, ...] = ()


@dataclass(frozen=True)
class BucketDef:
    id: str
    pattern: str


@dataclass(frozen=True)
class CategoryDef:
    id: str
    label: str
    buckets: tuple[BucketDef, ...]


@dataclass(frozen=True)
class ViewDef:
    id: str
    label: str
    start_paths: tuple[str, ...]
    default_depth: int = 5


@dataclass(frozen=True)
class MarkdownFile:
    path: Path
    rel: str
    content: str
    group: str
    bucket_key: str = ""
    bucket_label: str = ""


def load_config(path: Path | None) -> tuple[dict[str, Group], list[Rule], tuple[str, ...], list[CategoryDef], list[ViewDef]]:
    skip_dirs = DEFAULT_SKIP_DIRS

    if path is None or not path.exists():
        return {}, [], skip_dirs, [], []

    if not path.exists():
        return {}, [], skip_dirs, [], []

    raw = path.read_text(encoding="utf-8")
    if not raw.strip():
        return {}, [], skip_dirs, [], []

    data = tomllib.loads(raw)
    if "skip_dirs" in data:
        skip_dirs = tuple(str(item) for item in data.get("skip_dirs", []))

    categories: list[CategoryDef] = [
        CategoryDef(
            id=c["id"],
            label=c.get("label", c["id"]),
            buckets=tuple(
                BucketDef(
                    id=b["id"],
                    pattern=b.get("pattern") or b.get("prefix") or b["id"],
                )
                for b in c.get("buckets", [])
            ),
        )
        for c in data.get("categories", [])
    ]

    views: list[ViewDef] = [
        ViewDef(
            id=item["id"],
            label=item.get("label", item["id"]),
            start_paths=tuple(str(path) for path in item.get("start_paths", [])),
            default_depth=max(0, int(item.get("default_depth", 5))),
        )
        for item in data.get("views", [])
    ]

    groups = {
        item["id"]: Group(
            id=item["id"],
            label=item.get("label", item["id"]),
            row=int(item.get("row", 0)),
            col=int(item.get("col", 0)),
            colspan=int(item.get("colspan", 1)),
            rowspan=int(item.get("rowspan", 1)),
        )
        for item in data.get("groups", [])
    }

    rules = [
        Rule(
            group=item["group"],
            path=tuple(item.get("path", [])),
            content_regex=tuple(item.get("content_regex", [])),
        )
        for item in data.get("rules", [])
    ]

    if not groups and not categories:
        return {}, [], skip_dirs, [], views

    for rule in rules:
        if rule.group not in groups:
            raise ValueError(f"Rule references unknown group: {rule.group}")

    for view in views:
        if not view.start_paths:
            raise ValueError(f"View has no start_paths: {view.id}")

    return groups, rules, skip_dirs, categories, views


def resolve_config_path(repo_root: Path, script_dir: Path, explicit: Path | None = None) -> Path | None:
    if explicit is not None:
        return explicit if explicit.exists() else None

    candidates = [repo_root / ".md-fabric.toml", script_dir / "md-fabric.toml"]
    for candidate in candidates:
        if candidate.exists():
            return candidate

    return None


def normalize_rel(path: Path) -> str:
    s = path.as_posix()
    if s.startswith("./"):
        s = s[2:]
    return s


def detect_template_vars(repo: Path) -> dict[str, str]:
    vars: dict[str, str] = {}
    for candidate in sorted(repo.iterdir()):
        if not candidate.is_dir():
            continue
        core_toml = candidate / "config" / "core.toml"
        if core_toml.exists():
            try:
                data = tomllib.loads(core_toml.read_text(encoding="utf-8"))
                if "project_root" in data:
                    vars["cf-studio-path"] = candidate.relative_to(repo).as_posix()
                    break
            except Exception:
                pass
    return vars


def matches_rule(rel: str, content: str, rule: Rule) -> bool:
    path_match = any(fnmatch.fnmatch(rel, pattern) for pattern in rule.path)
    content_match = any(re.search(pattern, content) for pattern in rule.content_regex)
    return path_match or content_match


def assign_group(rel: str, content: str, rules: list[Rule], default_group: str) -> str:
    for rule in rules:
        if matches_rule(rel, content, rule):
            return rule.group
    return default_group


def _glob_segment_to_regex(segment: str) -> str:
    out: list[str] = []
    for ch in segment:
        if ch == "*":
            out.append("[^/]*")
        elif ch == "?":
            out.append("[^/]")
        else:
            out.append(re.escape(ch))
    return "".join(out)


@functools.lru_cache(maxsize=None)
def _compile_bucket_pattern(pattern: str) -> tuple[re.Pattern[str], tuple[str, ...]]:
    normalized = pattern.replace("\\", "/").strip("/")
    if not normalized:
        return re.compile(r"^$"), ()

    group_names: list[str] = []
    parts = normalized.split("/")
    regex_parts: list[str] = []
    dynamic_index = 0
    needs_sep = False
    for part in parts:
        if part == "**":
            if needs_sep:
                regex_parts.append("/")
            regex_parts.append("(?:[^/]+/)*")
            needs_sep = False
            continue
        if needs_sep:
            regex_parts.append("/")
        if part == "*":
            group_name = f"g{dynamic_index}"
            dynamic_index += 1
            group_names.append(group_name)
            regex_parts.append(f"(?P<{group_name}>[^/]+)")
            needs_sep = True
            continue
        regex_parts.append(_glob_segment_to_regex(part))
        needs_sep = True

    return re.compile("^" + "".join(regex_parts) + "$"), tuple(group_names)


def _bucket_display_label(bucket: BucketDef, rel: str, match: re.Match[str], group_names: tuple[str, ...]) -> str:
    parts = bucket.pattern.replace("\\", "/").strip("/").split("/")
    display_parts: list[str] = []
    group_index = 0
    for part in parts:
        if part == "**":
            break
        if part == "*":
            if group_index < len(group_names):
                display_parts.append(match.group(group_names[group_index]))
                group_index += 1
            continue
        if "*" in part or "?" in part:
            break
        display_parts.append(part)

    if display_parts:
        label = "/".join(display_parts)
        if not label.endswith(".md"):
            label += "/"
        return label
    return bucket.pattern


def assign_category_bucket(rel: str, cats: list[CategoryDef]) -> tuple[str, str, str]:
    """Return (category_id, bucket_key, bucket_label). First matching pattern wins."""
    for cat in cats:
        for b in cat.buckets:
            regex, group_names = _compile_bucket_pattern(b.pattern)
            match = regex.match(rel)
            if match:
                label = _bucket_display_label(b, rel, match, group_names)
                key = f"{cat.id}:{label.rstrip('/')}" if group_names else f"{cat.id}:{b.id}"
                return cat.id, key, label
    return "other", "", ""


def scan_markdown(
    repo: Path,
    groups: dict[str, Group],
    rules: list[Rule],
    skip_dirs: tuple[str, ...],
    template_vars: dict[str, str] | None = None,
    categories: list[CategoryDef] | None = None,
) -> list[MarkdownFile]:
    default_group = "others" if "others" in groups else next(iter(groups), "")
    files: list[MarkdownFile] = []
    explicit_skip_paths = tuple(normalize_rel(Path(item)) for item in skip_dirs if "/" in str(item).replace("\\", "/"))
    excluded_dir_names = {str(item).strip("/") for item in skip_dirs if "/" not in str(item).replace("\\", "/")}
    excluded_dir_names |= {".git", "node_gears", ".venv", "venv"}

    def is_skipped(path: Path) -> bool:
        rel = path.relative_to(repo)
        rel_str = normalize_rel(rel)
        return (
            any(part in excluded_dir_names for part in rel.parts)
            or any(rel_str == skip or rel_str.startswith(skip + "/") for skip in explicit_skip_paths)
        )

    for path in sorted(repo.rglob("*.md")):
        if is_skipped(path):
            continue

        rel = normalize_rel(path.relative_to(repo))
        content = path.read_text(encoding="utf-8", errors="replace")
        if template_vars:
            for key, val in template_vars.items():
                content = content.replace("{" + key + "}", val)

        if categories:
            cat_id, bk, bl = assign_category_bucket(rel, categories)
            files.append(MarkdownFile(path=path, rel=rel, content=content, group=cat_id, bucket_key=bk, bucket_label=bl))
        else:
            group = assign_group(rel, content, rules, default_group)
            files.append(MarkdownFile(path=path, rel=rel, content=content, group=group))

    return files


def slug_candidates(target: str) -> list[str]:
    clean = target.split("#", 1)[0].strip()
    clean = clean.replace("\\", "/")

    if not clean:
        return []

    candidates = [clean]

    if not clean.endswith(".md"):
        candidates.append(f"{clean}.md")

    return candidates  # raw strings; resolved relative to source_dir in resolve_link


def resolve_link(source: MarkdownFile, raw_target: str, known: set[str]) -> str | None:
    raw_target = raw_target.strip()

    if not raw_target or re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", raw_target):
        return None

    if raw_target.startswith("@/"):
        raw_target = raw_target[2:]

    candidates = slug_candidates(raw_target)
    source_dir = Path(source.rel).parent

    expanded: list[str] = []
    for candidate in candidates:
        expanded.append(normalize_rel(source_dir / candidate))
        expanded.append(candidate)

    for candidate in expanded:
        if candidate in known:
            return candidate

    return None


def _colorize_md_line(escaped: str) -> str:
    escaped = re.sub(r"^(#{1,6} .*)", r'<span class="md-h">\1</span>', escaped)
    escaped = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", escaped)
    escaped = re.sub(r"(?<!\*)\*([^*\n]+)\*(?!\*)", r"<em>\1</em>", escaped)
    escaped = re.sub(r"`([^`]+)`", r'<code class="md-code">\1</code>', escaped)
    escaped = re.sub(r"\[([^\]]+)\]\([^)]*\)", r'<span class="md-link">[\1]</span>', escaped)
    return escaped


def snippet_html(content: str, start_index: int, context: int = 5) -> str:
    lines = content.splitlines()
    total = len(lines)
    match_line = content[:start_index].count("\n")
    start = max(0, match_line - context)
    end = min(total, match_line + context + 1)

    above = f'<div class="snip-ell">\u2026\u00a0{start} lines above</div>' if start > 0 else ""
    below = f'<div class="snip-ell">\u2026\u00a0{total - end} lines below</div>' if end < total else ""

    rows = []
    for idx in range(start, end):
        cls = "snip-match" if idx == match_line else "snip-ctx"
        colored = _colorize_md_line(html.escape(lines[idx]))
        rows.append(
            f'<tr class="{cls}"><td class="snip-ln">{idx + 1}</td>'
            f'<td class="snip-lc">{colored}</td></tr>'
        )

    table = f'<table class="snip-tbl">{"".join(rows)}</table>'
    return above + table + below


def preview_markdown(content: str, max_lines: int = 18, max_chars: int = 1400) -> str:
    lines = content.strip().splitlines()
    if not lines:
        return "_(empty file)_"

    preview = "\n".join(lines[:max_lines]).strip()
    if len(preview) > max_chars:
        preview = preview[:max_chars].rstrip()
        preview += "\n\n..."

    return preview or "_(empty file)_"


def extract_frontmatter(content: str) -> tuple[list[str], str]:
    lines = content.splitlines()
    if not lines or lines[0].strip() != "---":
        return [], content
    for i in range(1, len(lines)):
        if lines[i].strip() in ("---", "..."):
            return lines[1:i], "\n".join(lines[i + 1:]).lstrip("\n")
    return [], content


def _render_frontmatter_html(fm_lines: list[str]) -> str:
    rows = []
    for line in fm_lines:
        m = re.match(r'^(\s*[\w][\w-]*\s*):(.*)', line)
        if m:
            key = html.escape(m.group(1))
            val = html.escape(m.group(2))
            rows.append(f'<span class="fm-key">{key}</span><span class="fm-sep">:</span><span class="fm-val">{val}</span>')
        else:
            rows.append(html.escape(line))
    inner = "".join(f'<span class="fm-line">{r}</span>' for r in rows)
    return f'<div class="fm-block">{inner}</div>\n\n'


def extract_toc(content: str) -> list[str]:
    toc: list[str] = []
    for line in content.splitlines():
        m = re.match(r'^(#{1,6})\s+(.+)', line)
        if m:
            level = len(m.group(1))
            title = m.group(2).strip()
            toc.append("  " * (level - 1) + "- " + title)
    return toc


def build_node_preview(rel: str, loc: int, content: str) -> str:
    fm_lines, body_content = extract_frontmatter(content)
    fm_html = _render_frontmatter_html(fm_lines) if fm_lines else ""
    toc = extract_toc(body_content)
    header = f"<small>`{rel}`</small>\n\n---\n\n"
    if toc:
        return header + fm_html + "\n".join(toc)
    body_lines = body_content.splitlines()
    body = "\n".join(body_lines[:50]) or "_(empty file)_"
    if len(body_lines) > 50:
        body += "\n\n..."
    return header + fm_html + body


def extract_references(files: list[MarkdownFile]) -> list[dict[str, Any]]:
    known = {file.rel for file in files}
    edges: list[dict[str, Any]] = []
    edge_id = 0

    markdown_link = re.compile(r"!?\[[^\]]*]\(([^)]+)\)")
    wiki_link = re.compile(r"\[\[([^\]|#]+)(?:#[^\]|]+)?(?:\|[^\]]+)?]]")
    backtick_link = re.compile(r"`([^`\s]+/[^`\s]+)`")

    for source in files:
        for pattern in (markdown_link, wiki_link, backtick_link):
            for match in pattern.finditer(source.content):
                target = resolve_link(source, match.group(1), known)

                if not target or target == source.rel:
                    continue

                snip = snippet_html(source.content, match.start())
                src_esc = html.escape(source.rel)
                tgt_esc = html.escape(target)
                header = (
                    f'<p class="snip-title">'
                    f'<span class="snip-src">{src_esc}</span>'
                    f'<span class="snip-arr"> \u2192 </span>'
                    f'<span class="snip-tgt">{tgt_esc}</span></p>'
                )
                edges.append(
                    {
                        "id": edge_id,
                        "from": source.rel,
                        "to": target,
                        "preview_html": header + snip,
                        "arrows": "to",
                    }
                )
                edge_id += 1

    return edges


def cpt_paragraph(content: str, match_start: int) -> str:
    """Return the paragraph block or table row that contains the CPT reference."""
    lines = content.splitlines()
    # Locate which line index contains match_start
    pos = 0
    line_idx = 0
    for i, line in enumerate(lines):
        end = pos + len(line)
        if pos <= match_start <= end:
            line_idx = i
            break
        pos = end + 1  # +1 for the stripped newline

    stripped = lines[line_idx].strip()

    # ── Table row ─────────────────────────────────────────────────────────
    if stripped.startswith('|'):
        # Walk back to the first row of this table block
        start = line_idx
        while start > 0 and lines[start - 1].strip().startswith('|'):
            start -= 1
        parts: list[str] = []
        if start < line_idx:
            # Include header row
            parts.append(lines[start].strip())
            # Include separator row if present
            if (start + 1 < line_idx
                    and re.match(r'^\|[\s\-:|]+\|', lines[start + 1].strip())):
                parts.append(lines[start + 1].strip())
            if line_idx > start + 2:
                parts.append('\u2026')  # ellipsis when rows were skipped
            parts.append(stripped)
        else:
            parts = [stripped]  # match IS the first row
        return '\n'.join(parts)

    # ── Regular paragraph ──────────────────────────────────────────────────
    # Find the nearest heading above the matched line
    heading = ''
    for i in range(line_idx - 1, -1, -1):
        if lines[i].startswith('#'):
            heading = lines[i].strip()
            break

    # Walk back to find the paragraph start (blank line or heading boundary)
    p_start = line_idx
    while p_start > 0:
        prev = lines[p_start - 1].strip()
        if not prev or prev.startswith('#'):
            break
        p_start -= 1

    # Walk forward to find the paragraph end
    p_end = line_idx
    while p_end < len(lines) - 1:
        nxt = lines[p_end + 1].strip()
        if not nxt or nxt.startswith('#'):
            break
        p_end += 1

    para = '\n'.join(line.rstrip() for line in lines[p_start:p_end + 1])
    return (heading + '\n\n' + para) if heading else para


def cpt_def_snippet(content: str, char_pos: int) -> str:
    """Return the full section containing the definition (nearest heading → next heading or EOF)."""
    lines = content.splitlines()
    pos = 0
    line_idx = 0
    for i, ln in enumerate(lines):
        end = pos + len(ln)
        if pos <= char_pos <= end:
            line_idx = i
            break
        pos = end + 1

    # Walk back to find the nearest heading line
    heading_idx = -1
    for i in range(line_idx - 1, -1, -1):
        if lines[i].startswith('#'):
            heading_idx = i
            break

    # Walk forward from the definition line to find the next heading (section boundary)
    section_end = len(lines) - 1
    for i in range(line_idx + 1, len(lines)):
        if lines[i].startswith('#'):
            section_end = i - 1
            break

    start = heading_idx if heading_idx >= 0 else 0
    section = lines[start:section_end + 1]

    # Trim trailing blank lines
    while section and not section[-1].strip():
        section.pop()

    return '\n'.join(line.rstrip() for line in section)


def extract_cpt_references(files: list[MarkdownFile]) -> list[dict[str, Any]]:
    cpt_def = re.compile(r'\*\*ID\*\*:\s*`(cpt-[a-z0-9][a-z0-9_-]*)`')
    cpt_ref = re.compile(r'`(cpt-[a-z0-9][a-z0-9_-]*)`')
    cpt_def_map: dict[str, tuple[str, int]] = {}
    file_content_map: dict[str, str] = {f.rel: f.content for f in files}
    for file in files:
        char_pos = 0
        for line in file.content.splitlines(keepends=True):
            for m in cpt_def.finditer(line):
                cpt_id = m.group(1)
                if cpt_id not in cpt_def_map:
                    cpt_def_map[cpt_id] = (file.rel, char_pos + m.start())
            char_pos += len(line)
    edges: list[dict[str, Any]] = []
    edge_id = 0
    for file in files:
        pair_refs: dict[str, list[dict[str, str]]] = {}
        char_pos = 0
        line_num = 0
        for line in file.content.splitlines(keepends=True):
            line_num += 1
            if not cpt_def.search(line):
                for m in cpt_ref.finditer(line):
                    cpt_id = m.group(1)
                    _def_info = cpt_def_map.get(cpt_id)
                    target = _def_info[0] if _def_info else None
                    if target and target != file.rel:
                        snippet = cpt_paragraph(file.content, char_pos + m.start())
                        if target not in pair_refs:
                            pair_refs[target] = []
                        if not any(
                            r['cpt_id'] == cpt_id and r['snippet'] == snippet
                            for r in pair_refs[target]
                        ):
                            _def_rel, _def_pos = _def_info  # type: ignore[misc]
                            _def_content = file_content_map.get(_def_rel, "")
                            _def_snip = cpt_def_snippet(_def_content, _def_pos) if _def_content else ""
                            _def_line = _def_content[:_def_pos].count('\n') + 1 if _def_content else 0
                            pair_refs[target].append({
                                "cpt_id": cpt_id,
                                "snippet": snippet,
                                "ref_line": line_num,
                                "def_snippet": _def_snip,
                                "def_line": _def_line,
                            })
            char_pos += len(line)
        for target, refs in pair_refs.items():
            edges.append({
                "id": f"cpt-{edge_id}",
                "from": file.rel,
                "to": target,
                "type": "cpt",
                "arrows": "to",
                "refs": refs,
            })
            edge_id += 1
    return edges


def group_rects(groups: dict[str, Group]) -> dict[str, dict[str, float]]:
    cell_w = 650
    cell_h = 420
    margin = 80

    return {
        group.id: {
            "x": group.col * cell_w + margin,
            "y": group.row * cell_h + margin,
            "w": group.colspan * cell_w - margin,
            "h": group.rowspan * cell_h - margin,
            "label": group.label,
        }
        for group in groups.values()
    }


def _key_parts(key: str) -> list[str]:
    return [p for p in key.split("/") if p]


def compute_path_boxes(files: list[MarkdownFile], min_box_size: int = 8) -> dict[str, list[MarkdownFile]]:
    # Pass 1: group by dir prefix up to depth 3, bubble up small groups.
    buckets: dict[str, list[MarkdownFile]] = {}
    for file in files:
        parts = Path(file.rel).parts[:-1]
        depth = min(3, len(parts))
        key = "/".join(parts[:depth]) if depth > 0 else ""
        buckets.setdefault(key, []).append(file)

    boxes: dict[str, list[MarkdownFile]] = {}
    for depth in range(3, 0, -1):
        new_buckets: dict[str, list[MarkdownFile]] = {}
        for key, group in buckets.items():
            kp = _key_parts(key)
            if len(kp) != depth:
                new_buckets.setdefault(key, []).extend(group)
                continue
            if len(group) >= min_box_size:
                boxes[key] = group
            else:
                parent = "/".join(kp[:-1])
                new_buckets.setdefault(parent, []).extend(group)
        buckets = new_buckets

    for key, group in buckets.items():
        if group:
            boxes.setdefault(key, []).extend(group)

    # Pass 2: merge lonely boxes upward.
    # A box is "lonely" at its level if no sibling box (same parent, same depth) has
    # >= min_box_size files. When lonely AND the parent-level bucket has < min_box_size
    # files, absorb this box into the parent key. Repeat until stable.
    changed = True
    while changed:
        changed = False
        for key in sorted(boxes, key=lambda k: len(_key_parts(k)), reverse=True):
            kp = _key_parts(key)
            if not kp:
                continue
            parent = "/".join(kp[:-1])
            depth = len(kp)
            has_big_sibling = any(
                k for k in boxes
                if k != key
                and len(_key_parts(k)) == depth
                and "/".join(_key_parts(k)[:-1]) == parent
                and len(boxes[k]) >= min_box_size
            )
            if not has_big_sibling and len(boxes.get(parent, [])) < min_box_size:
                boxes[parent] = boxes.pop(parent, []) + boxes.pop(key)
                changed = True
                break

    return boxes


def compute_category_layout(
    files: list[MarkdownFile],
    edges: list[dict[str, Any]],
    categories: list[CategoryDef],
    verbose: bool = False,
) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]], dict[str, dict[str, Any]]]:
    """Return (nodes, bucket_rects, category_bands) for category-bucketed layout."""
    spacing = 80
    pad_h = 40
    pad_top = 55
    pad_bottom = 40

    degrees: dict[str, int] = {}
    for edge in edges:
        degrees[edge["from"]] = degrees.get(edge["from"], 0) + 1
        degrees[edge["to"]]   = degrees.get(edge["to"],   0) + 1

    other_files = [f for f in files if f.group == "other"]
    other_map: dict[str, list[MarkdownFile]] = {}
    if other_files:
        for box_key, box_files in compute_path_boxes(other_files).items():
            bk = f"other:{box_key or '(root)'}"
            bl = box_key or "(root)"
            for f in box_files:
                other_map.setdefault(bk, []).append(
                    MarkdownFile(f.path, f.rel, f.content, "other", bk, bl)
                )

    grouped: dict[str, dict[str, list[MarkdownFile]]] = {}
    blabels: dict[str, str] = {}
    for f in files:
        if f.group == "other":
            continue
        grouped.setdefault(f.group, {}).setdefault(f.bucket_key, []).append(f)
        blabels[f.bucket_key] = f.bucket_label

    if other_map:
        grouped["other"] = other_map
        for bk, bfiles in other_map.items():
            blabels[bk] = bfiles[0].bucket_label

    cat_order = [c.id for c in categories] + ["other"]
    cat_labels = {c.id: c.label for c in categories}
    cat_labels["other"] = "Other"

    def _dims(n: int, total_files_in_category: int) -> tuple[int, int, int]:
        cols = max(1, math.ceil(math.sqrt(n * 1.3)))
        if total_files_in_category < 50:
            cols = max(cols, math.ceil(n / 2))
        rows = math.ceil(n / cols)
        w = max(180, 2 * pad_h + 18 + (cols - 1) * spacing)
        h = max(130, pad_top + pad_bottom + 36 + (rows - 1) * spacing)
        return w, h, cols

    def _make(file: MarkdownFile, x: int, y: int) -> dict[str, Any]:
        cat = file.group
        c = _NODE_COLORS.get(cat, {"background": "#ffffff", "border": "#555555"})
        loc = len(file.content.splitlines())
        return {
            "id": file.rel,
            "label": Path(file.rel).name,
            "path": str(file.path),
            "preview": build_node_preview(file.rel, loc, file.content),
            "text": "\n".join(file.content.splitlines()[:50]),
            "loc": loc,
            "x": x, "y": y,
            "shape": "dot", "size": 18, "margin": 10,
            "font": {"size": 11, "color": "rgba(0,0,0,0)"},
            "mass": max(1, degrees.get(file.rel, 1)),
            "group": cat,
            "category": cat,
            "bucket": file.bucket_key,
            "color": {**c, "highlight": {"background": "#fff7cc", "border": "#d99a00"}},
        }

    def _print_snapshot(snapshot: rectpack.StackedLayoutSnapshot) -> None:
        print(
            f"[md-fabric] iteration {snapshot.iteration}: "
            f"total={snapshot.metrics.total_width}x{snapshot.metrics.total_height} "
            f"aspect={snapshot.metrics.total_aspect:.3f} "
            f"density={snapshot.metrics.total_density:.3f} "
            f"category_density={snapshot.metrics.total_category_density:.3f}"
        )
        for category in snapshot.categories:
            print(
                f"[md-fabric]   {category.category_id}: {category.width}x{category.height} "
                f"density={category.density:.3f} rows={category.row_count} candidate={category.candidate_index}"
            )

    def _layout_metrics(
        choices: list[rectpack.LayoutCandidate],
        positions: dict[str, tuple[int, int]],
    ) -> rectpack.StackedLayoutMetrics:
        if not choices:
            return rectpack.StackedLayoutMetrics(0, 0, 0.0, 0.0, 0.0, 0.0)
        total_width = max(positions[entry["cat_id"]][0] + choice.width for entry, choice in zip(category_inputs, choices))
        total_height = max(positions[entry["cat_id"]][1] + choice.height for entry, choice in zip(category_inputs, choices))
        total_used_area = sum(choice.used_area for choice in choices)
        total_category_area = sum(choice.width * choice.height for choice in choices)
        total_aspect = total_width / max(1, total_height)
        total_density = total_used_area / max(1, total_width * total_height)
        total_category_density = total_category_area / max(1, total_width * total_height)
        aspect_error = abs(total_aspect - TARGET_ASPECT) / max(TARGET_ASPECT, 1e-9)
        return rectpack.StackedLayoutMetrics(
            total_width=total_width,
            total_height=total_height,
            total_aspect=total_aspect,
            total_density=total_density,
            total_category_density=total_category_density,
            aspect_error=aspect_error,
        )

    def _layout_score(metrics: rectpack.StackedLayoutMetrics) -> tuple[float, float, float, float, float]:
        density_loss = 1.0 - metrics.total_density
        return (
            0.5 * metrics.aspect_error + 0.5 * density_loss,
            metrics.aspect_error,
            density_loss,
            1.0 - metrics.total_category_density,
            float(metrics.total_height),
        )

    def _repack_improves(
        baseline: rectpack.StackedLayoutMetrics,
        candidate: rectpack.StackedLayoutMetrics,
    ) -> bool:
        eps = 1e-9
        return (
            candidate.aspect_error <= baseline.aspect_error + eps
            and candidate.total_density + eps >= baseline.total_density
            and candidate.total_category_density + eps >= baseline.total_category_density
            and (
                candidate.aspect_error < baseline.aspect_error - eps
                or candidate.total_density > baseline.total_density + eps
                or candidate.total_category_density > baseline.total_category_density + eps
            )
        )

    nodes_out: list[dict[str, Any]] = []
    bucket_rects: dict[str, dict[str, Any]] = {}
    cat_bands: dict[str, dict[str, Any]] = {}
    category_inputs: list[dict[str, Any]] = []

    for cat_id in cat_order:
        if cat_id not in grouped:
            continue
        bmap = grouped[cat_id]
        bkeys = sorted(bmap.keys())
        if not bkeys:
            continue

        total_files_in_category = sum(len(bmap[bk]) for bk in bkeys)
        dims = {bk: _dims(len(bmap[bk]), total_files_in_category) for bk in bkeys}
        sorted_items = sorted(
            [(bk, dims[bk][0], dims[bk][1]) for bk in bkeys],
            key=lambda t: (t[1] * t[2], t[1], t[2]), reverse=True,
        )

        category_inputs.append(
            {
                "cat_id": cat_id,
                "bmap": bmap,
                "bkeys": bkeys,
                "dims": dims,
                "candidates": rectpack.generate_layout_candidates(
                    sorted_items,
                    gap=BUCKET_GAP,
                    target_aspect=TARGET_ASPECT,
                    pad_side=CAT_PAD_SIDE,
                    pad_top=CAT_PAD_TOP,
                    pad_bottom=CAT_PAD_BOTTOM,
                    header_height=CATEGORY_HEADER_H,
                    pack_algos=_RECTPACK_ALGOS,
                    limit=16,
                ),
            }
        )
        if verbose:
            candidates = category_inputs[-1]["candidates"]
            print(
                f"[md-fabric] category {cat_id}: buckets={len(bkeys)} candidates={len(candidates)} "
                f"best={candidates[0].width}x{candidates[0].height} "
                f"aspect={candidates[0].aspect:.3f} density={candidates[0].density:.3f} rows={candidates[0].row_count}"
            )

    best_choices, _chosen_indexes, snapshots = rectpack.optimize_stacked_categories(
        [(entry["cat_id"], entry["candidates"]) for entry in category_inputs],
        category_gap=CATEGORY_GAP,
        target_aspect=TARGET_ASPECT,
        aspect_tolerance=0.10,
        max_iterations=10,
    )

    if verbose:
        for snapshot in snapshots:
            _print_snapshot(snapshot)
        final_metrics = rectpack.compute_stacked_metrics(
            best_choices,
            category_gap=CATEGORY_GAP,
            target_aspect=TARGET_ASPECT,
        )
        print(
            f"[md-fabric] final: total={final_metrics.total_width}x{final_metrics.total_height} "
            f"aspect={final_metrics.total_aspect:.3f} density={final_metrics.total_density:.3f} "
            f"category_density={final_metrics.total_category_density:.3f}"
        )

    choice_by_cat = {entry["cat_id"]: choice for entry, choice in zip(category_inputs, best_choices)}
    present_categories = set(choice_by_cat)
    file_to_category = {file.rel: file.group for file in files}
    category_links: dict[tuple[str, str], int] = {}
    category_link_totals = {cat_id: 0 for cat_id in present_categories}
    for edge in edges:
        left = file_to_category.get(edge["from"])
        right = file_to_category.get(edge["to"])
        if left not in present_categories or right not in present_categories or left == right:
            continue
        key = (left, right) if left < right else (right, left)
        category_links[key] = category_links.get(key, 0) + 1
        category_link_totals[left] = category_link_totals.get(left, 0) + 1
        category_link_totals[right] = category_link_totals.get(right, 0) + 1

    def _category_affinity(left: str, right: str) -> int:
        if left == right:
            return 0
        key = (left, right) if left < right else (right, left)
        return category_links.get(key, 0)

    def _affinity_penalty(
        positions: dict[str, tuple[int, int]],
        metrics: rectpack.StackedLayoutMetrics,
    ) -> float:
        total_weight = sum(category_links.values())
        if total_weight <= 0:
            return 0.0
        normalizer = max(1.0, math.hypot(metrics.total_width, metrics.total_height))
        weighted_distance = 0.0
        for (left, right), weight in category_links.items():
            left_choice = choice_by_cat[left]
            right_choice = choice_by_cat[right]
            left_x, left_y = positions[left]
            right_x, right_y = positions[right]
            left_center_x = left_x + left_choice.width / 2.0
            left_center_y = left_y + left_choice.height / 2.0
            right_center_x = right_x + right_choice.width / 2.0
            right_center_y = right_y + right_choice.height / 2.0
            weighted_distance += weight * (
                abs(left_center_x - right_center_x) + abs(left_center_y - right_center_y)
            ) / normalizer
        return weighted_distance / total_weight

    def _final_layout_score(
        metrics: rectpack.StackedLayoutMetrics,
        positions: dict[str, tuple[int, int]],
    ) -> tuple[float, float, float, float, float, float]:
        density_loss = 1.0 - metrics.total_density
        category_loss = 1.0 - metrics.total_category_density
        affinity_loss = _affinity_penalty(positions, metrics)
        return (
            0.45 * density_loss + 0.30 * metrics.aspect_error + 0.15 * category_loss + 0.10 * affinity_loss,
            density_loss,
            metrics.aspect_error,
            category_loss,
            affinity_loss,
            float(metrics.total_height),
        )

    def _final_layout_improves(
        baseline: rectpack.StackedLayoutMetrics,
        candidate: rectpack.StackedLayoutMetrics,
    ) -> bool:
        eps = 1e-9
        return (
            candidate.total_density + eps >= baseline.total_density
            and candidate.total_category_density + eps >= baseline.total_category_density
            and (
                candidate.total_density > baseline.total_density + eps
                or candidate.total_category_density > baseline.total_category_density + eps
                or candidate.aspect_error < baseline.aspect_error - eps
            )
        )

    def _greedy_affinity_order(seed: str) -> tuple[str, ...]:
        remaining = [cat_id for cat_id in choice_by_cat if cat_id != seed]
        order = [seed]
        while remaining:
            next_cat = max(
                remaining,
                key=lambda cat_id: (
                    sum(_category_affinity(cat_id, existing) for existing in order),
                    _category_affinity(cat_id, order[-1]),
                    category_link_totals.get(cat_id, 0),
                    choice_by_cat[cat_id].used_area,
                    choice_by_cat[cat_id].width * choice_by_cat[cat_id].height,
                ),
            )
            order.append(next_cat)
            remaining.remove(next_cat)
        return tuple(order)

    def _row_pack_positions(order: tuple[str, ...], width_limit: int) -> dict[str, tuple[int, int]]:
        positions: dict[str, tuple[int, int]] = {}
        cur_x = 0
        cur_y = 0
        row_height = 0
        for cat_id in order:
            choice = choice_by_cat[cat_id]
            if cur_x > 0 and cur_x + choice.width > width_limit:
                cur_x = 0
                cur_y += row_height + CATEGORY_REPACK_GAP
                row_height = 0
            positions[cat_id] = (cur_x, cur_y)
            cur_x += choice.width + CATEGORY_REPACK_GAP
            row_height = max(row_height, choice.height)
        return positions

    stacked_positions: dict[str, tuple[int, int]] = {}
    cur_y = 0
    for entry, candidate in zip(category_inputs, best_choices):
        stacked_positions[entry["cat_id"]] = (0, cur_y)
        cur_y += candidate.height + CATEGORY_GAP

    chosen_positions = stacked_positions
    stacked_metrics = _layout_metrics(best_choices, stacked_positions)
    chosen_metrics = stacked_metrics
    chosen_label = "stacked"
    repacked = rectpack.try_repack_rectangles(
        [(entry["cat_id"], candidate.width, candidate.height) for entry, candidate in zip(category_inputs, best_choices)],
        target_aspect=TARGET_ASPECT,
        gap=CATEGORY_REPACK_GAP,
        pack_algos=_RECTPACK_ALGOS,
    )
    if repacked is not None:
        repacked_positions, _repacked_metrics = repacked
        repacked_metrics = _layout_metrics(best_choices, repacked_positions)
        if _final_layout_improves(stacked_metrics, repacked_metrics) and _final_layout_score(repacked_metrics, repacked_positions) < _final_layout_score(chosen_metrics, chosen_positions):
            chosen_positions = repacked_positions
            chosen_metrics = repacked_metrics
            chosen_label = "rectpack"
            if verbose:
                print(
                    f"[md-fabric] category repack kept: total={repacked_metrics.total_width}x{repacked_metrics.total_height} "
                    f"aspect={repacked_metrics.total_aspect:.3f} density={repacked_metrics.total_density:.3f} "
                    f"category_density={repacked_metrics.total_category_density:.3f}"
                )
        elif verbose:
            print(
                f"[md-fabric] category repack rolled back: candidate total={repacked_metrics.total_width}x{repacked_metrics.total_height} "
                f"aspect={repacked_metrics.total_aspect:.3f} density={repacked_metrics.total_density:.3f} "
                f"category_density={repacked_metrics.total_category_density:.3f}"
            )

    seed_categories = sorted(
        choice_by_cat,
        key=lambda cat_id: (
            category_link_totals.get(cat_id, 0),
            choice_by_cat[cat_id].used_area,
            choice_by_cat[cat_id].width * choice_by_cat[cat_id].height,
        ),
        reverse=True,
    )[:5]
    order_candidates = {
        tuple(cat_id for cat_id in choice_by_cat),
        tuple(sorted(choice_by_cat, key=lambda cat_id: choice_by_cat[cat_id].width * choice_by_cat[cat_id].height, reverse=True)),
    }
    for seed in seed_categories:
        order = _greedy_affinity_order(seed)
        order_candidates.add(order)
        order_candidates.add(tuple(reversed(order)))

    max_cat_width = max(choice.width for choice in best_choices)
    natural_cat_width = sum(choice.width for choice in best_choices) + CATEGORY_REPACK_GAP * max(0, len(best_choices) - 1)
    width_candidates = sorted(
        {
            max_cat_width,
            stacked_metrics.total_width,
            max_cat_width + CATEGORY_REPACK_GAP + min(choice.width for choice in best_choices),
            int(math.ceil(math.sqrt(sum(choice.width * choice.height for choice in best_choices) * TARGET_ASPECT))),
            natural_cat_width,
        }
    )

    best_affinity_positions: dict[str, tuple[int, int]] | None = None
    best_affinity_metrics: rectpack.StackedLayoutMetrics | None = None
    for order in order_candidates:
        for width_limit in width_candidates:
            if width_limit < max_cat_width:
                continue
            positions = _row_pack_positions(order, width_limit)
            metrics = _layout_metrics(best_choices, positions)
            if not _final_layout_improves(stacked_metrics, metrics):
                continue
            if best_affinity_metrics is None or _final_layout_score(metrics, positions) < _final_layout_score(best_affinity_metrics, best_affinity_positions):
                best_affinity_positions = positions
                best_affinity_metrics = metrics

    if best_affinity_positions is not None and best_affinity_metrics is not None:
        if _final_layout_score(best_affinity_metrics, best_affinity_positions) < _final_layout_score(chosen_metrics, chosen_positions):
            chosen_positions = best_affinity_positions
            chosen_metrics = best_affinity_metrics
            chosen_label = "affinity"
            if verbose:
                print(
                    f"[md-fabric] category affinity layout kept: total={best_affinity_metrics.total_width}x{best_affinity_metrics.total_height} "
                    f"aspect={best_affinity_metrics.total_aspect:.3f} density={best_affinity_metrics.total_density:.3f} "
                    f"category_density={best_affinity_metrics.total_category_density:.3f}"
                )
        elif verbose:
            print(
                f"[md-fabric] category affinity layout rolled back: candidate total={best_affinity_metrics.total_width}x{best_affinity_metrics.total_height} "
                f"aspect={best_affinity_metrics.total_aspect:.3f} density={best_affinity_metrics.total_density:.3f} "
                f"category_density={best_affinity_metrics.total_category_density:.3f}"
            )

    if verbose and chosen_label == "stacked":
        print("[md-fabric] final category placement kept stacked layout")

    for entry, candidate in zip(category_inputs, best_choices):
        cat_id = entry["cat_id"]
        bmap = entry["bmap"]
        bkeys = entry["bkeys"]
        dims = entry["dims"]
        cat_x, cat_y = chosen_positions[cat_id]

        fill, stroke, title_c = _CATEGORY_STYLE.get(
            cat_id, ("rgba(80,80,80,0.04)", "rgba(80,80,80,0.20)", "rgba(50,50,50,0.75)")
        )
        cat_bands[cat_id] = {
            "x": cat_x, "y": cat_y,
            "w": candidate.width,
            "h": candidate.height,
            "label": cat_labels.get(cat_id, cat_id),
            "fill": fill, "stroke": stroke, "title_color": title_c,
        }

        for bk in bkeys:
            bx, by = candidate.positions[bk]
            w, h, cols = dims[bk]
            bucket_rects[bk] = {"x": bx + cat_x, "y": by + cat_y, "w": w, "h": h, "label": blabels.get(bk, bk)}
            for i, f in enumerate(sorted(bmap[bk], key=lambda x: x.rel)):
                nx = bx + cat_x + pad_h + (i % cols) * spacing
                ny = by + cat_y + pad_top + (i // cols) * spacing
                nodes_out.append(_make(f, int(nx), int(ny)))

    return nodes_out, bucket_rects, cat_bands


def build_nodes(
    files: list[MarkdownFile],
    groups: dict[str, Group],
    edges: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
    file_by_rel = {file.rel: file for file in files}
    degrees: dict[str, int] = {}
    for edge in edges:
        degrees[edge["from"]] = degrees.get(edge["from"], 0) + 1
        degrees[edge["to"]] = degrees.get(edge["to"], 0) + 1

    def make_node(file: MarkdownFile, x: int, y: int) -> dict[str, Any]:
        loc = len(file.content.splitlines())
        text = "\n".join(file.content.splitlines()[:50])
        return {
            "id": file.rel,
            "label": Path(file.rel).name,
            "path": str(file.path),
            "preview": build_node_preview(file.rel, loc, file.content),
            "text": text,
            "loc": loc,
            "x": x,
            "y": y,
            "shape": "dot",
            "size": 18,
            "margin": 10,
            "font": {"size": 11, "color": "rgba(0,0,0,0)"},
            "mass": max(1, degrees.get(file.rel, 1)),
            "category": file.group,
            "bucket": file.group,
        }

    spacing = 80
    pad_h = 40
    pad_top = 55
    pad_bottom = 40
    box_gap = 80
    max_row_width = 3200

    def box_dims(n: int) -> tuple[int, int, int]:
        cols = max(1, math.ceil(math.sqrt(n * 1.3)))
        rows = math.ceil(n / cols)
        w = max(180, 2 * pad_h + 18 + (cols - 1) * spacing)
        h = max(130, pad_top + pad_bottom + 36 + (rows - 1) * spacing)
        return w, h, cols

    def pack_boxes(
        box_keys: list[str],
        box_files: dict[str, list[MarkdownFile]],
        label_fn: Any = None,
    ) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
        dims = {k: box_dims(len(v)) for k, v in box_files.items()}
        cur_x, cur_y, row_h = 0, 0, 0
        positions: dict[str, tuple[int, int]] = {}
        for key in box_keys:
            w, h, _ = dims[key]
            if cur_x > 0 and cur_x + w > max_row_width:
                cur_x = 0
                cur_y += row_h + box_gap
                row_h = 0
            positions[key] = (cur_x, cur_y)
            row_h = max(row_h, h)
            cur_x += w + box_gap

        nodes_out: list[dict[str, Any]] = []
        rects_out: dict[str, dict[str, Any]] = {}
        for key in box_keys:
            bx, by = positions[key]
            w, h, cols = dims[key]
            label = label_fn(key) if label_fn else (key or "(root)")
            rects_out[key] = {"x": bx, "y": by, "w": w, "h": h, "label": label}
            for i, file in enumerate(sorted(box_files[key], key=lambda f: f.rel)):
                nx = bx + pad_h + (i % cols) * spacing
                ny = by + pad_top + (i // cols) * spacing
                nodes_out.append(make_node(file, int(nx), int(ny)))

        return nodes_out, rects_out

    if not groups:
        boxes = compute_path_boxes(files)
        return pack_boxes(sorted(boxes.keys()), boxes)

    rects_toml = group_rects(groups)
    grouped: dict[str, list[MarkdownFile]] = {}
    for file in files:
        grouped.setdefault(file.group, []).append(file)

    nodes_out, rects_out = pack_boxes(
        sorted(grouped.keys()),
        grouped,
        label_fn=lambda gid: rects_toml[gid]["label"] if gid in rects_toml else gid,
    )
    for node in nodes_out:
        if node.get("id") in file_by_rel:
            node["group"] = file_by_rel[node["id"]].group

    return nodes_out, rects_out


def render_html(
    nodes: list[dict[str, Any]],
    edges: list[dict[str, Any]],
    rects: dict[str, dict[str, Any]],
    data_script_tag: str,
    category_bands: dict[str, dict[str, Any]] | None = None,
    bucket_rects: dict[str, dict[str, Any]] | None = None,
    views: list[ViewDef] | None = None,
) -> str:
    _cat_bands_json = json.dumps(category_bands or {}, ensure_ascii=False)
    _bkt_rects_json = json.dumps(bucket_rects or {}, ensure_ascii=False)
    _views_json = json.dumps(
        [
            {
                "id": view.id,
                "label": view.label,
                "start_paths": list(view.start_paths),
                "default_depth": view.default_depth,
            }
            for view in (views or [])
        ],
        ensure_ascii=False,
    )
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<title>Markdown Dependency Graph</title>
<script src="https://unpkg.com/vis-network/standalone/umd/vis-network.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/marked/marked.min.js"></script>
<style>
  html, body {{
    height: 100%;
    margin: 0;
    font-family: system-ui, sans-serif;
    overflow: hidden;
  }}

  #graph {{
    position: absolute;
    left: 0; top: 0;
    width: 100vw;
    height: 100vh;
    background: #fafafa;
  }}

  #panel {{
    position: fixed;
    top: 12px;
    right: 12px;
    z-index: 10;
    display: grid;
    gap: 8px;
    background: #eef1f7;
    border: 1px solid #b9c3d6;
    border-radius: 10px;
    padding: 10px;
    box-shadow: 0 6px 18px rgba(32, 43, 67, .18);
    width: 286px;
    overflow: visible;
  }}

  #panel.collapsed {{
    gap: 0;
  }}

  #panelContent {{
    display: grid;
    gap: 8px;
  }}

  #panel.collapsed #panelContent {{
    display: none;
  }}

  #panelHeader {{
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 2px 2px 0;
    cursor: move;
    user-select: none;
    color: #2d3650;
  }}

  #panelHeaderTitle {{
    font-size: 13px;
    font-weight: 700;
    color: #2d3650;
    letter-spacing: 0.01em;
  }}

  #panel form {{
    display: flex;
    gap: 8px;
  }}

  #panel-top {{
    display: grid;
    gap: 8px;
  }}

  #viewControls {{
    display: grid;
    grid-template-columns: minmax(0, 1fr) 84px;
    gap: 8px;
  }}

  #viewControls.all-files {{
    grid-template-columns: minmax(0, 1fr);
  }}

  #referenceTypeField {{
    grid-column: 1 / -1;
  }}

  #referenceType:disabled {{
    opacity: 0.5;
    cursor: not-allowed;
  }}

  .controlField {{
    display: grid;
    gap: 4px;
    min-width: 0;
  }}

  .controlField.hidden {{
    display: none;
  }}

  .controlHeader {{
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.03em;
    color: #667;
    text-transform: uppercase;
    padding-left: 2px;
  }}

  #viewSelect {{
    min-width: 0;
  }}

  #panel input {{
    width: 100%;
    flex: 1;
    border: 1px solid #ccc;
    border-radius: 8px;
    padding: 7px 10px;
    font-size: 14px;
    outline: none;
    box-sizing: border-box;
  }}

  #panel select {{
    width: 100%;
    border: 1px solid #ccc;
    border-radius: 8px;
    padding: 7px 10px;
    font-size: 14px;
    outline: none;
    background: white;
    box-sizing: border-box;
  }}

  #viewDepth {{
    /* width: 100%; */
    width: 100%;
  }}

  #viewDepth.hidden {{
    display: none;
  }}

  #panel button {{
    cursor: pointer;
    border: 1px solid #ccc;
    background: #f7f7f7;
    border-radius: 8px;
    padding: 6px 10px;
    font-size: 14px;
  }}

  #controls {{
    display: flex;
    flex-direction: column;
    gap: 6px;
  }}

  #details {{
    border: 1px solid #e0e0e0;
    border-radius: 10px;
    padding: 10px 12px;
    background: #fcfcfc;
  }}

  #details.hidden {{
    display: none;
  }}

  #details .label {{
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.02em;
    color: #667;
    text-transform: uppercase;
    margin-bottom: 6px;
  }}

  #details .name {{
    font-size: 14px;
    font-weight: 600;
    margin-bottom: 4px;
    word-break: break-word;
  }}

  #details .path {{
    font-size: 12px;
    color: #555;
    word-break: break-word;
    line-height: 1.35;
  }}

  #tooltip {{
    position: fixed;
    z-index: 20;
    display: none;
    width: min(520px, calc(100vw - 24px));
    max-height: min(540px, calc(100vh - 24px));
    overflow: auto;
    background: rgba(255, 255, 255, 0.98);
    border: 1px solid #d8d8d8;
    border-radius: 12px;
    box-shadow: 0 12px 28px rgba(0, 0, 0, 0.16);
    padding: 12px 14px;
    color: #1f1f1f;
    pointer-events: auto;
  }}

  #tooltip h4 {{
    margin: 0 0 8px;
    font-size: 14px;
  }}

  #tooltip .markdown {{
    white-space: normal;
    overflow-wrap: anywhere;
    word-break: break-word;
    line-height: 1.45;
    font-size: 13px;
  }}

  #tooltip .markdown pre,
  #tooltip .markdown code {{
    white-space: pre-wrap;
  }}

  #tooltip .markdown p,
  #tooltip .markdown ul,
  #tooltip .markdown ol,
  #tooltip .markdown blockquote {{
    margin-top: 0;
    margin-bottom: 0;
  }}

  #tooltip .markdown > ul,
  #tooltip .markdown > ol {{
    padding-left: 12px;
  }}

  #tooltip .markdown li {{
    margin: 0;
    padding: 0;
  }}

  #tooltip .markdown li > p {{
    margin: 0;
  }}

  #tooltip .markdown ul ul,
  #tooltip .markdown ul ol,
  #tooltip .markdown ol ul,
  #tooltip .markdown ol ol {{
    padding-left: 20px;
  }}

  #minimap {{
    width: 240px;
    height: 160px;
    border: 1px solid #ddd;
    background: #fff;
    cursor: crosshair;
  }}

  #minimap-row {{
    display: flex;
    gap: 6px;
    align-items: flex-start;
    padding-top: 10px;
  }}

  #panel-top {{
    display: grid;
    gap: 8px;
  }}

  #panel-top form {{
    width: 100%;
  }}

  #panelToggle {{
    position: static;
    width: 26px;
    height: 26px;
    padding: 0;
    cursor: pointer;
    border: 1px solid #8ea0c3;
    background: #ffffff;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 700;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: none;
  }}

  #panelToggle:hover {{
    background: #f7f7f7;
  }}

  .snip-title {{ margin: 0 0 6px; font-size: 11px; word-break: break-all; color: #444; }}
  .snip-src, .snip-tgt {{ color: #0055aa; font-family: monospace; }}
  .snip-arr {{ color: #c07000; font-weight: bold; }}
  .snip-tbl {{ border-collapse: collapse; width: 100%; font-family: monospace; font-size: 12px; line-height: 1.45; }}
  .snip-tbl tr.snip-match td {{ background: #fffbe5; }}
  .snip-ln {{ color: #c0c0c0; text-align: right; padding: 1px 8px 1px 4px; border-right: 1px solid #ebebeb; user-select: none; min-width: 28px; vertical-align: top; white-space: nowrap; }}
  .snip-match .snip-ln {{ color: #999; background: #fff3c0; }}
  .snip-lc {{ padding: 1px 6px; white-space: pre-wrap; word-break: break-word; color: #1f1f1f; vertical-align: top; }}
  .snip-ell {{ color: #bbb; font-size: 11px; text-align: center; padding: 3px 0; letter-spacing: 0.05em; border-bottom: 1px dashed #eee; }}
  .snip-ell:last-child {{ border-bottom: none; border-top: 1px dashed #eee; }}
  .md-h {{ color: #0066cc; font-weight: bold; }}
  .md-link {{ color: #0366d6; }}
  .md-code {{ background: #f0f0f0; padding: 0 3px; border-radius: 3px; font-size: 11px; }}

  .fm-block {{
    background: #f0f4ff;
    border-left: 3px solid #7799dd;
    border-radius: 0 4px 4px 0;
    padding: 5px 10px;
    margin-bottom: 8px;
    font-family: monospace;
    font-size: 11px;
    display: flex;
    flex-direction: column;
    gap: 1px;
    color: #334;
  }}

  .fm-line {{ display: block; white-space: pre-wrap; word-break: break-word; }}
  .fm-key {{ color: #5555aa; font-weight: 700; }}
  .fm-sep {{ color: #aaa; }}
  .fm-val {{ color: #334; }}

  .path-copyable {{
    cursor: pointer;
    font-family: monospace;
    font-size: 11px;
    color: #444;
    background: #f2f2f5;
    border-radius: 4px;
    padding: 2px 6px;
    transition: background 0.15s, color 0.15s;
    display: inline-block;
    user-select: text;
  }}

  .path-copyable:hover {{
    background: #ddeeff;
    color: #0044aa;
  }}

  .path-copyable.copied {{
    background: #d4edda;
    color: #155724;
    font-weight: 600;
  }}

  #panel {{
    cursor: grab;
  }}

  #panel.dragging {{
    cursor: grabbing;
    user-select: none;
  }}

  #filterStats {{
    font-size: 12px;
    color: #666;
    padding: 2px 6px 0;
  }}

  .filter-stat-link {{
    cursor: pointer;
    color: #0066cc;
    font-weight: 700;
    text-decoration: underline;
    text-underline-offset: 2px;
  }}

  .filter-stat-link:hover {{
    color: #0044aa;
  }}

  #searchResultsToast {{
    position: fixed;
    z-index: 30;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(820px, calc(100vw - 48px));
    max-height: min(540px, calc(100vh - 48px));
    background: white;
    border: 1px solid #ddd;
    border-radius: 12px;
    box-shadow: 0 12px 40px rgba(0,0,0,0.22);
    padding: 16px;
    overflow: hidden;
    flex-direction: column;
  }}

  .srt-header {{
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
    flex-shrink: 0;
  }}

  .srt-title {{
    font-size: 14px;
    font-weight: 600;
  }}

  .srt-close {{
    cursor: pointer;
    border: 1px solid #ccc;
    background: #f7f7f7;
    border-radius: 8px;
    padding: 4px 12px;
    font-size: 13px;
  }}

  #srtTableWrap {{
    overflow-y: auto;
    flex: 1;
  }}

  #searchResultsTable {{
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }}

  #searchResultsTable th {{
    text-align: left;
    padding: 6px 8px;
    border-bottom: 2px solid #eee;
    font-size: 11px;
    text-transform: uppercase;
    color: #667;
    position: sticky;
    top: 0;
    background: white;
    z-index: 1;
  }}

  #searchResultsTable th:not([data-col="path"]),
  #searchResultsTable td.srt-num {{
    text-align: right;
  }}

  #searchResultsTable td {{
    padding: 5px 8px;
    border-bottom: 1px solid #f0f0f0;
    font-family: monospace;
    word-break: break-all;
  }}

  .srt-num {{
    white-space: nowrap;
    color: #666;
    font-family: monospace;
    text-align: right;
  }}

  #searchResultsTable th[data-col] {{
    cursor: pointer;
    user-select: none;
  }}

  #searchResultsTable th[data-col]:hover {{
    color: #333;
    background: #f5f5f5;
  }}

  .srt-th-active {{
    background: #eef3ff !important;
    color: #0044cc !important;
  }}

  .srt-loc {{
    white-space: nowrap;
    color: #667;
    font-family: monospace;
  }}

  #searchResultsTable tbody tr {{
    cursor: pointer;
    transition: background 0.1s;
  }}

  #searchResultsTable tbody tr:hover {{
    background: #f0f4ff;
  }}

  #searchResultsTable mark {{
    background: #fff3c0;
    border-radius: 2px;
    padding: 0 1px;
  }}

  #searchForm {{
    display: flex;
    gap: 8px;
    align-items: center;
  }}

  #searchInput {{
    min-width: 0;
  }}

  #filterDepthField {{
    flex: none;
    width: 84px;
  }}

  #tooltip h4 {{
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 0 0 8px;
    font-size: 13px;
    font-weight: 600;
  }}

  .tt-l1 {{
    font-size: 13px;
    font-weight: 700;
    color: #1a1a2e;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }}

  .tt-l2 {{
    font-size: 11px;
    font-family: monospace;
    color: #555;
    font-weight: 400;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }}

  .tt-l3 {{
    font-size: 11px;
    color: #667;
    font-weight: 400;
    line-height: 1.4;
  }}

  .tt-link {{
    color: #0055cc;
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 2px;
    font-weight: 600;
  }}

  .tt-link:hover {{
    color: #003399;
  }}

  .ldt-toast {{
    position: fixed;
    z-index: 40;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(760px, calc(100vw - 48px));
    max-height: min(520px, calc(100vh - 48px));
    background: white;
    border: 1px solid #ddd;
    border-radius: 12px;
    box-shadow: 0 12px 40px rgba(0,0,0,0.25);
    padding: 16px;
    overflow: hidden;
    flex-direction: column;
  }}

  .ldt-header {{
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    margin-bottom: 10px;
    flex-shrink: 0;
    gap: 12px;
  }}

  .ldt-titles {{
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }}

  .ldt-title {{
    font-size: 14px;
    font-weight: 700;
    color: #1a1a2e;
  }}

  .ldt-subtitle {{
    font-size: 11px;
    color: #667;
    font-family: monospace;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }}

  .ldt-count {{
    font-size: 12px;
    color: #555;
    margin-bottom: 8px;
    flex-shrink: 0;
  }}

  .ldt-close {{
    cursor: pointer;
    border: none;
    background: none;
    font-size: 18px;
    color: #888;
    line-height: 1;
    padding: 2px 4px;
    flex-shrink: 0;
  }}

  .ldt-close:hover {{
    color: #333;
  }}

  .ldt-table-wrap {{
    overflow-y: auto;
    flex: 1;
  }}

  .ldt-table {{
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
  }}

  .ldt-table th {{
    text-align: left;
    padding: 6px 8px;
    border-bottom: 2px solid #eee;
    font-size: 11px;
    text-transform: uppercase;
    color: #667;
    position: sticky;
    top: 0;
    background: white;
    z-index: 1;
    cursor: pointer;
    user-select: none;
  }}

  .ldt-table th:hover {{ color: #333; background: #f5f5f5; }}
  .ldt-table th.ldt-th-active {{ background: #eef3ff !important; color: #0044cc !important; }}

  .ldt-table th.ldt-num, .ldt-table td.ldt-num {{
    text-align: right;
  }}

  .ldt-table td {{
    padding: 5px 8px;
    border-bottom: 1px solid #f0f0f0;
    font-family: monospace;
    word-break: break-all;
  }}

  .ldt-table tbody tr {{
    cursor: pointer;
    transition: background 0.1s;
  }}

  .ldt-table tbody tr:hover {{ background: #f0f4ff; }}

  .cpt-cards-wrap {{
    overflow-y: auto;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding-right: 4px;
  }}

  .cpt-card {{
    border: 1px solid #e0e0e0;
    border-radius: 8px;
    flex-shrink: 0;
  }}

  .cpt-card-header {{
    position: sticky;
    top: 0;
    z-index: 2;
    background: #f0f4ff;
    padding: 6px 10px;
    font-size: 12px;
    font-family: monospace;
    font-weight: 700;
    color: #0044aa;
    border-bottom: 1px solid #d8e0f0;
    border-radius: 8px 8px 0 0;
    word-break: break-all;
  }}

  .cpt-card-refs {{
    padding: 6px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }}

  .cpt-ref-row {{
    display: flex;
    flex-direction: column;
    gap: 2px;
  }}

  .cpt-ref-id {{
    font-size: 11px;
    font-family: monospace;
    font-weight: 700;
    color: #7744cc;
  }}

  .cpt-ref-snippet {{
    font-size: 11px;
    font-family: monospace;
    color: #444;
    background: #fafafa;
    border-left: 3px solid #d0d0e8;
    padding: 3px 8px;
    white-space: pre-wrap;
    word-break: break-word;
    border-radius: 0 4px 4px 0;
    max-height: 160px;
    overflow-y: auto;
  }}

  .cpt-hl {{
    background: #fff3c0;
    border-radius: 2px;
    padding: 0 1px;
  }}
  .cpt-def-hl {{
    background: #c8f0d0;
    border-radius: 2px;
    padding: 0 1px;
  }}
  .edt-toast {{
    width: min(820px, calc(100vw - 48px));
    max-height: min(640px, calc(100vh - 48px));
  }}
  .edt-tabs {{
    display: flex;
    gap: 2px;
    flex-shrink: 0;
    border-bottom: 2px solid #e0e0e0;
    margin-bottom: 10px;
  }}
  .edt-tab {{
    padding: 5px 14px;
    border: none;
    background: none;
    cursor: pointer;
    font-size: 12px;
    font-weight: 600;
    color: #778;
    border-bottom: 2px solid transparent;
    margin-bottom: -2px;
    border-radius: 6px 6px 0 0;
    transition: color 0.12s, background 0.12s;
  }}
  .edt-tab:hover:not(:disabled) {{
    color: #334;
    background: #f0f0f0;
  }}
  .edt-tab.edt-tab-active {{
    color: #0044cc;
    border-bottom-color: #0044cc;
  }}
  .edt-tab:disabled {{
    opacity: 0.35;
    cursor: not-allowed;
  }}
  .edt-tab-panel {{
    overflow-y: auto;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
  }}
  .edt-select-row {{
    flex-shrink: 0;
  }}
  .edt-select {{
    width: 100%;
    border: 1px solid #ccc;
    border-radius: 6px;
    padding: 5px 8px;
    font-size: 12px;
    background: #fff;
    box-sizing: border-box;
  }}
  .edt-snippet-wrap {{
    overflow-y: auto;
    flex: 1;
    min-height: 0;
  }}
  .edt-section {{
    border: 1px solid #dde;
    border-radius: 7px;
    overflow: hidden;
    flex-shrink: 0;
  }}
  .edt-section-header {{
    background: #f4f6ff;
    padding: 5px 11px;
    font-size: 11px;
    font-family: monospace;
    font-weight: 700;
    color: #334;
    border-bottom: 1px solid #dde;
    word-break: break-all;
  }}
  .edt-section-header.edt-def-header {{
    background: #f0fff4;
    border-bottom-color: #c8ecd4;
    color: #1a5c2a;
  }}
  .edt-section-body {{
    padding: 6px 10px;
    font-size: 11px;
    font-family: monospace;
    color: #444;
    background: #fafafa;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 180px;
    overflow-y: auto;
  }}
</style>
</head>
<body>
<div id="graph"></div>

<div id="panel">
  <div id="panelHeader">
    <div id="panelHeaderTitle">Markdown Fabric View</div>
    <button type="button" id="panelToggle" title="Collapse panel">▾</button>
  </div>
  <div id="panelContent">
    <div id="panel-top">
      <div id="viewControls">
        <div class="controlField" id="viewField">
          <div class="controlHeader" id="viewHeader">View</div>
          <select id="viewSelect" aria-label="Select file view"></select>
        </div>
        <div class="controlField" id="viewDepthField">
          <div class="controlHeader" id="viewDepthHeader">Link Depth</div>
          <input id="viewDepth" type="number" min="0" step="1" value="5" aria-label="Reference depth" />
        </div>
        <div class="controlField" id="referenceTypeField">
          <div class="controlHeader" id="referenceTypeHeader">Reference type</div>
          <select id="referenceType" aria-label="Reference type" disabled>
            <option value="file">File reference</option>
            <option value="cpt">CPT ID reference</option>
            <option value="both">File &amp; CPT ID reference</option>
          </select>
        </div>
      </div>
      <form id="searchForm">
        <input id="searchInput" type="search" placeholder="Filter files..." autocomplete="off" spellcheck="false" />
        <div class="controlField hidden" id="filterDepthField">
          <input id="filterDepth" type="number" min="0" step="1" value="1" aria-label="Filter depth" />
        </div>
      </form>
    </div>
    <div id="filterStats"><span id="filterCount" class="filter-stat-link">0</span> files found,&nbsp;<span id="filterLOC" class="filter-stat-link">0</span> total LOC</div>
    <div id="panel-body">
      <div id="details" class="hidden">
        <div class="label">Selected file</div>
        <div class="name" id="detailsName">None selected</div>
        <div class="path" id="detailsPath">Click a node to inspect its full path.</div>
      </div>
      <div id="minimap-row">
        <canvas id="minimap" width="180" height="120"></canvas>
        <div id="controls">
          <button onclick="zoomBy(1.2)">+</button>
          <button onclick="zoomBy(0.8)">−</button>
          <button onclick="fitCurrentView()">Fit</button>
        </div>
      </div>
    </div>
  </div>
</div>

<div id="tooltip">
  <h4 id="tooltipTitle"></h4>
  <div class="markdown" id="tooltipBody"></div>
</div>

<div id="searchResultsToast" style="display:none">
  <div class="srt-header">
    <span class="srt-title" id="srtTitle">Filtered files</span>
    <button class="srt-close" id="srtClose">Close</button>
  </div>
  <div id="srtTableWrap">
    <table id="searchResultsTable">
      <thead><tr><th data-col="path" data-label="File path" title="Repository-relative Markdown file path">File path</th><th data-col="loc" data-label="LOC" title="Total lines of text in the file">LOC</th><th data-col="in" data-label="In" title="Inbound links count: how many Markdown files link to this file">In</th><th data-col="out" data-label="Out" title="Outbound links count: how many Markdown files this file links to">Out</th></tr></thead>
      <tbody id="srtBody"></tbody>
    </table>
  </div>
</div>

<div id="linksDetailToast" class="ldt-toast" style="display:none">
  <div class="ldt-header">
    <div class="ldt-titles">
      <div class="ldt-title" id="ldtTitle">File links</div>
      <div class="ldt-subtitle" id="ldtSubtitle"></div>
    </div>
    <button class="ldt-close" id="ldtClose" title="Close (ESC)">&#x2715;</button>
  </div>
  <div class="ldt-count" id="ldtCount"></div>
  <div class="ldt-table-wrap">
    <table class="ldt-table" id="ldtTable">
      <thead><tr>
        <th data-col="path" data-label="File path">File path</th>
        <th data-col="loc" data-label="LOC" class="ldt-num">LOC</th>
        <th data-col="links" data-label="Links" class="ldt-num">Links</th>
      </tr></thead>
      <tbody id="ldtBody"></tbody>
    </table>
  </div>
</div>

<div id="cptDetailToast" class="ldt-toast" style="display:none">
  <div class="ldt-header">
    <div class="ldt-titles">
      <div class="ldt-title" id="cdtTitle">CPT references</div>
      <div class="ldt-subtitle" id="cdtSubtitle"></div>
    </div>
    <button class="ldt-close" id="cdtClose" title="Close (ESC)">&#x2715;</button>
  </div>
  <div class="ldt-count" id="cdtCount"></div>
  <div class="cpt-cards-wrap" id="cdtCards"></div>
</div>

<div id="edgeDetailToast" class="ldt-toast edt-toast" style="display:none">
  <div class="ldt-header">
    <div class="ldt-titles">
      <div class="ldt-title" id="edtTitle">Link details</div>
      <div class="ldt-subtitle" id="edtSubtitle"></div>
    </div>
    <button class="ldt-close" id="edtClose" title="Close (ESC)">&#x2715;</button>
  </div>
  <div class="edt-tabs">
    <button class="edt-tab" id="edtTabFile" data-tab="file">File links</button>
    <button class="edt-tab" id="edtTabCpt" data-tab="cpt">CPT links</button>
  </div>
  <div class="edt-tab-panel" id="edtFilePanel">
    <div class="edt-select-row" id="edtFileSelectRow">
      <select id="edtFileSelect" class="edt-select"></select>
    </div>
    <div id="edtFileSnippet" class="edt-snippet-wrap"></div>
  </div>
  <div class="edt-tab-panel" id="edtCptPanel" style="display:none">
    <div class="edt-select-row" id="edtCptSelectRow">
      <select id="edtCptSelect" class="edt-select"></select>
    </div>
    <div id="edtCptRefSection" class="edt-section"></div>
    <div id="edtCptDefSection" class="edt-section" style="display:none"></div>
  </div>
</div>

{data_script_tag}
<script>
const groupRects   = {json.dumps(rects, ensure_ascii=False)};
const categoryBands = {_cat_bands_json};
const bucketRects = {_bkt_rects_json};
const controlPlaneViews = {_views_json};

function initGraph(rawNodes, rawEdges, rawCptEdges) {{

const allRawEdges = [
  ...rawEdges.map(e => ({{ ...e, type: "file" }})),
  ...rawCptEdges.map(e => ({{ ...e, type: "cpt" }})),
];
const fileEdgePairs = new Set(rawEdges.map(e => `${{e.from}}---${{e.to}}`));
const nodes = new vis.DataSet(rawNodes);
const edges = new vis.DataSet(allRawEdges);
const nodeById = new Map(rawNodes.map(node => [node.id, node]));
const edgeById = new Map(rawEdges.map((edge, index) => [edge.id ?? index, {{ ...edge, id: edge.id ?? index }}]));
const adjacency = new Map();
const outboundAdjacency = new Map();
const inboundAdjacency = new Map();

rawEdges.forEach((edge, index) => {{
  const edgeId = edge.id ?? index;
  if (!adjacency.has(edge.from)) adjacency.set(edge.from, new Set());
  if (!adjacency.has(edge.to)) adjacency.set(edge.to, new Set());
  adjacency.get(edge.from).add(edge.to);
  adjacency.get(edge.to).add(edge.from);
  if (!outboundAdjacency.has(edge.from)) outboundAdjacency.set(edge.from, []);
  outboundAdjacency.get(edge.from).push({{ to: edge.to, edgeId }});
  if (!inboundAdjacency.has(edge.to)) inboundAdjacency.set(edge.to, []);
  inboundAdjacency.get(edge.to).push({{ from: edge.from, edgeId }});
}});

const inLinkCount = new Map();
const outLinkCount = new Map();
for (const edge of rawEdges) {{
  outLinkCount.set(edge.from, (outLinkCount.get(edge.from) || 0) + 1);
  inLinkCount.set(edge.to,   (inLinkCount.get(edge.to)   || 0) + 1);
}}

const cptOutboundAdj = new Map();
const cptInboundAdj = new Map();
const cptOutLinkCount = new Map();
const cptInLinkCount = new Map();
rawCptEdges.forEach(edge => {{
  if (!cptOutboundAdj.has(edge.from)) cptOutboundAdj.set(edge.from, []);
  cptOutboundAdj.get(edge.from).push({{ to: edge.to, refs: edge.refs || [] }});
  if (!cptInboundAdj.has(edge.to)) cptInboundAdj.set(edge.to, []);
  cptInboundAdj.get(edge.to).push({{ from: edge.from, refs: edge.refs || [] }});
  cptOutLinkCount.set(edge.from, (cptOutLinkCount.get(edge.from) || 0) + 1);
  cptInLinkCount.set(edge.to,   (cptInLinkCount.get(edge.to)   || 0) + 1);
}});

const cptEdgeById = new Map(rawCptEdges.map(e => [e.id, e]));
const pairFileEdges = new Map();
for (const edge of rawEdges) {{
  const key = edge.from + "\x00" + edge.to;
  if (!pairFileEdges.has(key)) pairFileEdges.set(key, []);
  pairFileEdges.get(key).push(edge);
}}
const pairCptEdge = new Map();
for (const edge of rawCptEdges) {{
  pairCptEdge.set(edge.from + "\x00" + edge.to, edge);
}}

const container = document.getElementById("graph");
const viewControls = document.getElementById("viewControls");
const viewField = document.getElementById("viewField");
const viewSelect = document.getElementById("viewSelect");
const viewDepthField = document.getElementById("viewDepthField");
const viewDepth = document.getElementById("viewDepth");
const referenceType = document.getElementById("referenceType");
const searchInput = document.getElementById("searchInput");
const tooltip = document.getElementById("tooltip");
const tooltipTitle = document.getElementById("tooltipTitle");
const tooltipBody = document.getElementById("tooltipBody");
const details = document.getElementById("details");
const detailsName = document.getElementById("detailsName");
const detailsPath = document.getElementById("detailsPath");
let tooltipHideTimer = null;
let controlTooltipTimer = null;

const panel = document.getElementById("panel");
const panelHeader = document.getElementById("panelHeader");
let panelDrag = null;
panelHeader.addEventListener("mousedown", e => {{
  if (e.target.closest("button")) return;
  const rect = panel.getBoundingClientRect();
  panelDrag = {{ dx: e.clientX - rect.left, dy: e.clientY - rect.top }};
  panel.style.right = "auto";
  panel.style.left = rect.left + "px";
  panel.style.top = rect.top + "px";
  panel.classList.add("dragging");
  e.preventDefault();
}});
document.addEventListener("mousemove", e => {{
  if (!panelDrag) return;
  panel.style.left = Math.max(0, Math.min(window.innerWidth - panel.offsetWidth, e.clientX - panelDrag.dx)) + "px";
  panel.style.top = Math.max(0, Math.min(window.innerHeight - panel.offsetHeight, e.clientY - panelDrag.dy)) + "px";
}});
document.addEventListener("mouseup", () => {{
  if (panelDrag) {{
    panelDrag = null;
    panel.classList.remove("dragging");
  }}
}});

if (window.marked) {{
  marked.setOptions({{ gfm: true, breaks: true, headerIds: false, mangle: false }});
}}

const options = {{
  interaction: {{
    hover: true,
    tooltipDelay: 120,
    dragNodes: false,
    dragView: true,
    zoomView: true,
    zoomSpeed: 0.5,
    navigationButtons: false,
    multiselect: false
  }},
  layout: {{ improvedLayout: false }},
  physics: false,
  nodes: {{
    borderWidth: 1,
    shape: "dot",
    size: 18,
    shadow: {{ enabled: true, color: "rgba(0,0,0,0.15)", size: 4, x: 0, y: 1 }},
    color: {{
      background: "#ffffff",
      border: "#555555",
      highlight: {{
        background: "#fff7cc",
        border: "#d99a00"
      }}
    }}
  }},
  edges: {{
    smooth: false,
    color: {{
      color: "#777777",
      highlight: "#d98200",
      hover: "#d98200"
    }},
    font: {{
      align: "middle"
    }}
  }},
  groups: {{
    guidelines: {{ color: {{ background: "#fff8db", border: "#c99a00" }} }},
    documents:  {{ color: {{ background: "#eaf3ff", border: "#3572b0" }} }},
    others:     {{ color: {{ background: "#eeeeee", border: "#777777" }} }},
    cf-studio:  {{ color: {{ background: "#f4eeff", border: "#7744cc" }} }},
    toolkit:     {{ color: {{ background: "#fff5e6", border: "#c07000" }} }},
    gears:    {{ color: {{ background: "#edfff4", border: "#28a060" }} }},
    other:      {{ color: {{ background: "#f4f4f4", border: "#888888" }} }},
  }},
}};

const network = new vis.Network(container, {{ nodes, edges }}, options);
let selectedNode = null;
let hoveredNode = null;
let previewedNode = null;
let selectedEdge = null;
let tooltipIsEdge = false;
let activeViewNodeIds = new Set(rawNodes.map(node => node.id));
let activeViewEdgeIds = new Set(rawEdges.map((edge, index) => edge.id ?? index));
let activeCategoryIds = new Set();
let activeBucketIds = new Set();
let activeGroupIds = new Set();
const allCategoryIds = new Set(rawNodes.map(node => node.category).filter(Boolean));
const allBucketIds = new Set(rawNodes.map(node => node.bucket).filter(Boolean));
const allGroupIds = new Set(rawNodes.map(node => node.group).filter(Boolean));
let filterFromNodeSelection = false;
let manualFilter = "";
function normalized(text) {{
  return (text || "").trim().toLowerCase();
}}

function globToRegExp(pattern) {{
  let escaped = String(pattern).replaceAll(".", "\\\\.");
  escaped = escaped.replaceAll("**", "__DOUBLE_STAR__");
  escaped = escaped.replaceAll("*", "[^/]*");
  escaped = escaped.replaceAll("?", "[^/]");
  escaped = escaped.replaceAll("__DOUBLE_STAR__", ".*");
  return new RegExp("^" + escaped + "$");
}}

const compiledViews = controlPlaneViews.map(view => ({{
  ...view,
  startMatchers: (view.start_paths || []).map(globToRegExp),
}}));
const compiledViewsById = new Map(compiledViews.map(view => [view.id, view]));

function fitCurrentView() {{
  const query = normalized(searchInput.value);
  let ids;
  if (query) {{
    const directMatches = matchingNodeIds(query);
    const depth = Math.max(0, Number.parseInt(document.getElementById("filterDepth").value || "1", 10) || 0);
    ids = [...computeFilterExpansion(directMatches, depth)];
  }} else {{
    ids = [...activeViewNodeIds];
  }}
  if (!ids.length) return;
  network.fit({{ nodes: ids, animation: {{ duration: 250, easingFunction: "easeInOutQuad" }} }});
}}

function refreshActiveBands() {{
  activeCategoryIds = new Set();
  activeBucketIds = new Set();
  activeGroupIds = new Set();
  for (const id of activeViewNodeIds) {{
    const node = nodeById.get(id);
    if (!node) continue;
    if (node.category) activeCategoryIds.add(node.category);
    if (node.bucket) activeBucketIds.add(node.bucket);
    if (node.group) activeGroupIds.add(node.group);
  }}
}}

function rootIdsForView(view) {{
  const ids = [];
  for (const node of rawNodes) {{
    if (view.startMatchers.some(matcher => matcher.test(node.id))) ids.push(node.id);
  }}
  return ids;
}}

function computeActiveSubgraph(viewId, depth) {{
  if (viewId === "all") {{
    return {{
      nodeIds: new Set(rawNodes.map(node => node.id)),
      edgeIds: new Set(rawEdges.map((edge, index) => edge.id ?? index)),
    }};
  }}

  const view = compiledViewsById.get(viewId);
  if (!view) return computeActiveSubgraph("all", depth);

  const roots = rootIdsForView(view);
  const nodeIds = new Set(roots);
  const edgeIds = new Set();
  const queue = roots.map(id => [id, 0]);
  const seenDepth = new Map(roots.map(id => [id, 0]));

  while (queue.length) {{
    const [currentId, currentDepth] = queue.shift();
    if (currentDepth >= depth) continue;
    for (const link of outboundAdjacency.get(currentId) || []) {{
      edgeIds.add(link.edgeId);
      const nextDepth = currentDepth + 1;
      const previousDepth = seenDepth.get(link.to);
      if (previousDepth != null && previousDepth <= nextDepth) continue;
      seenDepth.set(link.to, nextDepth);
      nodeIds.add(link.to);
      queue.push([link.to, nextDepth]);
    }}
  }}

  return {{ nodeIds, edgeIds }};
}}

function computeFilterExpansion(matchingIds, depth) {{
  if (!depth || depth <= 0) return new Set(matchingIds);
  const result = new Set(matchingIds);
  const queue = [...matchingIds].map(id => [id, 0]);
  const seen = new Map([...matchingIds].map(id => [id, 0]));
  while (queue.length) {{
    const [cur, d] = queue.shift();
    if (d >= depth) continue;
    for (const link of (outboundAdjacency.get(cur) || [])) {{
      const nd = d + 1;
      if (seen.has(link.to) && seen.get(link.to) <= nd) continue;
      seen.set(link.to, nd);
      result.add(link.to);
      queue.push([link.to, nd]);
    }}
    for (const link of (inboundAdjacency.get(cur) || [])) {{
      const nd = d + 1;
      if (seen.has(link.from) && seen.get(link.from) <= nd) continue;
      seen.set(link.from, nd);
      result.add(link.from);
      queue.push([link.from, nd]);
    }}
  }}
  return result;
}}

function syncFilterDepthVisibility() {{
  const hasQuery = normalized(searchInput.value).length > 0;
  document.getElementById("filterDepthField").classList.toggle("hidden", !hasQuery);
}}

function populateViewControls() {{
  viewSelect.innerHTML = "";
  const allOption = document.createElement("option");
  allOption.value = "all";
  allOption.textContent = "All files view";
  viewSelect.appendChild(allOption);
  for (const view of compiledViews) {{
    const option = document.createElement("option");
    option.value = view.id;
    option.textContent = view.label;
    viewSelect.appendChild(option);
  }}
  viewSelect.value = "all";
  viewDepth.value = "5";
  syncViewDepthVisibility();
}}

function applyViewState({{ fit = false }} = {{}}) {{
  const viewId = viewSelect.value || "all";
  const depth = Math.max(0, Number.parseInt(viewDepth.value || "0", 10) || 0);
  const {{ nodeIds, edgeIds }} = computeActiveSubgraph(viewId, depth);
  activeViewNodeIds = nodeIds;
  activeViewEdgeIds = edgeIds;
  refreshActiveBands();

  if (selectedNode && !activeViewNodeIds.has(selectedNode)) {{
    clearHighlight();
  }} else {{
    if (previewedNode && !activeViewNodeIds.has(previewedNode)) hideTooltip();
    if (selectedEdge != null && !activeViewEdgeIds.has(selectedEdge)) {{
      selectedEdge = null;
      hideTooltip();
    }}
    updateStyles();
  }}

  if (fit) fitCurrentView();
}}

function matchingNodeIds(query) {{
  if (!query) return new Set(activeViewNodeIds);
  const isPrefix = query.startsWith("./");
  const searchTerm = isPrefix ? query.slice(2) : query;
  if (!searchTerm) return new Set(activeViewNodeIds);

  const matches = new Set();
  for (const nodeId of activeViewNodeIds) {{
    const node = nodeById.get(nodeId);
    if (!node) continue;
    if (isPrefix) {{
      if (node.id.toLowerCase().startsWith(searchTerm)) matches.add(node.id);
    }} else {{
      const haystack = [node.id, node.label].filter(Boolean).join(" ").toLowerCase();
      if (haystack.includes(searchTerm)) matches.add(node.id);
    }}
  }}
  return matches;
}}

function connectedSet(nodeId) {{
  if (!activeViewNodeIds.has(nodeId)) return {{ connectedNodes: new Set(), connectedEdges: new Set() }};
  const connectedNodes = new Set([nodeId]);
  const connectedEdges = new Set();

  for (const edgeId of activeViewEdgeIds) {{
    const edge = edgeById.get(edgeId);
    if (!edge) continue;
    if (edge.from !== nodeId && edge.to !== nodeId) continue;
    connectedEdges.add(edgeId);
    connectedNodes.add(edge.from);
    connectedNodes.add(edge.to);
  }}

  return {{ connectedNodes, connectedEdges }};
}}

function setDetails(nodeId) {{
  const node = nodeById.get(nodeId);
  if (!node) return;

  details.classList.remove("hidden");
  detailsName.textContent = node.label;

  detailsPath.innerHTML = '';
  const pathSpan = document.createElement('span');
  pathSpan.className = 'path-copyable';
  pathSpan.textContent = node.id;
  pathSpan.title = 'Click to copy path';
  pathSpan.addEventListener('click', () => {{
    const text = node.id;
    const restore = () => {{
      pathSpan.textContent = text;
      pathSpan.classList.remove('copied');
    }};
    if (navigator.clipboard) {{
      navigator.clipboard.writeText(text).then(() => {{
        pathSpan.textContent = '\u2713 Copied!';
        pathSpan.classList.add('copied');
        setTimeout(restore, 1500);
      }}).catch(restore);
    }} else {{
      const ta = document.createElement('textarea');
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand('copy');
      document.body.removeChild(ta);
      pathSpan.textContent = '\u2713 Copied!';
      pathSpan.classList.add('copied');
      setTimeout(restore, 1500);
    }}
  }});
  detailsPath.appendChild(pathSpan);
}}

function updateStyles() {{
  const query = normalized(searchInput.value);
  const directMatches = matchingNodeIds(query);
  const filterActive = query.length > 0;
  const depth = filterActive ? Math.max(0, Number.parseInt(document.getElementById("filterDepth").value || "1", 10) || 0) : 0;
  const visibleSet = filterActive ? computeFilterExpansion(directMatches, depth) : directMatches;
  const viewModeActive = (viewSelect.value || "all") !== "all";
  let totalLOC = 0;
  for (const id of visibleSet) {{
    const node = nodeById.get(id);
    if (node && node.loc) totalLOC += node.loc;
  }}
  document.getElementById("filterCount").textContent = visibleSet.size;
  document.getElementById("filterLOC").textContent = totalLOC;
  const selection = selectedNode && !filterActive ? connectedSet(selectedNode) : null;

  nodes.update(rawNodes.map(node => {{
    const inView = activeViewNodeIds.has(node.id);
    const isInVisible = visibleSet.has(node.id);
    const isSelected = selection ? selection.connectedNodes.has(node.id) : true;
    let opacity;
    if (filterActive) {{
      opacity = isInVisible ? 1 : 0.12;
    }} else if (selection) {{
      opacity = isSelected ? 1 : 0.15;
    }} else {{
      opacity = 1;
    }}
    if (viewModeActive && !inView) opacity = Math.min(opacity, 0.12);
    return {{ id: node.id, hidden: false, opacity }};
  }}));

  const refType = referenceType.value;
  edges.update(allRawEdges.map((edge, index) => {{
    const edgeId = edge.id ?? index;
    const isCpt = edge.type === "cpt";
    if (isCpt) {{
      if (refType === "file") return {{ id: edgeId, hidden: true }};
      if (refType === "both" && fileEdgePairs.has(`${{edge.from}}---${{edge.to}}`)) return {{ id: edgeId, hidden: true }};
      const bothVisible = filterActive ? (visibleSet.has(edge.from) && visibleSet.has(edge.to)) : true;
      return {{ id: edgeId, hidden: false, dashes: [5, 5], color: {{ color: "#9b59b6", opacity: bothVisible ? 1 : 0.1 }}, width: bothVisible ? 2 : 1 }};
    }}
    if (refType === "cpt") return {{ id: edgeId, hidden: true }};
    const inView = activeViewEdgeIds.has(edgeId) && activeViewNodeIds.has(edge.from) && activeViewNodeIds.has(edge.to);
    let active, inactiveOpacity;
    if (filterActive) {{
      const bothVisible = visibleSet.has(edge.from) && visibleSet.has(edge.to);
      active = bothVisible && (!viewModeActive || inView);
      inactiveOpacity = viewModeActive && !inView ? 0.12 : (bothVisible ? 0.3 : 0.1);
    }} else {{
      const sourceMatch = directMatches.has(edge.from) || directMatches.has(edge.to);
      const sourceSelected = selection ? selection.connectedEdges.has(edgeId) : true;
      active = inView && sourceMatch && sourceSelected;
      inactiveOpacity = viewModeActive && !inView ? 0.12 : (sourceSelected ? 1 : 0.12);
    }}
    return {{ id: edgeId, hidden: false, dashes: false, color: {{ color: active ? "#d98200" : "#bbbbbb", opacity: active ? 1 : inactiveOpacity }}, width: active ? 3 : 1 }};
  }}));
}}

function renderMarkdownPreview(node) {{
  tooltipTitle.textContent = node.label;
  const markdown = node.preview || "_(empty file)_";
  if (window.marked) {{
    tooltipBody.innerHTML = marked.parse(markdown);
  }} else {{
    tooltipBody.textContent = markdown;
  }}
}}

function positionTooltipAt(point) {{
  const width = tooltip.offsetWidth || 420;
  const height = tooltip.offsetHeight || 280;
  let left = point.x + 18;
  let top = point.y + 18;

  if (left + width > window.innerWidth - 12) left = point.x - width - 18;
  if (top + height > window.innerHeight - 12) top = window.innerHeight - height - 12;
  if (left < 12) left = 12;
  if (top < 12) top = 12;

  tooltip.style.left = `${{left}}px`;
  tooltip.style.top = `${{top}}px`;
}}

function positionNodeTooltip(nodeId) {{
  positionTooltipAt(network.canvasToDOM(network.getPosition(nodeId)));
}}

function positionEdgeTooltip(edgeId) {{
  const edge = edgeById.get(edgeId);
  if (!edge) return;

  const from = network.getPosition(edge.from);
  const to = network.getPosition(edge.to);
  positionTooltipAt(network.canvasToDOM({{ x: (from.x + to.x) / 2, y: (from.y + to.y) / 2 }}));
}}

function showTooltipMarkdown(title, markdown, positioner) {{
  tooltipIsEdge = false;
  if (tooltipHideTimer) {{
    clearTimeout(tooltipHideTimer);
    tooltipHideTimer = null;
  }}

  tooltipTitle.textContent = title;
  if (window.marked) {{
    tooltipBody.innerHTML = marked.parse(markdown);
  }} else {{
    tooltipBody.textContent = markdown;
  }}

  tooltip.style.display = "block";
  tooltip.dataset.kind = "markdown";
  requestAnimationFrame(positioner);
}}

function cancelControlTooltip() {{
  if (controlTooltipTimer) {{
    clearTimeout(controlTooltipTimer);
    controlTooltipTimer = null;
  }}
}}

function positionElementTooltip(element) {{
  const rect = element.getBoundingClientRect();
  positionTooltipAt({{ x: rect.left + rect.width / 2, y: rect.bottom }});
}}

function scheduleControlTooltip(element, title, markdown) {{
  cancelControlTooltip();
  controlTooltipTimer = setTimeout(() => {{
    tooltipIsEdge = false;
    tooltip.dataset.kind = "control";
    if (window.marked) {{
      tooltipTitle.textContent = title;
      tooltipBody.innerHTML = marked.parse(markdown);
    }} else {{
      tooltipTitle.textContent = title;
      tooltipBody.textContent = markdown;
    }}
    tooltip.style.display = "block";
    requestAnimationFrame(() => positionElementTooltip(element));
    controlTooltipTimer = null;
  }}, 1500);
}}

function attachControlTooltip(element, title, markdown) {{
  element.addEventListener("mouseenter", () => {{
    scheduleControlTooltip(element, title, markdown);
  }});
  element.addEventListener("mouseleave", () => {{
    cancelControlTooltip();
    if (tooltip.dataset.kind === "control") hideTooltip();
  }});
}}

function suppressControlTooltipOnInteract(element) {{
  const cancel = () => {{
    cancelControlTooltip();
    if (tooltip.dataset.kind === "control") hideTooltip();
  }};
  element.addEventListener("mousedown", cancel);
  element.addEventListener("focus", cancel);
  element.addEventListener("click", cancel);
  element.addEventListener("keydown", cancel);
}}

function showNodeTooltip(nodeId) {{
  const node = nodeById.get(nodeId);
  if (!node) return;

  tooltipIsEdge = false;
  if (tooltipHideTimer) {{ clearTimeout(tooltipHideTimer); tooltipHideTimer = null; }}

  const outCount    = outLinkCount.get(nodeId)    || 0;
  const inCount     = inLinkCount.get(nodeId)     || 0;
  const cptOut      = cptOutLinkCount.get(nodeId) || 0;
  const cptIn       = cptInLinkCount.get(nodeId)  || 0;
  const safeLabel   = escapeHtml(node.label || "");
  const safePath    = escapeHtml(nodeId);
  const locStr      = node.loc ? ` \u2014 ${{node.loc}} lines` : "";

  tooltipTitle.innerHTML =
    `<span class="tt-l1">${{safeLabel}}${{locStr}}</span>` +
    `<span class="tt-l2">${{safePath}}</span>` +
    `<span class="tt-l3">` +
    `File links \u2014 ` +
    `<a class="tt-link" data-node="${{safePath}}" data-dir="out" data-kind="file">${{outCount}}&nbsp;outbound</a>` +
    ` / <a class="tt-link" data-node="${{safePath}}" data-dir="in" data-kind="file">${{inCount}}&nbsp;inbound</a>` +
    `, CPT refs \u2014 ` +
    `<a class="tt-link" data-node="${{safePath}}" data-dir="out" data-kind="cpt">${{cptOut}}&nbsp;outbound</a>` +
    ` / <a class="tt-link" data-node="${{safePath}}" data-dir="in" data-kind="cpt">${{cptIn}}&nbsp;inbound</a>` +
    `</span>`;

  const markdown = node.preview || "_(empty file)_";
  if (window.marked) {{
    tooltipBody.innerHTML = marked.parse(markdown);
  }} else {{
    tooltipBody.textContent = markdown;
  }}

  tooltip.style.display = "block";
  tooltip.dataset.kind = "markdown";
  requestAnimationFrame(() => positionNodeTooltip(nodeId));
}}

tooltipTitle.addEventListener("click", e => {{
  const link = e.target.closest(".tt-link");
  if (!link) return;
  e.stopPropagation();
  const nid  = link.dataset.node;
  const dir  = link.dataset.dir;
  const kind = link.dataset.kind;
  if (kind === "file") {{
    showLinksDetailToast(nid, dir);
  }} else if (kind === "cpt") {{
    showCptDetailToast(nid, dir);
  }}
}});

function showNodePreview(nodeId) {{
  previewedNode = nodeId;
  selectedEdge = null;
  showNodeTooltip(nodeId);
}}

function selectNodeById(nodeId) {{
  selectedNode = nodeId;
  selectedEdge = null;
  setDetails(nodeId);

  const nodeFilter = "./" + nodeId;
  const query = normalized(searchInput.value);
  const depth = Math.max(0, parseInt(document.getElementById("filterDepth").value || "1", 10) || 0);
  const visibleSet = query
    ? computeFilterExpansion(matchingNodeIds(query), depth)
    : new Set(activeViewNodeIds);

  if (!filterFromNodeSelection && query) {{
    manualFilter = searchInput.value;
  }}

  searchInput.value = nodeFilter;
  filterFromNodeSelection = true;
  syncFilterDepthVisibility();
  updateStyles();
  network.selectNodes([nodeId], false);
}}

function directionalCandidate(anchor, candidate, direction) {{
  const dx = candidate.x - anchor.x;
  const dy = candidate.y - anchor.y;
  if (direction === "right" && dx <= 0) return null;
  if (direction === "left" && dx >= 0) return null;
  if (direction === "down" && dy <= 0) return null;
  if (direction === "up" && dy >= 0) return null;
  const primary = direction === "right" || direction === "left" ? Math.abs(dx) : Math.abs(dy);
  const secondary = direction === "right" || direction === "left" ? Math.abs(dy) : Math.abs(dx);
  const angle = Math.atan2(secondary, primary);
  const dist = Math.sqrt(dx * dx + dy * dy);
  return [angle, dist];
}}

function wraparoundCandidate(anchor, candidate, direction) {{
  const dx = candidate.x - anchor.x;
  const dy = candidate.y - anchor.y;
  const primary = direction === "right" || direction === "left" ? Math.abs(dx) : Math.abs(dy);
  const secondary = direction === "right" || direction === "left" ? Math.abs(dy) : Math.abs(dx);
  return [primary, secondary, dx * dx + dy * dy];
}}

function compareScore(score, bestScore) {{
  if (!bestScore) return true;
  const limit = Math.max(score.length, bestScore.length);
  for (let i = 0; i < limit; i += 1) {{
    const left = score[i] ?? 0;
    const right = bestScore[i] ?? 0;
    if (left < right) return true;
    if (left > right) return false;
  }}
  return false;
}}

function findBestDirectionalTarget(anchorId, direction, candidateIds, wrap = false) {{
  const positions = network.getPositions();
  const anchor = positions[anchorId];
  if (!anchor) return null;
  let bestId = null;
  let bestScore = null;
  for (const candidateId of candidateIds) {{
    if (candidateId === anchorId) continue;
    const candidate = positions[candidateId];
    if (!candidate) continue;
    const score = wrap ? wraparoundCandidate(anchor, candidate, direction) : directionalCandidate(anchor, candidate, direction);
    if (!score) continue;
    if (compareScore(score, bestScore)) {{
      bestId = candidateId;
      bestScore = score;
    }}
  }}
  return bestId;
}}

function filteredNodeIds() {{
  if (filterFromNodeSelection) return [...activeViewNodeIds];
  const query = normalized(searchInput.value);
  const direct = matchingNodeIds(query);
  if (!query) return [...direct];
  const depth = Math.max(0, Number.parseInt(document.getElementById("filterDepth").value || "1", 10) || 0);
  return [...computeFilterExpansion(direct, depth)];
}}

function nodeMeta(nodeId) {{
  return nodeById.get(nodeId) || null;
}}

function filteredByBucket(bucketId) {{
  return filteredNodeIds().filter(id => (nodeMeta(id)?.bucket || "") === bucketId);
}}

function filteredByCategory(categoryId) {{
  return filteredNodeIds().filter(id => (nodeMeta(id)?.category || "") === categoryId);
}}

function distinctBucketsInCategory(categoryId) {{
  const seen = new Set();
  const result = [];
  for (const id of filteredByCategory(categoryId)) {{
    const bucket = nodeMeta(id)?.bucket || "";
    if (!seen.has(bucket)) {{
      seen.add(bucket);
      result.push(bucket);
    }}
  }}
  return result;
}}

function distinctCategories() {{
  const seen = new Set();
  const result = [];
  for (const id of filteredNodeIds()) {{
    const category = nodeMeta(id)?.category || "";
    if (!seen.has(category)) {{
      seen.add(category);
      result.push(category);
    }}
  }}
  return result;
}}

function bucketBounds(bucketId) {{
  const rect = bucketRects[bucketId];
  if (!rect) return null;
  return {{ x: rect.x + rect.w / 2, y: rect.y + rect.h / 2 }};
}}

function categoryBounds(categoryId) {{
  const rect = categoryBands[categoryId];
  if (!rect) return null;
  return {{ x: rect.x + rect.w / 2, y: rect.y + rect.h / 2 }};
}}

function findTargetBucket(anchorBucketId, anchorCategoryId, direction) {{
  const anchor = bucketBounds(anchorBucketId);
  if (!anchor) return null;
  const bucketIds = distinctBucketsInCategory(anchorCategoryId).filter(id => id !== anchorBucketId && bucketRects[id]);
  let bestId = null;
  let bestScore = null;
  for (const bucketId of bucketIds) {{
    const candidate = bucketBounds(bucketId);
    if (!candidate) continue;
    const score = directionalCandidate(anchor, candidate, direction);
    if (!score) continue;
    if (compareScore(score, bestScore)) {{
      bestId = bucketId;
      bestScore = score;
    }}
  }}
  return bestId;
}}

function findTargetCategory(anchorCategoryId, direction) {{
  const anchor = categoryBounds(anchorCategoryId);
  if (!anchor) return null;
  const categoryIds = distinctCategories().filter(id => id !== anchorCategoryId && categoryBands[id]);
  let bestId = null;
  let bestScore = null;
  for (const categoryId of categoryIds) {{
    const candidate = categoryBounds(categoryId);
    if (!candidate) continue;
    const score = directionalCandidate(anchor, candidate, direction);
    if (!score) continue;
    if (compareScore(score, bestScore)) {{
      bestId = categoryId;
      bestScore = score;
    }}
  }}
  return bestId;
}}

function externalDirectionalScore(anchorId, anchorMeta, candidateId, direction, positions) {{
  const anchorNode = positions[anchorId];
  const candidateNode = positions[candidateId];
  if (!anchorNode || !candidateNode) return null;

  const candidateMeta = nodeMeta(candidateId);
  if (!candidateMeta) return null;

  const anchorCategoryId = anchorMeta.category || "";
  const candidateCategoryId = candidateMeta.category || "";
  if (candidateCategoryId === anchorCategoryId) return null;

  const anchorBucketId = anchorMeta.bucket || "";
  const candidateBucketId = candidateMeta.bucket || "";
  const anchorCategory = categoryBounds(anchorCategoryId);
  const candidateCategory = categoryBounds(candidateCategoryId);
  const anchorBucket = bucketBounds(anchorBucketId);
  const candidateBucket = bucketBounds(candidateBucketId);
  if (!anchorCategory || !candidateCategory || !anchorBucket || !candidateBucket) return null;

  const categoryScore = directionalCandidate(anchorCategory, candidateCategory, direction);
  if (!categoryScore) return null;
  const bucketScore = directionalCandidate(anchorBucket, candidateBucket, direction);
  if (!bucketScore) return null;
  const nodeScore = directionalCandidate(anchorNode, candidateNode, direction);
  if (!nodeScore) return null;
  return [...categoryScore, ...bucketScore, ...nodeScore];
}}

function findBestExternalDirectionalTarget(anchorId, anchorMeta, direction) {{
  const positions = network.getPositions();
  let bestId = null;
  let bestScore = null;
  for (const candidateId of filteredNodeIds()) {{
    if (candidateId === anchorId) continue;
    const score = externalDirectionalScore(anchorId, anchorMeta, candidateId, direction, positions);
    if (!score) continue;
    if (compareScore(score, bestScore)) {{
      bestId = candidateId;
      bestScore = score;
    }}
  }}
  return bestId;
}}

function wraparoundNode(anchorId, direction) {{
  return findBestDirectionalTarget(anchorId, direction, filteredNodeIds(), true);
}}

function moveNodeFocus(direction) {{
  const moveSelection = selectedNode !== null;
  const movePreview = previewedNode !== null && !tooltipIsEdge;
  const anchorId = previewedNode || selectedNode;
  if (!anchorId) return false;
  const anchorMeta = nodeMeta(anchorId);
  if (!anchorMeta) return false;

  let nextId = findBestDirectionalTarget(anchorId, direction, filteredNodeIds());
  if (!nextId) {{
    nextId = wraparoundNode(anchorId, direction);
  }}
  if (!nextId) return false;

  if (moveSelection) {{
    selectNodeById(nextId);
  }} else {{
    selectedEdge = null;
  }}

  if (movePreview) {{
    showNodePreview(nextId);
  }} else {{
    previewedNode = null;
    hideTooltip();
  }}

  drawMiniMap();
  return true;
}}

function showEdgeTooltip(edgeId) {{
  const edge = edgeById.get(edgeId);
  if (!edge) return;

  tooltipIsEdge = true;
  if (tooltipHideTimer) {{ clearTimeout(tooltipHideTimer); tooltipHideTimer = null; }}
  const fromName = String(edge.from).split('/').pop();
  const toName = String(edge.to).split('/').pop();
  tooltipTitle.textContent = `${{fromName}} \u2192 ${{toName}}`;
  if (edge.preview_html) {{
    tooltipBody.innerHTML = edge.preview_html;
  }} else {{
    const md = edge.preview || '_(no preview)_';
    tooltipBody.innerHTML = window.marked ? marked.parse(md) : md;
  }}
  tooltip.style.display = 'block';
  requestAnimationFrame(() => positionEdgeTooltip(edgeId));
}}

function hideTooltip() {{
  tooltip.style.display = "none";
  tooltipIsEdge = false;
  previewedNode = null;
  tooltip.dataset.kind = "";
}}

function isDetailToastOpen() {{
  return document.getElementById("linksDetailToast").style.display === "flex" ||
         document.getElementById("cptDetailToast").style.display === "flex" ||
         document.getElementById("edgeDetailToast").style.display === "flex";
}}

function scheduleHideTooltip() {{
  if (tooltipHideTimer) clearTimeout(tooltipHideTimer);
  tooltipHideTimer = setTimeout(() => {{
    if (tooltip.dataset.hover !== "1" && !isDetailToastOpen()) hideTooltip();
    tooltipHideTimer = null;
  }}, 90);
}}

tooltip.addEventListener("mouseenter", () => {{
  tooltip.dataset.hover = "1";
  if (tooltipHideTimer) {{
    clearTimeout(tooltipHideTimer);
    tooltipHideTimer = null;
  }}
}});

tooltip.addEventListener("mouseleave", () => {{
  tooltip.dataset.hover = "0";
  if (!tooltipIsEdge) scheduleHideTooltip();
}});

network.on("afterDrawing", function(ctx) {{
  const angle = 15 * Math.PI / 180;
  const positions = network.getPositions();

  ctx.save();
  ctx.font = "11px system-ui, sans-serif";
  ctx.textAlign = "left";
  ctx.textBaseline = "top";

  for (const [nodeId, pos] of Object.entries(positions)) {{
    const node = nodeById.get(nodeId);
    if (!node || !node.label) continue;

    const nodeState = nodes.get(nodeId);
    if (nodeState && nodeState.hidden) continue;
    const opacity = nodeState && nodeState.opacity != null ? nodeState.opacity : 1;
    ctx.fillStyle = `rgba(34,34,34,${{opacity}})`;

    ctx.save();
    ctx.translate(pos.x, pos.y + 22);
    ctx.rotate(angle);
    ctx.fillText(node.label, 0, 0);
    ctx.restore();
  }}

  ctx.restore();
}});

network.on("beforeDrawing", function(ctx) {{
  ctx.save();
  const viewModeActive = (viewSelect.value || "all") !== "all";

  for (const [id, band] of Object.entries(categoryBands)) {{
    if (!allCategoryIds.has(id)) continue;
    ctx.save();
    ctx.globalAlpha = viewModeActive && !activeCategoryIds.has(id) ? 0.12 : 1;
    ctx.fillStyle   = band.fill;
    ctx.strokeStyle = band.stroke;
    ctx.lineWidth   = 2;
    ctx.setLineDash([10, 5]);
    ctx.beginPath();
    ctx.roundRect(band.x, band.y, band.w, band.h, 18);
    ctx.fill();
    ctx.stroke();
    ctx.setLineDash([]);

    ctx.fillStyle = band.title_color;
    ctx.font = "bold 28px system-ui, sans-serif";
    ctx.fillText(band.label.toUpperCase(), band.x + 28, band.y + 44);

    ctx.strokeStyle = band.stroke;
    ctx.lineWidth   = 1;
    ctx.setLineDash([6, 4]);
    ctx.beginPath();
    ctx.moveTo(band.x + 28,          band.y + 56);
    ctx.lineTo(band.x + band.w - 28, band.y + 56);
    ctx.stroke();
    ctx.setLineDash([]);
    ctx.restore();
  }}

  for (const [id, rect] of Object.entries(bucketRects)) {{
    if (!allBucketIds.has(id)) continue;
    ctx.save();
    ctx.globalAlpha = viewModeActive && !activeBucketIds.has(id) ? 0.12 : 1;
    ctx.fillStyle   = "rgba(0, 0, 0, 0.03)";
    ctx.strokeStyle = "rgba(0, 0, 0, 0.15)";
    ctx.lineWidth   = 1;
    ctx.beginPath();
    ctx.roundRect(rect.x, rect.y, rect.w, rect.h, 14);
    ctx.fill();
    ctx.stroke();

    ctx.fillStyle = "rgba(0, 0, 0, 0.60)";
    ctx.font = "bold 13px system-ui, sans-serif";
    ctx.fillText(rect.label, rect.x + 12, rect.y + 22);
    ctx.restore();
  }}

  for (const [id, rect] of Object.entries(groupRects)) {{
    if (!allGroupIds.has(id)) continue;
    ctx.save();
    ctx.globalAlpha = viewModeActive && !activeGroupIds.has(id) ? 0.12 : 1;
    ctx.fillStyle = "rgba(0, 0, 0, 0.035)";
    ctx.strokeStyle = "rgba(0, 0, 0, 0.18)";
    ctx.lineWidth = 1;

    ctx.beginPath();
    ctx.roundRect(rect.x, rect.y, rect.w, rect.h, 18);
    ctx.fill();
    ctx.stroke();

    ctx.fillStyle = "rgba(0, 0, 0, 0.65)";
    ctx.font = "bold 22px system-ui, sans-serif";
    ctx.fillText(rect.label, rect.x + 20, rect.y + 36);
    ctx.restore();
  }}

  ctx.restore();
}});

function clearHighlight() {{
  selectedNode = null;
  selectedEdge = null;
  hoveredNode = null;
  searchInput.value = "";
  filterFromNodeSelection = false;
  manualFilter = "";
  syncFilterDepthVisibility();
  hideTooltip();
  updateStyles();
  network.unselectAll();
  details.classList.add("hidden");
  detailsName.textContent = "None selected";
  detailsPath.textContent = "Click a node to inspect its full path.";
}}

network.on("hoverNode", params => {{
  hoveredNode = params.node;
  showNodePreview(params.node);
}});

network.on("blurNode", () => {{
  if (previewedNode === hoveredNode) previewedNode = null;
  hoveredNode = null;
  scheduleHideTooltip();
}});

network.on("click", params => {{
  if (params.nodes.length) {{
    const clicked = params.nodes[0];
    const _viewActive = (viewSelect.value || "all") !== "all";
    if (_viewActive && !activeViewNodeIds.has(clicked)) return;
    if (selectedNode === clicked) {{
      clearHighlight();
    }} else {{
      previewedNode = null;
      hideTooltip();
      selectNodeById(clicked);
    }}
    drawMiniMap();
  }} else if (params.edges.length) {{
    const clickedEdgeId = params.edges[0];
    const _clickedEdgeState = edges.get(clickedEdgeId);
    if (_clickedEdgeState && _clickedEdgeState.color && _clickedEdgeState.color.opacity != null && _clickedEdgeState.color.opacity < 1) {{
      clearHighlight();
      return;
    }}
    const isCptEdge = typeof clickedEdgeId === "string" && clickedEdgeId.startsWith("cpt-");
    if (isCptEdge) {{
      const cptEdge = cptEdgeById.get(clickedEdgeId);
      if (cptEdge) showEdgeDetailToast(cptEdge.from, cptEdge.to, "cpt");
    }} else {{
      const fileEdge = edgeById.get(clickedEdgeId);
      if (fileEdge) showEdgeDetailToast(fileEdge.from, fileEdge.to, "file");
    }}
  }}
}});

network.on("dragEnd", () => {{
  drawMiniMap();
  if (selectedEdge && tooltip.style.display === "block") positionEdgeTooltip(selectedEdge);
  else if (previewedNode) positionNodeTooltip(previewedNode);
}});

network.on("zoom", () => {{
  drawMiniMap();
  if (selectedEdge && tooltip.style.display === "block") positionEdgeTooltip(selectedEdge);
  else if (previewedNode) positionNodeTooltip(previewedNode);
}});

function zoomBy(multiplier) {{
  const scale = network.getScale() * multiplier;
  network.moveTo({{ scale }});
  drawMiniMap();
}}

searchInput.addEventListener("input", () => {{
  filterFromNodeSelection = false;
  const _val = normalized(searchInput.value);
  manualFilter = _val ? searchInput.value : "";
  if (!_val) {{
    clearHighlight();
  }} else {{
    syncFilterDepthVisibility();
    updateStyles();
  }}
  if (document.getElementById("searchResultsToast").style.display === "flex") {{
    syncSRTWithFilter();
  }}
}});

let ldtSort = {{ col: null, dir: "desc" }};
let ldtData = [];

function ldtGetVal(row, col) {{
  if (col === "path")  return row.id;
  if (col === "loc")   return row.loc || 0;
  if (col === "links") return row.links;
  return row.id;
}}

function renderLdtRows() {{
  let rows = [...ldtData];
  if (ldtSort.col) {{
    const dir = ldtSort.dir === "desc" ? -1 : 1;
    rows.sort((a, b) => {{
      const va = ldtGetVal(a, ldtSort.col);
      const vb = ldtGetVal(b, ldtSort.col);
      return (typeof va === "string" ? va.localeCompare(vb) : va - vb) * dir;
    }});
  }}
  const tbody = document.getElementById("ldtBody");
  tbody.innerHTML = "";
  for (const row of rows) {{
    const tr = document.createElement("tr");
    const pathTd = document.createElement("td");
    pathTd.textContent = row.id;
    pathTd.title = row.id;
    const locTd = document.createElement("td");
    locTd.className = "ldt-num";
    locTd.textContent = row.loc != null ? row.loc : "\u2014";
    const linksTd = document.createElement("td");
    linksTd.className = "ldt-num";
    linksTd.textContent = row.links;
    tr.addEventListener("click", () => {{
      document.getElementById("linksDetailToast").style.display = "none";
      document.getElementById("searchResultsToast").style.display = "none";
      selectNodeById(row.id);
      network.focus(row.id, {{ scale: Math.max(network.getScale(), 1), animation: {{ duration: 400, easingFunction: "easeInOutQuad" }} }});
      drawMiniMap();
      setTimeout(() => showNodePreview(row.id), 450);
    }});
    tr.appendChild(pathTd);
    tr.appendChild(locTd);
    tr.appendChild(linksTd);
    tbody.appendChild(tr);
  }}
  document.querySelectorAll("#ldtTable th[data-col]").forEach(th => {{
    const col = th.dataset.col;
    const label = th.dataset.label;
    if (col === ldtSort.col) {{
      th.classList.add("ldt-th-active");
      th.textContent = label + (ldtSort.dir === "desc" ? " \u25bc" : " \u25b2");
    }} else {{
      th.classList.remove("ldt-th-active");
      th.textContent = label;
    }}
  }});
}}

function showLinksDetailToast(nodeId, dir) {{
  const adj = dir === "out" ? outboundAdjacency : inboundAdjacency;
  const links = adj.get(nodeId) || [];
  const fileCounts = new Map();
  for (const link of links) {{
    const fid = dir === "out" ? link.to : link.from;
    fileCounts.set(fid, (fileCounts.get(fid) || 0) + 1);
  }}
  ldtData = [...fileCounts.entries()].map(([id, cnt]) => {{
    const n = nodeById.get(id);
    return {{ id, loc: n ? n.loc : null, links: cnt }};
  }});
  ldtSort = {{ col: "links", dir: "desc" }};
  const node = nodeById.get(nodeId);
  const label = node ? node.label : nodeId;
  const dirLabel = dir === "out" ? "Outbound" : "Inbound";
  document.getElementById("ldtTitle").textContent = `${{dirLabel}} file links`;
  document.getElementById("ldtSubtitle").textContent = label;
  document.getElementById("ldtCount").textContent = `${{ldtData.length}} linked file${{ldtData.length !== 1 ? "s" : ""}}`;
  renderLdtRows();
  document.getElementById("linksDetailToast").style.display = "flex";
}}

document.getElementById("ldtClose").addEventListener("click", () => {{
  document.getElementById("linksDetailToast").style.display = "none";
  scheduleHideTooltip();
}});

document.querySelectorAll("#ldtTable th[data-col]").forEach(th => {{
  th.addEventListener("click", () => {{
    const col = th.dataset.col;
    if (ldtSort.col === col) {{
      ldtSort.dir = ldtSort.dir === "desc" ? "asc" : "desc";
    }} else {{
      ldtSort = {{ col, dir: "desc" }};
    }}
    renderLdtRows();
  }});
}});

function highlightCptInSnippet(snippet, cptId) {{
  const escaped = escapeHtml(snippet);
  const safeId = escapeHtml(cptId);
  return escaped.split('`' + safeId + '`').join('<mark class="cpt-hl">`' + safeId + '`</mark>');
}}

function showCptDetailToast(nodeId, dir) {{
  const adj = dir === "out" ? cptOutboundAdj : cptInboundAdj;
  const entries = adj.get(nodeId) || [];
  const node = nodeById.get(nodeId);
  const label = node ? node.label : nodeId;
  const dirLabel = dir === "out" ? "Outbound" : "Inbound";
  document.getElementById("cdtTitle").textContent = `${{dirLabel}} CPT references`;
  document.getElementById("cdtSubtitle").textContent = label;

  let totalRefs = 0;
  const container = document.getElementById("cdtCards");
  container.innerHTML = "";

  for (const entry of entries) {{
    const peerId = dir === "out" ? entry.to : entry.from;
    const refs   = entry.refs || [];
    totalRefs += refs.length;

    const card = document.createElement("div");
    card.className = "cpt-card";

    const header = document.createElement("div");
    header.className = "cpt-card-header";
    header.textContent = (dir === "out" ? "\u2192 " : "\u2190 ") + peerId;
    card.appendChild(header);

    const refsDiv = document.createElement("div");
    refsDiv.className = "cpt-card-refs";
    for (const ref of refs) {{
      const row = document.createElement("div");
      row.className = "cpt-ref-row";

      const idSpan = document.createElement("div");
      idSpan.className = "cpt-ref-id";
      idSpan.textContent = "`" + ref.cpt_id + "`";
      row.appendChild(idSpan);

      if (ref.snippet) {{
        const snip = document.createElement("div");
        snip.className = "cpt-ref-snippet";
        snip.innerHTML = highlightCptInSnippet(ref.snippet, ref.cpt_id);
        row.appendChild(snip);
      }}
      refsDiv.appendChild(row);
    }}
    card.appendChild(refsDiv);
    container.appendChild(card);
  }}

  document.getElementById("cdtCount").textContent =
    `${{entries.length}} file${{entries.length !== 1 ? "s" : ""}}, ${{totalRefs}} CPT reference${{totalRefs !== 1 ? "s" : ""}}`;
  document.getElementById("cptDetailToast").style.display = "flex";
}}

document.getElementById("cdtClose").addEventListener("click", () => {{
  document.getElementById("cptDetailToast").style.display = "none";
  scheduleHideTooltip();
}});

let edtFrom = "";
let edtTo = "";
let edtFileEdges = [];
let edtCptRefs = [];

function showEdgeDetailToast(from, to, preferTab) {{
  const key = from + "\x00" + to;
  edtFrom = from;
  edtTo = to;
  edtFileEdges = pairFileEdges.get(key) || [];
  const cptEdge = pairCptEdge.get(key);
  edtCptRefs = cptEdge ? (cptEdge.refs || []) : [];

  const fromName = String(from).split("/").pop() || from;
  const toName   = String(to).split("/").pop()   || to;
  document.getElementById("edtTitle").textContent    = fromName + " \u2192 " + toName;
  document.getElementById("edtSubtitle").textContent = from + " \u2192 " + to;

  const tabFile = document.getElementById("edtTabFile");
  const tabCpt  = document.getElementById("edtTabCpt");
  const hasFile = edtFileEdges.length > 0;
  const hasCpt  = edtCptRefs.length > 0;

  tabFile.textContent = "File links (" + edtFileEdges.length + ")";
  tabCpt.textContent  = "CPT links ("  + edtCptRefs.length  + ")";
  tabFile.disabled = !hasFile;
  tabCpt.disabled  = !hasCpt;

  let activeTab = preferTab;
  if (activeTab === "file" && !hasFile) activeTab = hasCpt ? "cpt" : "file";
  if (activeTab === "cpt"  && !hasCpt)  activeTab = hasFile ? "file" : "cpt";

  renderEdtTab(activeTab);
  document.getElementById("edgeDetailToast").style.display = "flex";
}}

function renderEdtTab(tab) {{
  document.getElementById("edtTabFile").classList.toggle("edt-tab-active", tab === "file");
  document.getElementById("edtTabCpt").classList.toggle("edt-tab-active",  tab === "cpt");
  document.getElementById("edtFilePanel").style.display = tab === "file" ? "flex" : "none";
  document.getElementById("edtCptPanel").style.display  = tab === "cpt"  ? "flex" : "none";
  if (tab === "file") renderEdtFileTab();
  else                renderEdtCptTab();
}}

function renderEdtFileTab() {{
  const sel       = document.getElementById("edtFileSelect");
  const selRow    = document.getElementById("edtFileSelectRow");
  const snippetEl = document.getElementById("edtFileSnippet");

  if (edtFileEdges.length <= 1) {{
    selRow.style.display = "none";
  }} else {{
    selRow.style.display = "block";
    sel.innerHTML = "";
    edtFileEdges.forEach((edge, i) => {{
      const opt = document.createElement("option");
      opt.value = i;
      const lm = edge.preview_html ? edge.preview_html.match(/snip-match[^>]*>.*?snip-ln[^>]*>(\\d+)/s) : null;
      opt.textContent = "Link " + (i + 1) + (lm ? " \u2014 line " + lm[1] : "");
      sel.appendChild(opt);
    }});
    sel.onchange = () => renderEdtFileSnippet(parseInt(sel.value, 10));
  }}
  renderEdtFileSnippet(0);
}}

function renderEdtFileSnippet(idx) {{
  const snippetEl = document.getElementById("edtFileSnippet");
  const edge = edtFileEdges[idx];
  snippetEl.innerHTML = edge ? (edge.preview_html || "(no preview)") : "";
}}

function renderEdtCptTab() {{
  const sel    = document.getElementById("edtCptSelect");
  const selRow = document.getElementById("edtCptSelectRow");

  if (edtCptRefs.length <= 1) {{
    selRow.style.display = "none";
  }} else {{
    selRow.style.display = "block";
    sel.innerHTML = "";
    edtCptRefs.forEach((ref, i) => {{
      const opt = document.createElement("option");
      opt.value = i;
      opt.textContent = "`" + ref.cpt_id + "`" + (ref.ref_line ? " — line " + ref.ref_line : "");
      sel.appendChild(opt);
    }});
    sel.onchange = () => renderEdtCptRef(parseInt(sel.value, 10));
  }}
  renderEdtCptRef(0);
}}

function renderEdtCptRef(idx) {{
  const refSection = document.getElementById("edtCptRefSection");
  const defSection = document.getElementById("edtCptDefSection");
  const ref = edtCptRefs[idx];
  if (!ref) {{ refSection.innerHTML = ""; defSection.style.display = "none"; return; }}

  const refLineStr = ref.ref_line ? " - line " + ref.ref_line : "";
  refSection.innerHTML =
    '<div class="edt-section-header">Reference from ' + escapeHtml(edtFrom) + escapeHtml(refLineStr) + '</div>' +
    '<div class="edt-section-body">' + highlightCptInSnippet(ref.snippet || "", ref.cpt_id) + '</div>';

  if (ref.def_snippet) {{
    const defLineStr = ref.def_line ? " - line " + ref.def_line : "";
    defSection.innerHTML =
      '<div class="edt-section-header edt-def-header">Definition in ' + escapeHtml(edtTo) + escapeHtml(defLineStr) + '</div>' +
      '<div class="edt-section-body">' + highlightCptDefInSnippet(ref.def_snippet, ref.cpt_id) + '</div>';
    defSection.style.display = "block";
  }} else {{
    defSection.style.display = "none";
  }}
}}

function highlightCptDefInSnippet(snippet, cptId) {{
  const escaped = escapeHtml(snippet);
  const safeId  = escapeHtml(cptId);
  return escaped.split('`' + safeId + '`').join('<mark class="cpt-def-hl">`' + safeId + '`</mark>');
}}

document.getElementById("edtClose").addEventListener("click", () => {{
  document.getElementById("edgeDetailToast").style.display = "none";
  scheduleHideTooltip();
}});
document.getElementById("edtTabFile").addEventListener("click", () => {{
  if (!document.getElementById("edtTabFile").disabled) renderEdtTab("file");
}});
document.getElementById("edtTabCpt").addEventListener("click", () => {{
  if (!document.getElementById("edtTabCpt").disabled) renderEdtTab("cpt");
}});

document.addEventListener("keydown", e => {{
  if (e.shiftKey && ["ArrowRight", "ArrowLeft", "ArrowUp", "ArrowDown"].includes(e.key)) {{
    const directionMap = {{ ArrowRight: "right", ArrowLeft: "left", ArrowUp: "up", ArrowDown: "down" }};
    if (moveNodeFocus(directionMap[e.key])) {{
      e.preventDefault();
      return;
    }}
  }}
  if (e.key !== "Escape") return;
  if (document.getElementById("edgeDetailToast").style.display === "flex") {{
    document.getElementById("edgeDetailToast").style.display = "none";
    scheduleHideTooltip();
    e.stopPropagation();
  }} else if (document.getElementById("cptDetailToast").style.display === "flex") {{
    document.getElementById("cptDetailToast").style.display = "none";
    scheduleHideTooltip();
    e.stopPropagation();
  }} else if (document.getElementById("linksDetailToast").style.display === "flex") {{
    document.getElementById("linksDetailToast").style.display = "none";
    scheduleHideTooltip();
    e.stopPropagation();
  }} else if (tooltip.style.display === "block") {{
    hideTooltip();
  }} else if (document.getElementById("searchResultsToast").style.display === "flex") {{
    document.getElementById("searchResultsToast").style.display = "none";
  }} else if (filterFromNodeSelection && manualFilter) {{
    searchInput.value = manualFilter;
    filterFromNodeSelection = false;
    selectedNode = null;
    selectedEdge = null;
    network.unselectAll();
    details.classList.add("hidden");
    syncFilterDepthVisibility();
    updateStyles();
  }} else if (selectedNode || filterFromNodeSelection) {{
    clearHighlight();
  }} else if (searchInput.value) {{
    searchInput.value = "";
    manualFilter = "";
    updateStyles();
    searchInput.focus();
  }} else if ((viewSelect.value || "all") !== "all") {{
    const curDepth = Math.max(0, parseInt(viewDepth.value || "0", 10) || 0);
    if (curDepth > 0) {{
      viewDepth.value = String(curDepth - 1);
      applyViewState();
    }} else {{
      viewSelect.value = "all";
      syncViewDepthVisibility();
      applyViewState({{ fit: true }});
    }}
  }}
}});

function escapeHtml(s) {{
  return String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}}

let srtCurrentIds = [];
let srtCurrentQuery = "";
let srtSort = {{ col: null, dir: "desc" }};

function srtGetVal(id, col) {{
  const node = nodeById.get(id);
  if (col === "path") return id;
  if (col === "loc")  return (node && node.loc != null) ? node.loc : 0;
  if (col === "in")   return inLinkCount.get(id) || 0;
  if (col === "out")  return outLinkCount.get(id) || 0;
  return id;
}}

function renderSrtRows() {{
  const query = srtCurrentQuery;
  let ids = [...srtCurrentIds];
  if (srtSort.col) {{
    const dir = srtSort.dir === "desc" ? -1 : 1;
    ids.sort((a, b) => {{
      const va = srtGetVal(a, srtSort.col);
      const vb = srtGetVal(b, srtSort.col);
      return (typeof va === "string" ? va.localeCompare(vb) : (va - vb)) * dir;
    }});
  }}
  const tbody = document.getElementById("srtBody");
  tbody.innerHTML = "";
  for (const id of ids) {{
    const node = nodeById.get(id);
    if (!node) continue;
    const inboundCount = inLinkCount.get(id) || 0;
    const outboundCount = outLinkCount.get(id) || 0;
    const tr = document.createElement("tr");
    const pathTd = document.createElement("td");
    pathTd.className = "srt-path";
    pathTd.title = `File path: ${{id}}`;
    if (query) {{
      const lower = id.toLowerCase();
      let out = "";
      let i = 0;
      while (i < id.length) {{
        const pos = lower.indexOf(query, i);
        if (pos === -1) {{ out += escapeHtml(id.slice(i)); break; }}
        out += escapeHtml(id.slice(i, pos)) +
               "<mark>" + escapeHtml(id.slice(pos, pos + query.length)) + "</mark>";
        i = pos + query.length;
      }}
      pathTd.innerHTML = out;
    }} else {{
      pathTd.textContent = id;
    }}
    const locTd = document.createElement("td");
    locTd.className = "srt-num";
    locTd.textContent = node.loc != null ? node.loc : "\u2014";
    locTd.title = node.loc != null ? `Lines of text in file: ${{node.loc}}` : "Lines of text in file: unknown";
    const inTd = document.createElement("td");
    inTd.className = "srt-num";
    inTd.textContent = inboundCount;
    inTd.title = `Inbound links count: ${{inboundCount}}`;
    const outTd = document.createElement("td");
    outTd.className = "srt-num";
    outTd.textContent = outboundCount;
    outTd.title = `Outbound links count: ${{outboundCount}}`;
    tr.addEventListener("click", () => {{
      document.getElementById("searchResultsToast").style.display = "none";
      selectNodeById(id);
      network.focus(id, {{ scale: Math.max(network.getScale(), 1), animation: {{ duration: 400, easingFunction: "easeInOutQuad" }} }});
      drawMiniMap();
      setTimeout(() => showNodePreview(id), 450);
    }});
    tr.appendChild(pathTd);
    tr.appendChild(locTd);
    tr.appendChild(inTd);
    tr.appendChild(outTd);
    tbody.appendChild(tr);
  }}
  document.querySelectorAll("#searchResultsTable th[data-col]").forEach(th => {{
    const col = th.dataset.col;
    const label = th.dataset.label;
    if (col === srtSort.col) {{
      th.classList.add("srt-th-active");
      th.textContent = label + (srtSort.dir === "desc" ? " \u25bc" : " \u25b2");
    }} else {{
      th.classList.remove("srt-th-active");
      th.textContent = label;
    }}
  }});
}}

function syncSRTWithFilter() {{
  const query = normalized(searchInput.value);
  const matches = matchingNodeIds(query);
  srtCurrentIds = [...matches].sort();
  srtCurrentQuery = query;
  document.getElementById("srtTitle").textContent =
    matches.size + " file" + (matches.size !== 1 ? "s" : "") +
    (query ? ` matching \u201c${{query}}\u201d` : "");
  renderSrtRows();
}}

function openSRT() {{
  syncSRTWithFilter();
  document.getElementById("searchResultsToast").style.display = "flex";
}}

document.getElementById("filterCount").addEventListener("click", openSRT);
document.getElementById("filterLOC").addEventListener("click", openSRT);

document.getElementById("srtClose").addEventListener("click", () => {{
  document.getElementById("searchResultsToast").style.display = "none";
}});

document.querySelectorAll("#searchResultsTable th[data-col]").forEach(th => {{
  th.addEventListener("click", () => {{
    const col = th.dataset.col;
    if (srtSort.col === col) {{
      srtSort.dir = srtSort.dir === "desc" ? "asc" : "desc";
    }} else {{
      srtSort = {{ col, dir: "desc" }};
    }}
    renderSrtRows();
  }});
}});

document.getElementById("searchForm").addEventListener("submit", event => {{
  event.preventDefault();
}});

attachControlTooltip(
  viewSelect,
  "View selector",
  `Choose how to **trace the repository from an agent or IDE perspective**.

- **All files view** shows the full markdown graph equally.
- An agent or IDE view keeps the full graph visible, but emphasizes the files and links reachable from that view's configured entry files.`
);
suppressControlTooltipOnInteract(viewSelect);
attachControlTooltip(
  document.getElementById("referenceTypeField"),
  "Reference type",
  `Controls which **link types** are shown on the graph.

- **File reference**: solid arrows for direct markdown file links.
- **CPT ID reference**: dashed purple arrows for CPT ID references (\\`cpt-*\\`) between files.
- **File & CPT ID reference**: both types; if a direct file link already covers the same pair, the CPT arrow is hidden.`
);
suppressControlTooltipOnInteract(referenceType);
attachControlTooltip(
  viewDepthField,
  "Link Depth",
  `Set how many **outbound markdown references** to follow from the selected view's entry files.

- **0** keeps only the starting files emphasized.
- Larger values expand the emphasized trace deeper into the graph.`
);

populateViewControls();
viewSelect.addEventListener("change", () => {{
  const selectedView = compiledViewsById.get(viewSelect.value || "");
  syncViewDepthVisibility();
  if (selectedView) {{
    viewDepth.value = String(selectedView.default_depth ?? 5);
  }}
  applyViewState({{ fit: true }});
}});
viewDepth.addEventListener("input", () => {{
  if (viewSelect.value === "all") return;
  applyViewState();
}});
document.getElementById("filterDepth").addEventListener("input", () => {{
  updateStyles();
}});
referenceType.addEventListener("change", () => {{
  updateStyles();
}});
applyViewState({{ fit: true }});

function syncViewDepthVisibility() {{
  const isAllView = viewSelect.value === "all";
  viewDepth.disabled = isAllView;
  viewDepth.classList.toggle("hidden", isAllView);
  viewDepthField.classList.toggle("hidden", isAllView);
  viewControls.classList.toggle("all-files", isAllView);
  referenceType.disabled = !isAllView;
  if (!isAllView) referenceType.value = "file";
}}

let mmState = {{ minX: 0, minY: 0, s: 1, pad: 12 }};

function drawMiniMap() {{
  const canvas = document.getElementById("minimap");
  const ctx = canvas.getContext("2d");
  ctx.clearRect(0, 0, canvas.width, canvas.height);

  const positions = network.getPositions();
  const values = Object.entries(positions)
    .filter(([nodeId]) => activeViewNodeIds.has(nodeId))
    .map(([, pos]) => pos);
  if (!values.length) return;

  const xs = values.map(p => p.x);
  const ys = values.map(p => p.y);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const minY = Math.min(...ys);
  const maxY = Math.max(...ys);

  const pad = 12;
  const sx = (canvas.width - pad * 2) / Math.max(1, maxX - minX);
  const sy = (canvas.height - pad * 2) / Math.max(1, maxY - minY);
  const s = Math.min(sx, sy);
  mmState = {{ minX, minY, s, pad }};

  ctx.fillStyle = "#555";
  for (const [id, p] of Object.entries(positions)) {{
    const x = pad + (p.x - minX) * s;
    const y = pad + (p.y - minY) * s;
    ctx.beginPath();
    ctx.arc(x, y, id === selectedNode ? 4 : 2, 0, Math.PI * 2);
    ctx.fill();
  }}

  const viewPos = network.getViewPosition();
  const scale = network.getScale();
  const vw = container.clientWidth / scale;
  const vh = container.clientHeight / scale;
  const vpLeft = pad + (viewPos.x - vw / 2 - minX) * s;
  const vpTop  = pad + (viewPos.y - vh / 2 - minY) * s;
  const vpW = vw * s;
  const vpH = vh * s;
  ctx.strokeStyle = "rgba(0,100,200,0.75)";
  ctx.lineWidth = 1.5;
  ctx.strokeRect(vpLeft, vpTop, vpW, vpH);
  ctx.fillStyle = "rgba(0,100,200,0.07)";
  ctx.fillRect(vpLeft, vpTop, vpW, vpH);
}}

setInterval(drawMiniMap, 1000);

const minimapEl = document.getElementById("minimap");
let minimapDrag = false;
function minimapMoveTo(e) {{
  const rect = minimapEl.getBoundingClientRect();
  const {{ minX, minY, s, pad }} = mmState;
  const graphX = (e.clientX - rect.left - pad) / s + minX;
  const graphY = (e.clientY - rect.top  - pad) / s + minY;
  network.moveTo({{ position: {{ x: graphX, y: graphY }}, animation: false }});
  drawMiniMap();
}}
minimapEl.addEventListener("mousedown", e => {{ minimapDrag = true; minimapMoveTo(e); e.stopPropagation(); }});
document.addEventListener("mousemove", e => {{ if (minimapDrag) minimapMoveTo(e); }});
document.addEventListener("mouseup",   () => {{ minimapDrag = false; }});

const panelContent = document.getElementById("panelContent");
const panelToggle = document.getElementById("panelToggle");
let panelCollapsed = false;
panelToggle.addEventListener("click", e => {{
  e.stopPropagation();
  panelCollapsed = !panelCollapsed;
  panel.classList.toggle("collapsed", panelCollapsed);
  panelContent.style.display = panelCollapsed ? "none" : "grid";
  panelToggle.textContent = panelCollapsed ? "▸" : "▾";
  panelToggle.title = panelCollapsed ? "Expand panel" : "Collapse panel";
}});

window.zoomBy = zoomBy;
window.network = network;
window.fitCurrentView = fitCurrentView;

}} // end initGraph

initGraph(window.__graphData.nodes, window.__graphData.edges, window.__graphData.cptEdges || []);
</script>
</body>
</html>
"""


def render_graph_data_js(graph_data: dict[str, Any], chunk_size: int = 4096) -> str:
    json_payload = json.dumps(graph_data, ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")
    chunks = [json_payload[idx:idx + chunk_size] for idx in range(0, len(json_payload), chunk_size)]
    joined_chunks = ",\n".join(f"  {json.dumps(chunk, ensure_ascii=False)}" for chunk in chunks)
    return "window.__graphData = JSON.parse([\n" + joined_chunks + "\n].join(\"\"));\n"


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate an interactive Markdown map and dependency graph.")
    parser.add_argument("--repo", default=".", help="Repository root.")
    parser.add_argument("--config", default=None, help="Optional TOML config path.")
    parser.add_argument("--out", default="md-fabric.html", help="Output HTML file.")
    parser.add_argument("--inline-data", action="store_true", help="Embed graph data into HTML instead of writing a separate JS file.")
    parser.add_argument("-v", "--verbose", action="store_true", help="Print layout optimization debug info.")
    args = parser.parse_args()

    repo = Path(args.repo).resolve()
    script_dir = Path(__file__).resolve().parent
    explicit_config = Path(args.config).expanduser().resolve() if args.config else None
    config_path = resolve_config_path(repo, script_dir, explicit_config)
    out_path = Path(args.out).resolve()

    groups, rules, skip_dirs, categories, views = load_config(config_path)
    template_vars = detect_template_vars(repo)
    files = scan_markdown(repo, groups, rules, skip_dirs, template_vars, categories or None)
    edges = extract_references(files)
    cpt_edges = extract_cpt_references(files)

    cat_bands: dict[str, dict[str, Any]] = {}
    bkt_rects: dict[str, dict[str, Any]] = {}
    if categories:
        nodes, bkt_rects, cat_bands = compute_category_layout(files, edges, categories, verbose=args.verbose)
        rects: dict[str, dict[str, Any]] = {}
    else:
        nodes, rects = build_nodes(files, groups, edges)

    graph_data = {"nodes": nodes, "edges": edges, "cptEdges": cpt_edges}
    js_content = render_graph_data_js(graph_data)
    js_path: Path | None = None
    if args.inline_data:
        data_script_tag = "<script>\n" + js_content + "</script>"
    else:
        js_filename = out_path.stem + ".js"
        js_path = out_path.with_name(js_filename)
        js_path.write_text(js_content, encoding="utf-8")
        data_script_tag = f'<script src="{js_filename}"></script>'

    html_output = render_html(nodes, edges, rects, data_script_tag, cat_bands, bkt_rects, views)
    out_path.write_text(html_output, encoding="utf-8")

    print(f"Config  : {config_path or '(none)'}")
    print(f"Mode    : {'categories' if categories else 'groups'}")
    print(f"Scanned : {len(files)} Markdown files.")
    print(f"Edges   : {len(edges)}")
    print(f"CPT Edges: {len(cpt_edges)}")
    print(f"Wrote   : {out_path}")
    if js_path is not None:
        print(f"Wrote   : {js_path} (graph data)")


if __name__ == "__main__":
    main()
