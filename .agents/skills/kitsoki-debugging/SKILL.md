---
name: kitsoki-debugging
description: Diagnose kitsoki-dev dogfood / bugfix / feature-story bugs. Use when the user reports "going back to idle", "silent bounce", "stuck at <state>", or any "this room didn't do what I expected" complaint. Drives the same state machine the TUI uses, against the real on-disk repo state, without spinning up a session. Surfaces the underlying host-call errors that the TUI's `on_error:` arcs swallow.
---

# Kitsoki Debugging

The TUI hides almost everything useful behind `on_error: idle` arcs. When the user says "it keeps going to idle", **assume the TUI is lying about what really failed** and reach for the tools below.

## When you read a user's "bounce to idle" report

You have five primary tools, in order of cost:

1. **`kitsoki trace --turns [--app <app>]`** — a compact per-turn digest of a real session: input → which routing tier resolved it (and why) → host calls → the **prompt each oracle verb dispatched** → editor context → on_error redirects → errors → outcome. This is the "what actually happened to my turn" view; it collapses a grep+jq loop into one command. **Use first to read a user's session.** No path needed — it resolves the newest session under `~/.kitsoki/sessions/` (filter with `--app kitsoki-dev`, or pass a session-id substring). Add `--turn <n>` to focus one turn and print its dispatched prompt **in full**.
2. **`kitsoki turn`** — one-shot a turn against the real repo, dump host calls + errors as JSON. Cheap, repeatable, runs against the actual on-disk state. **Use to reproduce in isolation.**
3. **Raw trace JSONL** (`kitsoki trace <file>` or `jq`) — every `machine.transition`, `host.on_error.redirect`, `machine.effect.applied`, `oracle.call.start`, `ide.context_captured`. **Use to confirm the digest and dig into a specific event.**
4. **Go tests under `internal/orchestrator/dogfood_smoke_test.go`** — `t.TempDir()` + real `git init` + real host registry, oracle stubbed. **Use to lock in regressions.**
5. **The actual TUI** — slow, hard to script, hard to inspect mid-flight. Use last.

## Two failure classes — pick the right lens

- **Bounce / stuck / silent fail** — a host call errored and `on_error:` swallowed it, or the intent was rejected. The state didn't go where expected. → §"Step 1" (`kitsoki turn`), §3 (store events).
- **The turn *ran* but did the wrong thing** — `next_state` advanced cleanly, no error, yet the output is wrong or empty (the LLM "didn't see" something; free text mis-routed; a feature silently no-op'd). **`next_state` advancing proves nothing about whether the work happened.** → read on.

### The turn ran but did the wrong thing

The single source of truth for *what the model actually saw* is the **dispatched prompt**, recorded as `oracle.call.start` (`payload.prompt`). If context you expected (an editor selection, a world value, a prior answer) isn't in that prompt, it never reached the model — regardless of what the host verbs returned upstream.

```sh
# The per-turn story — start here (newest kitsoki-dev session, no path needed):
kitsoki trace --turns --app kitsoki-dev

# Focus one turn and see the FULL prompt the model received:
kitsoki trace --turn 3 --app kitsoki-dev

# A specific session by id prefix:
kitsoki trace --turns 7ca57b33

# Raw jq, when you need a field the digest doesn't surface:
jq -r 'select(.kind=="oracle.call.start") | .payload.prompt' <session>.jsonl          # exact prompt
jq -c 'select(.kind=="turn.start") | {input:.payload.input, routed_by:.payload.routed_by, match_type:.payload.match_type}' <session>.jsonl   # why it routed
jq -c 'select(.kind=="ide.context_captured") | .payload' <session>.jsonl              # editor context
```

Provenance vocabulary on `turn.start`: `routed_by` ∈ {`deterministic`, `semantic`, `llm`, `default`, `turncache`}; `match_type` names the synonym/template or `free_text` for the default-intent tier. A `routed_by: "default"` means free text fell through to the room's `default_intent` sink (see semantic-routing.md §1.5). If an input you expected to converse instead shows `routed_by: "deterministic"` to `look`, that's a routing miss, not a prompt bug.

**Boundary caveat:** `kitsoki turn` drives the orchestrator + host layers, but **NOT** the TUI command surface — slash dispatch (`handleSlashCommand`), ambient capture (`captureIDEAmbient`), the prompt textarea. A bug there (a panic in `/foo`, a selection not captured) won't reproduce via `kitsoki turn`; it needs a TUI-level test (`internal/tui/*_test.go`) or the live TUI. When the trace shows a turn never started for an input the user typed, suspect the TUI layer.

## Step 1 — reproduce with `kitsoki turn`

Build a fresh binary into `/tmp` so you're not racing the user's running TUI:

```sh
go build -o /tmp/kitsoki-fixed ./cmd/kitsoki
```

Then dump the user's world state into a JSON file. The trace JSONL has every `turn.done` event with a `view_rendered` field — its prelude tells you state path, workspace_id, feature_branch, etc. Build the world file from those values:

```sh
cat > /tmp/world.json <<'EOF'
{
  "core__bf__ticket_id":          "<from trace>",
  "core__bf__workspace_id":       "bf-<ticket>",
  "core__bf__feature_branch":     "fix/<ticket>",
  "core__bf__workdir":            ".worktrees/bf-<ticket>",
  "core__bf__base_branch":        "main",
  "core__bf__bf_autostart_attempted": true,
  "core__bf__bugfix_mode":        "full",
  "core__bf__judge_mode":         "human",
  "core__bf__judge_confidence_threshold": 0.8,
  "core__bf__propose_fix_artifact": {
    "summary_title":    "...",
    "summary_markdown": "...",
    "affected_files":   ["..."],
    "confidence": 0.9
  }
}
EOF
```

Then fire the turn:

```sh
/tmp/kitsoki-fixed turn stories/kitsoki-dev/app.yaml \
  --state core.bf.proposing \
  --intent core__bf__accept \
  --world @/tmp/world.json \
  | python3 -c "import json,sys; d=json.load(sys.stdin); print('next:',d.get('next_state'),'err:',d.get('error_message')); [print(c.get('namespace'),'->',(c.get('error') or 'ok')[:140]) for c in d.get('host_calls',[])]"
```

What you get:
- `next_state` — where the session actually lands (vs. the user's report).
- `error_message` — set when the intent itself is rejected (INTENT_NOT_ALLOWED, GUARD_FAILED, MISSING_SLOTS).
- A list of every host invocation with its full error string. **This is the layer the TUI's `on_error:` arcs swallow.**

If `next_state` is the room the user expects, the bug is fixed (or never existed against current code) and the user is running a stale binary — tell them to rebuild. If it's something else, the host call that errored tells you exactly where to look.

### Gotchas with `kitsoki turn`

- World keys are fully-qualified after import-folding: a bugfix-story var named `workspace_id` becomes `core__bf__workspace_id` when invoked through `stories/kitsoki-dev/app.yaml`. Get the names wrong and the room's `on_enter:` chain sees defaults.
- Intent names are also import-folded: `core__bf__accept`, not `accept`.
- Skipping a required `propose_fix_artifact` (etc.) on the world doesn't fail the intent — it fails some host call inside the target room's `on_enter:` with a confusing template render error. If you see `effect ... render` errors, your world is incomplete.
- `--input "..."` routes through the real LLM harness (claude-cli) and burns budget. **Use `--intent` for diagnosis.**

## Step 1.5 — `kitsoki turn` does NOT write `KITSOKI_TRACE_FILE`

The `turn` subcommand has no slog destination wired in. Setting
`KITSOKI_TRACE_FILE=…` does nothing under `kitsoki turn` — only the TUI
(`run`) and `session create/continue` write events to the trace file.

What `kitsoki turn` does give you, per call, is the **full JSON output**:
`host_calls[]` (each with `args`, `data`, and `error` when it failed),
`effects_applied[]`, `next_state`, `world_before`/`world_after`, and
`view_rendered`. Aggregate those into your own JSONL if you need to
inspect a multi-turn drive:

```sh
kitsoki turn app.yaml --state X --intent Y --world @w.json \
  | tee -a /tmp/my-turn-trace.jsonl >/dev/null
```

For multi-turn drives where you want the canonical event stream
(`turn.start`, `harness.request`, `machine.transition`,
`host.on_error.redirect`, …) **use `kitsoki session create` +
`kitsoki session continue --intent <name>`** with
`KITSOKI_TRACE_FILE=/tmp/foo.jsonl`. The session subcommand persists
to SQLite and emits the trace events `turn` doesn't.

Trade-off: `session` carries a persistent state machine across calls
(useful), but you have to think about session keys and locks. `turn`
is stateless and you thread `--world` yourself between calls.

## Discover and inspect existing sessions

Every `kitsoki run` or `session` invocation writes a JSONL trace to `~/.kitsoki/sessions/<app>/` automatically — no `KITSOKI_TRACE_FILE` flag needed. This means any session the user ran can be discovered and inspected later:

```sh
# List all discovered sessions by app
ls -la ~/.kitsoki/sessions/

# List traces for a specific app (e.g., kitsoki-dev)
ls -la ~/.kitsoki/sessions/kitsoki-dev/

# Inspect the latest session for an app
tail -f ~/.kitsoki/sessions/kitsoki-dev/*.jsonl | grep 'turn.done' | jq -c '.event'
```

To diagnose a user's session after the fact:

1. **Locate the trace**: find the `.jsonl` file in `~/.kitsoki/sessions/<app>/` that corresponds to when the user ran the session (check file timestamps).
2. **Find the bounce**: grep for `host.on_error.redirect` or the last `turn.done` in the user's reported session.
3. **Extract world state**: the `turn.done` event includes `view_rendered` with the `world` snapshot at that moment.
4. **Replay**: use `kitsoki turn` with the extracted world to reproduce the error in isolation.

Example:

```sh
# Find what turn the user got stuck on
grep 'turn.done' ~/.kitsoki/sessions/kitsoki-dev/8e8f94c9-*.jsonl | tail -5 | jq -c '{turn, state_path, next_state}'

# Check for redirects that indicate on_error fired
grep 'host.on_error.redirect' ~/.kitsoki/sessions/kitsoki-dev/8e8f94c9-*.jsonl

# Extract the world from the last turn and dump it
grep 'turn.done' ~/.kitsoki/sessions/kitsoki-dev/8e8f94c9-*.jsonl | tail -1 | jq '.view_rendered.world' > /tmp/world-at-bounce.json
```

## Step 2 — cross-check the trace

`/tmp/kitsoki-dogfood-trace.jsonl` (path is `KITSOKI_TRACE_FILE` env or the TUI's default) is a slog JSONL log. Useful greps:

```sh
# Every turn boundary the user saw, latest first
tac /tmp/kitsoki-dogfood-trace.jsonl | grep -m 10 turn.done | jq -c '{turn, state_path, new_state}'

# Did an on_error redirect fire? (added in this skill's era)
grep host.on_error.redirect /tmp/kitsoki-dogfood-trace.jsonl | tail

# What host calls fired during a specific turn?
grep '"turn":6' /tmp/kitsoki-dogfood-trace.jsonl | grep 'effect.applied.*invoke'
```

Note: `HostReturned` is a **store event**, not a slog log. It carries the actual error from the handler, but the trace JSONL does NOT include it. To see host errors you must either use `kitsoki turn` or inspect the store events directly.

## Step 3 — host errors live in the store events, not the trace

If you need to inspect what really happened in a *real session* (not a one-shot), pull store events from the persisted DB:

```go
hist, _ := s.LoadHistory(sid)
for i := len(hist)-1; i >= 0; i-- {
    if hist[i].Kind == store.HostReturned {
        t.Logf("%s", string(hist[i].Payload))
    }
}
```

A `HostReturned` event with an `"error"` field is a host call that failed. If the source room had `on_error: <target>`, that's where you bounced — even though the trace shows no transition log for it (the redirect is logged as `host.on_error.redirect` in the orchestrator's slog, separate from `machine.transition`).

## Step 4 — pin the fix with a Go test

`internal/orchestrator/dogfood_smoke_test.go` has the pattern:

- `setupDogfoodRepo(t)` builds a real `git init` repo at `t.TempDir()` and copies the live `stories/` + `issues/` trees into it.
- `newSmokeOrchestrator(t, repoRoot)` wires the real host registry with the oracle stubbed (no LLM cost).
- Drive turns via `orch.SubmitDirect(ctx, sid, intent, slots)` exactly as the TUI does.
- Mutate the repo between turns (`os.RemoveAll`, `os.WriteFile`, `exec.Command("git", "worktree", "prune")`) to simulate real-world corruption shapes.

Two existing tests show the patterns:

- `TestDogfoodSmoke_ContinueFromProposingReachesImplementing` — pruned-worktree shape (dir + registration gone).
- `TestDogfoodSmoke_ProposingAccept_RegisteredWorktreeDirtyTree` — registered-worktree + dirty unrelated file shape (the trickier production case where path-comparison bugs hide).

## Step 5 — hand the user a warp to verify, not a vague proposal

When the diagnosis is done and you've confirmed the fix against current code, **do not** close with a hand-wavy "want me to change X?" or "you could re-answer that question." The user lost their place in a multi-turn session; an abstract proposal makes them reconstruct it by hand. Instead, **reconstruct their exact state from the trace and hand them a runnable warp** so they resume in one command and verify the fix live.

A "warp basis" is a small YAML (`state:` + `world:`) that `kitsoki run --warp <file>` applies at session boot. It teleports a **fresh** session straight into a primed mid-flow state. Crucially, `Teleport` (`internal/orchestrator/teleport.go`) only **re-renders the view — it does NOT fire `on_enter`**, so any expensive `on_enter:` chain (analyst/decide/task LLM calls) is skipped and your seeded world is *not* overwritten. That's exactly what you want for "drop me back where I was, with the corrected state."

Recipe:

1. **Pull the real world from the trace.** Every `world.update` event carries a `set:` payload; replay them to reconstruct the accumulated world (idea, the `decide` result object, the operator's answers, counters). `harness.returned` events hold the host-call `data` (e.g. the analyst's `clarifications`). Strip oracle sentinel markers (`⁣⁡…`) from any LLM-authored text.
2. **Seed the *expected* (post-fix) values**, not the buggy ones the trace captured. The whole point is to encode the state the user *should* have had. (E.g. if a missing effect left `answered_ids` empty, write `answered_ids: "|1|"` in the warp.)
3. **Write it to `.context/<name>.yaml` by default** — `.context/` is gitignored, so a one-off "recreate my session" warp stays ephemeral and doesn't clutter the repo. **Only** write to `stories/<app>/scenarios/<name>.yaml` (git-tracked) when the warp is meant to be *reusable* — a demo scenario or a regression-test basis others should run. When unsure, default to `.context/`; it's trivial to promote later with `git mv`. Canonical fields either way: `name`, `description`, `state`, `world`. Nested objects/lists are fine (`World map[string]any`). The loader is `goyaml.Strict()`, so only those top-level keys are allowed.
4. **Verify it before handing it over.** `kitsoki turn <app> --state <warp-state> --intent look --world @<world.json>` renders the destination view with NO `on_enter` — the Teleport-equivalent. Assert the fix is visible in `view_rendered` (e.g. the answered question dropped from the list). Convert the warp's `world:` block to the `--world` JSON with a one-liner: `python3 -c "import yaml,json;json.dump(yaml.safe_load(open('.context/x.yaml'))['world'],open('/tmp/w.json','w'))"`.
5. **Give the user the one-liner:**

   ```sh
   kitsoki run stories/<app>/app.yaml --warp .context/<name>.yaml
   ```

A warp doubles as a regression artifact: the same file is a flow-fixture-shaped basis (`initial_state`/`initial_world` are accepted aliases), so it can seed a smoke test later — which is exactly the kind of warp worth promoting from `.context/` into `stories/<app>/scenarios/`. See `stories/oregon-trail/scenarios/*.yaml` for the committed/reusable format.

## Patterns that hide bugs (and how to expose them)

| Symptom in TUI | Underlying cause | How to confirm |
|---|---|---|
| Bounce to idle, no diagnostic | Host call errored, `on_error: idle` fired silently | `kitsoki turn` — the `host_calls[]` array shows the actual error |
| Stuck in a room despite typing accept | Intent rejected (`INTENT_NOT_ALLOWED_IN_STATE`, missing slots, guard false) | `kitsoki turn` returns `mode:"rejected"` with `error_code` + `error_message` |
| Implementing crashes after a process restart | `bf_autostart_attempted=true` pinned but workspace gone | World has the flag; `git worktree list` doesn't show the dir |
| Commit fails with `git.commit: ` (empty message) | git's "nothing to commit" goes to **stdout**, not stderr; lenient-mode checks missed it | Check `gitCommit` in `internal/host/git_vcs.go` reads both streams |
| Worktree create says "already exists" but you can't find it | Path-comparison bug: relative vs absolute | `git worktree list --porcelain` always emits absolute; handlers must `filepath.Abs` or match by basename |
| Conversation "lost its state" / analyst forgot everything after a `/reload` | `on_enter` re-fired on reload and a non-idempotent `host.chat.create` spawned a fresh empty chat, overwriting the bound `*_chat_id` while world counters survived | Grep the trace for a `turn.end` with `outcome:"reloaded"`, then a `host.chat.create` (not `resolve`) firing right after and a new `chat_id` in `world.update`. Fix: use `host.chat.resolve` in `on_enter`. See state-machine.md §"`on_enter` must be idempotent". |
| "The model didn't see X" (a selection, the open doc, a world value) — yet the turn ran fine | The context never reached the dispatched prompt: a verb returned it but no prompt template/seam consumed it, OR an upstream parser returned empty against a real wire shape | `kitsoki trace --turns` → check the `prompt` line for that turn. If X isn't in it, it never reached the model. For host.ide.*, check `ide.context_captured` for `source:none` + `reason`. |
| Free text "did nothing" / re-rendered the room instead of conversing | Routed to a navigation intent (e.g. `look`) instead of the conversational sink | `turn.start.routed_by` / `match_type`. Fix: give the conversational room a `default_intent` (semantic-routing.md §1.5). |
| A feature "works" in tests but is broken live | A test double diverged from the real contract (stub returned invented shapes the parser was also written against), or the test asserted a verb result, not the model-facing prompt | Capture real wire bytes into the stub (the `ide.context_captured` `detail` field grabs raw editor envelopes); assert to the dispatched prompt; mutation-test the e2e (revert the fix → it must fail); add an opt-in live test (`//go:build ide_live`). |
| A dispatched agent seems stuck / a modal or question never appeared / the agent got blank answers | Headless `AskUserQuestion` auto-resolves *empty*, so it is hard-denied; real questions are forwarded via `mcp__operator__ask` **only when an `OperatorPrompter` is in ctx** (web/TUI run loops). No prompter (cassette/flow/headless) ⇒ no tool ⇒ the agent is told to proceed alone. | Grep the trace for `operator.question.asked` / `…answered` / `…unanswered` (each carries `question_id`, `headers`, `duration_ms`, `outcome`). No `asked` = no prompter attached (expected headless). `asked` but no `answered` = `unanswered` (timeout/cancel) — the agent got a tool error and proceeded. See §"Operator questions never reached the operator". |

## Operator questions never reached the operator

When a dispatched oracle agent "asks the user" but the operator saw no
modal — or the agent proceeded on blank answers — the cause is the
operator-ask forwarding bridge, not a room arc. Headless `claude -p`
auto-resolves the built-in `AskUserQuestion` with **empty** answers, so
it is hard-denied everywhere (`alwaysDeniedTools` in
`internal/host/agents.go`). Real questions are forwarded only through the
`mcp__operator__ask` tool, which the host attaches **only when an
`OperatorPrompter` is in the turn ctx** (the web/TUI run loops set it;
`kitsoki turn`, flows, cassettes, and `oracle-serve` do not). No prompter
⇒ no tool ⇒ the agent is instructed to decide on its own — by design.

Three greppable slog events tell the whole story (each carries
`question_id`, `headers`, `duration_ms`, `outcome`):

```sh
grep -E 'operator\.question\.(asked|answered|unanswered)' <session>.jsonl | jq -c '{kind, question_id: .payload.question_id, headers: .payload.headers, duration_ms: .payload.duration_ms, outcome: .payload.outcome}'
```

- **No `operator.question.asked`** — no prompter was attached. Expected
  for headless/cassette/flow runs; for a live TUI/web session it means
  the agent never invoked the tool (check the dispatched prompt has the
  system clause and `mcp__operator__ask` in allowed tools).
- **`asked` then `answered`** — the round-trip worked; the answer was
  returned to the agent as the tool result.
- **`asked` then `operator.question.unanswered`** — timeout, operator
  cancel, or ctx cancellation. The agent received a tool error ("proceed
  without this input") and continued, so the turn completes but the work
  reflects an *unanswered* question. `duration_ms` near the wait bound
  (~5 min) confirms a timeout. See
  [`docs/architecture/operator-ask.md`](../../architecture/operator-ask.md).

## A note on `on_error: idle` as an anti-pattern

The bugfix story's room arcs use `on_error: idle` heavily. This makes pipelines "fail safe" by landing back at a known parked state — but at the cost of erasing the diagnostic. Authors should be wary:

- A `on_error: idle` with no `last_error` surfacing in the destination view = silent failure.
- Always check that the destination room shows `world.last_error` somewhere so the operator gets a hint.
- For host calls whose failure modes are recoverable (e.g. "worktree already exists" → reuse, "no upstream tracking" → skip), prefer making the handler idempotent over relying on `on_error:` redirects.

## Happy-path tests are not enough

A test that asserts `next_state` advanced is NOT enough to prove the room did its job. Rooms can advance after running a no-op `on_enter:` chain — the user sees a clean transition while none of the actual work happened.

Concrete trap (regression-of-record): the bugfix `implementing` room was supposed to apply the proposed fix to the worktree. For months the room's `on_enter:` was `workspace.sync + vcs.commit + say "Fix applied"` — no oracle, no edits, no actual code change. Tests that only checked `next_state == implementing` (and `next_state == testing` on accept) passed every time. The bug surfaced only when the user noticed the testing room reporting "Fix not applied — repro tests still fail; files unchanged."

When writing smoke tests for a room that *should* produce side effects, assert the side effects directly:

- After a commit-step, run `git show --name-only HEAD` and assert the file you expected to be committed is there.
- After an oracle-edit step, write a real file from the stub and check the file lands in HEAD.
- After a workspace.create, `stat` the dir.
- After a host call that binds, check the world key got bound.

If your test would pass against a room whose `on_enter:` is empty, your test isn't testing what you think.

## Type-assertion landmines in handler args

YAML `{{ world.x }}` references where `x` is a list render as the underlying Go slice. Depending on how the runtime resolved the value, that slice may arrive at the handler as `[]any` *or* `[]string`. A handler that only checks `args["files"].([]any)` silently treats `[]string` as no-list-passed and falls through to its default behavior. Look at `gitCommit` in `internal/host/git_vcs.go` for the both-shapes pattern.

Same trap applies to `[]int`, `[]map[string]any`, nested `[]any` of any element type. When debugging "this handler dropped my list", check both type shapes before anything else.
