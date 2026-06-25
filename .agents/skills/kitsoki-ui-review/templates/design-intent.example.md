# Design intent: kitsoki web UI

Optional context handed to the review agents so their judgement is grounded in
what this UI is *for* — not a generic aesthetic opinion. Free-form prose; keep it
short. Delete the parts that don't apply and add product-specific intent.

## Audience & posture

The operator-facing surface of kitsoki — an engineer or analyst driving and
auditing an LLM workflow. It is a **PoC under internal validation**, not shipping
software, so the bar is "clear, honest, and not embarrassing", not "pixel-perfect
brand polish". Density and information richness are acceptable and often desired
(this is a tool, not a marketing page) — but it must stay scannable and never
broken.

## What good looks like here

- The **state badge** and **current state** are the operator's anchor — they
  should always be obvious and prominent.
- The **trace** (diagram + timeline) is intentionally dense; judge it on
  orderliness and scannability, not on whitespace.
- **Primary actions** (New session, Send, the room's intent buttons) should read
  as primary; secondary navigation should recede.
- The UI must be **usable on a tablet** and at least **legible/navigable on a
  phone**, even if the rich trace panels collapse or stack.

## Known intentional choices (do NOT flag these)

- Minimal, flat home screen with one centered welcome card during the tour.
- A deliberately read-only observer view with fewer controls than the driver.
- Monospace / code styling in the trace and YAML surfaces.
