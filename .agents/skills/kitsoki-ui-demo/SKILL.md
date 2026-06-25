---
name: kitsoki-ui-demo
description: 'Produce a deterministic, no-LLM demo / tour video of the kitsoki web UI (plus per-scene screenshots and a shareable MP4 / GIF / contact sheet) by driving a real `kitsoki web` server through Playwright. Use when asked to make, record, refresh, or author a tour demo video, feature-spotlight tour, walkthrough video, demo, or screen-capture of the kitsoki browser UI — whether a tour of one feature (golden example: agent-actions), the generic onboarding tour, or a full-product walkthrough. Also covers turning a REAL LLM-driven dogfood session into a deterministic demo: generating the no-LLM flow fixture + host cassette from a recorded trace via `kitsoki trace to-flow` (no hand-authoring, no LLM re-interpretation). Triggers on phrasings like "make a tour demo video", "record a demo of <feature>", "feature tour video", "walkthrough video", "turn this dogfood trace/session into a demo video".'
---

# Kitsoki UI demo videos

This skill records the **kitsoki web UI** as a deterministic, **no-LLM** video:
a Playwright spec spawns the real `kitsoki web` binary in the `--flow` /
`--host-cassette` posture (nil harness — intents are submitted explicitly, host
calls come from a cassette/stub), drives the SPA scene-by-scene at a
human-watchable pace, and records a MacBook-resolution video + per-scene
screenshots into `.artifacts/`. The recording is saved as a shareable **MP4**
(never `.webm` — it must play inline in VS Code / Keynote / Slack); bundled
scripts render an optional **GIF / contact-sheet** alongside it.

Why no-LLM: the recording must be **reproducible and free** — same input, same
frames, no API cost, no flakiness. This is the same posture the engine uses for
flow tests (see [[feedback_no_llm_tests]] and `docs/web/README.md` →
"Deterministic, no-LLM"). **Never** record against a live LLM.

> **Pick the worked reference that matches the ask — copy it, don't start blank:**
> - **A tour demo video of one feature** (the usual ask — "make a tour demo
>   video of X") → copy the **agent-actions** spec template
>   (`tools/runstatus/tests/playwright/agent-actions-video.spec.ts` +
>   `src/tour/agent-actions-manifest.ts`), which demonstrates the tour-narration
>   pattern: the *whole* video is tour-narrated — it opens on the home story
>   library, frames the demo story, drives home → new session → observer via
>   narrated action steps, then walks the feature. See **[Feature tour demo
>   video — the spec template](#feature-tour-demo-video--the-golden-example)**.
> - **The golden example of conversation-driven development** (iterative
>   clarification, brief refinement, multi-document publication in one session) →
>   the **dev-story PRD → Design** demo (`features/dev-story-prd-design.yaml` +
>   `stories/dev-story/flows/prd_to_design_full.yaml`). When slice 2 ships it
>   renders via `kitsoki tour --feature dev-story-prd-design` (binary-native, no
>   Playwright). See **[Dev-story PRD → Design](#dev-story-prd--design-golden-conversation-driven-example)**.
> - **The generic onboarding tour** → `tour-video.spec.ts` + `src/tour/manifest.ts`.
> - **A full-product walkthrough** (home → new session → drive/observe → reload →
>   active sessions) → `multi-story.spec.ts`. The single-purpose chat drive lives
>   there too.
>
> **Two production modes** (both no-LLM): the **live screen-record** mode above
> (screen-record a live `kitsoki web` drive — the default, and the ONLY option
> for `<canvas>`/`<video>`/WebGL surfaces) and the **rrweb capture → replay-render**
> mode (capture the DOM stream once, re-render server-free + offline, frame-exact)
> — see **[rrweb capture → replay-render](#rrweb-capture--replay-render-deterministic-server-free-mode)**.

## Start from a real dogfood trace (generate the flow + cassette — don't hand-author)

A demo's no-LLM `--flow` fixture + `--host-cassette` do **not** have to be written
by hand. If the scenario you want to film already happened as a **real,
LLM-driven session** (a dogfood run, a bugfix pipeline, a live drive), convert
its recorded trace into the replay artifacts deterministically — no LLM
re-interpretation, no transcription by hand:

```bash
# 1. Find the recorded session trace (JSONL). Live/record sessions write one to:
#      ~/.kitsoki/sessions/<app>/<session-id>.jsonl
#    or capture a fresh one via the MCP `session.trace` tool / `--trace-out`.

# 2. Convert the trace → a flow fixture (+ sibling host cassette) — a pure transform:
kitsoki trace to-flow <trace.jsonl> \
  --app ../app.yaml \
  --out stories/<story>/flows/<scenario>.yaml
#   → writes <scenario>.yaml and (when the trace had host calls)
#     <scenario>.cassette.yaml beside it, referenced via host_cassette:.

# 3. Verify it replays no-LLM and capture a fresh trace:
kitsoki test flows stories/<story>/app.yaml --flows stories/<story>/flows/<scenario>.yaml \
  --trace-out .artifacts/<scenario>/replay.jsonl
```

The generated flow is exactly what the recording pipeline already consumes — point
`kitsoki web --flow stories/<story>/flows/<scenario>.yaml` (or a `*-video.spec.ts`
spec's `--flow` arg) at it and record as below. Two properties make this a clean
fit for demos:

- **Each `machine.transition` → one turn** (resolved intent name + slots,
  verbatim, in order). The LLM/semantic routing decision is *not* re-run on
  replay — the resolved intent is re-driven directly, so it's deterministic and
  free.
- **`display_input:` preserves the operator's real free-text words** (from the
  trace's `turn.input`), so a conversation demo's user bubbles — and the strings
  you type into the composer — are the operator's actual utterance, not a
  synthetic `[intent] <name>`. This is what makes a trace-derived conversation
  video followable (see [Demoing human usage](#demoing-human-usage--the-conversation-must-be-followable)).

**Caveats, all by design** (full discussion + the trace→fixture mapping table:
[`docs/tracing/trace-format.md` §11](../../tracing/trace-format.md#11-kitsoki-trace-to-flow--trace--replayable-flow-fixture)):

- The converter emits **no `expect_state` / `expect_world`** (story-drift
  tolerance). Add expectations by hand only if you want to pin a known-drift-free
  path.
- Per-call-varying agent/host responses replay correctly because each recorded
  call becomes one **ordered** cassette episode (not `replay:any`) — the i-th call
  consumes the i-th episode.
- If the *current* story routes a turn into a room that didn't exist when the
  trace was recorded, that room's `on_enter` may need a host call the cassette
  has no episode for → a hard cassette miss / `on_error` bounce. That's honest
  drift, not a tooling fault: re-record the trace against the current story.

Once the flow + cassette exist, everything below (spec, pacing, MP4) is unchanged
— the source of the fixture (hand-authored vs trace-derived) is invisible to the
recorder.

## Prerequisites (once)

```bash
make build                                  # bundle the SPA into ./kitsoki + bin/kitsoki
cp ./kitsoki bin/kitsoki                    # the specs spawn bin/kitsoki
pnpm -C tools/runstatus playwright:install  # chromium + ffmpeg for Playwright (once)
```

`make build` is **mandatory before every recording** — the SPA is `go:embed`'d
into the binary, so an un-rebuilt binary serves a stale UI. Rebuild after any
change under `tools/runstatus/src/`.

## Deterministic recording (read this first)

A demo recording has a few non-obvious traps. They're solved once, in
`tests/playwright/_helpers/demo.ts` — **use those helpers; don't re-derive
them.** The reference spec is `tests/playwright/diagram-showcase.spec.ts`.

- **`recordVideo` captures from PAGE CREATION**, so off-camera setup (home
  screen, the new-session click, live RPC room-flips) flashes by, rushed, at the
  head of the video. There is no "off camera" — the camera rolls from frame 0.
  → `installCurtain(page, title)` **before the first `goto`** drops a full-screen
  title card that survives `page.reload` (sessionStorage), then `liftCurtain(page)`
  once the scene is fully staged. Drive all setup behind it.
- **Drive setup off-camera via RPC, advance on-camera via a real UI click.**
  RPC (`runstatus.session.submit` / `patch_world`) from the test is a *different*
  client; its turns reach the page only via cross-client SSE — a timing race
  (sometimes live, sometimes needs a reload → nondeterministic frames). A real
  `intent-btn-*` click in the *driving* page renders the turn result directly:
  one deterministic visual path, no reload. Use RPC behind the curtain; click on
  camera. (Match the flow's `initial_world` with `patch_world` if a gate needs
  it — e.g. `judge_mode: human`.)
- **Captions/overlays must be `pointer-events: none`** or they silently
  intercept clicks on the UI beneath (an opaque banner over the tab bar = every
  tab click times out). `makeCaption(page)` already is; so is the curtain.
- **Playwright's default `actionTimeout` is 0 (INFINITE)** — a click on a
  missing/covered element hangs the whole run with no error. The config now caps
  it (15s); keep it. Don't write un-timeouted `.click()` in a loop.
- **The Claude Code harness suppresses Playwright's stdout** — a failing
  recording prints only "Exit code 1". `captureDiagnostics(page, artifactDir)`
  writes the failure + a `mark(step)` breadcrumb to `<artifactDir>/ERROR.txt`;
  read that file and the `NN-*.png` screenshots after the run. (Run in the
  background and read the task-output file, or redirect to a repo file.)
- **Unique `ADDR` port per spec; never `pkill -f kitsoki`.** A broad kill takes
  down the user's `make web-dev` servers (`/tmp/kitsoki-fixed web …`). Give each
  spec its own port and let `afterAll` stop only its own server.

The shared helpers (`_helpers/demo.ts`): `installCurtain` / `liftCurtain`,
`makeCaption` → `beat`, `captureDiagnostics`, `dwell` (PACE-scaled),
`DEMO_VIEWPORT`. For the recording lifecycle use `_helpers/server.ts`'s
`prepareVideoDir` (beforeAll) + `saveAndRemuxVideo` (after `context.close`) — the
remux pattern documented below, **not** a plain copy from the video dir.

## The loop

1. **Pick / author the spec.** Copy `multi-story.spec.ts` and trim to the scenes
   you want. The anatomy that matters (all proven in that file):
   - **Server lifecycle** — `spawn(BIN, ["web", "--stories-dir", DIR, "--flow",
     FLOW, "--addr", ADDR, "--db", tmpDb])`, then poll `GET /` until healthy.
     `--stories-dir stories` shows the **whole catalogue**; a single story dir
     shows just that one. The `--flow` fixture is **story-specific** (it encodes
     that story's intents + host stubs) and is applied to *every* session the
     home screen creates, so only that story can be driven no-LLM — drive its
     card, browse the rest.
   - **Drive by `data-testid`** — home: `home-view`, `story-card` (+
     `[data-story-path]`), `story-title`, `new-session-btn`, `rescan-btn`,
     `session-row`, `session-open`; observe (`RunView`): `breadcrumb`,
     `reload-button`, `reload-warning`, `drive-link`; drive (`InteractiveView`):
     `chat-section`, `current-state`, `state-badge`, `chat-transcript` →
     `chat-row-agent`, `composer-select`, `composer-input`, `composer-send`,
     `intent-btn-<intent>`, `observe-link`. Assert the `current-state` /
     `state-badge` after each turn — that is the hard signal the turn landed.
   - **A `SCENES` table** mapping each turn to a UI action (type into the
     composer for text-slot intents, click `intent-btn-*` for slot-less ones)
     and the state it should reach.
   - **Recording context** — `viewport {1440,900}`, `deviceScaleFactor: 2`
     (retina), `recordVideo: { dir: VIDEO_DIR, size }`.
   - **Video save pattern** — always use the three-step pattern from
     `_helpers/server.ts` (see below). Never `fs.copyFileSync` from `VIDEO_DIR`
     — that picks a stale file from a previous run.
   - **Pacing** — gate every delay on `WEB_CHAT_PACE` (typing delay, a beat
     before each click, a dwell on each settled scene) so the same spec runs
     fast for CI and slow for the camera. This applies to the **opening
     orchestration too**, not just the tour/scene loop: navigation that sits
     outside the paced loop (home → new session → observer) flashes past in
     well under a second if you `page.goto` straight through it. Use the shared
     `_helpers/server.ts` primitives — `cinematicGoto(page, url, {waitForTestId})`
     (goto + render-anchor + settle), `pacedClick(page, locator)` (beat before,
     settle after), and the shared `dwell` / `SETTLE_MS` — for every
     surface change, so the camera arrives and rests rather than lurching.
   - **Tour-driven intro (feature tours)** — for a feature tour, make the WHOLE
     video tour-narrated (including the opening) rather than silently
     `cinematicGoto`-ing into the observer. This is the golden pattern below —
     see **[Feature tour demo video](#feature-tour-demo-video--the-golden-example)**.
   - **Hash routing** — URLs are `#/`, `#/s/:id`, `#/s/:id/chat`.

## Demoing human usage — the conversation must be followable

A demo of someone USING kitsoki is worthless if a viewer can't follow the
conversation. `kitsoki-ui-qa` now **fails** a demo that breaks any of these
(rules 7–8 there), so author for them up front — don't fix it by hand after:

- **SHOW EVERY USER INPUT being typed, then HOLD it on screen — the #1 recurring
  defect.** The reflex is to set the value atomically (`input.fill(value)`,
  `el.value = value` + `dispatchEvent('input')`) and submit a beat later — the
  input then *flashes* and the viewer never sees what was asked. This applies to
  **every** operator input: a chat message, a slot value, a **free-text
  instruction / refinement**, a search query — NOT just the first message. For
  each input:
  1. **Type it character-by-character** —
     `input.pressSequentially(value, { delay })` with a pace-scaled per-char delay
     (e.g. `42 * WEB_CHAT_PACE` ms → 0 under the fast gate, readable in record
     mode). Never `fill()` / `el.value =` for a demo input.
  2. **Leave the input COMPOSED-BUT-UNSENT across the narration screenshot** (scroll
     the composer into view first), so the captured frame literally shows the
     operator's words. Drive in two phases — **type in the pre-step, submit in the
     post-step** (after the dwell/shot), never type-and-submit in one breath.
  3. **Frame each meaningful input with its own beat** anchored to `composer-input`
     (e.g. an "Author the deck" step AND a separate "Type the refinement" step) so
     the popover narrates *what* is being asked while it is on screen. A gesture
     buried in a pre-step hook with no beat is invisible.
  4. **Never submit via a page hook (`__kitsokiSubmitIntent`) without first showing
     the operator type it.** The hook is for *deterministic routing* of a
     semantic/free-text room, not a substitute for the visible input — type the
     words, hold, THEN route via the hook.
  Worked reference: `slidey-edit-video.spec.ts` (`composeVisibly` + `submitComposed`
  — type → hold across the shot → submit in the post-step; one beat per input).
  This is distinct from the response-scroll rule below: SHOW THE INPUT, then SCROLL
  THE RESPONSE.
- **Keep the chat visible — never let it be covered.** When the operator opens a
  document (the brief/PRD/diff via `host.ide.*`, or any file), it must appear
  BESIDE the conversation, not ON it. The extension already opens host.ide docs in
  the column beside the popped-out chat (`chatDocColumn` in
  `tools/vscode-kitsoki/src/ide-tools.ts`); the recording must keep that
  split (chat in one editor column, docs in the next) and minimise the sidebar so
  both read clearly. Verify the chat transcript is visible in EVERY beat where a
  file is open.
- **Scroll through every message — reveal, don't snap.** This is the single most
  important rule for a conversation video, and the one most often gotten wrong.
  `ChatTranscript.vue` **snaps the scroller to the BOTTOM** on every new message,
  so a reply taller than the viewport renders with its OPENING lines already
  scrolled off — the viewer never sees where the message starts, and the camera
  jumps. **Do not record against that auto-scroll.** Drive the camera the way a
  person follows a chat: after each turn settles, ease the new operator input to
  the TOP of the transcript, hold to read it, then ease DOWN through the reply at a
  readable pace (duration scaling with the reply's height) so every line passes
  on-camera and someone can pause on any of it. Type inputs visibly
  (character-by-character in record mode) so the viewer sees the message composed.
  - **Use the shared helper — never hand-tune dwells per turn.** The canonical
    implementation is `revealTurn` + `installConversationScroll` in
    `tools/vscode-kitsoki/tests/_helpers/conversation.ts` (it neuters the
    auto-scroll-to-bottom and owns the eased scroll); the native web tour has the
    same technique inline in
    `tools/runstatus/tests/playwright/gears-prd-design.spec.ts` (`revealTurn`).
    Wrap EVERY turn in it: `reveal(action, settle, label)`. Fixed `dwell()`s
    between turns + the component's native snap is exactly what makes a demo
    "jumpy" — never record that way.
- **No overlapping tour labels.** A coachmark/popover/tooltip must never sit on top
  of the chat. Dismiss any onboarding/tour overlay before driving, and keep
  spotlight popovers off the conversation.
- **Word intents naturally — never the raw `_` names.** Drive via natural language
  in the composer where the deterministic router allows; never type or surface the
  underscored internal intent names (`core__prd__start`). Visible labels (buttons,
  the state-diagram "via …" breadcrumb) are humanised by the UI — keep them that
  way; if a label shows a raw `name`, give the intent a natural `title:` rather
  than papering over it in the spec.
  - **Rebuild the SPA + binary before you trust the wording (the embed-staging
    trap).** The humanised labels live in the runstatus SPA, which `kitsoki web`
    serves from the **go:embed** bundle (`internal/runstatus/web/assets/index.html`).
    A source fix to `tools/runstatus/src/lib/intent.ts` does **nothing** to a
    recorded video until you `make build` (rebuilds the SPA → stages the embed →
    rebuilds `./kitsoki`). If a raw `core__…` name still shows after you "fixed" the
    wording, the recording almost certainly ran against a **stale embed/binary** —
    `make build` and re-record before debugging anything else. (See the
    `web-embed-staging` memory.)
  - **VS Code demos have a SECOND SPA staging — `make build` is NOT enough.** The
    extension's webview loads `tools/vscode-kitsoki/media/spa/index.html`, a
    SEPARATE copy from the binary's go:embed. `make build` does not touch it; it is
    staged only by the extension build (`node esbuild.mjs` `stageSpa`, i.e.
    `pnpm -C tools/vscode-kitsoki build`). So an SPA fix can land in `dist`/the
    binary yet the recording still serves the **stale media/spa** (a composer fixed
    from `<input>` to `<textarea>` still rendered the old input). Tell by the Vue
    scope hash: `grep -o 'data-v-[0-9a-f]*' …/dist/index.html` vs
    `…/media/spa/index.html` — different hashes ⇒ stale. The full VS Code rebuild is
    `make build` **then** `pnpm -C tools/vscode-kitsoki build`. (`packageExtension`
    in `tests/_helpers/launch.ts` now re-stages dist → media/spa at package time so
    the recording can't pick up a stale copy.)

## Video recording — the correct pattern

**Always emit MP4, never `.webm`.** Playwright records VP8 `.webm`, which (a)
omits the `DURATION`/`CUES` container atoms so most players show only the first
frame, and (b) does **not** play inline in VS Code, Keynote, Slack, or iMessage.
So the canonical recording artifact is the `.mp4` — the helper transcodes the
intermediate webm away. Never ship or commit a `.webm` as the deliverable.

Two shared helpers in `_helpers/server.ts` wrap this correctly:

```ts
// 1. In beforeAll — clear VIDEO_DIR so stale files never pollute the run:
prepareVideoDir(VIDEO_DIR);

// 2. In the context setup — capture the Video reference BEFORE context.close():
const page = await context.newPage();
const video = page.video();   // ← must happen before close()

// 3. In finally — save + transcode to MP4 AFTER context.close(), BEFORE browser.close():
await context.close();        // finalises the recording
await saveVideoAsMp4(video, ARTIFACT_DIR, "my-demo");  // → my-demo.mp4
await browser.close();
```

`saveVideoAsMp4(video, artifactDir, name)` saves `<name>-raw.webm`, transcodes
it to `<name>.mp4` (libx264 / yuv420p / +faststart / 30fps — the same settings
as `scripts/webm-to-mp4.sh`), and removes the raw webm on success. Only if ffmpeg
is unavailable or fails does it fall back to a `<name>.webm` with a warning (so a
recording is never silently lost). The helper is already imported in
`tour-video.spec.ts`, `agent-actions-video.spec.ts`, `trace-features-video.spec.ts`,
and `multi-story.spec.ts` — copy that pattern for any new recording spec.

**A fast run-through can never become the demo (tooling guard).** The canonical
`<name>.mp4` is reserved for a REAL recording. `saveVideoAsMp4` bakes the run kind
into the filename so a fast/assert run and a user-facing video can never collide
or clobber each other:
- `WEB_CHAT_PACE=0` (the CI gate) → `<name>.fast.mp4`, never the canonical name.
- A paced recording shorter than `KITSOKI_MIN_DEMO_SECONDS` (default 25s) is
  treated as an under-dwelled run-through → down-named `<name>.SHORT-<n>s.mp4`
  with a loud warning, and the canonical `<name>.mp4` is left ABSENT. Increase
  per-beat dwell (and/or the pace) and re-record to earn the canonical name.

The VS Code recorder has the same guard via `saveRecordingAsMp4` in
`tools/vscode-kitsoki/tests/_helpers/launch.ts` (keyed on `record` + the same min).

**Emitting a chapter sidecar (optional, for the video-review loop).**
A recorded tour can also emit a producer-agnostic **chapter sidecar**
(`<video>.chapters.json`) mapping each step's dwell window back to its
`TourStep` — the same shape `host.slidey.render` writes for slidey decks (see
[`hosts.md` → the chapter sidecar](../../architecture/hosts.md#the-chapter-sidecar)).
This is what lets the `/review` feedback panel flag a moment and resolve it to
the step that produced it. Use the `ChapterRecorder` + `writeChapters` helpers
in `_helpers/server.ts`:

```ts
import { ChapterRecorder, writeChapters } from "./_helpers/server.js";

const chapters = new ChapterRecorder();          // start the clock at record time
for (const step of TOUR_STEPS) {
  // … once the step's spotlight is settled and on-screen:
  chapters.open(step.id, step.title, "tools/runstatus/src/tour/manifest.ts");
  await dwell(page, step.dwellMs ?? 3000);        // this dwell becomes the window
}
// after the MP4 is saved:
const mp4 = await saveVideoAsMp4(video, ARTIFACT_DIR, "tour-video-demo");
writeChapters(mp4, chapters.list());              // → <mp4>.chapters.json (kind=tour)
```

`tour-video.spec.ts` is the worked example. The clock starts when the
`ChapterRecorder` is constructed, so call it right after the recording context
is created; each `open()` auto-closes the prior chapter.

**Why `video.saveAs()` and not `fs.readdirSync(VIDEO_DIR)[0]`?**
`readdirSync` picks the alphabetically-first file, which is the OLDEST webm in
the dir if the dir was never cleared. `video.saveAs()` gives you the specific
page's recording regardless of what else is in the dir.

2. **Validate fast (assertions only).** Iterate here until green — no waiting on
   dwells:
   ```bash
   cd tools/runstatus && WEB_CHAT_PACE=0 pnpm exec playwright test <name> --project=chromium
   ```
   Capture the real exit code — **never pipe the runner through `tail`**, it
   masks the exit status (see [[feedback_integration_verify_not_per_package]]).

3. **Record at watch-speed.** Drop `WEB_CHAT_PACE` (defaults to 1):
   ```bash
   cd tools/runstatus && pnpm exec playwright test <name> --project=chromium
   ```
   Output lands in `.artifacts/<name>/`: the canonical **`<name>-demo.mp4`**
   (the spec transcodes the raw webm away — never ship the webm) and numbered
   `NN-<scene>.png` screenshots.

4. **(Optional) Render GIF + contact sheet.** The MP4 is already the shareable
   deliverable; only run this if you also want a looping GIF or a storyboard.
   All write to `.artifacts/`, never committed ([[feedback_artifacts_dir]]):
   ```bash
   S=.agents/skills/kitsoki-ui-demo/scripts
   $S/render.sh .artifacts/<name>/<name>-demo.mp4    # gif + contact sheet (mp4 already made)
   # …or individually:
   $S/webm-to-gif.sh   .artifacts/<name>/<name>-demo.mp4 --width 900 # looping GIF for PRs/docs
   $S/contact-sheet.sh .artifacts/<name>/                            # NN-*.png → one contact sheet
   ```

5. **Verify the frames.** Open a couple of the `NN-*.png` (or the contact sheet)
   and confirm each scene renders correctly. The kitsoki rule holds in video too
   (`tools/runstatus/CLAUDE.md`): if a room view looks wrong, **fix the
   trace/render, not the recording** — never a UI hack to paper over a bad trace.

## The tools (`scripts/`)

The recording spec already emits the canonical MP4 (via `saveVideoAsMp4`), so
these are post-production extras, not part of the critical path.

| Script | Does | Notes |
|---|---|---|
| `render.sh <demo.(mp4\|webm)>` | One-shot: GIF + contact sheet (the sibling `NN-*.png` from the video's dir); transcodes to MP4 first only if handed a legacy webm | Convenience wrapper over the two below |
| `webm-to-mp4.sh <in.webm> [out.mp4] [--fps N] [--width W]` | H.264 + `yuv420p` + `+faststart` — the universally-playable share format | Only needed to convert a stray/legacy `.webm`; specs already emit MP4 |
| `webm-to-gif.sh <in.(mp4\|webm)> [out.gif] [--fps N] [--width W]` | Two-pass palettegen/paletteuse high-quality looping GIF | For embedding in PRs / markdown; keep `--width ≤ 900` |
| `contact-sheet.sh <dir> [out.png] [--cols N] [--tile-width W]` | Tiles the numbered scene screenshots into one image | A storyboard for quick review / PR description |

All require `ffmpeg` on PATH (Playwright's browser install or a system ffmpeg).

## Feature tour demo video — the golden example

When the ask is **"make a tour demo video"** of a specific feature (a drawer, a
new panel, a capability), copy the **agent-actions** demo — it is the golden,
maintained reference:

- spec:     `tools/runstatus/tests/playwright/agent-actions-video.spec.ts`
- manifest: `tools/runstatus/src/tour/agent-actions-manifest.ts`
- (sibling) `trace-features-video.spec.ts` + `trace-manifest.ts` — same shape,
  for the trace-introspection feature.

What makes it the template:

- **The whole video is tour-driven.** The manifest's first four steps live on
  `home`/`interactive` routes: a centered welcome that names the feature and the
  demo story (the bug-fix pipeline), a spotlight on the `story-card`, then two
  `kind: "action"` + `advance: "route-match"` steps that click `new-session-btn`
  then `observe-link`. Navigation is narrated by popovers, never a silent
  `page.goto`. The remaining steps walk the feature on the observer (route
  `"any"`).
- **One manifest drives both the live overlay and the video.** The spec injects
  the array via `window.__startTourWithSteps(JSON.stringify(STEPS))` and asserts
  each popover `title` against the manifest, so the recording can't drift from
  what users actually see.
- **Submit AFTER you reach the observer.** Capture the session id at the
  `new-session-btn` step, click `observe-link` while the chat view is STATIC,
  `waitForURL` the observer, and only THEN fire `patch_world` / `submit` so the
  trace streams into the observer's live trace. Submitting *before* the click
  re-renders the chat under the click and the route-match advance is lost
  (mirrors `tour-video.spec.ts`).
- **Pre-step hooks open the surfaces a step needs.** Before a step whose target
  lives inside a drawer/pane, the spec opens that pane (e.g. `openDrawerForCall`,
  `openTaskDetail`) and `dwell(page, SETTLE_MS)` so the spotlight lands on a
  composed frame, not a half-rendered flicker.
- **The single backdrop only blanks the page for anchorless (`center`) steps;**
  targeted steps leave a click-through hole over the real control.

**Author + record** (the four commands — MP4 is the deliverable):

```bash
# 1. Rebuild the SPA into the binary (mandatory — go:embed)
make build && cp ./kitsoki bin/kitsoki

# 2. Validate fast (assertions only, no dwells)
cd tools/runstatus && WEB_CHAT_PACE=0 pnpm exec playwright test agent-actions-video --project=chromium

# 3. Record at watch-speed → .artifacts/agent-actions/agent-actions-demo.mp4
cd tools/runstatus && pnpm exec playwright test agent-actions-video --project=chromium

# 4. (optional) GIF + contact sheet from the MP4
.agents/skills/kitsoki-ui-demo/scripts/render.sh .artifacts/agent-actions/agent-actions-demo.mp4
```

**To make a tour demo video for a NEW feature:** copy `agent-actions-manifest.ts`
→ `<feature>-manifest.ts` and rewrite the step `title`/`body`/`target` for your
feature — **keep the four-step home → observer intro** so the whole video stays
tour-narrated. Copy `agent-actions-video.spec.ts` → `<feature>-video.spec.ts`,
point it at the new manifest and a fresh `ADDR` port, adjust the pre-step hooks
to open your feature's surfaces, then run the four commands above with the new
spec name. Anchor every `target` to a `data-testid` the feature actually ships.

## Dev-story PRD → Design (golden conversation-driven example)

When the ask is **making a demo of conversation-driven development** (iterative
clarification, brief refinement, multi-document publication in one session),
copy the **dev-story PRD → Design** demo — it is the golden, maintained reference:

- feature:  `features/dev-story-prd-design.yaml`
- manifest: `tools/runstatus/src/tour/generated/dev-story-prd-design.ts` (generated — `make features`)
- flow:     `stories/dev-story/flows/prd_to_design_full.yaml` (no-LLM, cassette-driven)
- spec (Playwright, stub until slice 2):
  `tools/runstatus/tests/playwright/dev-story-prd-design-video.spec.ts`

What makes it the golden example:

- **The whole video is tour-driven.** The manifest's 11 steps walk the canonical
  conversation-driven-development loop: PRD discovery → multi-round clarification
  → PRD publish (to `docs/prd/<slug>.md`) → design intake (seeded from the PRD) →
  design brief refinement → design publish (to `docs/proposals/<slug>.md`) +
  auto-minted feature ticket (`issues/features/`) → back to the hub. Narration
  and `drive:` actions are inseparable — every action is framed by a popover
  explaining *why* kitsoki does it that way.
- **It is kitsoki's self-targeting dogfood — "kitsoki on kitsoki".** Unlike the
  gears-rust demo (now an external `stories/gears-rust/` instance in the gears
  repo, which retargets an external
  repo and skips the ticket mint), this walk authors kitsoki proposals using
  kitsoki itself — the cleanest proof that the system can improve itself.
- **Binary-rendered.** This demo renders straight from the binary with
  `kitsoki tour --feature dev-story-prd-design` (no Playwright, no Node —
  headless Chrome + ffmpeg alone), the proof that the binary-native tour renderer
  works end-to-end. The flow fixture is no-LLM (cassette-driven) and passes under
  `kitsoki test flows stories/dev-story/app.yaml`, so the *content* is verified
  independently of the recording.

**Record via the binary:**

```bash
kitsoki tour --feature dev-story-prd-design --out .artifacts/dev-story-prd-design/
```

See the [`kitsoki tour` reference](../../web/tour.md) for the full flag surface,
output artifacts, and the no-LLM contract. The Playwright path
(`agent-actions-video.spec.ts` is the template) remains available for demos that
need browser features the binary renderer does not yet drive, but the binary
command is the default for any tour bound to a feature catalog entry.

This demo is the **proof that conversation-driven-development methodology** (the
epic at `docs/proposals/conversation-driven-development.md`) works for kitsoki
itself — and it runs no-LLM, deterministic, and verifiable.

## rrweb capture → replay-render (deterministic, server-free mode)

The default mode above screen-records a **live** `kitsoki web` drive — the camera
rolls against a running server, so timing varies run-to-run. The **rrweb mode**
splits production into two deterministic halves so the video becomes a pure
function of `(captured events, holds, viewport, DSF)`:

1. **Capture (one live drive).** Drive the existing live tour ONCE with
   `installCapture(page)` attached, recording the session's **full** rrweb
   DOM-mutation stream, then `dumpCapture(page)` + `writeEvents(events, path,
   viewport)` to persist `<tour>.rrweb.json` **and** its `<tour>.rrweb.capture.json`
   viewport sidecar.
2. **Render (server-free, re-runnable).** Replay that stream through an rrweb
   `Replayer` while Playwright screen-records — no server, no story runtime, no
   live-timing variance. Re-render frame-exact forever from the JSON + the pinned
   local rrweb bundle, offline.

**The determinism win.** rrweb is the **local pinned bundle**
(`node_modules/rrweb/dist/rrweb.umd.min.cjs`, injected via `page.addScriptTag({
path })` — **never a CDN**), so the render depends only on the committed JSON +
that pinned bundle: offline, reproducible, re-renderable without ever rebuilding
or rerunning the server. Capture once live (the slow part); iterate the render
fast and free.

**⚠️ Canvas/video boundary — this mode does NOT cover every surface.** Capture
runs `recordCanvas:false` and is validated only on **SVG + HTML/CSS** surfaces
(the agent-actions drawer, the StateDiagram). Any tour with a `<canvas>`,
`<video>`, or WebGL surface will **not** reconstruct under this config and MUST
stay on the **live screen-record path** (the `*-video.spec.ts` specs — the
fallback). Do not move a canvas/video/WebGL tour onto rrweb mode.

**⚠️ Capture == render viewport/DSF invariant.** The render forces
`transform:none` on the rrweb player wrapper to defeat rrweb's fit-scale — that
is clip-safe **only** when the render viewport/DSF equals the capture's;
otherwise it silently clips to the top-left. `writeEvents(...viewport)` records
the capture viewport/DSF in the sidecar and the render helpers
(`assertViewportMatchesCapture`) **throw loudly** on any mismatch rather than
ship a clipped video. (Guard test: `rrweb-replay-viewport-assert.spec.ts`.)

### Worked reference specs

| Spec | Role |
|---|---|
| `tests/playwright/agent-actions-rrweb-capture.spec.ts` | **capture** — the simple all-DOM tour (forks the golden `agent-actions-video.spec.ts`; same live drive + baseline, plus the rrweb hooks). 1600×900, DSF 1. |
| `tests/playwright/diagram-showcase-rrweb-capture.spec.ts` | **capture** — the complex view-dwell tour (SVG StateDiagram). 1600×900, DSF 1. |
| `tests/playwright/rrweb-replay-render.spec.ts` | **render** — replays a captured stream (`RRWEB_TARGET=agent-actions\|diagram-showcase`, `RRWEB_HOLDS=1` for the held render). |
| `tests/playwright/rrweb-replay-smoke.spec.ts` | fast end-to-end smoke of the whole round-trip. |
| `tests/playwright/_helpers/rrweb-replay.ts` | the harness: `installCapture` / `dumpCapture` / `writeEvents` / `renderReplayWithHolds` / `renderReplayToMp4` (+ `assertViewportMatchesCapture`). |

These point at the rrweb path the same way the live-record sections point at
`agent-actions-video.spec.ts`.

### Chapter-keyed holds — render each view for its real dwell

A straight-through replay (`renderReplayToMp4`) reproduces the DOM-mutation
**timeline**, but during a multi-second dwell the reconstructed DOM is static, so
the recorder drops frames and a view that held ~7s live collapses to ~1s in the
extracted frames. The fix (`renderReplayWithHolds`) drives the Replayer **chapter
by chapter**: `pause(seekMs)` to freeze each step's settled view, then **hold**
it on-screen for `holdMs` wall-clock before advancing. Pass a `chapters` array of
`{ id, seekMs, holdMs }`, **keyed off a `holds-chapters.json`** whose `holdMs` is
the tour manifest's per-step `dwellMs` (the dwell is the source of truth for how
long each view must hold) and whose `seekMs` is the capture timeline's settled
instant for that step. Use `renderReplayWithHolds` for any tour that lingers on a
view (the diagram-showcase class); `renderReplayToMp4` is fine only for short,
mutation-dense tours.

### Run it

```bash
make build && cp ./kitsoki bin/kitsoki   # rebuild (go:embed) — capture drives a live server

# 1. CAPTURE (one live drive) → .artifacts/rrweb-eval/<tour>/<tour>.rrweb.json (+ .capture.json sidecar)
cd tools/runstatus && pnpm exec playwright test agent-actions-rrweb-capture --project=chromium
#   diagram-showcase capture is LONG (~minutes) — run in the background and poll.

# 2. RENDER (server-free; re-run as often as you like — no rebuild, offline)
cd tools/runstatus && RRWEB_TARGET=agent-actions \
  pnpm exec playwright test rrweb-replay-render --project=chromium
#   view-dwell tours: add RRWEB_HOLDS=1 (needs <tour>/holds-chapters.json beside the events)
cd tools/runstatus && RRWEB_TARGET=diagram-showcase RRWEB_HOLDS=1 \
  pnpm exec playwright test rrweb-replay-render --project=chromium
```

### QA the replay video at ≥2fps

When handing a replay-rendered video to `kitsoki-ui-qa`, **sample at ≥2fps**
(`renderReplayWithHolds` extracts at fps=2 by default) — held views make a
slightly higher sample rate cheap and guarantee the QA sampler lands inside every
legible window rather than on a transition frame. Note one benign false positive
for the diagram tour: the dark diagram-canvas background trips the blank-scan
"large monochromatic region" advisory on a few frames — that is the background,
not a blank, and the scenarios pass on those frames; don't read it as a
regression.

### Known minor follow-up (not a blocker)

**G6** — the agent-actions `aa-rollup` tour step is an *explain* step with no
expand action, so neither the rrweb render NOR the live baseline ever shows a
rollup row expanding into a drawer. This is a pre-existing **tour-coverage gap
shared with the live baseline**, not an rrweb defect. Fix in the tour (give
`aa-rollup` an expand action) or set the scenario step `required:false` — a minor
follow-up, not a blocker for adopting rrweb mode.

## Onboarding tour recording

The generic onboarding tour has a dedicated, maintained spec that records it as a
first-class demo mode:

```
tools/runstatus/tests/playwright/tour-video.spec.ts
```

The spec imports the **same step manifest** the live overlay uses
(`src/tour/manifest.ts`) and asserts the popover title against it for every
step — a drift guard baked into the recording. It drives all 13 tour steps in
Oregon Trail no-LLM mode, submits one intent during the input-bar step so the
trace lights up, and captures a labeled `NN-<step-id>.png` per step.

**One-liner record** (rebuild + record + render MP4/GIF/contact-sheet):

```bash
make demo-tour
```

**Validate fast** (CI — assertions only, no dwells):

```bash
make demo-tour-fast
```

**Record + run the vision QA gate** (requires `claude` CLI):

```bash
make demo-tour-qa
```

Or step-by-step:

```bash
# 1. Validate
cd tools/runstatus && WEB_CHAT_PACE=0 pnpm exec playwright test tour-video --project=chromium

# 2. Record at watch-speed
cd tools/runstatus && pnpm exec playwright test tour-video --project=chromium

# 3. (optional) GIF + contact sheet — the MP4 is already produced by step 2
S=.agents/skills/kitsoki-ui-demo/scripts
$S/render.sh .artifacts/tour-video/tour-video-demo.mp4
```

Output lands in `.artifacts/tour-video/`: the canonical `tour-video-demo.mp4`,
an optional `.gif` + contact sheet, and numbered `NN-<step-id>.png` screenshots.

To QA the recording against the tour scenarios:

```bash
.agents/skills/kitsoki-ui-qa/scripts/qa.sh \
  .artifacts/tour-video/tour-video-demo.mp4 \
  --frames .artifacts/tour-video \
  --feature .agents/skills/kitsoki-ui-qa/templates/tour-feature.md \
  --scenarios .agents/skills/kitsoki-ui-qa/templates/tour-scenarios.yaml
```

The `--frames` flag passes the labeled PNGs directly (one per step, highest
fidelity — skips the ffmpeg scene-detection pass).

To add a new step to the tour, append a `TourStep` to `TOUR_STEPS` in
`src/tour/manifest.ts` — data-only, no other code changes:

```ts
{
  id: "my-feature",           // stable id; also the screenshot label
  route: "any",               // "home" | "interactive" | "any" (/s/:id observer)
  target: "my-testid",        // data-testid to spotlight (must be universal)
  waitForTarget: "my-testid", // wait for DOM presence before showing
  title: "Short feature name",
  body: "One sentence explaining what this does and why it matters.",
  placement: "bottom",        // top | bottom | left | right | center
  kind: "explain",            // explain (Next advances) | action (clicking advances)
  advance: "next",
  dwellMs: 4000,              // ms the video spec pauses on this step
},
```

Route mapping: `/` → `"home"`, `/s/:id/chat` → `"interactive"`,
`/s/:id` (observe) → `"any"`. An `"any"` step shows on all three routes in the
live overlay, so only anchor to elements that exist there (e.g. `view-mode-tabs`,
`trace-event-row`, `confidence-bar`).

## Cross-site / multi-act demos (driving a site other than kitsoki)

A demo can span **several surfaces** — multiple kitsoki acts, or kitsoki **plus
an external site** — recorded separately and composited with ffmpeg. The worked
reference is the **gh-issues** demo (bug → GitHub issue → triage):

- Acts 1 + 3 are ordinary kitsoki tours (`report-bug-video`,
  `dev-story-bugfix-video`), driven by the kitsoki tour overlay.
- **Act 2 drives GitHub** — `gh-issue-review-video.spec.ts` +
  `src/tour/gh-issue-review-manifest.ts`. The kitsoki tour overlay
  (`__startTourWithSteps`, `[data-testid=tour-*]`) only exists inside the kitsoki
  SPA, so an external page is narrated with the **portable** helpers
  `makeCaption` + `makeSpotlight` (`_helpers/demo.ts`) — both inject plain DOM
  and work on any page. The page itself is a **deterministic static fixture**
  (`fixtures/gh-issue-review.html`) driven over `file://`, never live GitHub:
  same no-network/no-cost posture as every kitsoki demo.
- **Composite** with `scripts/concat-videos.sh` (mpegts intermediates → clean
  concat; accepts `video:clip.mp4` and `card:img.png[:sec]` segments). Title
  cards are rendered by `scripts/make-title-card.mjs` (Chromium — this repo's
  ffmpeg has no `drawtext`). `scripts/record-gh-issues-demo.sh` orchestrates
  record-3-acts → cards → composite into
  `.artifacts/gh-issues-demo/gh-issues-cross-site-demo.mp4`.

For an external act: add a fixture (or a real allowed URL), a manifest of
`{id,target,title,body,dwellMs}` steps, and a spec that `page.goto`s it and walks
the steps with `spotlight(`[data-testid="…"]`)` + `caption(title, body)`. Anchor
each `target` to something the page actually renders.

To run kitsoki acts via **`go run`** (local dev — no binary to build/keep fresh)
set `KITSOKI_WEB_GO_RUN=1` (the default when `bin/kitsoki` is absent); stage the
go:embed SPA first with `make web`. Build a real binary (`KITSOKI_WEB_GO_RUN=0`)
for an actual client/CI capture.

## Full-editor (VS Code) mode

The same deterministic, no-LLM tour pipeline records the kitsoki UI **embedded in
a real VS Code window** (the extension under `tools/vscode-kitsoki/`) — a
full-editor walkthrough showing the Kitsoki sidebar, an open story file, a driven
session, the live trace, and the bottom Kitsoki Trace panel, all themed to the
editor. The worked reference is **`tools/vscode-kitsoki/tests/vscode-tour.e2e.spec.ts`**.

It mirrors this skill's patterns, adapted to VS Code:

- **One spec, two modes — gated on `KITSOKI_VSCODE_PACE`** (the analogue of
  `WEB_CHAT_PACE`). `0` (default) is the **assert-only de-risk gate** — every beat
  is a hard `toBeVisible`/`toHaveText`, no dwells, no recording. `≥1` is **record
  mode**: the SAME asserted beats plus per-beat dwells, narration, the editor
  beats, and `recordVideo`. Record mode only ADDS on top of the proven path, so it
  can't drift from the gate.
- **Real VS Code via an `_electron` launch helper** —
  `tools/vscode-kitsoki/tests/_helpers/launch.ts` (`launchVSCode`,
  `packageExtension`, `webviewFrame`) downloads + launches pinned VS Code 1.96.4
  through Playwright's `_electron`, strips all `VSCODE_*` env, and descends into
  the webview guest (`iframe.webview.ready >>> iframe[title]`). `recordVideo` is
  passed only in record mode.
- **recordVideo → MP4 + chapter sidecar.** Playwright records a `.webm`;
  `app.close()` flushes it, then the spec transcodes to a universally-playable
  **`.artifacts/vscode-tour/vscode-tour.mp4`** (libx264 / yuv420p / +faststart /
  30fps — same settings as `saveVideoAsMp4` / `scripts/webm-to-mp4.sh`) and
  removes the webm. **Never ship the `.webm`.** A single `ChapterRecorder` clock
  spans every beat → `<mp4>.chapters.json` (the same shape as the web tours).
- **Manifest reuse + drift guard.** The webview beats are narrated by the SAME
  `WEATHER_REPORT_TOUR_STEPS` the live web tour uses (injected into the webview
  via `window.__startTourWithSteps`, driven by `window.__tourGoTo`, dismissed by
  `window.__tourSkip`); each popover `title` is asserted against the manifest, so
  the recording can't drift from what users see. Beats **outside** the webview
  (open `app.yaml` in the editor, open the Kitsoki Trace panel) are driven by a
  thin in-spec editor-beat manifest (`{id,title,dwellMs}`) — no popover is
  possible there. Each beat is staged so the DOM visibly differs, dwelled until
  settled, then captured as a labeled `NN-<beat>.png` (the kitsoki-ui-qa
  `--frames` input).
- **Watch-speed staging tips proven here:** widen the Kitsoki sidebar (drag the
  vertical sash) AFTER the lobby submit so the report renders legibly without
  tripping the side-by-side breakpoint; clear the narration overlay
  (`__tourSkip`) before any SPA interaction or editor beat (its backdrop
  intercepts clicks); keep the leading `>` when filling the Command Palette
  (replacing it searches files, not commands); suppress VS Code chrome noise in
  the throwaway workspace settings (`git.enabled:false`,
  `editor.minimap.enabled:false`) so frames stay clean.

**Run it** (full build → gate, then record):

```bash
make vscode-e2e-fast                 # assert-only de-risk gate (KITSOKI_VSCODE_PACE=0)
make vscode-e2e                      # record the paced tour (KITSOKI_VSCODE_PACE defaults to 1)
KITSOKI_VSCODE_PACE=2 make vscode-e2e   # slower dwells
```

Then QA the produced MP4 — see [[kitsoki-ui-qa]] → "Full-editor (VS Code)
evidence" (pass the labeled `NN-*.png` via `--frames`).

## Terminal surface (MCP / coding-agent demos)

A demo where an *external coding agent* drives kitsoki over the **MCP** server
(`kitsoki mcp`) is recorded on a **terminal** surface, not the web SPA: an xterm.js
terminal **replays a committed `termcast` cassette** and is filmed through the same
camera / `ChapterRecorder` / `saveVideoAsMp4` contracts as every other demo. Claude
Code (TUI) is the POC; codex/copilot are additional cassettes. The harness is
`tools/mcp-demo/` (its `README.md` is the full recipe; the QA contract is the
`mcp-feature.md` / `mcp-scenarios.yaml` templates).

Same posture as the web demos — **no-LLM by construction**: the replay plays a
static cassette and never spawns a model (enforced by
`tools/mcp-demo/scripts/lint-no-llm.mjs`), with a single **gated** live `claude`
capture producing the cassette (*record once, replay forever*,
`scripts/capture-live.py`).

```
make mcp-demo-fast    # no-LLM validate (lint + PACE=0 assert)
make mcp-demo         # watch-speed record → .artifacts/mcp-demo/<agent>.mp4
make mcp-qa           # vision QA gate (GATED: local claude CLI)
```

## Pointers

- **Full-editor (VS Code) tour spec + launch helper:**
  `tools/vscode-kitsoki/tests/vscode-tour.e2e.spec.ts` +
  `tools/vscode-kitsoki/tests/_helpers/launch.ts` (one-spec-two-modes on
  `KITSOKI_VSCODE_PACE`; MP4 + `.chapters.json`; manifest reuse via
  `__startTourWithSteps`)
- **Golden feature-tour spec + manifest:**
  `tools/runstatus/tests/playwright/agent-actions-video.spec.ts` +
  `tools/runstatus/src/tour/agent-actions-manifest.ts`
- **Cross-site / multi-act demo:** `gh-issue-review-video.spec.ts` +
  `src/tour/gh-issue-review-manifest.ts` + `fixtures/gh-issue-review.html`;
  composited by `scripts/record-gh-issues-demo.sh` + `scripts/concat-videos.sh`
- **rrweb capture → replay-render (deterministic, server-free):**
  `tests/playwright/_helpers/rrweb-replay.ts` + `agent-actions-rrweb-capture.spec.ts`
  (simple) / `diagram-showcase-rrweb-capture.spec.ts` (complex view-dwell) /
  `rrweb-replay-render.spec.ts` (render) / `rrweb-replay-smoke.spec.ts` (smoke) /
  `rrweb-replay-viewport-assert.spec.ts` (viewport-match guard). Canvas/video
  surfaces stay on the live `*-video.spec.ts` path.
- Sibling feature tour: `trace-features-video.spec.ts` + `src/tour/trace-manifest.ts`
- Sibling feature tour (cassette slow-play streaming): `chat-stream-video.spec.ts` +
  `src/tour/chat-stream-manifest.ts` — films the live turn-stream in the MAIN
  CHAT (set `KITSOKI_CASSETTE_SLOWPLAY`; the spec defaults it to 1.5), then
  repeats the loop in the META OVERLAY (stub-paced via
  `KITSOKI_META_STREAM_DELAY_MS`) to prove both chats share one activity
  presentation
- Onboarding tour spec + manifest: `tour-video.spec.ts` + `src/tour/manifest.ts`
- Tour robustness test: `tools/runstatus/tests/playwright/tour-onboarding.spec.ts`
- Full-product walkthrough spec: `tools/runstatus/tests/playwright/multi-story.spec.ts`
- Tour QA templates: `.agents/skills/kitsoki-ui-qa/templates/tour-{feature,scenarios}.*`
- Shared helpers (video→MP4, server, pacing): `tests/playwright/_helpers/server.ts`
- Playwright config + globalSetup: `tools/runstatus/playwright.config.ts`,
  `tools/runstatus/tests/playwright/_helpers/`
- **Trace → flow/cassette generator (start a demo from a real dogfood session):**
  `kitsoki trace to-flow` — CLI in `cmd/kitsoki/trace.go` (`traceToFlowCmd`),
  transform in `internal/testrunner/fromtrace.go` (`ConvertTraceToFlow`),
  authoritative docs in `docs/tracing/trace-format.md` §11
- No-LLM posture + UI surfaces: `docs/web/README.md`
- File:// snapshot artifacts (static, no server): `_helpers/artifact.ts`

## Maintenance

Codex discovers this skill directly. Refresh the project-local Claude Code
symlink after adding or moving skills:

```
make setup
```
