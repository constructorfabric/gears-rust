#!/usr/bin/env python3
"""Positive and negative tests for check-design-invariants.py.

An assertion that has never failed is unproven. Worse, an assertion whose
pattern silently matches nothing reports a pass over zero checks — the exact
false reassurance the checker exists to remove.

Each case below copies a design set to a scratch directory, injects one defect,
runs the checker, and asserts it fails **with a message naming the defect**.
A case that does not fail is reported as a hole in the checker.

Usage:
    python3 scripts/test-design-invariants.py [GEAR_DOCS_DIR]

Defaults to gears/bss/orders-lifecycle/docs. Exit status 1 if any case fails to
be detected, or if the unmodified set does not pass cleanly.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CHECKER = REPO / "scripts" / "check-design-invariants.py"


@dataclass
class Case:
    family: str
    name: str
    path: str          # relative to the docs dir
    find: str          # literal text to replace (must be present)
    replace: str       # the injected defect
    expect: str        # substring the failure message must contain
    passes: bool = False  # valid editorial variation must remain accepted


CASES: list[Case] = [
    # --- family: counts -------------------------------------------------
    Case(
        "counts", "stale decision count in the index",
        "DESIGN.md",
        "entries plus", "entries plus",          # rewritten below by mutate()
        expect="decision entries",
    ),
    # --- family: reasons ------------------------------------------------
    Case(
        "reasons", "one reason registered by two slices",
        "design/06-workflow-seam.md",
        "**Reasons contributed to the registry**:",
        "**Reasons contributed to the registry**: cancel-reason-required,",
        expect="registered by 2 slices",
    ),
    # --- family: retired terms ------------------------------------------
    Case(
        "retired", "a retired reason name reappears",
        "design/04-versioning.md",
        "**Reasons contributed to the registry**:",
        "**Reasons contributed to the registry**: verdict-version-stale,",
        expect="retired the term",
    ),
    # --- family: step citations -----------------------------------------
    Case(
        "steps", "step citation loses its algorithm name",
        "DECISIONS.md",
        "`01 §3.6` *Attempt Transition* step 1",
        "`01 §3.6` step 1",
        expect="is ambiguous",
    ),
    Case(
        "steps", "step citation points past the end of its algorithm",
        "DECISIONS.md",
        "`01 §3.6` *Attempt Transition* step 1",
        "`01 §3.6` *Attempt Transition* step 97",
        expect="does not exist",
    ),
    # --- family: column ownership ---------------------------------------
    Case(
        "columns", "a slice references a column that does not exist",
        "design/06-workflow-seam.md",
        "`orders_order.compensation_evidence`",
        "`orders_order.compensation_proof`",
        expect="not a column of that table",
    ),
    # --- family: index column ownership ---------------------------------
    Case(
        "indexes", "an index filters on another table's column",
        "design/01-foundation.md",
        "`(payer_tenant_id, overlap_scope_key) WHERE released_at IS NULL`",
        "`(payer_tenant_id, overlap_scope_key) WHERE state IN ('submitted')`",
        expect="cannot filter on another table's columns",
    ),
    # --- family: enum membership ----------------------------------------
    Case(
        "enums", "a value assigned to an enum column is undeclared",
        "design/05-preconditions.md",
        "`requirement_source`",
        "`requirement_source` = `assumed`",
        expect="not a declared value",
    ),
    # --- family: prose counts -------------------------------------------
    Case(
        "prose", "a prose port count drifts from the deadline table",
        "design/03-gate-and-pin.md",
        "", "",                                   # rewritten by mutate()
        expect="outbound ports",
    ),
    # --- family: guard ordering -----------------------------------------
    Case(
        "ordering", "the normative sentence puts idempotency before authorization",
        "design/01-foundation.md",
        "**authorization, then idempotency resolution",
        "**idempotency resolution, then authorization",
        expect="contradicting",
    ),
    Case(
        "ordering", "the authorization step disappears from the algorithm",
        "design/01-foundation.md",
        "`inst-probe-idempotency`",
        "`inst-unrelated-operation`",
        expect="no authorization pre-guard step",
    ),
    # --- family: ADR reciprocity ----------------------------------------
    Case(
        "adr", "a decision loses its ADR citation",
        "DECISIONS.md",
        "", "",                                   # rewritten by mutate()
        expect="contains no reference to that ADR",
    ),
    # --- family: version vocabulary -------------------------------------
    Case(
        "vocab", "the version-appending row count drifts",
        "design/04-versioning.md",
        "Five transition rows append a version",
        "Seven transition rows append a version",
        expect="version-appending rows",
    ),
    # --- family: table-inventory membership -----------------------------
    Case(
        "inventory", "a defined table is missing from the canonical inventory",
        "DESIGN.md",
        "| `orders_inflight_overlap_claim` | `01 §3.7` | gate-and-pin |",
        "| `orders_REMOVED_from_inventory` | `01 §3.7` | gate-and-pin |",
        expect="absent from DESIGN.md",
    ),
    # --- family: step contiguity ----------------------------------------
    Case(
        "contiguity", "an algorithm's steps skip a number",
        "design/02-capture.md",
        "2. [ ] - `p1` - Assign a stable line_id",
        "4. [ ] - `p1` - Assign a stable line_id",
        expect="step-numbering gap",
    ),
    # --- family: pre-engine refusals ------------------------------------
    Case(
        "pre-engine", "a slice refuses before calling the engine",
        "design/07-hold-and-expiry.md",
        "1. [ ] - `p1` - Declare no admissibility guard",
        "1. [ ] - `p1` - **RETURN** hold-not-admitted-in-state refusal - `x` - Declare no admissibility guard",
        expect="before its engine call",
    ),
    # --- family: state-machine claims -----------------------------------
    Case(
        "states", "a transition originates in a terminal state",
        "design/01-foundation.md",
        "21. [ ] - `p1` - **FROM** `submitted`, `pending_approval`, `approved` or `in_fulfillment` **TO** `on_hold`",
        "21. [ ] - `p1` - **FROM** `submitted`, `pending_approval`, `cancelled` or `in_fulfillment` **TO** `on_hold`",
        expect="terminal state admits no outgoing transition",
    ),
    Case(
        "states", "the in_fulfillment expiry exemption stops being structural",
        "design/01-foundation.md",
        "24. [ ] - `p1` - **FROM** `submitted`, `pending_approval`, `approved` **TO** `expired`",
        "24. [ ] - `p1` - **FROM** `submitted`, `in_fulfillment`, `approved` **TO** `expired`",
        expect="the exemption is structural",
    ),
    Case(
        "states", "the event catalogue and the rows disagree",
        "design/01-foundation.md",
        "| `OrderHeld` | 21 |",
        "| `OrderHeld` | 21, 12 |",
        expect="not the same set",
    ),
    # --- family: absolute prohibitions ----------------------------------
    Case(
        "prohibit", "a schema column would store the indicative tax",
        "design/03-gate-and-pin.md",
        "| outcome_id | uuid |",
        "| indicative_tax_amount | numeric |\n| outcome_id | uuid |",
        expect="indicative tax",
    ),
    # --- family: trigger addressing -------------------------------------
    Case(
        "trigger", "two rows share a (from-state, trigger) key",
        "design/01-foundation.md",
        "**WHEN** `reflect-approval-not-required` \u2014 the requirement verdict says approval is not required",
        "**WHEN** `reflect-approval-required` \u2014 the requirement verdict says approval is not required",
        expect="cannot choose between them",
    ),
    Case(
        "trigger", "a row describes its trigger instead of naming it",
        "design/01-foundation.md",
        "**WHEN** `hold` \u2014 storing the outgoing state",
        "**WHEN** hold, storing the outgoing state",
        expect="does not name a backticked trigger",
    ),
    # --- family: register status board ----------------------------------
    Case(
        "board", "the status board's Q-range drifts from the register",
        "DECISIONS.md",
        "| Open questions | Q-01\u2026Q-31 |",
        "| Open questions | Q-01\u2026Q-18 |",
        expect="but the register's highest is",
    ),
    # --- family: retention executor -------------------------------------
    Case(
        "retention", "a declared retention loses its purge worker",
        "DESIGN.md",
        "purges Preview gate-outcome rows past 7 days",
        "handles Preview gate-outcome rows past 7 days",
        expect="no executor is an unbounded store",
    ),
    # --- family: settlement claim ---------------------------------------
    Case(
        "settle", "the settlement count drifts from its own enumeration",
        "design/01-foundation.md",
        "**Four of the seven also settle their idempotency",
        "**Six of the seven also settle their idempotency",
        expect="the count and its own list disagree",
    ),
    Case(
        "settle", "the ADR and the design state different settlement counts",
        "ADR/0005-cpt-cf-bss-orders-lifecycle-adr-refusals-commit.md",
        "**Four of the seven classes settle**",
        "**Five of the seven classes settle**",
        expect="must be stated",
    ),
    # --- family: GTS claim is specified ---------------------------------
    Case(
        "gts-ids", "every GTS base type identifier disappears",
        "design/01-foundation.md", "", "",          # handled in mutate()
        expect="specifies no a base type identifier",
    ),
    Case(
        "gts-registry", "the GTS registry dependency disappears",
        "design/01-foundation.md", "", "",          # handled in mutate()
        expect="specifies no the registry it resolves against",
    ),
    # --- family: propagation --------------------------------------------
    # 47% of the suite's assertions and, until now, no negative case at all.
    Case(
        "propagation", "a propagation address names a section that does not exist",
        "DECISIONS.md",
        "**Propagated**: `02 §1.2`, `§4.2`; `03 §3.6` *Run Gate and Submit* step 12",
        "**Propagated**: `02 §9.9`, `§4.2`; `03 §3.6` *Run Gate and Submit* step 12",
        expect="names a section that does not exist",
    ),
    Case(
        "propagation", "a propagated section shares no term with its decision",
        "DECISIONS.md", "", "",          # handled in mutate()
        expect="shares no distinctive term with the decision",
    ),
    # --- family: endpoint ownership -------------------------------------
    Case(
        "endpoints", "one endpoint is claimed by two slices",
        "design/04-versioning.md",
        "| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/versions` |",
        "| `POST` | `/bss-orders-lifecycle/v1/orders/preview` | duplicate claim | unstable |\n| `GET` | `/bss-orders-lifecycle/v1/orders/{orderId}/versions` |",
        expect="declared as owned by 2 slices",
    ),
    # --- family: claims about sibling gears -----------------------------
    Case(
        "sibling", "the register calls a specified sibling gear unspecified",
        "UPSTREAM_REQS.md",
        "### 2.2 Rating / price evaluation",
        "### 2.2 Rating / price evaluation\n\nFor Rating no specification exists in this repository.\n",
        expect="but `gears/bss/rating/docs/PRD.md` is present",
    ),
    # --- family: reverse ADR traceability -------------------------------
    Case(
        "adr-rev", "a design home stops citing the ADR that names it",
        "design/01-foundation.md",
        "", "",                                   # rewritten by mutate()
        expect="resolves in one direction only",
    ),
]

# Positive cases prove the parser accepts valid constructs; paired negative
# cases prevent a false-positive fix from merely switching an assertion off.
CASES += [
    Case("indexes", "a UNIQUE declaration rejects a nonexistent column",
         "design/01-foundation.md",
         "**partial UNIQUE** on\n`(payer_tenant_id, overlap_scope_key) WHERE released_at IS NULL`",
         "partial constraint `UNIQUE (payer_tenant_id, missing_claim_column) WHERE released_at IS NULL`",
         "references column `missing_claim_column`"),
    Case("indexes", "a UNIQUE declaration accepts actual columns",
         "design/01-foundation.md",
         "**partial UNIQUE** on\n`(payer_tenant_id, overlap_scope_key) WHERE released_at IS NULL`",
         "partial constraint `UNIQUE (payer_tenant_id, overlap_scope_key) WHERE released_at IS NULL`",
         "", passes=True),
    Case("indexes", "a PRIMARY KEY declaration rejects a nonexistent column",
         "design/01-foundation.md", "**PK**: claim_id",
         "**PK**: `PRIMARY KEY (missing_claim_column)`",
         "references column `missing_claim_column`"),
    Case("indexes", "a PRIMARY KEY declaration accepts an actual column",
         "design/01-foundation.md", "**PK**: claim_id",
         "**PK**: `PRIMARY KEY (claim_id)`", "", passes=True),
    Case("reasons", "an inline reuse clause does not claim ownership",
         "design/03-gate-and-pin.md",
         "**Reasons contributed to the registry**:",
         "**Reasons contributed to the registry**: reuse capture's currency-mixed;",
         "", passes=True),
    Case("indexes", "Rust calls in table guidance are not SQL columns",
         "design/01-foundation.md", "**PK**: claim_id",
         "**PK**: claim_id\n\nUse `Ok(Refused)` and `QuerySelect::lock_exclusive()` with `scope_with(&scope).one(secure_tx)`.",
         "", passes=True),
    Case("indexes", "a referenced FK column belongs to the referenced table",
         "design/01-foundation.md",
         "`(order_id, accepted_version)` to `orders_order_version(order_id, version)`",
         "`(order_id, accepted_version)` to `orders_order_version(order_id, version)` (the target version differs from accepted_version)",
         "", passes=True),
    Case("indexes", "an FK target must actually declare its referenced column",
         "design/01-foundation.md",
         "`(order_id, accepted_version)` to `orders_order_version(order_id, version)`",
         "`(order_id, accepted_version)` to `orders_order_version(order_id, absent_version)`",
         "has no column `absent_version`"),
    Case("ordering", "step wording may change while its stable identity remains",
         "design/01-foundation.md",
         "Resolve and lock the authoritative idempotency record",
         "Lock and resolve the authoritative registry entry",
         "", passes=True),
    Case("ordering", "the declared workflow exception needs its algorithm branch",
         "design/01-foundation.md", "`inst-if-workflow-version-conflict`",
         "`inst-unrelated-workflow-branch`", "no workflow version-check branch"),
    Case("ordering", "the normative order sentence cannot silently disappear",
         "design/01-foundation.md",
         "Guard evaluation order is normative and total, with one declared exception:",
         "Guard evaluation order is described elsewhere:",
         "no longer states the normative guard evaluation order"),
    Case("ordering", "the registry resolution step cannot disappear",
         "design/01-foundation.md", "`inst-resolve-idempotency`",
         "`inst-unrelated-resolution`", "no idempotency-resolution step"),
    Case("settle", "later sentences do not extend the settlement enumeration",
         "design/01-foundation.md",
         "and failed slice guard. The common transactional",
         "and failed slice guard. Replay and recovery, too, preserve ownership. The common transactional",
         "", passes=True),
]


def mutate(case: Case, root: Path) -> bool:
    """Apply one injected defect. Returns False if the anchor text is absent."""
    target = root / case.path
    text = target.read_text(encoding="utf-8")

    if case.family == "propagation" and not case.find:
        # Replace a decision's body with text sharing no vocabulary with the
        # section it claims to have landed in, leaving the address intact.
        path = root / "DECISIONS.md"
        body = path.read_text(encoding="utf-8")
        # The entry's *heading* also feeds the vocabulary comparison, so a body
        # swap alone leaves enough overlap to pass. Blank both.
        m = re.search(r"^### D-09[^\n]*\n(.+?)(?=\n\*\*Propagated\*\*:)",
                      body, re.M | re.S)
        if not m:
            return False
        path.write_text(
            body[:m.start()]
            + "### D-09 (H) Lorem\n\n**Decision**: lorem ipsum dolor sit amet.\n"
            + body[m.end(1):],
            encoding="utf-8",
        )
        return True

    if case.family in {"gts-ids", "gts-registry"}:
        # These assert the GTS model exists *at all*, so a single substitution
        # proves nothing — the pattern has to be removed from every file.
        pattern = (r"gts\.[a-z0-9_]+(?:\.[a-z0-9_]+)+\.v\d+~"
                   if case.family == "gts-ids" else r"types-registry")
        hit = False
        for path in [root / "DESIGN.md"] + sorted((root / "design").glob("*.md")):
            body = path.read_text(encoding="utf-8")
            stripped = re.sub(pattern, "REDACTED", body)
            if stripped != body:
                path.write_text(stripped, encoding="utf-8")
                hit = True
        return hit

    if case.family == "prose":
        # Bump a stated port count so it no longer matches the deadline table.
        m = re.search(r"\*\*([A-Za-z-]+)\*\* outbound (?:ports|operations)", text)
        if not m:
            return False
        text = text.replace(m.group(0), "**Twelve** outbound ports", 1)
    elif case.family == "adr-rev":
        # Remove every way 01-foundation cites ADR-0005 — the filename link and
        # the cpt ID in its Key ADRs table — so the reverse trace really breaks.
        before = text
        text = text.replace(
            "ADR/0005-cpt-cf-bss-orders-lifecycle-adr-refusals-commit.md", "ADR/0005-gone.md"
        ).replace("cpt-cf-bss-orders-lifecycle-adr-refusals-commit", "adr-gone")
        if text == before:
            return False
    elif case.family == "adr":
        # Strip every reference to ADR-0004 from D-60's section only, so the
        # ADR's "these entries cite it" claim becomes false.
        m = re.search(r"^### D-60\b.*?(?=^#{2,3}\s)", text, re.M | re.S)
        if not m:
            return False
        body = m.group(0)
        text = text.replace(body, re.sub(r"ADR[-/]0004", "ADR-XXXX", body), 1)
    elif case.family == "counts":
        # The index no longer duplicates this count. Inject a false claim rather
        # than depending on the stale prose the checker originally targeted.
        text += "\nThe decision register has 17 entries plus 30 routed open questions.\n"
    else:
        if case.find not in text:
            return False
        text = text.replace(case.find, case.replace, 1)

    target.write_text(text, encoding="utf-8")
    return True


def run_checker(docs: Path) -> tuple[int, str]:
    proc = subprocess.run(
        [sys.executable, str(CHECKER), str(docs)],
        capture_output=True, text=True,
    )
    return proc.returncode, proc.stdout + proc.stderr


def main(argv: list[str]) -> int:
    docs = Path(argv[0]).resolve() if argv else (
        REPO / "gears/bss/orders-lifecycle/docs"
    )
    if not (docs / "DECISIONS.md").is_file():
        print(f"no design set at {docs}")
        return 1

    # Guard: the unmodified set must pass, or every case below is meaningless.
    code, out = run_checker(docs)
    if code != 0:
        print("✗ the unmodified set does not pass — fix it before running these tests\n")
        print(out)
        return 1
    print(f"✓ baseline: unmodified set passes\n")

    failures = 0
    for case in CASES:
        with tempfile.TemporaryDirectory() as tmp:
            # Mirror the real layout — gears/bss/<gear>/docs — so families that
            # resolve sibling gears relative to the docs directory have a tree
            # to resolve against.
            root = Path(tmp) / "gears" / "bss" / docs.parent.name / "docs"
            root.parent.mkdir(parents=True)
            shutil.copytree(docs, root)
            for sibling in ("rating", "ledger", "subscriptions", "pricing"):
                src = docs.parent.parent / sibling / "docs" / "PRD.md"
                if src.is_file():
                    dst = root.parent.parent / sibling / "docs"
                    dst.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(src, dst / "PRD.md")

            if not mutate(case, root):
                print(f"  ⚠ {case.family:8} {case.name}")
                print(f"      SKIPPED — anchor text not found, the case needs updating")
                failures += 1
                continue

            code, out = run_checker(root)
            if case.passes:
                if code != 0:
                    print(f"  ✗ {case.family:8} {case.name}: valid text rejected\n{out}")
                    failures += 1
                else:
                    print(f"  ✓ {case.family:8} {case.name} (valid text accepted)")
            elif code == 0:
                print(f"  ✗ {case.family:8} {case.name}")
                print(f"      NOT DETECTED — the checker passed an injected defect")
                failures += 1
            elif case.expect not in out:
                print(f"  ~ {case.family:8} {case.name}")
                print(f"      detected, but no message contained {case.expect!r}")
                for line in out.split("\n"):
                    if line.strip().startswith("-"):
                        print(f"        got: {line.strip()[:110]}")
                failures += 1
            else:
                print(f"  ✓ {case.family:8} {case.name}")

    print()
    if failures:
        print(f"{failures} of {len(CASES)} cases did not behave as specified.")
        return 1
    print(f"All {len(CASES)} positive/negative cases passed. The assertions are live.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
