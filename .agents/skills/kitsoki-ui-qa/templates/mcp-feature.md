# Plan being verified — MCP studio, driven by an external coding agent (Claude Code TUI)

## What changed

kitsoki ships a studio **MCP server** (`kitsoki mcp`) that lets an external coding
agent author stories, drive/introspect sessions, and *see* the TUI of any state —
all through one stdio facade (`story.*` / `session.*` / `render.*`). This demo
records the **Claude Code** terminal driving that server end to end: the agent
authors a tiny story, checks it, tests it, drives a live session, and renders the
kitsoki screen — covering the whole surface in one run.

The demo is recorded through the shared kitsoki demo→QA pipeline (camera 1600×900,
ChapterRecorder sidecar, 25s duration floor, the same gates as the web/vscode
demos). It is **deterministic and no-LLM at replay**: the agent's session is a
committed `termcast` cassette replayed in an xterm.js terminal — recorded once from
a gated live `claude` run, then replayed for free, forever. The replay never spawns
a model or the live MCP server.

## What the operator should now SEE inside one terminal window

Numbered because the video QA reads this as a walkthrough. Each is observable on
screen — a Claude-Code tool-call card (`⏺ kitsoki - <tool> (MCP)`) and its result,
narrated by a caption banner at the top of the frame.

1. **A coding agent attached to the kitsoki MCP server** — a real terminal window
   (titlebar ending `kitsoki mcp`, an `MCP` badge) working through tool-call cards
   on a branded studio backdrop (a `kitsoki · mcp studio · driven by Claude Code
   over MCP` footer).
2. **Reading or authoring over MCP** — a `story_read` or `story_write` tool card
   surfacing the story (its YAML source, or a written/validated result) for the
   `barista` story (rooms `lobby → order → confirm`).
3. **Checking it** — a `story_validate` card reporting `✓ valid` and a `story_graph`
   card printing the room graph with its intents.
4. **Testing the flows** — a `story_test` card showing the flows pass against a
   **flow/cassette oracle (no LLM)**.
5. **Driving a live session** — `session_new` (harness **replay**, no LLM), then
   the agent advances the machine via `session_submit` (a direct intent) or
   `session_drive` (free text), reaching `confirm` with world `{ drink: flat white }`.
6. **Seeing the result** — a `render_tui` card followed by the **rendered kitsoki
   TUI** for the `confirm` room (room title, body, the intent menu, the state/world
   footer) — the same screen a human operator would see.
7. **Introspecting + done** — a `session_inspect` card (state / world / allowed
   intents / last turns) and the agent's closing summary.

## What this is NOT

- **Not a live-LLM run.** At replay nothing calls a model; the agent's bytes come
  from a committed cassette. (One *gated* live `claude` run captures the cassette;
  every render after is free and identical.)
- **Not a web screenshot or a mockup.** The terminal is a real xterm.js rendering
  the agent's session — ANSI tool-call cards, a real cursor, a real rendered TUI
  frame — inside the recorded video, not a static image.
- **Not kitsoki's own TUI being driven.** This is an *external* agent (Claude Code)
  calling the kitsoki MCP tools; the rendered TUI in beat 6 is what that agent
  fetched via `render.tui`, shown back inside its terminal.
