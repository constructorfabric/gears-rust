---
name: session-idea-mining
description: Mine the user's Claude Code chat history for ideas/feedback/pain about a specific topic, synthesize a ranked themed brief, then optionally cross-reference against existing docs and file tickets. Use when the user says "review all my chats/sessions for ideas about X", "what have I said about Y across my history", "mine my conversations for feedback on Z", or wants to harvest scattered design notes / complaints / abandoned threads about a focus area. This is FOCUSED topical mining (local, not shared) — distinct from the shareable pattern-mining in tools/session-mining/README.md.
---

# Session idea mining

Mine the user's real Claude Code transcripts for everything they've said about a
**focus topic** — feature ideas, pain points, design musings, abandoned threads —
and turn the scatter into a deduped, ranked, source-attributed brief. Then
(optionally) cross-reference against existing proposals/docs and file tickets for
the genuine gaps.

This is the **focused** counterpart to the determinism-ladder *pattern* mining
documented in `tools/session-mining/README.md`. Pattern-mining asks "which
recurring workflows are worth scripting?" and produces a shareable, redacted
report. Idea-mining asks "what have I said about **X**?" and stays local.

## The corpus

Sessions live at `~/.claude/projects/<project-slug>/*.jsonl` (one dir per repo).
There are often hundreds, many huge. You cannot read them raw — `prep.py`
distills each to a compact action trace (typically 50–150× smaller) and bin-packs
them into byte-balanced batches so a fan-out of reader agents can cover the whole
corpus. Tooling lives in `tools/session-mining/` (relative to the repo root).

## Recipe

### 1. Scope, then prep (deterministic, free)

Confirm two things with the user before spending tokens (use AskUserQuestion):
- **scope** — all substantive sessions, or a recency/grep-limited subset?
- **what counts as an idea** — which of feature / pain / design / abandoned.

Then run `prep.py` — it filters, distills, optionally redacts, and bin-packs in
one command:

```sh
cd tools/session-mining
python3 prep.py ~/.claude/projects/<slug> --out /tmp/sm-<tag>
# common flags:
#   --grep <word>      keep only sessions mentioning <word> (repeatable, OR) — cheap topical prefilter
#   --min-bytes N      skip raw sessions smaller than N (default 30000)
#   --sample recency   take newest first (with --max to cap); default takes all
#   --max N            cap session count
#   --budget BYTES     target bytes per batch (default 200000 ≈ a comfortable reader load)
#   --redact           ONLY for shareable pattern-mining; OMIT for local idea-mining
```

It prints, on the last lines, `BATCHES=<n>` and `BATCHDIR=<path>` — you need both
for step 2. (Full counts + per-batch sizes go to stderr and `manifest.json`.)

> **Do NOT `--redact` for idea-mining.** Redaction strips the very content
> (paths, names, the user's phrasing) you're trying to harvest. The traces stay
> in `/tmp` and never leave the machine, so there's nothing to scrub. `--redact`
> exists only for the shareable pattern-mining path.

### 2. Fan out + synthesize (the workflow)

This step spawns one reader agent per batch and a synthesis agent — a genuine
multi-agent fan-out over potentially hundreds of sessions. **It burns real tokens
(often 1M+ for a full corpus), so it is opt-in: only run it when the user has
asked for the mining pass** (which, if they invoked this skill, they have).

Run the bundled workflow, passing the focus, an orienting context paragraph, the
category set, and the prep outputs:

```
Workflow({
  scriptPath: "<this-skill-dir>/mine.workflow.js",
  args: {
    focus:      "the kitsoki-dev story (dev-story hub + bugfix/feature pipelines)",
    context:    "<one paragraph: what the focus IS, its parts and vocabulary, so readers recognize relevant signal and ignore noise>",
    categories: ["feature","pain","design","abandoned"],
    batchDir:   "<BATCHDIR from prep.py>",
    batchCount: <BATCHES from prep.py>,
    title:      "<focus> — ideas mined from chats"
  }
})
```

The `context` paragraph is the single biggest quality lever — it tells the readers
what the focus is, names its sub-parts and vocabulary, and lists what to treat as
noise. Write it specifically; a vague focus yields a vague brief.

The workflow returns `{ tracesRead, rawFindingCount, headline, themes, rawFindings }`.
Each theme carries priority (now/soon/later), rationale, categories, target,
summary, supporting_ideas, session_count, and sessions.

### 3. Render the brief (deterministic)

Pipe the workflow result through `focus_brief.py` to get a ranked Markdown brief.
Save it under `.context/` (transient, per repo convention — never commit):

```sh
python3 tools/session-mining/focus_brief.py <workflow-result.json> \
  --title "<focus> — ideas mined from chats" \
  --subtitle "<N> sessions, <M> findings -> <K> themes" \
  > .context/<focus>-ideas-from-chats.md
```

The result JSON is the workflow's task output (the `<task-id>.output` file, or any
file containing `{headline, themes}` or `{result:{headline, themes}}`).
`focus_brief.py` html-unescapes prose, sorts themes now→soon→later (by session
count within a tier), and is byte-deterministic for a given synthesis.

### 4. Cross-reference (optional)

If the user wants to act on the brief, map each theme to existing coverage before
filing anything. Inventory `docs/proposals/*.md` (read each Status line) and
`docs/stories/*.md`, then label every theme:

- **SHIPPED** — a proposal/doc owns it and the work largely landed.
- **PROPOSED (unbuilt)** — a proposal owns it but little is built. *Prioritize, don't re-file.*
- **PARTIAL** — partly covered; a real slice is uncovered.
- **GAP** — no proposal/doc owns it. *Candidate for a new ticket/proposal.*

Write the cross-reference to `.context/<focus>-ideas-crossref.md`. The point is to
avoid filing duplicate tickets for things that already have a home.

### 5. File tickets for the gaps (optional)

For the high-priority **GAP** themes (not the ones already owned by a proposal),
file tickets in `issues/` per the format in `issues/README.md` /
`docs/stories/bugs.md`:

- filename `issues/bugs/<RFC3339-utc-compact>-<slug>.md` (use `date -u +%Y-%m-%dT%H%M%SZ`)
- frontmatter: `id` (= filename sans `.md`), `title`, `target: kitsoki`,
  `filed_at` (RFC3339), `status: open`, `severity: P0..P3`, `component`,
  `kitsoki_rev` (= `git rev-parse --short HEAD`), `trace_ref: ""`, `external: {}`,
  `assignee: ""`, `url`.
- body: Body / Steps to reproduce / Expected vs actual / Proposed fix sketch /
  Severity rationale / Files involved. Cite the mining brief.

Tickets filed here are immediately searchable + workable from
`kitsoki run stories/kitsoki-dev/app.yaml` (the dogfood reads `issues/`).
Themes that are **design**-shaped (a new mechanism, not a defect) usually want a
proposal via the `proposal-authoring` skill, not a bug ticket — say so rather than
forcing them into the bug tracker.

## Quality notes

- **Recurrence is the ranking signal.** The same idea surfacing across many
  sessions matters more than one emphatic mention. The synthesis counts distinct
  sessions per theme; trust that over raw finding counts.
- **Keep the weak signals.** Single-session inferences land as `later` themes, not
  dropped — the user asked to review *all* their chats, so nothing real is silently
  truncated.
- **Scale the fan-out to the ask.** "Quick look at recent chats" → `--sample
  recency --max 40`. "Review everything" → no cap. The batch count from prep.py is
  the reader-agent count; ~200KB/batch keeps each reader comfortable.
- **Re-runs are cheap to render.** prep.py + the workflow are the expensive steps;
  once you have the result JSON, re-render the brief with different `--title` /
  ordering for free.

## Files

```
tools/session-mining/prep.py          distill + (optional redact) + bin-pack into batches; prints BATCHES=/BATCHDIR=
tools/session-mining/focus_brief.py   render synthesis JSON -> ranked Markdown brief (deterministic)
tools/session-mining/distill.jq       raw JSONL -> compact action trace (shared with pattern-mining)
.agents/skills/session-idea-mining/mine.workflow.js   the fan-out + synthesis workflow (parameterized by args)
```

See `tools/session-mining/README.md` for the sibling pattern-mining mode (shareable,
redacted, determinism-ladder) and its safety model. For driving a kitsoki **story**'s
tests + features from real transcripts (intent mining → coverage verdicts), use the
**`story-coverage-mining`** skill instead — a different shape (scoped, story-driven,
no-LLM-testable).
