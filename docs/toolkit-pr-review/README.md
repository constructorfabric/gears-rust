# toolkit-pr-review

The canonical workflow is `.claude/skills/toolkit-pr-review/SKILL.md`.
Claude, Codex, Cursor, Devin and Windsurf entrypoints use it. Constructor Studio's
`docs/pr-review/` workflow remains separate.

The calling agent runs the models. The Python commands only prepare pinned Git
sources, validate responses, group original claims, build formatter inputs and
control publication. They use the standard library and install no model or RAG service.
Each of the seven modules has a discovery pass and a separate evidence pass.

- `agents/`: shared instructions and module ownership.
- `rules/`: review criteria; `RULES.md` is a generated index.
- `policies/`: severity, evidence, grouping and output contracts.
- `review-conventions.md`, `comment-style.md`: reporting and wording rules.

For a PR, use `/toolkit-pr-review NUMBER [--repo OWNER/REPO]`.
For a local report, use `/toolkit-pr-review local [BRANCH]`.
LOW findings stay in local results and never enter the GitHub payload.
Incomplete evidence blocks publication. Each run freezes its sources and instructions
with hashes in its own working manifest.

## Development checks

`python -B tools/scripts/toolkit-pr-review/lint.py`

`python -B -m unittest discover -s tools/scripts/toolkit-pr-review/tests`

These are developer/CI regression tests for the Python commands, not part of invoking
this skill. They protect pinned sources, IDs, LOW filtering, grouping and safe POST
retries. They make no LLM or network calls and do not measure model review quality.
The skill does not run builds or tests against the code under review.
