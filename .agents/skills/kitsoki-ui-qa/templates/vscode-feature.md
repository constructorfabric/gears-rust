# Plan being verified — Embed the kitsoki web UI in a VS Code extension

## What changed

The kitsoki web UI (the chat + trace SPA normally served by `kitsoki web` in a
browser) is now embedded INSIDE a VS Code extension. The extension spawns the
`kitsoki` backend as a child of the extension host, relays the SPA's JSON-RPC
over `postMessage` through a `BridgeTransport`, and renders the same single-file
SPA into an **editor-area WebviewPanel** — chat **front and center** in the wide
editor, not crushed into the narrow sidebar.

The Activity Bar **Kitsoki** icon hosts a thin launcher (an "Open Kitsoki Chat"
button); clicking the icon opens the editor panel. Inside that panel the SPA runs
its **embed layout**: the **chat is dominant**, and **trace + graph live in a thin
hint rail** as collapsed, live cards (event count, current room, status, rooms,
intents) that the operator can **maximize** beside the chat (a horizontal split)
and minimize again.

Everything runs in ONE VS Code window: the operator's code and kitsoki side by
side. The embed is themed to the editor (dark VS Code chrome; the SPA inherits
`--vscode-editor-background` / `-foreground` via an additive theme shim).

The demo is recorded against a deterministic, **no-LLM** backend: the
`weather-report` story under `flows/tour.yaml`, whose `starlark_http_cassette`
replays every HTTP call (geocode + forecast). No model, no network — same input,
same frames.

## What the operator should now SEE inside one VS Code window

1. **The Kitsoki editor panel** rendering the **story library** — the "Weather &
   Climate" story card — in the editor area, themed to the dark editor chrome (not
   a browser tab).
2. **A session started** ("New session" on the story card) → the interactive chat
   view opening in the **lobby** room, with the **chat front and center** and a
   thin **hint rail** to its right holding a **Trace** card and a **Graph** card
   (live counts: events, room, rooms, intents).
3. **A turn driven and the state advancing** — submitting a "Tokyo" forecast
   advances the room **lobby → report**; the resolved **"Tokyo, Japan"** forecast
   report (place, coordinates, current conditions, 5-day table) renders in the
   chat while the hint rail updates live. This is the no-LLM cassette replay
   producing real, derived output.
4. **Maximizing the Trace hint** → the full **trace timeline** expands beside the
   chat, showing event rows including a `host.starlark.run` row — the audit record
   that is kitsoki's whole point.
5. **Switching to the Graph** → the full **state diagram** (the room graph) expands
   in place, with the current **report** station marked ("you are here") and the
   room transitions drawn.
6. **Code and kitsoki side by side** — the story's source (`app.yaml`) opens in a
   split editor BESIDE the Kitsoki panel, both themed to the editor chrome: your
   code and kitsoki in one workspace.

## What this is NOT

Not a browser screenshot, not a mockup, not a live-LLM run. The evidence must
show the kitsoki UI rendered **inside the VS Code chrome** (Activity Bar, editor
pane) — the whole point is the embed, with chat front/center and trace/graph as a
maximizable hint rail, not the UI in isolation and not crushed into the sidebar.
