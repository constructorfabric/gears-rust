# External-target PRD → Design demo — what the video must prove

The `gears-prd-design` tour video demonstrates kitsoki's dev-story hub driving
an **external project** (`constructorfabric/gears-rust`) through its
**PRD → Design** spec chain, entirely in the main chat, no LLM. The whole walk
is narrated by a tour overlay.

The evidence under review is the recording (or its per-scene / per-scroll-stage
PNG frames). It must show — **legibly** — the full conversation, not just the
last room view of each step.

## What the video must show

1. **Tour-driven intro** — opens on the home story library, spotlights the
   gears-rust story card, then drives New session → the interactive chat view.
2. **PRD discovery** — the operator's pitch for the *notes-service* gear and the
   interviewer's distilled reply, in the chat.
3. **Multi-round clarification** — a structured clarification round (actors, the
   success metric) answered in the chat, then a **second** round (tenant
   isolation, admin visibility) with the first round preserved in the
   "earlier rounds" log. Both rounds must be readable.
4. **Drafted PRD** — the authored PRD surfaced in the chat (title, confidence,
   body) for review.
5. **Published into the gears tree** — the chat reports the PRD published to
   `gears/notes-service/docs/PRD.md` (the fixed gears-sdlc name, inside the
   gears-rust checkout — NOT kitsoki's docs/prd/).
6. **PRD → Design handoff** — `continue` seeds the design intake with a pointer
   to the just-published PRD (`Author a design from the PRD at
   gears/notes-service/docs/PRD.md`).
7. **Design brief refinement** — the design's own clarification loop: a brief
   with the refiner's flagged gaps (components, cpt-IDs, NFR allocation), then a
   quality check before drafting.
8. **DESIGN published alongside** — the chat reports the design published to
   `gears/notes-service/docs/DESIGN.md`, with **no** kitsoki feature ticket.

## The bug this QA guards against (read this)

The earlier cut of this video drove many turns per step and then **jumped the
chat to the bottom instantly**, capturing only ONE settled frame per step — the
last room view. Everything in between (the clarification questions, the answer,
the brief) **scrolled past too fast to read** and was never held in a frame.

So the gate is not just "does the final state appear" — it is **"is the whole
conversation visible at a readable pace."** Each conversation segment above must
appear in its **own held, legible frame**. A step whose intermediate turns are
absent from every frame (only the bottom of that step is shown) is a FAIL for
this video, even if the final room view is crisp: the demo scrolled content past
before it could be seen.

Concretely, the fix the video must exhibit: the chat is **panned through its
content in readable stages** (a labeled `NN-<scene>-sK.png` frame per stage),
so a reviewer — and the operator watching — can read each part of the
conversation before it scrolls away.
