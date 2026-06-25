---
name: story-coverage-mining
description: Drive a kitsoki story's tests + features from real Claude Code transcripts. Mine the intents users actually drove, recover each command's real outcome and whether the user corrected it, then map each in-scope intent to a coverage verdict (CONFORMS / DIVERGES / FIXTURE-GAP / COVERAGE-GAP / OUT-OF-SCOPE) and ticket the gaps. Use when the user says "what does <story> actually cover", "mine transcripts for <story> gaps", "drive <story>'s fixtures from real sessions", "what is <story> missing or getting wrong", or wants evidence-backed conformance for a story. This is STORY-DRIVEN coverage mining — distinct from session-idea-mining (topical scatter) and the shareable pattern-mining in tools/session-mining/README.md.
---

# Story coverage mining

Point the [`session-mining`](../../../tools/session-mining/README.md) intent
pipeline at a kitsoki **story** and ask: of the workflows people actually drive in
real Claude Code sessions, which does the story model correctly, which does it get
*wrong*, and which is it *missing*? It turns recorded reality into a prioritised
list of fixtures to add and rooms to fix.

**Read these first — don't reinvent them:**
- The loop (mine → map → author) and the five verdicts:
  [`docs/stories/story-coverage-mining.md`](../../../docs/stories/story-coverage-mining.md).
  This is the authoritative narrative; this skill is the *driver*, not a re-statement.
- The worked instance: [`tools/session-mining/examples/git-ops/`](../../../tools/session-mining/examples/git-ops/)
  — a committed corpus, a `run.sh`, and a filled `coverage.worked.md`.

## Start with the flagship (no cost, ~5s)

Before mining a new story, run the git-ops flagship once to see the whole
deterministic spine end-to-end with **no LLM**:

```sh
bash tools/session-mining/examples/git-ops/run.sh
```

Then read `examples/git-ops/coverage.worked.md` — the worksheet with verdicts
filled in. That is the shape every other story's output takes.

## Prerequisite: the per-story profile

Each story self-describes its mine via `stories/<story>/mining.profile.yaml`. It is
**scope configuration, not an auto-classifier**: `scope.{grep,sample,max}` (the
recall-only prefilter), the in-scope `action_tags`, the `owns` candidate-room map (a
*starting point* for the human map, never an assertion), and `non_goals` +
`non_goal_markers` (matched against `user_text`, since the coarse action tags can't
separate a non-goal from in-scope work). If the story has no profile, author one by
copying [`stories/git-ops/mining.profile.yaml`](../../../stories/git-ops/mining.profile.yaml).

The profile YAML is a **subset** (indent-nested maps, scalars, flow lists
`key: [a, "b c", c]`). Block lists (`- item`) and tab indentation are rejected with
an error — use flow lists and spaces.

## The mine → map loop

Run from `tools/session-mining/`. Only **step B touches an LLM** — gate it; every
other step is deterministic and free.

```sh
JOB=cover-<story>-$(date +%Y%m%d)
PROJ=~/.claude/projects/<repo-slug>        # one dir per repo
JOBDIR=../../.artifacts/session-mining/$JOB

# A. distill + scope by the profile (recall-only prefilter; this mode is local)
python3 prep.py "$PROJ" --job "$JOB" --sample recency --max 25 \
  --grep <word> --grep <word> ...          # the profile's scope.grep words

# B. the ONE LLM step — the strictly-validated oracle pass (schema-constrained).
#    Workflow({ scriptPath: "tools/session-mining/intents.workflow.js", args:{
#      batchDir: "$JOBDIR/batches", batchCount: <BATCHES from prep.py>,
#      outDir: "$JOBDIR/oracle" } })

# C-F. deterministic spine (run outcomes BEFORE emit so --outcomes can attach them)
python3 ground.py     --oracle "$JOBDIR/oracle"   --traces "$JOBDIR/traces" --out "$JOBDIR/grounded.json"
python3 tag_score.py  --grounded "$JOBDIR/grounded.json" --traces "$JOBDIR/traces" --out "$JOBDIR/scored.json"
python3 outcomes.py   --raw "$PROJ" --out "$JOBDIR/outcomes.json"
python3 emit.py       --scored "$JOBDIR/scored.json" --traces "$JOBDIR/traces" \
                      --raw "$PROJ" --outcomes "$JOBDIR/outcomes.json" --out-dir "$JOBDIR" --job "$JOB"
python3 verify_link.py "$JOBDIR" && python3 validate_reports.py "$JOBDIR"

# G. mechanical data-prep → the worksheet SKELETON + scope-filtered intents
python3 coverage_prep.py --job-dir "$JOBDIR" --profile ../../stories/<story>/mining.profile.yaml --out-dir "$JOBDIR"
```

Then the **irreducible human/LLM step** — fill the worksheet (`$JOBDIR/coverage.md`):

- **② Map.** For each in-scope intent, read the candidate room's bash against the
  **recovered outcome** (`outcome.is_error`/stdout) and the **`satisfaction`** flag,
  and assign one verdict. `coverage_prep.py` assigns *no* verdicts and reads *no*
  room bash — that is this step. A `satisfaction.corrected:true` intent (succeeded
  on exit 0 yet the next turn corrected it) is the loudest pointer to a missing gate.
- **③ Decide.** Rank FIXTURE-GAP / COVERAGE-GAP by frequency × mechanicalness (the
  `groups` block in `intents.git.json` gives the arg-aware frequency).
- **④ Author.** Author flow fixtures / new rooms; the recorded outcome is the
  evidence the expectation is right. Ship green `kitsoki test flows <story>`.

The honest ceiling (see the doc): the recovered outcome is *raw git*; the story's is
*bound world*. Matching them is a grounded human judgment, evidence-backed — not a
string compare.

## No-LLM testing rule (load-bearing — [AGENTS.md](../../../AGENTS.md))

Automated tests must **never** call a live LLM or incur cost. A coverage test runs
the deterministic spine over a **committed** corpus + a frozen `oracle.json` (the
step-B output), exactly like `tests/test_git_ops_coverage.py`. Flow fixtures stub
the oracle/host gates via cassettes — never a real oracle. The one-time live mine
(step B against real transcripts) is a manual, gated action — only when explicitly
requested, never automatically.

## Files

| Path | What |
|---|---|
| `docs/stories/story-coverage-mining.md` | the authoritative loop + the five verdicts |
| `stories/<story>/mining.profile.yaml` | per-story scope config (copy git-ops') |
| `tools/session-mining/coverage_prep.py` | mechanical data-prep → `coverage.md` skeleton + `intents.git.json` |
| `tools/session-mining/examples/git-ops/` | the flagship: corpus + `run.sh` + `coverage.worked.md` |
| `tools/session-mining/tests/test_git_ops_coverage.py` | the no-LLM end-to-end test pattern |

## Maintenance

Codex discovers this skill directly. After adding/moving it, re-link into Claude
Code's `.claude/skills/`:

```
make setup
```
