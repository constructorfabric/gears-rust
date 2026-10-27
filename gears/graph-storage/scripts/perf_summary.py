#!/usr/bin/env python3
"""Summarize the graph-storage perf lane's RESULT lines as markdown tables (stdlib only).

The lane (`graph-storage/tests/perf.rs`) prints one `RESULT` line per scenario,
repeat and added round-trip time. This turns a log of them into the tables
`docs/PERF.md` asks a run to report:

  * per scenario and added round-trip time: the median p50 across repeats,
    [min-max] of the per-repeat p50, and the median p95, in ms;
  * per repeat: the slope of p50 against the added round-trip time, in ms per
    ms -- an estimate of how many sequential round trips the request makes.

Usage (from the repo root):
  python3 gears/graph-storage/scripts/perf_summary.py perf.log [more.log ...]
"""

import re
import statistics
import sys
from collections import defaultdict

LINE = re.compile(
    r'RESULT rep=(\d+) rtt_ms=([\d.]+) backend=(\S+) scenario="([^"]+)" '
    r"p50_ms=([\d.]+) p95_ms=([\d.]+) max_ms=([\d.]+) runs=(\d+)"
)


def load(path):
    """(scenario, backend) -> rtt -> [(repeat, p50, p95)]"""
    data = defaultdict(lambda: defaultdict(list))
    with open(path, encoding="utf-8") as log:
        for line in log:
            match = LINE.search(line)
            if not match:
                continue
            rep, rtt, backend, scenario, p50, p95 = match.groups()[:6]
            data[(scenario, backend)][float(rtt)].append((int(rep), float(p50), float(p95)))
    return data


def slope(points):
    """Least-squares slope of p50 over the added round-trip time."""
    xs = [x for x, _ in points]
    ys = [y for _, y in points]
    if len(set(xs)) < 2:
        return None
    mx, my = statistics.mean(xs), statistics.mean(ys)
    den = sum((x - mx) ** 2 for x in xs)
    return sum((x - mx) * (y - my) for x, y in points) / den


def cell(samples):
    p50s = [s[1] for s in samples]
    p95s = [s[2] for s in samples]
    if len(samples) == 1:
        return f"{p50s[0]:.1f} · {p95s[0]:.1f}"
    return (
        f"{statistics.median(p50s):.1f} [{min(p50s):.1f}–{max(p50s):.1f}]"
        f" · {statistics.median(p95s):.1f}"
    )


def table(data, title):
    if not data:
        print(f"\n### {title}\n\nno RESULT lines found")
        return
    rtts = sorted({rtt for by_rtt in data.values() for rtt in by_rtt})
    print(f"\n### {title}\n")
    head = " | ".join(f"+{rtt:g} ms p50 [min–max] · p95" for rtt in rtts)
    print(f"| scenario | backend | {head} | slope per repeat |")
    print("|---|---|" + "---|" * len(rtts) + "---|")
    for (scenario, backend), by_rtt in sorted(data.items()):
        cells = [cell(by_rtt[rtt]) if by_rtt.get(rtt) else "—" for rtt in rtts]
        repeats = sorted({s[0] for samples in by_rtt.values() for s in samples})
        slopes = []
        for rep in repeats:
            points = [(rtt, s[1]) for rtt, samples in by_rtt.items() for s in samples if s[0] == rep]
            value = slope(points)
            slopes.append("—" if value is None else f"{value:.1f}")
        print(f"| {scenario} | {backend} | " + " | ".join(cells) + f" | {', '.join(slopes)} |")


def main(paths):
    if not paths:
        print(__doc__, file=sys.stderr)
        return 2
    for path in paths:
        table(load(path), path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
