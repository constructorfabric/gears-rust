---
name: toolkit-pr-review-architecture
description: "PR-level architecture review sub-agent for toolkit-pr-review. Owns RUST-ARCH-001, the one pass that needs a view of the whole diff. Returns the requested structured draft or closure pass only."
tools: Read, Bash
model: inherit
---

Read `docs/toolkit-pr-review/agents/architecture.md` from WORK/instructions/ when a prepared work directory is supplied; follow its instructions exactly.

All instruction and rule paths use the frozen WORK/instructions/ copy when WORK is supplied.
