#!/usr/bin/env python3
"""Fails when an ownership filter makes the consume decision too slow.

Reads the criterion estimates the ``ownership_filter`` group of
``benches/delivery.rs`` writes and compares the throughput of each filtered
case (``only_mine``, ``entire_tenant``) with the ``unfiltered`` baseline, the
same decision with no ownership narrowing. A case below 90% of the baseline
fails the run. Every case decides the same batch, so throughput is the inverse
of the mean time per iteration.

Usage: compare_delivery.py [CRITERION_DIR]

CRITERION_DIR defaults to ``target/criterion``; run the bench first:
``cargo bench -p cf-gears-event-broker --bench delivery -- ownership_filter``.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

GROUP = "ownership_filter"
BASELINE = "unfiltered"
FILTERED = ("only_mine", "entire_tenant")
MIN_RATIO = 0.90


def mean_ns(criterion_dir: Path, case: str) -> float:
    """The mean time per iteration of ``case``, in nanoseconds, from its last run."""
    path = criterion_dir / GROUP / case / "new" / "estimates.json"
    with path.open(encoding="utf-8") as estimates:
        return float(json.load(estimates)["mean"]["point_estimate"])


def main(argv: list[str]) -> int:
    criterion_dir = Path(argv[1]) if len(argv) > 1 else Path("target/criterion")
    try:
        baseline = mean_ns(criterion_dir, BASELINE)
        filtered = {case: mean_ns(criterion_dir, case) for case in FILTERED}
    except (OSError, KeyError, ValueError) as err:
        print(f"cannot read the {GROUP} estimates under {criterion_dir}: {err}", file=sys.stderr)
        return 2

    print(f"{BASELINE}: {baseline:.0f} ns per batch")
    failed = []
    for case, mean in filtered.items():
        ratio = baseline / mean
        verdict = "ok" if ratio >= MIN_RATIO else "FAIL"
        print(f"{case}: {mean:.0f} ns per batch, throughput {ratio:.1%} of {BASELINE} ({verdict})")
        if ratio < MIN_RATIO:
            failed.append(case)
    if failed:
        print(
            f"below {MIN_RATIO:.0%} of the {BASELINE} throughput: {', '.join(failed)}",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
