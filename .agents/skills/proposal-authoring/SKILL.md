---
name: proposal-authoring
description: Author, scope, and decompose kitsoki proposals using the templates under docs/proposals/templates/. Use when the user wants to write a new proposal, pick the right template for a change, split a large/raw proposal into focused reviewable pieces, or keep a proposal's Status line / lifecycle honest. Covers the four focused kinds (story, runtime, tui, tracing), the epic-decomposition flow, the shared spine, and the trim-on-ship / delete-when-done lifecycle.
---

# Kitsoki Proposal Authoring

Proposals live in `docs/proposals/` and are a **small, current queue** of
what's being worked toward — not an archive. The templates in
[`docs/proposals/templates/`](../../proposals/templates/) give every draft
a consistent, skim-in-two-minutes shape. This skill picks the right one,
fills it well, and splits big changes into reviewable slices.

Read these first — they're the contract, not background:

- [`docs/proposals/README.md`](../../proposals/README.md) — the lifecycle
  (Status line, trim-on-ship, delete-when-done) and the current queue.
- [`docs/proposals/templates/README.md`](../../proposals/templates/README.md)
  — the shared spine, the which-template table, and the decomposition flow.

## The shared spine

Every focused proposal is: a **Status / Kind / Epic** header, then
`Why / What changes / Impact`, then kind-specific design sections, then
`Tasks / Open questions / Non-goals`. `Why/What/Impact` is the OpenSpec
core; the rest is existing kitsoki convention. Keep prose tight and
**link to code (`file:line`), existing docs, and the gold-standard
stories** instead of restating them.

## Picking a template

| The change is mainly about… | Template |
|---|---|
| A new/reworked operator story (rooms, world, prompts, flows) | `story.md` |
| Engine/runtime behavior (gates, deciders, effects, host calls, world semantics, load invariants) | `runtime.md` |
| TUI layout, typed-view rendering, slash commands, input | `tui.md` |
| Tracing events, cassette fidelity, run-status surfaces | `tracing.md` |
| Something that spans several of the above | `epic.md` → decompose |

Tie-breaker: choose the template whose **design sections you'll actually
fill in**, and note spillover under **Impact**. If two kinds each carry
real design weight, it's an epic.

## Two ways to author: by hand, or the interactive design pipeline

This skill covers authoring a proposal **by hand** (below). kitsoki also
ships the same discipline as a **process story** you can drive
interactively — the **design pipeline** in `stories/dev-story/` (reachable
from the dogfood instance). Enter it ad-hoc via `idea`, or as the back half
of the **PRD → Design walk** (type `prd`, author a PRD, then `continue` from
the `prd_published` room to carry it into the design intake — see the
[dev-story README](../../../stories/dev-story/README.md#prd--design-walk)):

```
kitsoki run stories/kitsoki-dev/app.yaml   # land in main; type `idea` (or `prd`)
```

> **Naming:** the in-story pipeline is the **design** pipeline (rooms
> `design` / `design_search` / `design_refine` / `design_draft` / …); the
> templates (`docs/proposals/templates/`), the `docs/proposals/` output
> directory, and this **`proposal-authoring`** skill keep their names — the
> pipeline emits proposal-shaped design docs into `docs/proposals/`.

It walks the proposal process from `docs/proposals/proposals.md`:

1. **intake** — a discovery conversation that distils a crisp idea.
2. **brief** — scaffolds a pre-filled `001-brief.md` (the shared
   spine — Why / What changes / Impact + kind) into a per-session
   workspace at `docs/proposals/.workspace/<slug>/`; **you edit it in VS
   Code**, then an `oracle.decide` judge sanity-checks it (continue /
   clarify).
3. **existing-state** — the overlap gate: scans in-progress
   (`.workspace/`) and accepted (`docs/proposals/*.md`) proposals + feature
   docs, and steers you to **amend** an existing one rather than create a
   duplicate (proposals.md's core rule).
4. **idea-completeness** — gate on problem / why-kitsoki / usage.
5. **references** — curates the docs the proposal must build on
   (`004-references.json`).
6. **draft** — an `oracle.task` that classifies the kind, copies the
   matching `templates/<kind>.md`, and writes `005-proposal.md`.
7. **publish** — moves the draft into `docs/proposals/<slug>.md`; the
   numbered checks stay in the (gitignored) workspace as the record.

The interactive flow **applies the same spine + template selection** this
skill describes — use whichever fits. The design discipline behind it
(deterministic vs. decide vs. task, the validation sandwich, the
working-folder ergonomics) is [`docs/proposals/process-design.md`](../../proposals/process-design.md) §§1–5;
the room-by-room mechanics are in `stories/dev-story/rooms/design*.yaml`,
modelled on the gold-standard `stories/prd/`.

## Authoring a focused proposal

1. **Confirm the kind** with the table above. State your pick and why in
   one line before writing.
2. **Copy the template** to `docs/proposals/{slug}.md`. Use a descriptive
   kebab-case slug; no need for a `-proposal` suffix (the folder says so).
3. **Fill the spine first** — `Why / What changes / Impact`. If you can't
   write a crisp `Why`, the proposal isn't ready; ask the user, don't
   pad it.
4. **Fill only the design sections that apply.** The templates are a
   menu. Delete headings you won't use — an empty "Migration" section is
   noise. Delete every `{placeholder}` and `<!-- guidance -->` comment as
   you go; a finished proposal has neither.
5. **Ground every claim.** Replace "reuse the bugfix pattern" with the
   actual `stories/bugfix/rooms/…:line`. Mimic the closest existing
   proposal of the same kind (see the queue in `proposals/README.md`).
6. **Write the Tasks checklist** as the execution contract — phased,
   small enough that each box is one sitting, ending in "migrate to docs/
   and trim/delete this proposal."
7. **Set the Status line** honestly: `Draft v1. Nothing implemented yet.`

Kind-specific reminders the templates already encode, worth holding:

- **story** — novelty should be story-layer only; if you're inventing
  effects/host calls/widgets, that's a `runtime` slice — split it. Lean on
  the `kitsoki-story-authoring` skill for YAML shape.
- **runtime** — guard the moat: separate interpretive decisions from
  deterministic execution, keep decision points pluggable, record every
  decision. Say loudly if a change blurs that line.
- **tui** — rendering stays typed-elements + pongo2 (never hand-rolled Go
  strings); anything touching concurrent I/O needs a combined-I/O test that
  you've verified **fails without the change** (CLAUDE.md, `rendering-tests`
  skill).
- **tracing** — be explicit whether you're *recording* something new or
  *consuming* what's already traced, and protect replay determinism.

## Decomposing a large proposal

When a change spans kinds (a story **and** an engine seam **and** a TUI
surface), it's an epic. Two entry points:

**A. Greenfield epic** — the user describes something big from scratch:

1. Create `docs/proposals/{epic-slug}.md` from `epic.md`. Fill the
   big-picture `Why / What changes / Impact`.
2. Identify slices. A slice is right-sized when it has **one coherent
   Why**, fits in one reviewer's head, and could ship alone or with one
   named dependency. Cut along kind boundaries first (runtime vs. story
   vs. tui vs. tracing), then along shippable units within a kind.
3. For each slice, create a focused child proposal from its template,
   setting `**Epic:** ../{epic-slug}.md`. Push all detail into the child;
   the epic keeps only the seams *between* slices.
4. Fill the epic's **Slices** table (kind, one-line scope, depends-on,
   status, file link) and **Sequencing** (usually: runtime substrate →
   story → tui, with tracing slotted where its events are produced).
5. Record any **Shared decision** that spans slices in the epic so no
   child re-litigates it.

**B. Refactor an existing oversized proposal** — a single file already
tries to do too much:

1. Read it; list the distinct concerns and tag each with a kind.
2. Propose the split to the user (slice list + kinds) before moving text.
3. Create the epic, move each concern's text into a child of the right
   template (rewriting to the spine — don't just paste), and link them.
4. Replace the original file with the epic, or delete it if the epic
   slug differs. Preserve nothing redundant — git history holds the old
   form.

Keep the epic's slice table current as children ship — it's the source of
truth for "where is this epic."

## Lifecycle (don't let the queue rot)

Per `proposals/README.md`:

1. Lands as a draft with a Status line saying what's *not* implemented.
2. As it ships, **migrate implemented sections into normal `docs/`**
   (`docs/stories/`, `docs/tui/`, `docs/tracing/`, `docs/architecture/`),
   trim the proposal to what's still in design, and update the Status
   line to point at where the shipped pieces went.
3. When everything has shipped or been superseded, **delete the file**
   (and, for an epic, delete it once every slice is gone). Add/remove the
   entry in `proposals/README.md`'s "Current proposals" list to match.

The standing instruction in `CLAUDE.md`: complete the implementation,
move content to narrative docs, delete the proposal — don't leave
unfinished work unless told to, and if so, trim the proposal to the
remaining work.

## After adding a new template or skill

Codex discovers project skills under `.agents/skills/` directly. Expose new
skills to Claude Code through the project-local symlinks:

```
make setup
```

If you add a **new template kind**, also: add a row to the which-template
tables in `templates/README.md` and in this skill, and add a
`**Kind:**` value to the spine.
