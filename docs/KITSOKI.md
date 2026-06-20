# Authoring gears-sdlc specs with kitsoki

[**kitsoki**](https://github.com/constructorfabric/kitsoki) is a deterministic
state-machine runtime for LLM workflows. One of its stories — **`gears-rust`**
— drives *this* repo's
**PRD → Design** spec chain as a guided, conversational walk: you describe an
idea in a chat UI, answer a few clarifying questions, and it authors two
[gears-sdlc](spec-templates/gears-sdlc/)-shaped documents straight into the
target gear's docs folder.

It is the worked example of kitsoki's *external-target profile*: the same
hub kitsoki uses to build itself, pointed at this repo purely through
configuration (where docs land, what shape they take). No kitsoki engine code
is specific to gears-rust.

> This is a **spec-authoring aid**, not a code generator. It produces PRD.md
> and DESIGN.md you review and edit — it never touches `src/`, `Cargo.toml`,
> or any gear implementation.

## What it produces

A full walk lands two fixed-name docs in the target gear, in the gears-sdlc
shape (the [`DESIGN`](spec-templates/gears-sdlc/DESIGN/) /
[`PRD`](spec-templates/gears-sdlc/PRD/) templates, with `cpt-…` IDs):

```
gears/<gear>/docs/PRD.md      # gears-sdlc PRD
gears/<gear>/docs/DESIGN.md   # gears-sdlc DESIGN
```

The default target gear is **`notes-service`** — a scratch gear used so the
demo never clobbers a real gear's docs. No GitHub issue / feature ticket is
filed (this repo tracks work in GitHub issues; that integration is a separate,
deferred piece — see [Improving the story](#improving-the-story)).

## Prerequisites

1. **The `kitsoki` binary on your `PATH`.** kitsoki ships its base story
   library (`dev-story` and its sub-stories) **embedded in the binary**, so
   this repo's instance — which imports the base via
   `source: "@kitsoki/dev-story"` — runs with **only the binary present**: no
   kitsoki checkout, no sibling clone. Install a release, or build once from a
   kitsoki checkout (`make build && install ./kitsoki /usr/local/bin/`) and
   then discard it.

2. **Python 3** for the publish glue. No Node/pnpm needed — the web UI assets
   and the demo-video renderer (`kitsoki tour`) are baked into the binary too.

3. **An LLM, or a recorded cassette.** A live run dispatches `claude` agents
   to author the docs (cost + latency). The no-LLM flows and the demo run
   entirely against recorded fixtures — see [No-LLM validation](#no-llm-validation).

## Run it

All commands run **from this repo's root**, with `kitsoki` on your `PATH`.

### Web UI (recommended)

```bash
kitsoki web
```

Discovery is zero-config: `kitsoki web` walks the default `./stories` dir and
finds `stories/gears-rust/`. Open the printed URL, pick **gears-rust**, and
drive the walk in the chat box.

### Terminal UI

```bash
kitsoki run stories/gears-rust/app.yaml
```

### The walk

From the landing room (`core.main`):

```
prd                         # start the PRD pipeline (discovery chat opens)
<describe your gear idea>   # converse — the analyst reads this repo for context
prd__start                  # distil the idea → prior-art scan
prd__confirm                # no overlap → clarifying questions
<answer the questions>      # the last answer auto-advances
prd__confirm                # brief → reference curation
prd__confirm                # → draft the PRD
prd__accept                 # publish gears/<gear>/docs/PRD.md
continue                    # → design intake, seeded from the published PRD
<refine / accept>           # search → brief → draft → publish DESIGN.md
```

Each authoring step runs an agent **with this repo as its working directory**,
so it reads the real gear code, `docs/`, and `guidelines/` to ground the spec
— and writes the result through the vendored gears-sdlc template.

### VS Code extension

The same PRD → Design walk runs **inside VS Code**, where it stops being a chat
in a box and becomes editor-native: the conversation lives in a Kitsoki panel,
but every document it authors — the brief, the PRD, the DESIGN — is mirrored
into a **real editor tab** you can read, search, and hand-edit, and a refine
opens a **native side-by-side diff** with an in-editor **Accept / Reject** whose
verdict flows back into the walk.

**Setup.**

1. Install the **Kitsoki VS Code extension** (built from `tools/vscode-kitsoki`
   in the kitsoki repo). It drives the same `kitsoki` binary on your `PATH`.
2. Open **this repo** as the VS Code workspace, so the authoring agents run with
   it as their working directory — the same grounding the Web/TUI runs get.
3. Run **`Kitsoki: Open Chat`** from the Command Palette. The extension spawns
   `kitsoki web` as a child process and connects to it as an IDE bridge
   automatically (no port wrangling), then pop the chat out to the editor so the
   conversation sits beside the documents it opens.

**Drive the walk.** Pick **gears-rust** in the chat, then drive exactly the
[same intents](#the-walk) (`prd` → `prd__start` → … → `prd__accept` →
`continue` → …). What's new is what happens in the editor at each beat:

| Beat | In the editor |
|---|---|
| **Clarify → submit** | The **brief** opens as an editor tab and **grows** as you answer each round of clarifying questions — you watch it accrete on disk, not in a chat bubble. |
| **Drafting** | The **PRD** opens as a real tab (`gears/<gear>/docs/PRD.md`) showing the full gears-sdlc document — headings, requirements, open questions — editable and searchable like any file. |
| **Refine** | Typing a refinement opens a **native diff** (the current PRD ↔ the proposed revision). The turn **blocks on your verdict**: **Accept** (the editor title-bar action or the CodeLens at the top of the diff) applies the change — it writes the file and the walk continues — or **Reject** discards it and re-drafts. Multiple refine rounds work the same way. |
| **Design half** | The DESIGN authoring mirrors identically: drafted into a tab, refined through the diff/verdict gate, published to `gears/<gear>/docs/DESIGN.md`. |

Because the docs are real on-disk files, your normal VS Code muscle memory
applies — Explorer, search, save/undo, stock diff navigation (next/previous
change, inline vs side-by-side). Nothing hijacks it.

**How the editor link works.** The extension is a Claude-Code-style **IDE MCP
server**; the spawned `kitsoki web` auto-connects to it (via
`CLAUDE_CODE_SSE_PORT`), and the story's editor actions — `host.ide.open_file`
for the brief/PRD, `host.ide.open_diff` for a refine — dispatch over that link.
`open_diff` **blocks the turn** until you Accept/Reject, so the verdict is a real
gate, not fire-and-forget. With no IDE attached (a plain `kitsoki web` in a
browser, or the no-LLM flows) those verbs report *not connected* and the walk
degrades gracefully to the in-chat path — the same story, no VS-Code-specific
branches.

## Targeting a different gear or checkout

The target gear and checkout path are configuration, overridable per run via a
*warp scenario* without editing the story:

```bash
kitsoki run stories/gears-rust/app.yaml --warp stories/gears-rust/scenarios/gears-rust.yaml
```

Edit that scenario (or the `world:` defaults in
`stories/gears-rust/app.yaml`) to change:

| Key | Default | Meaning |
|---|---|---|
| `workdir` / `repo_root` | `.` | this repo's checkout (the default) |
| `publish_durable_path` | `gears/notes-service/docs` | PRD home (relative to `workdir`) |
| `design_durable_path` | `gears/notes-service/docs` | DESIGN home |
| `prd_doc_filename` / `design_doc_filename` | `PRD` / `DESIGN` | fixed output filenames |
| `design_template_dir` | `stories/gears-rust/templates` | the gears-sdlc templates the author reads |
| `design_ticket_dir` | `""` | empty ⇒ skip filing a tracking ticket |

To author for, say, `chat-engine` instead of `notes-service`, set both
`*_durable_path` to `gears/chat-engine/docs`.

## No-LLM validation

The full walk is pinned by deterministic fixtures (recorded agent outputs, no
LLM call, no cost):

```bash
kitsoki test flows stories/gears-rust/app.yaml
```

These assert the resolved publish paths (`gears/notes-service/docs/PRD.md`,
`…/DESIGN.md`) and that no ticket is filed. A tour-narrated demo video of the
same walk renders **from the binary** — no Node/pnpm/Playwright:

```bash
kitsoki tour --feature gears-prd-design
# → .artifacts/gears-prd-design/gears-prd-design.mp4 (+ chapters + per-step PNGs)
```

See [`stories/gears-rust/README.md`](../stories/gears-rust/README.md) for the
instance end to end.

## How the retargeting works

The story is `kitsoki-dev` (kitsoki building kitsoki) with one thing changed:
a handful of *doc-profile* world keys are set at the instance level and
projected into the shared `dev-story` hub. Every key has a default that
reproduces kitsoki's own behaviour; overriding them **is** the profile. The
two publish scripts (`prd_publish.py`, `publish_design.py`) take the durable
path / filename / ticket-dir as parameters, so the placement seam needs no
code change. Full detail lives in the [kitsoki
repo](https://github.com/constructorfabric/kitsoki):

- [`stories/gears-rust/README.md`](https://github.com/constructorfabric/kitsoki/blob/main/stories/gears-rust/README.md)
  — this instance, end to end.
- [`stories/dev-story/README.md`](https://github.com/constructorfabric/kitsoki/blob/main/stories/dev-story/README.md)
  → "Doc profile — targeting an external project" — the seam and every world key.

## Improving the story

The PRD → Design chain is the shipped slice. Natural next steps, roughly in
priority order:

1. **GitHub-issue ticket adapter (the big one).** Today the walk files no
   tracking ticket (`design_ticket_dir: ""`). A `gh`-backed adapter satisfying
   kitsoki's abstract `ticket` interface would, on publish, open/label a
   GitHub issue linking back to the DESIGN — closing the loop into this repo's
   actual workflow. This is glue (a script against `gh`), not a new runtime
   primitive; it is the one deferred slice of the external-targeting epic in
   the kitsoki repo. Once present, set `design_ticket_dir` / bind the adapter
   in `stories/gears-rust/app.yaml`'s `host_bindings`.

2. **Extend the chain to later gears-sdlc stages.** This repo's SDLC is
   PRD → DESIGN → **ADR → FEATURE → DECOMPOSITION** (templates and checklists
   already vendored under `docs/spec-templates/gears-sdlc/` and
   `docs/checklists/`). The story stops at DESIGN. Adding rooms that author
   ADRs (per architecturally-significant decision) and decompose a DESIGN into
   FEATUREs would mirror the existing design pipeline — each is another
   `intake → draft → publish` triple with its own template dir and fixed
   filename. The vendored templates in the story's `templates/ADR` and
   `templates/FEATURE` are already in place for this.

3. **Conventional-commit + DCO commit discipline.** A live run authors files
   but does not commit. Binding the `vcs` interface to a glue script that
   stages the new docs and commits them with a DCO sign-off + conventional
   message (`docs(<gear>): add PRD/DESIGN`) would make the walk land a
   review-ready branch, matching `CONTRIBUTING.md`.

4. **`make check` as the `ci` gate.** The hub declares a `ci` interface; wiring
   it to this repo's `make check` (fmt / clippy / lychee link-check on the new
   markdown) would let the story self-verify its output before handing back.

5. **Per-gear template / placement presets.** Targeting a different gear today
   means editing two `*_durable_path` keys. A small "pick a gear" room reading
   `gears/*/` could turn that into a menu, removing the manual path edit.

6. **Sharpen the spec prompts for gears vocabulary.** The author prompts are
   written to adapt to whichever template set they're handed; they could be
   tightened to actively use gears terms (GTS types, extension points, plugin
   vs adapter interfaces, the p1–p5 phase tags from `docs/GEARS.md`) so first
   drafts need less editing.

To make a *second* external target (another GitHub repo), copy
`stories/gears-rust/` into that repo, repoint the world keys, vendor that
target's templates, and adjust the two flows — no kitsoki code change. The
procedure is in `stories/gears-rust/README.md` → "Copy this dir for a NEW
external target".

## Troubleshooting

- **`kitsoki: command not found`.** The binary isn't on your `PATH` — install a
  release or build it once from a kitsoki checkout (see Prerequisites). No
  kitsoki checkout needs to be present once the binary is installed; the base
  stories are embedded.

- **`gears-rust` isn't listed by `kitsoki web`.** Discovery walks `./stories`
  from the directory you launched in — run `kitsoki web` from this repo's root
  so `stories/gears-rust/` is found (or pass `--stories-dir stories`).

- **The spec talks about *kitsoki* instead of this repo.** The authoring agent
  must run with this repo as its working directory and the gears-sdlc templates
  as its template dir — both are set by the profile. Confirm you picked the
  `gears-rust` instance (not the bare `kitsoki-dev` one) and that `workdir` /
  `repo_root` resolve to this checkout (default `.`).

- **Docs land in kitsoki's own `docs/proposals/` instead of here.** The profile
  keys aren't resolving; check `workdir` / `*_durable_path` in
  `stories/gears-rust/app.yaml`.
