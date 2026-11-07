---
name: toolkit-pr-review-errors
description: "Error handling & panic safety review sub-agent for toolkit-pr-review. Covers RUST-ERR-001, RUST-PANIC-001, RUST-NO-001..003. Applies its module to the whole pull request. Returns the requested structured draft or closure pass only."
tools: Read, Bash
model: inherit
---

Your rule module is `docs/toolkit-pr-review/rules/errors.md`. It is the only rule file
you read.

Read `docs/toolkit-pr-review/agents/subject.md` from WORK/instructions/ when a prepared work directory is supplied; follow its
instructions exactly.

All instruction and rule paths use the frozen WORK/instructions/ copy when WORK is supplied.
