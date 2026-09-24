#!/usr/bin/env python3
"""Assert the self-referential invariants of a gear's design document set.

The cf-studio deterministic gate (`cfs validate`) checks artifact *form*: TOCs,
ID syntax, language, link targets. It is blind to a document set contradicting
itself — a decision whose propagation address was never edited, a count that no
longer matches what it counts, two slices registering one reason name, an
endpoint with no permission declaration.

Three remediation waves over `gears/bss/orders-lifecycle` produced 63 findings,
of which six were unapplied propagation claims and several were stale counts
introduced *by* an earlier wave. Every one of those classes is mechanical, which
is what this script exists to assert.

Usage:
    python3 scripts/check-design-invariants.py [GEAR_DOCS_DIR ...]

Defaults to every `gears/*/*/docs` directory that has a DECISIONS.md.
Exit status 1 if any invariant fails.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

TERMINAL_STATES = {"completed", "rejected", "cancelled", "fulfillment_failed", "expired"}
NON_TERMINAL_STATES = {
    "draft", "submitted", "pending_approval", "approved", "in_fulfillment", "on_hold",
}

WORD_NUMBERS = {
    "one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6,
    "seven": 7, "eight": 8, "nine": 9, "ten": 10, "eleven": 11,
    "twelve": 12, "thirteen": 13, "fourteen": 14, "fifteen": 15,
    "sixteen": 16, "seventeen": 17, "eighteen": 18, "nineteen": 19,
    "twenty": 20, "twenty-one": 21, "twenty-two": 22, "twenty-three": 23,
    "twenty-four": 24, "twenty-five": 25, "twenty-six": 26,
    "twenty-seven": 27, "thirty": 30, "fifty": 50,
    "fifty-seven": 57, "fifty-eight": 58, "fifty-nine": 59, "sixty": 60,
    "sixty-one": 61, "sixty-two": 62, "sixty-three": 63,
}

# Words too common to use as propagation evidence.
STOPWORDS = {
    "the", "a", "an", "and", "or", "is", "are", "be", "to", "of", "in", "on",
    "for", "it", "its", "that", "this", "with", "as", "by", "from", "at",
    "not", "no", "every", "each", "one", "own", "per", "so", "which", "than",
    "rather", "into", "where", "when", "what", "how", "any", "all", "both",
    "design", "slice", "order", "orders", "gear", "engine", "state", "row",
    "rows", "table", "section", "reason", "value", "values", "carries",
    "carry", "becomes", "become", "gains", "gain", "adds", "add", "added",
    "declared", "declare", "declares", "recorded", "record", "records",
}


@dataclass
class Result:
    failures: list[str] = field(default_factory=list)
    checks: int = 0

    def check(self, ok: bool, message: str) -> None:
        self.checks += 1
        if not ok:
            self.failures.append(message)


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.is_file() else ""


def strip_markup(text: str) -> str:
    """Reduce markdown to comparable words."""
    text = re.sub(r"`[^`]*`", " ", text)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = re.sub(r"[*_#|>]", " ", text)
    return text.lower()


def words(text: str) -> set[str]:
    """Tokenise for comparison, splitting hyphenated compounds.

    A decision may write "spawn-signal" where its target section writes "spawn
    signal"; without splitting, the two share no token and every such
    propagation claim fails spuriously.
    """
    out: set[str] = set()
    for token in re.findall(r"[a-z_][a-z_0-9-]{3,}", strip_markup(text)):
        out.add(token)
        out.update(part for part in token.split("-") if len(part) >= 4)
    return out


def sections(text: str) -> dict[str, str]:
    """Map a section number ('3.7', '4.2', '4') to its body.

    A parent section's body includes its descendants: an address of `§4` must
    match content that lives in `§4.2`, or every parent-level propagation
    address would fail spuriously.
    """
    out: dict[str, str] = {}
    lines = text.split("\n")
    starts = [
        (i, m.group(1))
        for i, line in enumerate(lines)
        if (m := re.match(r"^#{2,4}\s+(\d+(?:\.\d+)?)[.\s]", line))
    ]
    for idx, (line_no, number) in enumerate(starts):
        end = len(lines)
        for later_line, later_number in starts[idx + 1:]:
            # Stop at the next section that is not a descendant of this one.
            if not later_number.startswith(f"{number}."):
                end = later_line
                break
        out.setdefault(number, "")
        out[number] += "\n".join(lines[line_no:end])
    return out


# --------------------------------------------------------------------------
# 1. Propagation assertions
# --------------------------------------------------------------------------

DOC_ALIASES = {
    "DESIGN.md": "DESIGN.md",
    "DECISIONS.md": "DECISIONS.md",
    "UPSTREAM_REQS.md": "UPSTREAM_REQS.md",
    "design/README.md": "design/README.md",
}


def resolve_doc(token: str, docs: Path) -> Path | None:
    """Resolve a propagation address's document token to a file."""
    token = token.strip().strip("`")
    if token in DOC_ALIASES:
        return docs / DOC_ALIASES[token]
    if m := re.fullmatch(r"(\d{2})", token):
        # Guarded by the caller: bare digits are only a slice in document position.
        matches = sorted((docs / "design").glob(f"{m.group(1)}-*.md"))
        return matches[0] if matches else None
    if token.endswith(".md"):
        candidate = docs / token
        return candidate if candidate.is_file() else None
    return None


def check_propagation(docs: Path, res: Result) -> None:
    decisions = read(docs / "DECISIONS.md")
    if not decisions:
        return

    # Entry bodies: a narrative "### D-nn (S) title ... **Propagated**: ..." or a
    # table row "| D-nn | S | decision | rationale | targets |".
    entries: list[tuple[str, str, str]] = []  # (id, body, addresses)

    for m in re.finditer(
        r"^#{3}\s+((?:D|Q)-\d+)\b(.*?)(?=^#{2,3}\s|\Z)", decisions, re.M | re.S
    ):
        body = m.group(2)
        addr = ""
        if p := re.search(r"\*\*Propagated\*\*:\s*(.+?)(?:\n\n|\Z)", body, re.S):
            addr = p.group(1)
        entries.append((m.group(1), body, addr))

    for m in re.finditer(r"^\|\s*(D-\d+)\s*\|(.+)$", decisions, re.M):
        cells = [c.strip() for c in m.group(2).split("|")]
        if len(cells) >= 4:
            entries.append((m.group(1), " ".join(cells[:-1]), cells[-1]))

    for did, body, addresses in entries:
        if not addresses.strip():
            continue
        body_words = {w for w in words(body) if w not in STOPWORDS}
        # Addresses look like: `01 §3.7`, `§4.2`, `DESIGN.md §3.3`, `design/README.md`
        current_doc: Path | None = None
        for chunk in re.split(r"[;,]", addresses):
            chunk = chunk.strip()
            if not chunk:
                continue
            # `steps 6, 8, 12` are step numbers, not slice prefixes.
            if re.match(r"steps?\b", chunk, re.I):
                doc_token = None
            else:
                doc_token = re.match(
                    r"`?([A-Za-z0-9_/.]+\.md|\d{2})`?(?=\s*(?:§|`|$))", chunk
                )
            if doc_token:
                raw = doc_token.group(1)
                resolved = resolve_doc(raw, docs)
                if resolved is None:
                    # A bare two-digit token that matches no slice file is a step
                    # or item number carried in the address prose, not a document.
                    if raw.endswith(".md"):
                        res.check(
                            False,
                            f"{did}: propagation address names an unresolvable "
                            f"document '{raw}'",
                        )
                    continue
                current_doc = resolved
            if current_doc is None:
                continue
            res.check(
                current_doc.is_file(),
                f"{did}: propagation target {current_doc.name} does not exist",
            )
            if not current_doc.is_file():
                continue
            secs = sections(read(current_doc))
            for section_no in re.findall(r"§(\d+(?:\.\d+)?)", chunk):
                if section_no not in secs:
                    res.check(
                        False,
                        f"{did}: propagation address {current_doc.name} §{section_no} "
                        f"names a section that does not exist",
                    )
                    continue
                target_words = words(secs[section_no])
                overlap = body_words & target_words
                res.check(
                    bool(overlap),
                    f"{did}: propagation claims {current_doc.name} §{section_no} "
                    f"but that section shares no distinctive term with the decision "
                    f"— the edit may never have been applied",
                )


# --------------------------------------------------------------------------
# 2. Count assertions
# --------------------------------------------------------------------------

def stated_number(text: str, pattern: str) -> int | None:
    """Find a number stated as a word or digits near a phrase."""
    if m := re.search(pattern, text, re.I):
        token = m.group("n").lower().replace("‑", "-")
        if token.isdigit():
            return int(token)
        return WORD_NUMBERS.get(token)
    return None


def check_counts(docs: Path, res: Result) -> None:
    design = read(docs / "DESIGN.md")
    decisions = read(docs / "DECISIONS.md")
    foundation = next(iter(sorted((docs / "design").glob("01-*.md"))), None)
    core = read(foundation) if foundation else ""

    # Decisions and open questions.
    actual_d = len(
        set(re.findall(r"^#{3}\s+(D-\d+)", decisions, re.M))
        | set(re.findall(r"^\|\s*(D-\d+)\s*\|", decisions, re.M))
    )
    actual_q = len(set(re.findall(r"^\|\s*(Q-\d+)\s*\|", decisions, re.M)))
    stated_d = stated_number(design, r"(?P<n>[a-z-]+|\d+)\s+entries plus")
    stated_q = stated_number(design, r"entries plus (?P<n>[a-z-]+|\d+)\s+routed")
    if stated_d is not None:
        res.check(
            stated_d == actual_d,
            f"DESIGN.md states {stated_d} decision entries; DECISIONS.md has {actual_d}",
        )
    if stated_q is not None:
        res.check(
            stated_q == actual_q,
            f"DESIGN.md states {stated_q} routed open questions; DECISIONS.md has {actual_q}",
        )

    # Transition rows.
    actual_rows = len(re.findall(r"^\s*\d+\.\s+\[[ x]\].*\*\*FROM\*\*", core, re.M))
    stated_rows = stated_number(core, r"\*\*Transitions\*\*[^\n]*?(?P<n>[a-z-]+|\d+)\s+rows")
    if stated_rows is not None:
        res.check(
            stated_rows == actual_rows,
            f"01-foundation states {stated_rows} transition rows; the table has {actual_rows}",
        )

    # Endpoint inventory.
    if "### 3.3" in design:
        api = sections(design).get("3.3", "")
        actual_eps = len(re.findall(r"^\|\s*`(?:GET|POST|PATCH|PUT|DELETE)`", api, re.M))
        stated_eps = stated_number(api, r"(?P<n>[a-z-]+|\d+)[- ]endpoints? against")
        if stated_eps is not None:
            res.check(
                stated_eps == actual_eps,
                f"DESIGN.md §3.3 states {stated_eps} endpoints; the table has {actual_eps}",
            )

    # Table inventory.
    if "### 3.7" in design:
        db = sections(design).get("3.7", "")
        actual_tables = len(re.findall(r"^\|\s*`orders_\w+`", db, re.M))
        stated_tables = stated_number(db, r"of the (?P<n>[a-z-]+|\d+)\s+are deliberately mutable")
        actual_mutable = len(re.findall(r"^\|\s*`orders_\w+`.*mutable", db, re.M | re.I))
        if stated_tables is not None:
            res.check(
                stated_tables == actual_tables,
                f"DESIGN.md §3.7 immutability sentence counts {stated_tables} tables; "
                f"the inventory has {actual_tables}",
            )
        stated_mut = stated_number(db, r"(?P<n>[a-z-]+|\d+)\s+of the [a-z-]+ are deliberately mutable")
        if stated_mut is not None:
            res.check(
                stated_mut == actual_mutable,
                f"DESIGN.md §3.7 states {stated_mut} mutable tables; "
                f"the inventory marks {actual_mutable}",
            )


# --------------------------------------------------------------------------
# 3. Reason-registry uniqueness and reachability
# --------------------------------------------------------------------------

def check_reasons(docs: Path, res: Result) -> None:
    owners: dict[str, list[str]] = {}
    for path in sorted((docs / "design").glob("*.md")):
        text = read(path)
        for m in re.finditer(
            r"\*\*Reasons contributed to the registry\*\*:\s*(.+?)(?:\n\n|\Z)", text, re.S
        ):
            listing = m.group(1)
            # Inline reuse clauses do not transfer ownership. Stop only that
            # semicolon-delimited clause, retaining later contributed reasons.
            listing = re.sub(r"\breuse\b[^;\n.]*[;.]?", "", listing, flags=re.I)
            # Stop at the first sentence that hands ownership elsewhere.
            listing = re.split(r"(?<=\.)\s+(?=[A-Z])", listing)[0]
            for name in re.findall(r"[a-z][a-z0-9]*(?:-[a-z0-9]+){1,}", listing):
                owners.setdefault(name, [])
                if path.name not in owners[name]:
                    owners[name].append(path.name)

    for name, files in sorted(owners.items()):
        res.check(
            len(files) == 1,
            f"reason '{name}' is registered by {len(files)} slices ({', '.join(files)}) "
            f"— one condition must have exactly one owner",
        )


# --------------------------------------------------------------------------
# 4. Endpoint ownership and permission exhaustiveness
# --------------------------------------------------------------------------

def check_endpoints(docs: Path, res: Result) -> None:
    design = read(docs / "DESIGN.md")
    api = sections(design).get("3.3", "")
    union = re.findall(
        r"^\|\s*`(GET|POST|PATCH|PUT|DELETE)`\s*\|\s*`([^`]+)`\s*\|\s*([^|]+)\|", api, re.M
    )
    if not union:
        return

    declared: dict[tuple[str, str], list[str]] = {}
    for path in sorted((docs / "design").glob("*.md")):
        if path.name == "README.md":
            continue
        text = read(path)
        contracts = sections(text).get("3.3", "")
        for method, route, desc in re.findall(
            r"^\|\s*`(GET|POST|PATCH|PUT|DELETE)`\s*\|\s*`([^`]+)`\s*\|([^|]*)\|",
            contracts,
            re.M,
        ):
            # A row that names another slice as surface owner is a delegation note.
            if re.search(r"surface owned by", desc, re.I):
                continue
            declared.setdefault((method, route), []).append(path.name)

    for method, route, owner in union:
        key = (method, route)
        claimants = declared.get(key, [])
        res.check(
            len(claimants) <= 1,
            f"endpoint {method} {route} is declared as owned by {len(claimants)} slices "
            f"({', '.join(claimants)}) — the union claims exactly one owner each",
        )
        # An engine-owned operator surface is declared as an interface in the
        # foundation slice rather than as a REST table row; accept either.
        if not claimants:
            tail = route.rstrip("/").split("/")[-1].strip("{}")
            foundation = next(iter(sorted((docs / "design").glob("01-*.md"))), None)
            core_text = read(foundation) if foundation else ""
            declared_as_interface = bool(
                tail and re.search(re.escape(tail).replace(r"\-", "-"), core_text)
            )
            res.check(
                declared_as_interface,
                f"endpoint {method} {route} appears in the DESIGN.md union with owner "
                f"'{owner.strip()}' but no slice declares it as a route or an interface",
            )



# --------------------------------------------------------------------------
# 5. Retired-term assertions
# --------------------------------------------------------------------------

def check_retired_terms(docs: Path, res: Result) -> None:
    """Assert a rename decision's retired vocabulary is actually gone.

    The propagation heuristic detects a target that lacks the decision's
    vocabulary. A *rename* fails the other way: the target still carries the
    term the decision retired. A decision makes that checkable by declaring

        **Retires**: archived, archival — except: WAL archiving

    and this check asserts each term is absent from every propagation target,
    honouring the `except:` allowances.
    """
    decisions = read(docs / "DECISIONS.md")
    if not decisions:
        return

    for m in re.finditer(
        r"^#{3}\s+((?:D|Q)-\d+)\b(.*?)(?=^#{2,3}\s|\Z)", decisions, re.M | re.S
    ):
        did, body = m.group(1), m.group(2)
        retires = re.search(r"\*\*Retires\*\*:\s*(.+?)(?:\n\n|\Z)", body, re.S)
        if not retires:
            continue
        spec = retires.group(1)
        allowed: list[str] = []
        if "except:" in spec:
            spec, tail = spec.split("except:", 1)
            allowed = [a.strip().lower() for a in re.split(r"[,;]", tail) if a.strip()]
        terms = [
            t.strip().strip("`*_.").lower()
            for t in re.split(r"[,;]|\bor\b", spec.replace("—", ""))
            if t.strip().strip("`*_.")
        ]

        addresses = ""
        if prop := re.search(r"\*\*Propagated\*\*:\s*(.+?)(?:\n\n|\Z)", body, re.S):
            addresses = prop.group(1)
        docs_seen: list[Path] = []
        for chunk in re.split(r"[;,]", addresses):
            token = re.match(r"`?([A-Za-z0-9_/.]+\.md|\d{2})`?(?=\s*(?:§|`|$))", chunk.strip())
            if token and not re.match(r"steps?\b", chunk.strip(), re.I):
                if (resolved := resolve_doc(token.group(1), docs)) is not None:
                    if resolved not in docs_seen:
                        docs_seen.append(resolved)

        for target in docs_seen:
            text = read(target)
            for line_no, line in enumerate(text.split("\n"), start=1):
                low = line.lower()
                if any(a and a in low for a in allowed):
                    continue
                for term in terms:
                    if term and re.search(rf"\b{re.escape(term)}\b", low):
                        res.check(
                            False,
                            f"{did} retired the term '{term}' but "
                            f"{target.name}:{line_no} still uses it",
                        )



# --------------------------------------------------------------------------
# 6. Step-citation resolution
# --------------------------------------------------------------------------

def algorithms(text: str, sec: str) -> dict[str, dict[int, str]]:
    """Map an algorithm name to its numbered steps, within one section.

    A `§3.6` in this set holds two or three numbered algorithms, so a bare
    "step N" is ambiguous no matter how the steps are numbered. Citations must
    name their algorithm.
    """
    m = re.search(rf"^#{{2,4}}\s+{re.escape(sec)}[.\s]", text, re.M)
    if not m:
        return {}
    body = text[m.end():]
    nxt = re.search(r"^#{2,4}\s+\d", body, re.M)
    body = body[:nxt.start()] if nxt else body

    out: dict[str, dict[int, str]] = {}
    marks = list(re.finditer(r"\*\*Algorithm:\s*([^*]+?)\*\*", body))
    for i, mk in enumerate(marks):
        end = marks[i + 1].start() if i + 1 < len(marks) else len(body)
        chunk = body[mk.end():end]
        steps = {
            int(sm.group(1)): sm.group(2)
            for sm in re.finditer(
                r"^(\d+)\.\s+\[[ x]\] - `p\d+` - (.+?) - `inst-", chunk, re.M
            )
        }
        out[mk.group(1).strip().lower()] = steps
    return out


def check_step_citations(docs: Path, res: Result) -> None:
    """Assert every step citation names an existing algorithm and step.

    A citation is `<doc> §<sec> *<Algorithm>* step <N>`. Without the algorithm
    name the reference is unresolvable when the section holds more than one,
    which every `§3.6` in this set does.
    """
    sources = [docs / "DECISIONS.md", docs / "DESIGN.md"]
    sources += sorted((docs / "ADR").glob("*.md"))
    sources += sorted((docs / "design").glob("*.md"))

    for src in sources:
        text = read(src)
        if not text:
            continue
        for line_no, line in enumerate(text.split("\n"), start=1):
            if not re.search(r"\bsteps?\s+\d", line):
                continue
            # `<doc>` §<sec> [*Algorithm*] step(s) N[, N][ and N]
            for m in re.finditer(
                r"`?([A-Za-z0-9_/.]+\.md|\d{2})`?\s*§(\d+(?:\.\d+)?)`?"
                r"(?P<mid>[^§]{0,80}?)"
                r"steps?\s+(?P<nums>\d+(?:\s*,\s*\d+)*(?:\s+and\s+\d+)?)",
                line,
            ):
                doc_token, sec, mid, nums = m.group(1), m.group(2), m.group("mid"), m.group("nums")
                target = resolve_doc(doc_token, docs)
                if target is None or not target.is_file():
                    continue
                target_text = read(target)
                # Another gear may cite steps under a different convention (no
                # `**Algorithm:` headings). This family only understands sets
                # that use that convention; skip the rest rather than invent a
                # finding about a document it cannot parse.
                if "**Algorithm:" not in target_text:
                    continue
                algos = algorithms(target_text, sec)
                if not algos:
                    res.check(
                        False,
                        f"{src.name}:{line_no}: step citation into {target.name} §{sec}, "
                        f"which contains no numbered algorithm",
                    )
                    continue

                named = [n for n in algos if n in mid.lower()]
                if not named:
                    if len(algos) > 1:
                        res.check(
                            False,
                            f"{src.name}:{line_no}: '{doc_token} §{sec} … step {nums}' is "
                            f"ambiguous — §{sec} holds {len(algos)} algorithms "
                            f"({', '.join(sorted(algos))}); the citation must name one",
                        )
                        continue
                    named = list(algos)

                steps = algos[named[0]]
                for raw in re.findall(r"\d+", nums):
                    res.check(
                        int(raw) in steps,
                        f"{src.name}:{line_no}: cites {target.name} §{sec} "
                        f"*{named[0]}* step {raw}, which does not exist "
                        f"(that algorithm has steps 1-{max(steps) if steps else 0})",
                    )



# --------------------------------------------------------------------------
# 7. Schema: column ownership, index validity, enum membership
# --------------------------------------------------------------------------

def block_tables(heading: str) -> list[str]:
    """Table names declared by one `#### Table:` heading.

    A heading may name more than one table sharing an identical shape, as in
    "orders_order_admin / orders_order_line_admin".
    """
    return [t.strip().strip("`") for t in heading.split("/") if t.strip()]


def schema(docs: Path) -> dict[str, dict[str, str]]:
    """Parse every `#### Table:` block in the set into {table: {column: type}}.

    Two block shapes exist: a markdown column table under `**Schema**:`, and a
    prose `**Schema**: a, b, c` list. Both are read.
    """
    tables: dict[str, dict[str, str]] = {}
    for path in sorted((docs / "design").glob("*.md")):
        text = read(path)
        blocks = list(re.finditer(r"^#### Table:\s*(.+?)\s*$", text, re.M))
        for i, blk in enumerate(blocks):
            end = blocks[i + 1].start() if i + 1 < len(blocks) else len(text)
            body = text[blk.end():end]
            nxt = re.search(r"^#{2,3}\s", body, re.M)
            body = body[:nxt.start()] if nxt else body
            cols: dict[str, str] = {}
            # markdown table rows: | column | type | description |
            for row in re.finditer(r"^\|\s*([^|]+?)\s*\|\s*([^|]+?)\s*\|", body, re.M):
                names, typ = row.group(1), row.group(2).strip()
                if set(names) <= set("-: ") or set(typ) <= set("-: "):
                    continue
                # A row may declare more than one column: "| order_id, version |"
                parsed = [n.strip("` ") for n in names.split(",")]
                if not parsed or parsed[0].lower() in {"column", "col"}:
                    continue
                for name in parsed:
                    if re.fullmatch(r"\w+", name):
                        cols[name] = typ
            # prose form: **Schema**: `a`, `b` (nullable ...), `c`
            if not cols:
                m = re.search(r"\*\*Schema\*\*:\s*(.+?)(?:\n\n|\Z)", body, re.S)
                if m:
                    for name in re.findall(r"`(\w+)`", m.group(1)):
                        cols[name] = "unspecified"
            if cols:
                for name in block_tables(blk.group(1)):
                    tables.setdefault(name, {}).update(cols)
    return tables


def check_column_ownership(docs: Path, res: Result) -> None:
    """Assert every `orders_<table>.<column>` reference names a real column."""
    tables = schema(docs)
    if not tables:
        return
    files = (
        sorted((docs / "design").glob("*.md"))
        + sorted((docs / "ADR").glob("*.md"))
        + [docs / "DESIGN.md", docs / "DECISIONS.md", docs / "UPSTREAM_REQS.md"]
    )
    for path in files:
        text = read(path)
        for m in re.finditer(r"`?(orders_\w+)\.(\w+)`?", text):
            table, column = m.group(1), m.group(2)
            if table not in tables:
                continue
            line = text.count("\n", 0, m.start()) + 1
            res.check(
                column in tables[table],
                f"{path.name}:{line}: references `{table}.{column}`, which is not a "
                f"column of that table (it has: {', '.join(sorted(tables[table]))})",
            )


def check_index_columns(docs: Path, res: Result) -> None:
    """Assert an index or constraint declared in a table block names only that
    table's columns.

    A single-table index cannot filter on another table's columns, so an index
    declared on the wrong table can never be created. The declaration appears as
    a backticked span inside the block's Constraints or Additional-info prose,
    introduced variously by "index", "indexed on", "partial UNIQUE", "PK" or
    "FK" — so the trigger is the span's *shape* (a parenthesised column list, or
    a WHERE clause) rather than any one keyword.
    """
    SQL = {
        "where", "in", "is", "not", "null", "nulls", "and", "or", "true", "false",
        "distinct", "asc", "desc", "on", "to", "default", "check", "unique",
        "primary", "key", "foreign", "references", "deferrable", "initially",
        "deferred", "partial", "index", "using", "btree", "gin", "coalesce",
        # upsert and savepoint vocabulary — a normative mechanism may now be
        # written inline in a constraints block, and its keywords are not columns
        "conflict", "do", "nothing", "returning", "savepoint", "rollback",
        "release", "insert", "select", "update", "delete", "values", "set",
        "exists", "with", "as", "from", "by", "order", "limit",
    }
    tables = schema(docs)
    for path in sorted((docs / "design").glob("*.md")):
        text = read(path)
        blocks = list(re.finditer(r"^#### Table:\s*(.+?)\s*$", text, re.M))
        for i, blk in enumerate(blocks):
            names = [n for n in block_tables(blk.group(1)) if n in tables]
            if not names:
                continue
            table = names[0]
            stop = blocks[i + 1].start() if i + 1 < len(blocks) else len(text)
            body = text[blk.end():stop]
            nxt = re.search(r"^#{2,3}\s", body, re.M)
            body = body[:nxt.start()] if nxt else body
            # Narrowing to `**PK**`/`**Constraints**`/`**Indexes**` paragraphs
            # was wrong: it dropped `orders_order`'s whole canonical index table,
            # the claim table's release index and the outbox's drain index, and
            # a wrong index in any of them passed clean. The real discriminator
            # is not the label but whether a paragraph is *declaring* on this
            # table or *discussing* another one — 07 §3.7 explains which dwell
            # input lives on `orders_order`, and that is a citation, not a
            # declaration. So keep every paragraph from `**PK**:` onward and drop
            # only those that name a different table.
            pk = re.search(r"\*\*PK\*\*:", body)
            body = body[pk.start():] if pk else ""
            others = set(tables) - {table}
            def declares_here(para: str) -> bool:
                # An FK names another table by definition, so it must not make
                # the paragraph look like a discussion of that table — the
                # claim table's Constraints line is `FK (order_id, version) to
                # orders_order_version`, and dropping it hid a real defect.
                probe = re.sub(r"\bFK\b[^;.]*", "", para)
                return not any(o in probe for o in others)
            body = "\n\n".join(
                para for para in re.split(r"\n\s*\n", body) if declares_here(para)
            )

            own = set(tables[table])
            for span in re.finditer(r"`([^`]+)`", body):
                blob = span.group(1)
                # Rust calls in transaction guidance are not declarations.
                # SQL column lists start with '(' or a SQL constraint keyword;
                # a referenced table's column list is checked against that table.
                target = re.fullmatch(r"(\w+)\(([^()]*)\)", blob)
                if target and target.group(1) in tables:
                    for column in re.findall(r"\b[a-z_][a-z_0-9]*\b", target.group(2)):
                        res.check(
                            column in tables[target.group(1)],
                            f"{path.name}: referenced table `{target.group(1)}` "
                            f"has no column `{column}`",
                        )
                    continue
                if not re.match(
                    r"\s*(?:\(|ON\s+CONFLICT\b|WHERE\b|CHECK\b|UNIQUE\b|PRIMARY\s+KEY\b|COALESCE\s*\()",
                    blob, re.I,
                ):
                    continue
                # A bare table reference (an FK target) names no column here.
                if blob in tables:
                    continue
                # A qualified `table.column` is checked against the table it
                # names. Skipping these was an evasion: the same wrong index
                # passed when written qualified and failed when written bare, so
                # detection depended on spelling.
                for qt, qc in re.findall(r"\b(\w+)\.(\w+)\b", blob):
                    if qt in tables:
                        res.check(
                            qc in tables[qt],
                            f"{path.name}: a declaration on `{table}` names "
                            f"`{qt}.{qc}`, which is not a column of `{qt}`",
                        )
                blob = re.sub(r"\b\w+\.\w+\b", "", blob)
                if "(" not in blob and not re.search(r"\bWHERE\b", blob, re.I):
                    continue
                stripped = re.sub(r"'[^']*'", " ", blob)          # drop literals
                idents = [
                    w for w in re.findall(r"[A-Za-z_][A-Za-z_0-9]*", stripped)
                    if w.lower() not in SQL and w not in tables
                ]
                if not idents:
                    continue
                line = text.count("\n", 0, blk.end() + span.start()) + 1
                for column in idents:
                    if column in own:
                        continue
                    holder = [t for t, c in tables.items() if column in c and t != table]
                    res.check(
                        False,
                        f"{path.name}:{line}: a constraint or index on `{table}` "
                        f"references column `{column}`, which belongs to "
                        f"{'`' + holder[0] + '`' if holder else 'no table in the set'} "
                        f"— a single-table index cannot filter on another "
                        f"table's columns",
                    )


def check_enum_membership(docs: Path, res: Result) -> None:
    """Assert a value written to an enum column is one the column declares."""
    tables = schema(docs)
    enums: dict[tuple[str, str], set[str]] = {}
    for path in sorted((docs / "design").glob("*.md")):
        text = read(path)
        blocks = list(re.finditer(r"^#### Table:\s*(.+?)\s*$", text, re.M))
        for i, blk in enumerate(blocks):
            end = blocks[i + 1].start() if i + 1 < len(blocks) else len(text)
            body = text[blk.end():end]
            for row in re.finditer(
                r"^\|\s*`?(\w+)`?\s*\|\s*enum[^|]*\|\s*([^|]+?)\s*\|", body, re.M | re.I
            ):
                values = set(re.findall(r"`(\w+)`", row.group(2)))
                if values:
                    for name in block_tables(blk.group(1)):
                        enums[(name, row.group(1))] = values

            # The engine slice declares columns as a table; the slices declare
            # them as a prose `**Schema**:` line, where an enumeration reads
            # ``outcome` (`served` or `refused`)`. Without this the enum family
            # sees only the engine's tables.
            for row in re.finditer(
                r"`(\w+)`\s*\(((?:`\w+`(?:\s*,\s*|\s+or\s+))+`\w+`)\)", body
            ):
                values = set(re.findall(r"`(\w+)`", row.group(2)))
                if values:
                    for name in block_tables(blk.group(1)):
                        enums.setdefault((name, row.group(1)), set()).update(values)

    # One column name may be declared on several tables with different value
    # sets — `outcome` is declared on the audit, idempotency and read-access-log
    # tables. An unqualified `outcome = served` names no table, so it can only
    # be judged against the union; asserting it against each table separately
    # fails every value that is valid on exactly one of them.
    union: dict[str, set[str]] = {}
    owners: dict[str, set[str]] = {}
    for (table, column), allowed in enums.items():
        union.setdefault(column, set()).update(allowed)
        owners.setdefault(column, set()).add(table)

    files = sorted((docs / "design").glob("*.md")) + [docs / "DESIGN.md"]
    for path in files:
        text = read(path)

        # Qualified: `table.column` = value — judged against that table alone.
        for (table, column), allowed in enums.items():
            pat = rf"`{re.escape(table)}\.{re.escape(column)}`?\s*=\s*`?(\w+)`?"
            for m in re.finditer(pat, text):
                value = m.group(1)
                line = text.count("\n", 0, m.start()) + 1
                res.check(
                    value in allowed,
                    f"{path.name}:{line}: assigns `{table}.{column} = {value}`, which is "
                    f"not a declared value of that column "
                    f"({', '.join(sorted(allowed))})",
                )

        # Unqualified: judged against the union of every table declaring it.
        for column, allowed in union.items():
            pat = rf"(?<![.\w])`{re.escape(column)}`?\s*=\s*`?(\w+)`?"
            for m in re.finditer(pat, text):
                value = m.group(1)
                line = text.count("\n", 0, m.start()) + 1
                where = ", ".join(f"`{t}`" for t in sorted(owners[column]))
                res.check(
                    value in allowed,
                    f"{path.name}:{line}: assigns `{column} = {value}`, which is not a "
                    f"declared value of `{column}` on any table declaring it "
                    f"({where}: {', '.join(sorted(allowed))})",
                )



# --------------------------------------------------------------------------
# 8. Prose enumerations, guard ordering, ADR reciprocity, version vocabulary
# --------------------------------------------------------------------------

def _numeric(token: str) -> int | None:
    token = token.lower().strip()
    return int(token) if token.isdigit() else WORD_NUMBERS.get(token)


def check_step_contiguity(docs: Path, res: Result) -> None:
    """Assert every algorithm's steps run 1..n with no gap.

    Review wave 5 found *Author Line* numbered 1, then 4-11: two steps had been
    merged into step 1 without renumbering the rest. Nothing caught it, because
    the step-citation family only resolves citations that exist, and no document
    happened to cite that algorithm. A gap makes every future citation into it
    unresolvable and hides whether steps were merged or lost.
    """
    for path in sorted((docs / "design").glob("*.md")):
        text = read(path)
        blocks = list(re.finditer(r"^\*\*Algorithm:\s*(.+?)\*\*\s*$", text, re.M))
        for i, blk in enumerate(blocks):
            end = blocks[i + 1].start() if i + 1 < len(blocks) else len(text)
            body = text[blk.end():end]
            # The body ends at its Description; beyond that lie §4's own
            # numbered lists, which are not steps of this algorithm.
            stop = re.search(r"^\*\*Description\*\*", body, re.M)
            if stop:
                body = body[:stop.start()]
            name = blk.group(1).strip()

            # Group by indent so a parent's sub-steps are judged as their own
            # run: "1." and "   1." are different levels, not one sequence.
            levels: dict[int, list[int]] = {}
            for m in re.finditer(r"^([ ]*)(\d+)\. \[ \]", body, re.M):
                levels.setdefault(len(m.group(1)), []).append(int(m.group(2)))

            for indent, numbers in sorted(levels.items()):
                if indent == 0:
                    runs = [numbers]
                else:
                    # A nested level restarts at 1 under each parent; split the
                    # sequence wherever it drops back to 1.
                    runs, current = [], []
                    for n in numbers:
                        if n == 1 and current:
                            runs.append(current)
                            current = []
                        current.append(n)
                    if current:
                        runs.append(current)
                for run in runs:
                    res.check(
                        run == list(range(1, len(run) + 1)),
                        f"{path.name}: *{name}* has a step-numbering gap at indent "
                        f"{indent}: {run} — steps must run 1..n so every citation "
                        f"into the algorithm resolves",
                    )


def check_no_pre_engine_refusals(docs: Path, res: Result) -> None:
    """Assert no slice algorithm refuses before it calls the engine.

    `01 §4.1` makes this a MUST and ADR-0005 explains why: a check that refuses
    ahead of the engine writes no audit row and settles no idempotency record,
    so it is exactly the silent drop the 100 %-audit NFR forbids. Review wave 5
    found nine such returns across five slices; the register entry that forbids
    them (D-09) had been in place for four waves. A refusal must be a declared
    guard the engine evaluates, or a contribution passed to the engine call.
    """
    engine_call = re.compile(
        r"[Rr]equest the .*transition|as the contribution to the .*transition",
    )
    refusal = re.compile(r"\*\*RETURN\*\*[^`]*refus")
    for path in sorted((docs / "design").glob("0[2-8]*.md")):
        text = read(path)
        blocks = list(re.finditer(r"^\*\*Algorithm:\s*(.+?)\*\*\s*$", text, re.M))
        for i, blk in enumerate(blocks):
            end = blocks[i + 1].start() if i + 1 < len(blocks) else len(text)
            body = text[blk.end():end]
            stop = re.search(r"^\*\*Description\*\*", body, re.M)
            if stop:
                body = body[:stop.start()]
            name = blk.group(1).strip()
            lines = body.split("\n")

            calls = [j for j, l in enumerate(lines) if engine_call.search(l)]
            if not calls:
                continue  # a read or a pure-computation algorithm
            first = min(calls)
            for j, line in enumerate(lines[:first]):
                if not refusal.search(line):
                    continue
                # The gate's own all-failures report is not a pre-engine
                # refusal: it is returned *after* contributing the outcome, so
                # the engine has audited and settled it. Those branches say so.
                preceding = "\n".join(lines[max(0, j - 3):j])
                if re.search(r"(?:Pass|Contribute).*engine.*(?:persists|audits|settles)", preceding, re.I):
                    continue
                if "the gate refuses" in line:
                    continue  # the gate's refusal already audited, in 03
                res.check(
                    False,
                    f"{path.name}: *{name}* returns a refusal at "
                    f"\"{line.strip()[:60]}\" before its engine call — `01 §4.1` "
                    f"forbids a slice refusing ahead of the engine, because such "
                    f"a refusal audits nothing and settles nothing; declare it "
                    f"as a guard instead",
                )
            res.check(True, f"{path.name}: *{name}* refuses only through the engine")


def check_prose_counts(docs: Path, res: Result) -> None:
    """Assert a count stated in prose matches the thing it counts.

    The headline-count family already compares a figure to its own inventory.
    This one catches the other half: a *second* statement of the same figure,
    in a summary or a responsibility scope, that was not updated with the first.
    Each entry is (phrase pattern, counter) — declarative so adding one is cheap.
    """
    core = next(iter(sorted((docs / "design").glob("01-*.md"))), None)
    gate = next(iter(sorted((docs / "design").glob("03-*.md"))), None)
    authz = next(iter(sorted((docs / "design").glob("08-*.md"))), None)
    seam = next(iter(sorted((docs / "design").glob("06-*.md"))), None)

    def deadline_ports() -> int | None:
        if not gate:
            return None
        body = sections(read(gate)).get("2.2", "")
        # A port row is "| <name> | <n> ms |". The total row carries a bolded
        # figure in seconds and must not be counted as a port.
        rows = re.findall(r"^\|\s*([^|*][^|]*?)\s*\|\s*(\d+)\s*ms\s*\|", body, re.M)
        return len(rows) or None

    def delta_predicates() -> int | None:
        if not gate:
            return None
        body = sections(read(gate)).get("4.2", "")
        return len(re.findall(r"^\d+\.\s+\*\*", body, re.M)) or None

    def matrix_rows() -> int | None:
        if not authz:
            return None
        body = sections(read(authz)).get("4.3", "")
        rows = re.findall(r"^\|\s*(?!Operation\b)([^|]+?)\s*\|", body, re.M)
        return len([r for r in rows if set(r) > set("-: ")]) or None

    def adr_files() -> int:
        return len(list((docs / "ADR").glob("*.md")))

    def artifacts() -> int:
        # PRD.md sits in this directory but is the requirements authority, not a
        # member of the design set the count refers to.
        top = [f for f in docs.glob("*.md") if f.name != "PRD.md"]
        return (
            len(top)
            + len(list((docs / "ADR").glob("*.md")))
            + len(list((docs / "design").glob("*.md")))
        )

    def workflow_ops() -> int | None:
        if not seam:
            return None
        body = sections(read(seam)).get("3.3", "")
        return len(re.findall(r"^\|\s*`POST`", body, re.M)) or None

    checks = [
        (r"(?P<n>[a-z-]+|\d+)\s+outbound (?:ports|operations)", deadline_ports, "outbound ports/operations"),
        (r"the (?P<n>[a-z-]+|\d+) together sit inside", deadline_ports, "ports in the budget"),
        (r"(?P<n>[a-z-]+|\d+)\s+delta predicates", delta_predicates, "delta predicates"),
        (r"(?P<n>[a-z-]+|\d+)\s+matrix rows", matrix_rows, "permission-matrix rows"),
        (r"(?P<n>[a-z-]+|\d+)\s+workflow-only operations", workflow_ops, "workflow-only operations"),
        (r"(?P<n>[a-z-]+|\d+)\s+ADRs", adr_files, "ADR files"),
        (r"that is (?P<n>[a-z-]+|\d+) artifacts", artifacts, "artifacts in the set"),
        (r"(?P<n>[a-z-]+|\d+)-artifact (?:gear )?set", artifacts, "artifacts in the set"),
    ]

    files = (
        sorted(docs.glob("*.md"))
        + sorted((docs / "ADR").glob("*.md"))
        + sorted((docs / "design").glob("*.md"))
    )
    for path in files:
        # Emphasis markers sit between the number and its noun ("**Five**
        # outbound ports"), so match against a flattened copy. Removing `**`
        # removes no newlines, so line numbers stay correct.
        text = read(path).replace("**", "")
        for pattern, counter, label in checks:
            actual = counter()
            if actual is None:
                continue
            for m in re.finditer(pattern, text, re.I):
                stated = _numeric(m.group("n"))
                if stated is None:
                    continue
                line = text.count("\n", 0, m.start()) + 1
                res.check(
                    stated == actual,
                    f"{path.name}:{line}: states {stated} {label}; the set has {actual}",
                )


def check_guard_ordering(docs: Path, res: Result) -> None:
    """Assert the transition algorithm's step order matches its normative sentence.

    Authorization must precede idempotency resolution, or a caller who supplies a
    matching key and fingerprint is served a stored outcome before being
    authorized. The order is stated twice — as a sentence in §4.1 and as step
    order in §3.6 — and the two must agree.
    """
    core = next(iter(sorted((docs / "design").glob("01-*.md"))), None)
    if not core:
        return
    text = read(core)

    algos = algorithms(text, "3.6")
    steps = next((v for k, v in algos.items() if "attempt transition" in k), None)
    if not steps:
        # A gear with no single-entry transition engine has nothing to order-check.
        # Only assert its absence where the set claims to have one.
        if re.search(r"Attempt Transition", text):
            res.check(
                False,
                "01-foundation names an *Attempt Transition* algorithm but §3.6 has no "
                "numbered steps for it to order-check",
            )
        return

    # Stable instruction IDs survive editorial changes to the prose. Restrict
    # matching to this algorithm and its top-level steps (not nested branches).
    body = re.split(r"\*\*Algorithm: Attempt Transition\*\*", text, maxsplit=1)[1]
    body = re.split(r"\*\*Algorithm:", body, maxsplit=1)[0]
    positions = {
        ident: int(number)
        for number, ident in re.findall(
            r"^(\d+)\.\s+\[[ x]\] - `p\d+` - .* - `(inst-[^`]+)`\s*$", body, re.M
        )
    }
    auth = positions.get("inst-probe-idempotency")
    idem = positions.get("inst-resolve-idempotency")
    txn = positions.get("inst-open-transaction")

    res.check(auth is not None, "01 §3.6 *Attempt Transition* has no authorization pre-guard step")
    res.check(idem is not None, "01 §3.6 *Attempt Transition* has no idempotency-resolution step")
    if auth is None or idem is None:
        return
    res.check(
        auth < idem,
        f"01 §3.6 *Attempt Transition*: authorization is step {auth} but idempotency "
        f"resolution is step {idem} — a caller matching a stored key would be served "
        f"before being authorized",
    )
    if txn is not None:
        res.check(
            auth < txn,
            f"01 §3.6 *Attempt Transition*: authorization is step {auth}, after the "
            f"transaction opens at step {txn}",
        )

    # And the normative sentence must state the same order. It may declare one
    # exception (D-110: workflow-class triggers check the version before
    # admissibility); when it does, the algorithm must implement that branch
    # ahead of the not-admissible refusal, or the sentence and steps disagree.
    m = re.search(
        r"Guard evaluation order is normative and total(, with one declared exception)?:"
        r"\s*\*\*(.+?)\*\*",
        text, re.S,
    )
    res.check(
        m is not None,
        "01 §4.1 no longer states the normative guard evaluation order the "
        "*Attempt Transition* steps are checked against",
    )
    if m and m.group(1) and re.search(r"workflow", m.group(2), re.I):
        wf = body.find("`inst-if-workflow-version-conflict`")
        na = body.find("`inst-if-not-admissible`")
        res.check(
            wf != -1 and na != -1 and wf < na,
            "01 §4.1 declares the workflow-trigger exception (version check before "
            "admissibility) but §3.6 *Attempt Transition* has no workflow version-check "
            "branch ahead of its not-admissible refusal",
        )
    if m:
        order = [w.strip() for w in re.split(r",|then", m.group(2)) if w.strip()]
        lowered = [w.lower() for w in order]
        ai = next((i for i, w in enumerate(lowered) if "authoriz" in w), None)
        ii = next((i for i, w in enumerate(lowered) if "idempot" in w), None)
        if ai is not None and ii is not None:
            res.check(
                ai < ii,
                f"01 §4.1's normative order lists idempotency before authorization, "
                f"contradicting §3.6's step order",
            )


def check_adr_reciprocity(docs: Path, res: Result) -> None:
    """Assert an ADR's claimed citations exist, both ways."""
    adr_dir = docs / "ADR"
    if not adr_dir.is_dir():
        return
    decisions = read(docs / "DECISIONS.md")
    design = read(docs / "DESIGN.md")

    for adr in sorted(adr_dir.glob("*.md")):
        text = read(adr)
        m = re.search(r"\*\*ID\*\*:\s*`([^`]+)`", text)
        if m:
            res.check(
                m.group(1) in design,
                f"{adr.name}: its ID `{m.group(1)}` is cited nowhere in DESIGN.md, so the "
                f"gate's Key ADRs table does not reach it",
            )
        # Reverse traceability: every document this ADR names as a design home
        # must cite the ADR back, or the trace resolves in one direction only.
        trace = re.search(r"^-\s*\*\*DESIGN\*\*:(.*?)(?=^-\s*\*\*|\Z)", text, re.M | re.S)
        if trace:
            named = set()
            for tok in re.findall(r"(DESIGN\.md|design/(?:README|\d{2}-[a-z-]+)\.md|README)", trace.group(1)):
                named.add("design/README.md" if tok == "README" else tok)
            for rel in sorted(named):
                target = docs / rel
                if not target.is_file():
                    continue
                target_text = read(target)
                # Accept the ADR's filename or its cpt ID; a prefix such as
                # "ADR/0005" is not enough, since it also matches a renamed file.
                adr_id = re.search(r"\*\*ID\*\*:\s*`([^`]+)`", text)
                cited = adr.name in target_text or (
                    adr_id is not None and adr_id.group(1) in target_text
                )
                res.check(
                    cited,
                    f"{adr.name} names {rel} as a design home, but {rel} never cites "
                    f"that ADR — the trace resolves in one direction only",
                )

        # "the N register entries this ADR consolidates (D-a, D-b) ... cite it"
        for claim in re.finditer(
            r"entries (?:this ADR consolidates|it consolidates)\s*\(([^)]*)\)[^.]*?cite it",
            text,
            re.S,
        ):
            for did in re.findall(r"D-\d+", claim.group(1)):
                body = re.search(
                    rf"^#{{3}}\s+{did}\b(.*?)(?=^#{{2,3}}\s|\Z)", decisions, re.M | re.S
                ) or re.search(rf"^\|\s*{did}\s*\|(.*)$", decisions, re.M)
                found = bool(body) and bool(
                    re.search(rf"{adr.stem[:4]}|{re.escape(adr.name)}", body.group(1))
                )
                res.check(
                    found,
                    f"{adr.name} claims {did} cites it; {did} contains no reference to "
                    f"that ADR",
                )


def check_version_vocabulary(docs: Path, res: Result) -> None:
    """Assert the version-appending row set agrees with every stated count."""
    core = next(iter(sorted((docs / "design").glob("01-*.md"))), None)
    if not core:
        return
    text = read(core)
    body = sections(text).get("4.3", "")
    versioning = re.findall(r"^\d+\.\s+\[[ x]\].*?\(versioning[,)]", body, re.M)
    actual = len(versioning)
    if not actual:
        return
    files = sorted((docs / "design").glob("*.md")) + [docs / "DECISIONS.md"]
    for path in files:
        t = read(path).replace("**", "")
        for m in re.finditer(
            r"(?:only\s+)?(?P<n>[a-z-]+|\d+)\s+transition rows append a version", t, re.I
        ):
            stated = _numeric(m.group("n"))
            if stated is None:
                continue
            line = t.count("\n", 0, m.start()) + 1
            res.check(
                stated == actual,
                f"{path.name}:{line}: states {stated} version-appending rows; §4.3 has {actual}",
            )



# --------------------------------------------------------------------------
# 9. Table-inventory membership
# --------------------------------------------------------------------------

def check_table_inventory(docs: Path, res: Result) -> None:
    """Assert the canonical inventory and the defined tables are the same set.

    Comparing only counts is not enough: one table missing from the inventory
    and one phantom row in it cancel out, and the count check passes. Membership
    must be asserted in both directions.
    """
    design = read(docs / "DESIGN.md")
    m = re.search(r"### 3\.7(.*?)(?=^###\s|\Z)", design, re.S | re.M)
    if not m:
        return
    inventory = set(re.findall(r"^\|\s*`(orders_\w+)`", m.group(1), re.M))
    if not inventory:
        return

    defined: dict[str, str] = {}
    for path in sorted((docs / "design").glob("*.md")):
        for blk in re.finditer(r"^#### Table:\s*(.+?)\s*$", read(path), re.M):
            for name in block_tables(blk.group(1)):
                defined[name] = path.name

    for table in sorted(defined.keys() - inventory):
        res.check(
            False,
            f"{defined[table]} defines table `{table}`, which is absent from "
            f"DESIGN.md §3.7's canonical inventory",
        )
    for table in sorted(inventory - defined.keys()):
        res.check(
            False,
            f"DESIGN.md §3.7 lists table `{table}`, which no slice defines with a "
            f"`#### Table:` block",
        )


def transition_rows(docs: Path) -> dict[int, tuple[set[str], set[str], str | None, str]]:
    """Parse the state-machine table into (sources, targets, event, raw) per row.

    A row's FROM may name one state, several, or "any non-terminal state"; a row
    may also carry two FROM/TO pairs (the expiry row does). Everything below
    reads the table through here, so a claim is compared with the table rather
    than with another sentence about the table.
    """
    text = read(docs / "design" / "01-foundation.md")
    rows: dict[int, tuple[set[str], set[str], str | None, str]] = {}
    for m in re.finditer(
        r"^(\d+)\. \[ \] - `p1` - \*\*FROM\*\* (.+?) - `inst-tr-", text, re.M
    ):
        body = m.group(2)
        sources: set[str] = set()
        for seg in re.finditer(
            r"\*\*FROM\*\* ((?:`\w+`(?:,| or|\s)*)+|any non-terminal state|nothing)",
            "**FROM** " + body,
        ):
            token = seg.group(1).strip()
            if "non-terminal" in token:
                sources |= NON_TERMINAL_STATES
            elif token != "nothing":
                sources |= set(re.findall(r"`(\w+)`", token))
        targets = set(re.findall(r"\*\*TO\*\* `(\w+)`", body))
        if "the same state" in body:
            targets |= sources
        event = re.search(r"`(Order\w+)`", body)
        rows[int(m.group(1))] = (
            sources, targets, event.group(1) if event else None, body,
        )
    return rows


def check_trigger_addressing(docs: Path, res: Result) -> None:
    """Assert every transition row is uniquely addressable by (from-state, trigger).

    `01 §3.6` step 10 looks up *the* row for `(current state, trigger)` before any
    guard runs, so two rows sharing a key leave the engine's choice undefined.
    Four pairs did — rows 7/8, 9/10, 13/14 and 15/16 — because the WHEN column
    described a guard condition instead of naming a trigger. D-12 had removed the
    same defect from the amendment rows; nothing stopped it elsewhere, and no
    family here could see it while triggers were unnamed prose.
    """
    text = read(docs / "design" / "01-foundation.md")
    rows = list(re.finditer(
        r"^(\d+)\. \[ \] - `p1` - \*\*FROM\*\* (.+?) - `inst-tr-", text, re.M
    ))
    if len(rows) < 20:
        return

    keys: dict[tuple[str, str], list[str]] = {}
    for m in rows:
        number, body = m.group(1), m.group(2)
        trigger = re.search(r"\*\*WHEN\*\* `([a-z-]+)`", body)
        res.check(
            trigger is not None,
            f"01 §4.3 row {number} does not name a backticked trigger in its WHEN "
            f"clause — the trigger is half the row's lookup key, so it must be a "
            f"name from §4.6's closed vocabulary and not a description",
        )
        if trigger is None:
            continue
        sources: set[str] = set()
        for seg in re.finditer(r"\*\*FROM\*\* (.+?)(?=\*\*TO\*\*)", "**FROM** " + body, re.S):
            token = seg.group(1)
            if "non-terminal" in token:
                sources |= NON_TERMINAL_STATES
            elif token.strip().startswith("nothing"):
                sources |= {"nothing"}   # row 1: creation has no prior state
            else:
                sources |= set(re.findall(r"`(\w+)`", token))
        for state in sources:
            keys.setdefault((state, trigger.group(1)), []).append(number)

    for (state, trigger), numbers in sorted(keys.items()):
        res.check(
            len(numbers) == 1,
            f"01 §4.3 rows {', '.join(numbers)} all match `({state}, {trigger})` — "
            f"§3.6 step 10 resolves one row per key before guards run, so the "
            f"engine cannot choose between them; give them distinct triggers",
        )

    # Every trigger used must be declared in §4.6's closed vocabulary.
    declared = set()
    if vocab := re.search(
        r"triggers are therefore named, not described: (.+?)\. Where one", text, re.S
    ):
        declared = set(re.findall(r"`([a-z-]+)`", vocab.group(1)))
    if declared:
        used = {t for _, t in keys}
        for trigger in sorted(used - declared):
            res.check(
                False,
                f"01 §4.3 uses the trigger `{trigger}`, which §4.6's closed "
                f"vocabulary does not declare",
            )
        for trigger in sorted(declared - used):
            res.check(
                False,
                f"01 §4.6 declares the trigger `{trigger}`, which no row in §4.3 "
                f"uses — the vocabulary is closed, so an unused name is drift",
            )
        res.check(True, "01 §4.6's trigger vocabulary matches §4.3's usage exactly")


def check_register_board(docs: Path, res: Result) -> None:
    """Assert the status board's open-question row matches the register.

    The board read "Q-01…Q-18 | 17 routed" while the register carried Q-01…Q-25
    with two closed. It survived three separate count edits in one session
    because each was a `str.replace` whose anchor no longer existed — a silent
    no-op that reported success. Nothing compared the board to the rows.
    """
    text = read(docs / "DECISIONS.md")
    if not text:
        return
    rows = re.findall(r"^\| (Q-(\d+)) \|(.*)$", text, re.M)
    if not rows:
        return
    highest = max(int(n) for _, n, _ in rows)
    closed = sum(1 for _, _, body in rows if re.search(r"\bclosed\b", body, re.I))

    board = re.search(r"^\| Open questions \| Q-01\u2026Q-(\d+) \|([^|]*)\|([^|]*)\|", text, re.M)
    res.check(
        board is not None,
        "DECISIONS.md: the status board has no `| Open questions | Q-01...Q-nn |` row "
        "to compare against the register",
    )
    if board is None:
        return
    res.check(
        int(board.group(1)) == highest,
        f"DECISIONS.md status board says the open questions run to "
        f"Q-{board.group(1)}, but the register's highest is Q-{highest:02d}",
    )
    stated = stated_number(board.group(3), r"(?P<n>[\w-]+) routed")
    if stated is not None:
        res.check(
            stated == len(rows) - closed,
            f"DECISIONS.md status board says {stated} routed and unanswered, but the "
            f"register has {len(rows)} Q rows of which {closed} are marked closed "
            f"({len(rows) - closed} routed)",
        )


def check_retention_executor(docs: Path, res: Result) -> None:
    """Assert every declared retention window has something that enforces it.

    `DESIGN.md` declared four bounded retentions and only the outbox had a purge.
    The audit table additionally forbids DELETE to its own role, so its window was
    not merely unimplemented but unimplementable — and ADR-0005 cites that window
    as the reason writing on every refusal is a bounded cost.
    """
    text = read(docs / "DESIGN.md")
    m = re.search(
        r"([A-Za-z]+) stores carry bounded retention by design[^:]*:(.+?)\.", text, re.S
    )
    # The declaration sentence gained a clause between "by design" and the
    # colon once, and this family silently stopped matching — contributing
    # zero assertions while still reporting green. `[^:]*` tolerates that.
    # Only a set that declares bounded retentions owes this sentence; pricing
    # and subscriptions declare none, and asserting it on them would block the
    # monorepo's docs CI on a claim they never made.
    if "bounded retention" in text:
        res.check(
            m is not None,
            "DESIGN.md declares bounded retention but no longer states how many "
            "stores carry it in a form this family can read; the retention "
            "assertions would silently contribute nothing",
        )
    if not m:
        return
    claimed = _numeric(m.group(1))
    windows = re.findall(r"\((\d+)\s*days?\)", m.group(2))
    res.check(
        claimed is not None and claimed == len(windows),
        f"DESIGN.md says {m.group(1)} stores carry bounded retention and lists "
        f"{len(windows)} windows",
    )
    body = "\n".join(
        read(p) for p in [docs / "DESIGN.md"] + sorted((docs / "design").glob("*.md"))
    )
    # Each retained store must be named by a sentence that also names a purge —
    # counting the bare word "purge" anywhere passes trivially and catches nothing.
    stores = {
        "outbox": r"outbox",
        "gate outcome": r"gate[- ]outcome",
        "audit": r"refus\w+[- ]attempt audit|audit rows",
        "read access log": r"read[- ]access[- ]log",
    }
    purge_sentences = [
        sent for sent in re.split(r"(?<=[.;])\s+", body)
        if re.search(r"\b[Pp]urge[sd]?\b", sent)
    ]
    for label, pattern in stores.items():
        if not re.search(pattern, m.group(2), re.I):
            continue          # this set does not declare a window for that store
        res.check(
            any(re.search(pattern, sent, re.I) for sent in purge_sentences),
            f"DESIGN.md declares a bounded retention for the {label} store, but no "
            f"sentence in the set names both that store and a purge — a retention "
            f"with no executor is an unbounded store, and ADR-0005 relies on the "
            f"refusal window to bound writing on every refusal",
        )


def check_settlement_claim(docs: Path, res: Result) -> None:
    """Assert the settlement count matches its own enumeration and its ADR.

    `01 §4.1` claimed "six of the seven also settle" and ADR-0005 claimed all
    seven "with one deliberate scoping caveat". The algorithm settles four. The
    number was stated twice, in two strengths, next to no list — so nothing
    could compare it to anything. Matching runs against whitespace-normalised
    text: the first attempt at this family anchored on ". The other" and silently
    matched nothing because the source wraps between "The" and "other".
    """
    flat = re.sub(r"\s+", " ", read(docs / "design" / "01-foundation.md"))
    m = re.search(
        r"\*\*([A-Za-z]+) of the seven also settle[^*]*\*\*\s*[—-]\s*([^.]+)\.", flat
    )
    if not m:
        return
    stated = _numeric(m.group(1))
    listed = len([x for x in re.split(r",| and ", m.group(2)) if x.strip()])
    res.check(
        stated is not None and stated == listed,
        f"01 §4.1 says {m.group(1)} of the seven refusal classes settle, then "
        f"enumerates {listed} ({m.group(2)}) — the count and its own list disagree",
    )

    adr = next(iter(sorted((docs / "ADR").glob("*refusals-commit.md"))), None)
    if adr is None:
        return
    adr_flat = re.sub(r"\s+", " ", read(adr))
    a = re.search(r"\*\*([A-Za-z]+) of the seven classes settle", adr_flat)
    res.check(
        a is not None,
        "ADR-0005 no longer states how many refusal classes settle; the count is "
        "the premise of its rejection of the audit-without-settle option",
    )
    if a is not None:
        res.check(
            _numeric(a.group(1)) == stated,
            f"ADR-0005 says {a.group(1)} of the seven refusal classes settle while "
            f"01 §4.1 says {m.group(1)} — the settlement contract must be stated "
            f"once and identically",
        )


def check_gts_claim_is_specified(docs: Path, res: Result) -> None:
    """Assert that naming GTS as a technology is backed by an actual GTS model.

    Four places named GTS as the domain layer's technology and the set contained
    no base type, no extension field, no registry client, no identifier
    ownership and no validation flow — `guidelines/GTS.md` §14 lists seven
    mandatory items for a DESIGN and six were absent. A technology label is
    cheap to write and invisible to every other family here, so it needs its own
    check: if the claim is made, the model must exist.
    """
    body = "\n".join(
        read(p) for p in [docs / "DESIGN.md"] + sorted((docs / "design").glob("*.md"))
    )
    # Gate on the set having *adopted* the model, not merely mentioned GTS.
    # `pricing` and `subscriptions` also name GTS as a technology without
    # specifying it — which is a finding for their own reviews, not something
    # this checker may assert on their behalf. Failing them here would block the
    # monorepo's docs CI on a judgement nobody reviewed, so an unadopted set is
    # NOT CHECKED rather than failed. Once a set claims a namespace, this family
    # holds it to the rest of §14.
    if not re.search(r"owns the namespace", body):
        return

    required = {
        "a base type identifier": r"gts\.[a-z0-9_]+(?:\.[a-z0-9_]+)+\.v\d+~",
        "the extension field": r"extension field",
        "an abstract or final rule": r"x-gts-abstract|x-gts-final",
        "the registry it resolves against": r"types-registry",
        "identifier-namespace ownership": r"owns the namespace|namespace [`']?[a-z]+[`']? inside",
        "a validation flow at the boundary": r"parse\*{0,2} the identifier|\*\*parse\*\*",
        "trait usage": r"x-gts-traits",
    }
    for label, pattern in required.items():
        res.check(
            re.search(pattern, body, re.I) is not None,
            f"the design set names GTS as a technology but specifies no {label} — "
            f"`guidelines/GTS.md` §14 requires a DESIGN to make the GTS model "
            f"explicit, and a bare technology label leaves each slice team to "
            f"invent its own answer",
        )

    # A claimed namespace must be the gear's own, not another package's.
    for m in re.finditer(r"gts\.cf\.([a-z0-9_]+)\.([a-z0-9_]+)\.", body):
        res.check(
            m.group(1) in {"core", "bss", "genai", "example"},
            f"the set declares an identifier under the package `{m.group(1)}`, "
            f"which is not one of the platform's allocated packages",
        )


def check_state_machine_claims(docs: Path, res: Result) -> None:
    """Assert prose claims about state sets against the transition table.

    This is the `Rc3-005` shape: a sentence enumerating states — which states a
    trigger is admitted from, which carry a TTL, which release a claim — sitting
    beside a table that is the actual authority, with nothing comparing them.
    That finding cost a permanently-stuck overlap claim on every rejected order,
    and the enumeration it got wrong looked entirely plausible.
    """
    rows = transition_rows(docs)
    if len(rows) < 20:
        return  # not this set's table shape

    # A terminal state is terminal: nothing may transition out of one.
    for n, (sources, _, _, _) in rows.items():
        for state in sorted(sources & TERMINAL_STATES):
            res.check(
                False,
                f"01 §4.3 row {n} originates in `{state}`, which §4.1 lists as a "
                f"terminal state — a terminal state admits no outgoing transition",
            )
        res.check(True, f"01 §4.3 row {n} does not originate in a terminal state")

    # 07 §4.4 makes the `in_fulfillment` expiry exemption *structural*: the
    # claim is that no such row exists, not that a guard refuses it.
    for n, (sources, targets, _, _) in rows.items():
        res.check(
            not ("in_fulfillment" in sources and "expired" in targets),
            f"01 §4.3 row {n} expires from `in_fulfillment`, which 07 §4.4 says "
            f"no row may do — the exemption is structural",
        )

    # Trigger admissibility: each of these prose sets must equal the table's.
    for label, marker, expected, cite in [
        ("amendment", "amendment", {"submitted", "pending_approval", "approved"}, "04 §4.1"),
        ("hold", "`hold` \u2014 storing", {"submitted", "pending_approval", "approved", "in_fulfillment"}, "07 §4.1"),
        ("per-state TTL", "per-state TTL elapses",
         {"submitted", "pending_approval", "approved", "on_hold"}, "07 §4.4"),
    ]:
        actual: set[str] = set()
        for sources, targets, _, body in rows.values():
            if marker in body:
                actual |= sources - TERMINAL_STATES if label == "per-state TTL" else sources
        res.check(
            actual == expected,
            f"01 §4.3 admits {label} from {sorted(actual)}, but {cite} says "
            f"{sorted(expected)} — the table and the prose disagree about which "
            f"states the trigger applies to",
        )

    # The event catalogue and the rows that declare an event must be a bijection.
    text = read(docs / "design" / "01-foundation.md")
    catalogue: dict[str, set[int]] = {}
    for m in re.finditer(r"^\| `(Order\w+)` \| ([\d, ]+) \|", text, re.M):
        catalogue[m.group(1)] = {int(x) for x in re.findall(r"\d+", m.group(2))}
    declaring = {n for n, (_, _, event, _) in rows.items() if event}
    mapped: set[int] = set().union(*catalogue.values()) if catalogue else set()
    res.check(
        declaring == mapped,
        f"01 §4.4's event catalogue and §4.3's event-declaring rows are not the "
        f"same set: rows {sorted(declaring - mapped)} declare an event the "
        f"catalogue omits; the catalogue cites rows {sorted(mapped - declaring)} "
        f"that declare none",
    )
    for event, numbers in catalogue.items():
        for n in sorted(numbers):
            if n in rows:
                res.check(
                    rows[n][2] == event,
                    f"01 §4.4 maps `{event}` to row {n}, whose own text in §4.3 "
                    f"declares `{rows[n][2]}`",
                )


def check_tax_never_stored(docs: Path, res: Result) -> None:
    """Assert 03 §4.6's absolute prohibition against a schema that could break it.

    "The indicative tax MUST NOT be stored on any order" is the kind of
    prohibition a later column quietly violates, and no reviewer re-reads every
    schema to check it.
    """
    columns: set[tuple[str, str]] = set()
    for path in sorted((docs / "design").glob("*.md")):
        text = read(path)
        for blk in re.finditer(r"^#### Table:(.+?)(?=^#### |\Z)", text, re.M | re.S):
            body = blk.group(1)
            found = set(re.findall(r"^\|\s*`?(\w+)`?\s*\|", body, re.M))
            schema = re.search(r"\*\*Schema\*\*:(.+?)(?:\*\*PK\*\*|\Z)", body, re.S)
            if schema:
                found |= set(re.findall(r"`(\w+)`", schema.group(1)))
            columns |= {(path.name, c) for c in found}
    offenders = sorted(f"{p}:{c}" for p, c in columns if "tax" in c.lower())
    res.check(
        not offenders,
        f"03 §4.6 says the indicative tax **MUST NOT** be stored on any order, "
        f"but a schema declares {offenders}",
    )


def check_sibling_gear_claims(docs: Path, res: Result) -> None:
    """Assert a claim that a sibling gear is unspecified against the filesystem.

    `UPSTREAM_REQS.md` claimed "Rating, the billing chain and Payments have no
    register to cite: no specification for them exists in this repository".
    Rating carries a PRD, a DESIGN, ADRs and its own SEAMS.md register, and the
    billing chain is specified as `gears/bss/ledger`. Only Payments wasly absent.
    Every family before this one compares the design set against itself; a claim
    about a *sibling* gear had nothing checking it at all — which is why a
    sweeping and false one survived every review wave.
    """
    text = read(docs / "UPSTREAM_REQS.md")
    if not text:
        return
    gears_root = docs.parent.parent          # gears/bss/<gear>/docs -> gears/bss
    if not gears_root.is_dir():
        return

    # Named capabilities the register may assert are unspecified, mapped to the
    # directory that would specify them.
    known = {
        "Rating": "rating",
        "Subscriptions": "subscriptions",
        "Pricing": "pricing",
        "Contracts": "contracts",
        "Products": "products",
        "the billing chain": "ledger",
        "Orders Workflow": "orders-workflow",
    }
    absent_claim = re.compile(
        r"(?:no specification (?:for (?:them|it) )?exists|has no specification|"
        r"no gear (?:or|and no) specification|does not exist)", re.I
    )
    # A sentence may *deny* the absence — the corrected register says Rating and
    # the billing chain **are** specified. Scoping to a sentence and skipping
    # affirmations is what keeps this from firing on its own fix.
    affirms = re.compile(r"\bare\b[^.]{0,40}specified|\bis specified\b", re.I)
    for sentence in re.split(r"(?<=[.;])\s+", text):
        if not absent_claim.search(sentence) or affirms.search(sentence):
            continue
        for name, directory in known.items():
            if name not in sentence:
                continue
            spec = gears_root / directory / "docs" / "PRD.md"
            res.check(
                not spec.is_file(),
                f"UPSTREAM_REQS.md claims no specification exists for "
                f"{name!r}, but `gears/bss/{directory}/docs/PRD.md` is present "
                f"— raise the ask against that specification instead of "
                f"recording it as unowned",
            )
    res.check(True, "UPSTREAM_REQS.md: no false unspecified-sibling claim")


# --------------------------------------------------------------------------

def supported(docs: Path) -> bool:
    """Whether this checker understands the set's register convention.

    Only a register that uses `**Propagated**:` addresses carries the structure
    every family below relies on. Running gear-specific assertions against a set
    shaped differently produces noise, not findings — and a failing check on an
    unrelated gear would block CI for everyone. An unsupported set is reported as
    NOT CHECKED rather than passed or failed.
    """
    return "**Propagated**:" in read(docs / "DECISIONS.md")


def run(docs: Path) -> Result:
    res = Result()
    if not supported(docs):
        return res
    check_propagation(docs, res)
    check_counts(docs, res)
    check_reasons(docs, res)
    check_endpoints(docs, res)
    check_retired_terms(docs, res)
    check_step_citations(docs, res)
    check_step_contiguity(docs, res)
    check_no_pre_engine_refusals(docs, res)
    check_column_ownership(docs, res)
    check_index_columns(docs, res)
    check_enum_membership(docs, res)
    check_prose_counts(docs, res)
    check_guard_ordering(docs, res)
    check_adr_reciprocity(docs, res)
    check_version_vocabulary(docs, res)
    check_table_inventory(docs, res)
    check_state_machine_claims(docs, res)
    check_trigger_addressing(docs, res)
    check_register_board(docs, res)
    check_retention_executor(docs, res)
    check_settlement_claim(docs, res)
    check_gts_claim_is_specified(docs, res)
    check_tax_never_stored(docs, res)
    check_sibling_gear_claims(docs, res)
    return res


def main(argv: list[str]) -> int:
    if argv:
        targets = [Path(a).resolve() for a in argv]
    else:
        targets = sorted(
            p.parent for p in REPO.glob("gears/*/*/docs/DECISIONS.md")
        )
    if not targets:
        print("no design sets with a DECISIONS.md found — nothing to check")
        return 0

    total_failures = 0
    unchecked: list[str] = []
    for docs in targets:
        label = docs.relative_to(REPO) if docs.is_relative_to(REPO) else docs
        res = run(docs)
        if res.failures:
            print(f"\n✗ {label}: {len(res.failures)} of {res.checks} invariants failed")
            for failure in res.failures:
                print(f"    - {failure}")
            total_failures += len(res.failures)
        elif res.checks == 0:
            # Never report a pass for a set nothing was checked against: a green
            # tick over zero checks is the false reassurance this script exists
            # to remove.
            decisions = read(docs / "DECISIONS.md")
            entries = len(re.findall(r"^[#|]+\s*\|?\s*[A-Z]*-?D-?\d+", decisions, re.M))
            print(
                f"— {label}: NOT CHECKED — register format not recognised "
                f"({entries} decision-like entries found, 0 checkable). "
                f"Extend the parser or exclude the path deliberately."
            )
            unchecked.append(str(label))
        else:
            print(f"✓ {label}: {res.checks} invariants hold")

    if unchecked:
        print(
            f"\nNote: {len(unchecked)} design set(s) were not checked — "
            f"{', '.join(unchecked)}. This is reported, not passed."
        )
    if total_failures:
        print(f"\n{total_failures} design invariant(s) failed")
        return 1
    print("\nAll checked design invariants hold.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
