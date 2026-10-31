#!/usr/bin/env python3
"""Estimate review input tokens before starting model calls.

Each subject module reads the scoped diff and source. The pipeline estimate includes
separate discovery/evidence passes plus collect, matching and formatting. It is a
planning estimate, not a tokenizer, billing total or limit on source exploration.
"""

from __future__ import annotations

import math
import os
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

# Calibrated against two measured diffs: 718 KB -> ~179k tokens (4.01 B/token) and
# 547 KB -> ~136k (4.02). Source code sits close enough to 4 bytes per token for
# budgeting; this is not a tokenizer and must not be used as one.
BYTES_PER_TOKEN = 4.0

ROOT = Path(__file__).resolve().parents[3]
DOCS = ROOT / "docs/toolkit-pr-review"
RULES_DIR = DOCS / "rules"
AGENT_DIR = DOCS / "agents"

# The six subject agents, in the order `prepare` reports them. Each name is both the
# rule module's filename stem and the agent's identity in the spawn prompt.
MODULES = ["errors", "security", "async", "design", "tests", "toolkit"]

# Read alongside a module by every subject agent.
SHARED_DOCS = ["review-conventions.md", "comment-style.md"]

# The spawn prompt itself: file list, coordinates note, scope rules. Measured at roughly
# 1.5k tokens on a 56-file PR and it grows with the file list, so it is charged per agent.
PROMPT_TOKENS = 2_000

DEFAULT_MAX_TOTAL_TOKENS = 4_000_000


def est_tokens(n_bytes: int) -> int:
    return math.ceil(n_bytes / BYTES_PER_TOKEN)


def _size(p: Path) -> int:
    try:
        return p.stat().st_size
    except OSError:
        return 0


def instruction_tokens(module: str) -> int:
    """What one subject agent reads before it opens a single source file.

    Measured from the files on disk rather than hardcoded, so growing a rule module shows
    up in the estimate instead of silently inflating every run.
    """
    b = _size(RULES_DIR / f"{module}.md") + _size(AGENT_DIR / "subject.md")
    b += sum(_size(DOCS / d) for d in SHARED_DOCS)
    return est_tokens(b) + PROMPT_TOKENS


def architecture_tokens() -> int:
    b = _size(AGENT_DIR / "architecture.md") + sum(_size(DOCS / d) for d in SHARED_DOCS)
    return est_tokens(b) + PROMPT_TOKENS


def agent_costs(corpus_tokens: int, diff_tokens: int) -> dict[str, int]:
    """Estimated input tokens per agent.

    Every subject agent reads the whole diff and every changed file. The architecture
    agent reads the diff alone, since RUST-ARCH-001 is about the shape of the change
    rather than the contents of any one file.
    """
    out = {m: instruction_tokens(m) + corpus_tokens + diff_tokens for m in MODULES}
    out["architecture"] = architecture_tokens() + diff_tokens
    return out


def total_cost(corpus_tokens: int, diff_tokens: int) -> int:
    return sum(agent_costs(corpus_tokens, diff_tokens).values())


def pipeline_costs(corpus_tokens: int, diff_tokens: int) -> tuple[dict[str, int], int]:
    """Two source passes per module, plus draft/receipt and post-processing input.

    This is a planning estimate, not a cap on source exploration or a billing claim.
    Closure may need unchanged consumers beyond this initial corpus.
    """
    policy = sum(_size(p) for p in (DOCS / "policies").glob("*.*"))
    costs = {m: 2 * (n + est_tokens(policy)) + 8_000
             for m, n in agent_costs(corpus_tokens, diff_tokens).items()}
    # Architecture also needs targeted full-source context during closure.
    costs["architecture"] += corpus_tokens
    post = 3 * (est_tokens(policy) + 12_000)  # collect, history matching, format
    return costs, post
