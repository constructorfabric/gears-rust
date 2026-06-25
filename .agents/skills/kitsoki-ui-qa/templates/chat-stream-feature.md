# Feature: Live chat streaming (thinking + tools) with preserved activity — main chat AND meta chat

The kitsoki web UI's main chat (InteractiveView) streams the agent's native
execution feed while a turn is in flight, then preserves that feed after the
turn lands. The demo drives the bug-fix story under cassette slow-play
(KITSOKI_CASSETTE_SLOWPLAY), so the recorded task + decide transcripts replay
into the live turn-stream path at real-ish pace — deterministic, no LLM.

NEW in this iteration: the META OVERLAY chat (the ✦ Meta launcher → Story
Q&A) now renders the agent's activity with the SAME shared presentation —
one component set (ActivityFeed / ActivityDisclosure) drives both chats. The
demo's second half opens the overlay, asks a question, and must show the
identical thinking/tool feed treatment there: streamed live in the overlay's
bubble (the deterministic stub oracle emits a thinking event, a Read tool
call, then the reply — paced by KITSOKI_META_STREAM_DELAY_MS), then collapsed
into the assistant message and expandable. The reply itself must NOT be
duplicated into the feed (the feed holds the reasoning, the bubble holds the
answer).

Key surfaces a recording of this feature must show:

- **Tour-driven intro** — the home story library, the bug-fix story card, and
  a New Session action that lands in the chat (interactive) view.
- **The streaming turn** — after the `start` choice is clicked, a thinking
  bubble appears and FILLS OVER TIME with the agent's activity in ARRIVAL
  order: each 🧠-marked thought stays ABOVE the tool calls that follow it
  (Read, Grep, then a thought, then Edit, Bash, then a thought, then two
  mcp__validator__submit calls). Tool rows show a blue tool name plus an
  argument preview. Thoughts must NEVER sit clumped at the bottom below the
  tools, and thoughts carried as extended-thinking blocks (e.g. "The
  off-by-one is in the loop bound.") must appear — not only `text`-block
  narration.
- **Collapse on landing** — when the turn completes, the live bubble
  dissolves into the agent's final reply (the REPRODUCING room view), and a
  collapsed one-line summary "🧠 3 thoughts · 6 tool calls" appears INSIDE
  that agent bubble, above the view. The full feed is hidden at this point.
- **Expand** — clicking the summary expands the same interleaved feed (same
  🧠 thoughts, same tool rows, same order) inside the bubble, with the final
  view still visible beneath it.
- **The meta overlay arc** — after the main-chat walk: the ✦ Meta launcher
  opens its mode menu; Story Q&A opens the dark overlay chat; a question is
  typed and sent; while the meta turn streams, the overlay's bubble shows the
  same feed presentation (a 🧠 thought row "Let me look at the story
  definition first." above a blue Read tool row) plus the reply text arriving;
  when the reply lands, a collapsed "🧠 1 thought · 1 tool call" summary sits
  inside the assistant message above the reply; expanding it shows the same
  two feed rows — and the reply text appears ONLY in the message bubble, never
  as a feed row.
- **Tour popovers** — each step is narrated by a spotlight popover whose
  title matches the step; the popover must not fully obscure the surface it
  describes (in particular the streaming bubbles and the expanded feeds).
