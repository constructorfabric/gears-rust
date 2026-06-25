# Kitsoki Story Authoring

A "story" is a directory the engine loads as one app: `app.yaml` (the
manifest), `rooms/*.yaml` (state definitions, glued in via `include:`),
`prompts/*.md` (LLM templates), `views/*.pongo` (typed-element base
templates), `flows/*.yaml` (Mode-2 deterministic tests), optional
`schemas/*.json` (typed-JSON contracts for `decide`/`task`/`ask`), and an
optional `README.md`.

The gold-standard reference stories live in this repo. Read them in
this order — most authoring questions resolve by mimicking the closest
existing room:

| Story | Why look here |
|---|---|
| `stories/oregon-trail/` | The canonical typed-elements view layout, status bar, multi-import composition (imports `frontier_event` which imports `robbery`). |
| `stories/bugfix/` | Operator-facing pipeline shape: phase rooms with `on_enter` oracle calls, `iface.*` host_interfaces, `accept`/`refine`/`restart_from` checkpoint intents, `@exit:done` / `@exit:abandoned`. |
| `stories/robbery/` | Smallest complete importable sub-story — `host_interfaces`, `exits` with `requires:`, `world_in:`, an importer README contract. |
| `stories/dev-story/` | Live-result lists (`iface.ticket.search` bound into a `code:` `{% for %}`), readiness banners with `available()` / `blocked_reason()` helpers. |

The authoritative schema is `kitsoki docs app-schema`. The authoring
prose lives at `docs/stories/authoring.md`, the style guide at
`docs/stories/story-style.md`, the state-machine semantics at
`docs/stories/state-machine.md`, the imports / composition reference at
`docs/stories/imports.md`, and the host registry at `docs/architecture/hosts.md`. When in
doubt about a field, **grep the gold-standard stories first; consult
the schema doc second; ask the user last**.

## 1. Anatomy of a story

```
stories/<name>/
├── app.yaml                  manifest — app/world/intents/root/include
├── rooms/                    one file per logical room; merged via include:
│   └── <room>.yaml
├── prompts/*.md              oracle prompt templates (Go-template + pongo)
├── views/                    optional; typed-element base templates
│   ├── base.pongo            block contract: status / heading / body / choices / footer
│   └── partials/             reusable {% include %}-able fragments
├── schemas/*.json            JSON-schema contracts for decide/task/ask
├── flows/*.yaml              Mode-2 deterministic test fixtures
├── scenarios/*.yaml          optional; warp bases for operator smoke tests
└── README.md                 mandatory if the story is importable
```

## 2. Top-level shape of `app.yaml`

```yaml
app:
  id: my-app
  version: 0.1.0
  title: "Short human title"
  author: "Owner"
  license: "CC0"

# Every host.* the story invokes must be listed here. iface defaults
# (declared under host_interfaces:) are added implicitly by the loader.
hosts:
  - host.oracle.decide
  - host.inbox.add

# Provider-neutral capability surfaces. Importers rebind via host_bindings.
host_interfaces:
  ticket:
    operations:
      get: { input: { id: string }, output: { id: string, title: string } }
    default: host.local_files.ticket

# Typed key/value bag. EVERY world.* read in any view, effect, or guard
# MUST be declared here with a type and default; loader rejects unknowns.
world:
  ticket_id:    { type: string, default: "" }
  cycle:        { type: int,    default: 0 }
  judge_mode:   { type: string, default: "human" }
  artifact:     { type: object, default: {} }
  party_alive:  { type: int,    default: 5 }
  ready:        { type: bool,   default: false }

intents:                       # global intent library
  accept:
    description: "Advance to the next phase."
    examples: ["continue", "accept", "lgtm"]
    priority: 85
    slots:
      feedback: { type: string, required: false }

exports:                       # intents lifted into a parent's table
  intents: [accept, refine, quit, look]

exits:                         # named return points for an importer
  done:
    requires: [done_artifact]  # static check on every @exit:done transition
  abandoned: {}

root: idle                     # initial state

include:
  - rooms/*.yaml               # merge each file's `states:` block in
```

`include:` merges files into one flat AppDef (same namespace, same
world). `imports:` is a separate mechanism for embedding an *aliased*
sub-story with private world and explicit boundary projections — see
§7 below.

## 3. The shape of a room (state)

```yaml
states:
  proposing:
    description: "Draft the fix proposal; review and advance to implementing."
    relevant_world: [ticket_id, artifact, cycle]   # location indicator keys
    view:
      extends: "base"
      blocks:
        body:
          - kv:
              pairs:
                Ticket:     "{{ world.ticket_id }} — {{ world.ticket_title }}"
                Confidence: '{{ world.artifact.confidence|default:"(pending)" }}'
          - heading: "Artifact"
          - code: '{{ world.artifact.summary_markdown|default:"(pending — oracle has not returned)" }}'
        choices:
          - heading: "Actions"
          - list:
              items:
                - label: "continue"
                  hint:  "post the proposal and advance"
                - label: "refine feedback=…"
                  hint:  "re-draft with feedback"
                - label: "quit"
                - label: "look"
    on_enter:
      - invoke: host.oracle.decide
        with:
          prompt: prompts/proposing_executing.md
          schema: schemas/proposing_artifact.json
          working_dir: "{{ world.workdir }}"
          args:
            ticket_id:    "{{ world.ticket_id }}"
            ticket_title: "{{ world.ticket_title }}"
        bind:
          artifact: submitted
        on_error: idle
    on:
      accept:
        - target: implementing
          effects:
            - set: { cycle: 0 }
      refine:
        - when: "world.cycle >= 3"
          target: "@exit:abandoned"
          effects:
            - set: { abandon_reason: "proposing_budget_exhausted" }
        - target: proposing
          effects:
            - set:
                refine_feedback: "{{ slots.feedback }}"
                cycle:           "{{ world.cycle + 1 }}"
      quit:
        - target: "@exit:abandoned"
      look:
        - target: .
```

The shape rules:

- **Order matters inside each intent's transition list.** First matching
  `when:` wins. Always end with `default: true` (the catch-all) — a missing
  default lets `GUARD_FAILED` reach the user.
- `target: .` means "stay in the same atomic state" — use for `look` and
  for any read-only intent.
- `target: "@exit:<name>"` is the importable exit form; the loader
  rewrites it based on the parent's `imports.<alias>.exits.<name>.to` (or
  synthesises a terminal in standalone mode).
- `relevant_world:` keys MUST exist in the top-level `world:` schema.

### Compound and parallel states

```yaml
bar:
  type: compound
  initial: dark               # required; supports {{ world.x }} templating
  states:
    dark: { ... }
    lit:  { ... }
```

```yaml
game:
  type: parallel
  states:
    lighting: { type: compound, initial: bright, states: { ... } }
    narrator: { type: compound, initial: idle,   states: { ... } }
```

Children inherit the parent's `on:` bindings unless overridden. `emit:
foo` from one parallel region is observed as an event by siblings.

## 4. Effect vocabulary

Effects run in declaration order inside one transition. The world is
immutable per turn; later effects see the snapshot built by earlier
ones.

| Effect | Shape | Notes |
|---|---|---|
| `set` | `set: { k: "{{ ... }}", k2: 7 }` | Templated. Strings are pongo; numerics/bools render as their Go literal. |
| `increment` | `increment: { counter: 1 }` | Integer delta, +/-. |
| `say` | `say: "..."` | Appends a narrative line to the rendered view. Templated. |
| `emit` | `emit: event_name` | Broadcasts to parallel siblings. |
| `emit_intent` | `emit_intent: accept` + optional `slots: { ... }` | Dispatches a synthetic intent against the current state — used to auto-advance from `on_enter` after a confident LLM judge. Mutually exclusive with `target:`. Depth-capped at 8. |
| `invoke` | `invoke: host.X` + `with:` `bind:` `on_error:` `once:` | See §5. `once: true` skips the call on re-entry while its `bind:` targets are already set — reload-safe (§10). |

`when:` on an effect (not just on a transition) gates that single
effect — useful inside `on_enter:` for conditional host calls.

`once: true` on an `invoke:` makes it **idempotent on re-entry**: the
engine skips the call when every one of its `bind:` targets is already
set (non-empty — `nil`/`""`/`{}`/`[]` count as unset), so `/reload`,
self-transitions, and `on_error:` re-entry re-render from the cached
world instead of re-running an expensive, non-idempotent host call. The
bind target IS the cache — clear it (in the re-run intent's effects) to
force a fresh run. Requires a non-empty `bind:` (load error otherwise);
suited to object/string binds, not scalar `int`/`bool` (a real `0`/`false`
reads as "set"). The lean replacement for a hand `when: "<result> == ''"`
guard — see §10 and `docs/stories/state-machine.md` §"on_enter must be
idempotent".

## 5. Host calls (`invoke:`)

```yaml
- invoke: host.oracle.decide
  id: proposing_verdict        # optional call-site address (see below)
  with:
    prompt: prompts/proposing_executing.md
    schema: schemas/proposing_artifact.json
    working_dir: "{{ world.workdir }}"
    args:
      ticket_id:    "{{ world.ticket_id }}"
      ticket_title: "{{ world.ticket_title }}"
  bind:
    artifact: submitted        # copies Result.Data.submitted into world.artifact
  on_error: idle               # transition target if the handler errors
```

Rules:

- Every `invoke: host.X` requires `host.X` (or one of its prefix
  ancestors) in the top-level `hosts:` allow-list. The loader rejects
  the manifest otherwise. `iface.<name>.<op>` invocations are wired up
  implicitly via `host_interfaces.<name>.default`.
- `with:` arguments are templated. Map/slice values render as compact
  JSON (sorted keys for maps) when spliced into a string.
- `bind:` copies fields out of `Result.Data` into world keys. The right-hand
  side may be a bare key name (`bind: artifact: submitted`), a dotted
  path into the result (`party_member_1: "submitted.names[0]"`), or a
  templated expression.
- `id:` is an optional author-assigned address for the call site. When two
  invokes in one room share a handler name (e.g. an analyst and a judge both
  using `host.oracle.decide`), the `id:` is what lets a single flow fixture
  stub them apart (`host_handlers.<handler>.by_call.<id>`) and a cassette
  match them apart (`match: { call: <id> }`). It threads into the args under
  the reserved `call` key. **Never pick a different oracle verb just to dodge
  a stub-name collision** — that distorts the story to satisfy the harness;
  give the call an `id:` instead. Distinct from the deterministic 16-hex
  `call_id` trace correlator.
- `on_error:` redirects to the named state when the handler returns an
  error. The orchestrator sets `$host_error.{code,hint}` for the target
  state's first guard. **Beware the silent-bounce anti-pattern** — see §10.
- `background: true` runs the call asynchronously; pair with
  `on_complete:` (an effect list) for the completion turn. Result lands
  in `world.last_job_result` only inside `on_complete:`.

The built-in handlers (full reference in `docs/architecture/hosts.md`):

| Handler | Use for |
|---|---|
| `host.run` | Shell out (argv mode preferred when args come from world). Returns `{stdout, exit_code, ok, stdout_json}`. |
| `host.oracle.extract` | Tiered resolver: synonyms → slot_template → llm. Returns typed JSON + `resolved_by`. |
| `host.oracle.decide` | Typed LLM verdict; schema required; `submit` auto-attached; read-only tools. The canonical pattern for "Claude produces a structured artifact." |
| `host.oracle.ask` | Read-only one-shot prose call; schema optional. |
| `host.oracle.task` | Agentic write call with acceptance loop (schema required). |
| `host.oracle.converse` | Conversational, optionally chat-aware via `chat_id`. |
| `host.transport.post` | Post a message to a registered transport (tui / jira / bitbucket). |
| `host.inbox.add` | Mirror an artifact into the operator's local inbox. |
| `host.chat.*` | Persistent multi-turn chat threads scoped by `(app, room, scope_key)`. |

## 6. Views (the typed-element form)

**Always `view: extends: "base"`. Never a `view: |` string** unless
you're touching legacy code you can't migrate yet — typed elements
isolate render failures per-element and keep the chrome alive.

Base templates live at `views/base.pongo`; they define five blocks
(`status`, `heading`, `body`, `choices`, `footer`) that rooms override.

Element kinds — pick the right one, never reach for ANSI or backticks:

| Kind | Use for |
|---|---|
| `prose:` | One paragraph of narration. Reflows. |
| `heading:` | Section break. No trailing colon. Never bulleted. |
| `list:` | Bulleted actions or enumerations. Optional `hint:` column. |
| `kv:` | Short key/value status. Key column auto-aligns. |
| `code:` | Layout-preserved content (ASCII tables, `{% include %}`, `{% for %}` loops over a world array). |
| `template:` | Raw pongo — escape hatch for legacy / unported shapes. |

Every element can carry a `when:` guard; the renderer drops elements
whose guard is false. Use this to fan out per-element conditional
rendering instead of `{% if %}` inside a `view: |` string.

### Author checklist before shipping a new room

- [ ] `view: extends: "base"`, not `view: |`.
- [ ] Each paragraph is its own `prose:`; section breaks are `heading:`.
- [ ] Status pairs are in a `kv:`, never hand-aligned.
- [ ] Actions are a `list:` with `hint:` for cost/consequence.
- [ ] Empty / pending values use the lowercase parenthetical placeholders
      (`(pending)`, `(none)`, `(not yet chosen)`, `(empty — type X to search)`).
- [ ] `look` is the last action and `target: .`.
- [ ] The view renders to ≥ 1 visible line against an empty world `{}`.
      Action menu / reply prompt is unconditional.
- [ ] No `{% for %}` over a world key that might be absent / nil; use a
      typed `list:` or guard with `{% if %}`.
- [ ] The intent name IS the label — no backticks, no paraphrase.
- [ ] `look` needs no hint. `quit` always reads `"abandon the pipeline"`
      or similar.

### Action-menu availability — two standard shapes

**Affordability** (action always offered, hint shows cost):

```yaml
- label: "pay"
  hint:  "buy them off (${{ world.threat_level * 50 }})"
  when:  "world.party_money >= world.threat_level * 50"
- label: "pay"
  hint:  "not enough money (${{ world.threat_level * 50 }} needed)"
  when:  "world.party_money < world.threat_level * 50"
```

**Prerequisite** (action greyed-out until reachable):

```yaml
- label: "start the journey"
  when:  "available('start_journey')"
- label: "✗ start_journey — {{ blocked_reason('start_journey') }}"
  when:  "!available('start_journey')"
```

The `available(name)` / `blocked(name)` / `blocked_reason(name)` /
`intent_status(name)` helpers read the computed menu derived from the
state's `on:` bindings + each transition's first arm's guard +
`guard_hint:`.

### Two voices, never mixed inside a room

- **In-character** — Oregon Trail, Robbery. Full sentences, quoted
  dialog gets its own `prose:`. `say:` follows suit.
- **Operator-facing** — dev-story, bugfix, implementation, kitsoki-dev.
  Terse, declarative. "Bug-fix pipeline parked. Waiting for `start`."
  not "The bug glares at you menacingly."

### Placeholder vocabulary

Always lowercase, always in parentheses. Splice via `|default:` (pongo
filter) on the world reference:

| Meaning | Render as |
|---|---|
| Unset configurable value | `(not yet chosen)` |
| Empty list | `` (empty — type `tickets` to search) `` |
| Awaiting host result | `(pending)` or `(pending — <what's running>)` |
| Not applicable | `(n/a)` |
| Nothing / null / absent | `(none)` |

## 7. Imports, exits, and composition

Use `imports:` (not `include:`) when embedding another *app* as an
aliased sub-story. State paths, intent names, and world keys get the
alias prefix at load time; nothing crosses the boundary unless declared.

```yaml
imports:
  bf:
    source: ./bugfix             # path | @kitsoki/<name> | absolute
    entry: idle
    hosts: declared              # strict allow-list mode; default "inherit"
    world_in:
      ticket_id:    "{{ world.picked_ticket }}"
      base_branch:  "main"
    exits:
      done:
        to: pr_open
        set:                     # world_out — evaluated in the PARENT flat world
          pr_url: "{{ world.bf__pr_url }}"   # child keys are ALIAS-PREFIXED here
      abandoned:
        to: ticket_search
    host_bindings:
      ticket:   host.jira
      vcs:      host.git
    intents:
      export: [look]             # parent → child
      import: [start, accept, refine, restart_from, quit]
    overrides:
      states:
        idle: { ... }            # full state replacement (not deep-merge)
      intents:
        accept: { ... }
      prompts:
        "prompts/judge_proposing.md": "prompts/judge_proposing_jira.md"
```

Child stories declare named return points with `exits:` and target them
with `target: "@exit:<name>"`. `requires:` keys are statically checked:
every transition into `@exit:<name>` must set every required key in its
effects, or the loader rejects the story.

**`world_in:` and `world_out` (`exits.<name>.set:`) both evaluate in the FLAT
parent world — not the child's scope.** The imported child's world keys are
visible to the parent **alias-prefixed** (`<alias>__<key>`, e.g. `bf__pr_url`,
`maker__worktree_path`). So to lift a value the CHILD minted up into a parent key
on exit, you MUST read the prefixed form:
`set: { worktree_path: "{{ world.maker__worktree_path }}" }`. Reading the bare
`{{ world.worktree_path }}` there reads the PARENT's same-named key (usually still
empty) — a silent handoff drop that looks like the child "lost" its work. (This is
a real bug class: a maker import whose `achieved` exit set `worktree_path` from the
un-prefixed key handed an empty path to the integrator every time.)

**Imports are acyclic.** If A imports B, B cannot import A — imports resolve at
load time and a mutual import is a cycle the loader cannot flatten. When two
stories need the same tail/sub-flow, extract it into a THIRD importable story that
both import as peers (e.g. ship-it and bugfix both importing a shared
`delivery-tail`), rather than one importing the other.

`host_interfaces:` declares named capability surfaces the child invokes
as `iface.<name>.<op>`; the parent's `host_bindings.<name>: <handler>`
swaps in the concrete dispatcher. The host registry's prefix-fallback
means one handler at `host.git` satisfies `iface.vcs.commit`,
`iface.vcs.push`, etc., unless you register per-op handlers.

Every importable story needs a `README.md` documenting entry state,
exits + `requires:`, `world_in:` contract, intent export/import surface,
`host_interfaces:` contract, and host requirements. `stories/robbery/README.md`
and `stories/bugfix/README.md` are the templates.

### Prompt extension (`spec_` blocks + overlays)

Prompt files (`prompts/*.md`) are extensible templates: they can
`{% extends %}` / `{% include %}` other prompts, and a *project* can drop an
overlay that extends a story's base prompt and overrides named blocks — so a
generic story is specialized for a project **without forking it**. Full
contract: [`docs/stories/prompts.md`](../../stories/prompts.md).

When authoring a prompt, mark the sections a project will plausibly need to
change with a `spec_`-prefixed block — the one machine-readable signal that
the default is *provisional*:

- **Hole** (project must fill): `{% block spec_project_context %}{% endblock %}`
- **Provisional default** (project may refine): `{% block spec_rubric %}working default{% endblock %}`
- **Structural** (not a specialization target): a plain non-`spec_` block or plain text.

Checklist when adding/editing a prompt:

- [ ] Project-specific context (repo layout, domain, house tone) → a `spec_` **hole**.
- [ ] Generic-but-likely-to-change guidance → a `spec_` **provisional default**.
- [ ] Scaffolding that every project shares → leave structural (no `spec_`).
- [ ] Name shared fragments and pull them via `{% include "@shared/…" %}`.
- [ ] An overlay extends the base via `{% extends "@story/<path>" %}` — never
      duplicate the base prompt.
- [ ] Verify the surface: `kitsoki prompts spec <app.yaml>`.

When a request is about a project-specific gap a `spec_` section covers, fix it
by **specializing that block in an overlay** (`--prompt-overlay` or the
`prompts.overlay:` config), not by editing the story's shared base prompt.
`overrides.prompts` (above) is the whole-file *swap* — reach for it only when
you mean to replace a prompt wholesale, not extend it.

**Embedding a document as evidence.** To inline external material (a spec, a
report) the LLM should read and cite — line-numbered, with traceable
attribution — pipe its content through the built-in `reference` filter:
`{{ args.spec | reference:"api-spec.md" }}`. Unlike `{% include %}` it embeds the
content **verbatim** (no re-parsing) and resolves no path, so it works in any
render context. Bring the bytes in via a host file-read or a passed arg. See
[`docs/stories/prompts.md`](../../stories/prompts.md) § Embedding reference material.

## 8. Phase templates (compressing repeated rooms)

When a story repeats the same shape (execute → post → await reply →
retry on failure) over many phases, declare it once:

```yaml
phase_templates:
  reviewed_phase:
    parameters:
      id:         { type: string,  required: true }
      title:      { type: string,  required: true }
      checkpoint: { type: boolean, default: false }
    states:
      "{id}_executing": { ... }
      "{id}_awaiting_reply": { ... }
      "{id}_error": { ... }

phases:
  template: reviewed_phase
  graph:
    phase_a:
      title: "Phase A"
      next: { continue: phase_b }
    phase_b:
      title: "Phase B"
      checkpoint: true
      next: { continue: phase_c, on_failure: phase_a }
      cycle_budgets:
        on_failure: 2     # synthesises increment+guard+default → phase_b_error
```

`{name}` substitutes inside state keys; `{{ tpl.X }}` inside bodies;
`{{ phase.next.<arc> }}` inside `target:` resolves to
`<next-phase>_executing`. `cycle_budgets:` synthesises a counter
+ guard + fall-through-to-error trio per declared arc — use it instead
of hand-rolling retry caps.

`checkpoint_intents:` (a top-level map) is merged into every
`*_awaiting_reply` state — that's where you declare `continue`,
`refine`, `restart_from`, `quit`, etc. Slot schemas force context:
`refine` requires `feedback`, `restart_from` requires an enum `stage`.

## 9. Flow fixtures (Mode-2 deterministic tests)

Every non-trivial room deserves at least one flow fixture under
`flows/`. They run intent-only (no LLM, no harness) — fast, hermetic,
checkable in CI.

```yaml
test_kind: flow
app: ../app.yaml
initial_state: proposing            # dotted-path also works (bf.proposing)
initial_world:
  ticket_id:       "TKT-1"
  ticket_title:    "Fix the thing"
  artifact:        { summary_title: "Patch X", confidence: 0.85 }
  judge_mode:      "human"
turns:
  - intent:
      name: accept
    expect_state: implementing
    expect_world:
      cycle: 0
  - intent:
      name: refine
      slots: { feedback: "miss IPv6" }
    expect_state: proposing
    expect_world:
      refine_feedback: "miss IPv6"
      cycle: 1
```

Run:

```sh
kitsoki test flows stories/<name>/app.yaml
kitsoki test flows stories/<name>/app.yaml --flows flows/single_case.yaml --v
```

Flow fixtures double as **warp bases** — `kitsoki run ... --warp
flows/scenario.yaml` boots straight into `initial_state` with
`initial_world`. Check live scenarios in next to `app.yaml` under
`scenarios/`.

## 10. Pitfalls (the load-time and run-time checklist)

The loader rejects these at parse time — fix them before you bother
running:

- **`invoke: host.X` for an undeclared host.** Add to top-level `hosts:`.
- **`relevant_world: [foo]` for an undeclared world key.** Declare it
  with a type and default.
- **Transition `target:` to a non-existent state.** Either define the
  state or use `{{ world.dynamic }}` if you really mean dynamic.
- **`@exit:X` referenced by the child but not mapped by the parent's
  `imports.<alias>.exits.X.to:`.**
- **`@exit:X` with `requires:` keys the transition's effects don't set.**
- **Alias collision** between an `imports.<alias>` name and an existing
  state in the same scope.
- **State name collision across includes.** Rename one.
- **`overrides.states.<X>` / `.intents.<X>` / `.prompts.<X>` where `<X>`
  isn't declared in the child.**

The renderer / runtime traps — invisible until a user hits them:

- **No `default: true` on the last transition.** Benign cases hit
  `GUARD_FAILED` and the user sees a hint instead of a clean fallthrough.
  Always provide one (even if it's `target: . effects: [say: "Can't do
  that here."]`).
- **`view: |` string with a single bad `{{ … }}` →** the orchestrator's
  render-after-bind silently swallows the error and ships zero bytes —
  the user is dropped into a blank screen. **`view_bytes: 0` in the
  trace is a P0 bug.** Use `view: extends: "base"` so the chrome
  carries the floor.
- **Action menu conditional on world state.** Even if the body can't
  render, the user MUST still see the menu. Put it in `choices:` or as
  a non-guarded `prose:` line at the bottom of `body:`.
- **`{% for %}` over a possibly-absent world key.** Guard with `{% if %}`
  or use a typed `list:` with `from:`.
- **`param:` on a choice item in a conversational room produces two text
  inputs in the web UI.** Any global intent with a `string` slot also
  appears as a free-text textarea alongside the choice buttons. In a
  `mode: conversational` room the `discuss` textarea already provides
  free-text input — adding `param:` on a choice item that maps to
  `discuss` creates a duplicate. Use pre-filled `slots:` instead of
  `param:` for "run with no extra input" buttons:
  `slots: { message: "" }` fires the intent while leaving the textarea
  for typed feedback. See [choice-widget.md §3.8](../../stories/choice-widget.md#38-param-in-conversational-rooms-causes-duplicate-text-inputs) for the full pattern.
- **`on_error: idle` everywhere.** This is "silent fail" — the user gets
  bounced with no diagnostic. Prefer making the handler idempotent
  (idle's auto-create with the `bf_autostart_attempted` flag is the
  template). When you do use `on_error:`, ensure the destination view
  surfaces `world.last_error` somewhere.
- **Non-idempotent `on_enter:` side effects.** `on_enter` re-fires on
  `/reload` (`RerunOnEnter`), on explicit self-target re-entry, and on
  `on_error:` redirects — so any `invoke:` there runs 2+ times per
  session. A `host.chat.create` (unconditional INSERT) in `on_enter`
  spawns a *fresh empty chat* on every `/reload`, orphaning the
  conversation; use `host.chat.resolve` (get-or-create) instead. For an
  expensive/non-idempotent call that *binds a result* (an `host.oracle.*`
  decide/task/converse, a workspace/artifact write), set **`once: true`**
  on the invoke (§4) — the engine skips it on re-entry while its `bind:`
  target is already populated, and the re-run intent clears that target to
  force a fresh run. `once:` is the preferred remedy; a hand
  `when: "world.<key> == ''"` guard is the manual fallback (and the only
  option for a scalar bind or a no-`bind:` mutator). **Reload must always be
  safe.** See [state-machine.md §"`on_enter` must be idempotent"](../../stories/state-machine.md#on_enter-must-be-idempotent).
- **Happy-path test that only checks `next_state`.** Rooms can advance
  while running a no-op `on_enter:`. Assert the side effects: `git show
  --name-only HEAD` after a commit, `stat` after a workspace.create,
  the actual world key after a bind.
- **Background job referencing `world.last_job_result` outside
  `on_complete:`.** That key only exists inside the completion turn.
- **`emit_intent:` and `target:` on the same effect.** Mutually
  exclusive. The runtime depth-caps `emit_intent:` at 8.
- **World values typed as `object` reading dotted paths in views without
  a fallback.** A bound key from `bind: artifact: submitted` is present
  by the time the view renders (the orchestrator re-renders after bind);
  a *conditionally* invoked bind still needs `?? "(pending)"` because
  the field is absent on the not-taken branch.
- **Ceremony steps — a room that costs a turn without deciding, side-
  effecting, or collecting input.** A pass-through `idle`/`begin`/`continue`
  landing room, or a checkpoint with a single forward path, makes the operator
  click to advance the one place they could go. Make the first *real* room the
  `root:` (delete the landing room), or `emit_intent:` auto-advance a no-choice
  room from its `on_enter:`. One forward path is not a decision gate. See
  [`docs/stories/authoring.md`](../../../docs/stories/authoring.md) §3.1 (the
  authoritative rule).
- **Interpolating an operator/dynamic command into a `bash -c` heredoc that also
  defines shell locals.** A gate/command rendered as `CMD="{{ world.x }}"` into the
  SAME script that sets `WORKTREE=…`, `BRANCH=…` is RE-EXPANDED by that shell: a
  value whose text legitimately contains `$WORKTREE` (or any local, or its own
  `$`-refs) is mangled before it runs — a false failure that passed at the layer
  that built the command. Isolate the command from the script's scope: write it to
  a temp file and `bash <file>`, pass it positionally (`bash -c "$1" _ "$CMD"`), or
  interpolate under a QUOTED heredoc delimiter so no expansion happens at
  interpolation time.
- **A failure arc that binds an empty diagnostic.** A `host.run` whose failing
  branch produced no stdout binds `last_error: ""`, so the needs-human/error view
  renders blank — the operator sees a featureless dead-end (and an
  `@exit:needs-human` with `requires:[last_error]` is "satisfied" by `""`). Always
  capture a non-empty diagnostic on failure: exit code + a note, never blank.
- **Trusting a flow fixture to test logic INSIDE a `host.run` script.** Flow
  fixtures mock `host.run` WHOLESALE — the whole call returns a canned result — so
  they cannot exercise what the script body computes (which template var lands in
  which shell var, what a `grep`/`jq` decides). To guard logic inside the script,
  assert on the RENDERED script text (a structural test) or run it for real against
  a temp fixture; a green mocked flow is NOT coverage of the script body.
- **A maker-style loop that hands off a DIRTY worktree.** If you author a loop
  whose terminal handoff is "the branch is ready to integrate," it must COMMIT the
  work first — a dirty worktree has no commit for an integrator to rebase/merge.
  And `iface.vcs.commit` / `host.git` commit with an EMPTY `files:` list falls back
  to `git commit -a`, which DROPS new/untracked files — pass the maker's
  changed-files list, or add a `stage_all` / `git add -A` step, so created files
  actually land.

## 11. The authoring loop

The order most authors settle into:

1. **Sketch the graph** in `app.yaml` + `rooms/`. Placeholder views are
   fine; typed `extends: "base"` from the start saves migration work.
2. **`kitsoki turn`** to probe one state-shape at a time. Stateless,
   JSON output, no DB:
   ```sh
   kitsoki turn stories/<name>/app.yaml \
     --state <state.path> \
     --intent <intent_name> \
     --world '@/tmp/world.json'
   ```
3. **Write a flow fixture** for the path you just probed; lock it with
   `kitsoki test flows`.
4. **`kitsoki viz stories/<name>/app.yaml`** to sanity-check the graph
   shape (or `kitsoki viz --mermaid`).
5. **`kitsoki render -o APP.md`** for review-friendly docs.
6. **`kitsoki run stories/<name>/app.yaml`** to play it for real.
   Hot-reload picks up edits as you go.

If a user reports a runtime misbehaviour (silent bounce, wrong target,
view blank, "going back to idle") — **stop authoring and switch to the
`kitsoki-debugging` skill.** It drives `kitsoki turn` against the
real on-disk state and surfaces the host-call errors the TUI's
`on_error:` arcs swallow.

## 12. Constraints when editing in this repo

- Never call `git`. The user owns commit/push.
- Stay inside the story directory the user is editing; don't drift to
  `testdata/` (engine fixture data — its own tests depend on it).
- Don't refactor across rooms unless asked. A one-file request is a
  one-file change.
- When the user references a label or phrase, edit the room that
  *produces* that view — not the first grep hit elsewhere in the tree.
- Hot reload watches `mtime` on the watched `app.yaml`; editor temp-file
  writes (vim default) may need `:set nobackup nowritebackup` for the
  reload to fire. Save in place.
