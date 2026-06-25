---
name: session-recap
description: Reconstruct "what we've been working on lately" from the current repo's actual Claude Code transcripts. Use when the user opens with "we've been working on…", "what were we doing", "catch me up", "remind me where we left off", or after a /clear and wants recent work restored. Thin trigger that delegates to the Haiku `session-recap` subagent so the transcript bulk never enters this context.
---

# Session Recap

This skill is a thin shell. The real work — deterministically distilling the current repo's recent transcripts and reading them — happens in the `session-recap` subagent (`.claude/agents/session-recap.md`, pinned to `model: haiku`). Delegating keeps the distilled transcript volume out of *this* session's context; you only relay the agent's short recap.

## What to do

1. **Launch the `session-recap` subagent** via the Agent tool. Do **not** run `recap.sh` or read transcripts yourself — that would pull the bulk into this context, defeating the point.

2. **Forward the user's intent verbatim**, plus any flags they gave, as the agent prompt. The agent understands a free-form focus hint and these flags (pass through whatever the user supplied; otherwise let the agent default):
   - `--max N` — how many recent sessions (default 8)
   - `--since 24h|3d|…` — restrict to a recent window
   - `--grep WORD` — topical prefilter when the user asks about a specific thread
   - `--dir PATH` — recap a different repo/worktree

   Example prompts to the agent:
   > Catch me up on what we've been working on in this repo recently.
   > What have we been doing on the reload work lately? --grep reload --since 3d

3. **Relay the agent's recap** to the user, optionally prefixed with a one-line scope header (e.g. `Recap: 8 most-recent sessions`). Don't editorialize on top — the user wants the recap, not your reaction to it.

## Notes

- If the agent reports no session history / nothing in the window, relay that plainly. Don't fabricate a recap.
- This reads the **real distilled transcripts** (via `tools/session-mining/recap.sh`), so it works regardless of whether the Stop-hook `.context/` summaries exist, and gives turn-level recency.
