---
cf: true
type: workflow
name: cf-gears-gear-creator
description: Invoke when the user wants to create a new ToolKit gear from scratch, or add a feature to an existing one (REST endpoints, DB entities, OData filtering, plugins, lifecycle/background tasks, SSE events, error types) — e.g. "create a new gear", "scaffold a gear", "add a REST endpoint to <gear>", "/cf-gears-gear-creator"
version: 1.0
purpose: Thin Studio-kit wrapper over the gear-creator skill.
---

# Gear Creator Workflow

ALWAYS open and follow `{cf-studio-path}/.core/skills/studio/SKILL.md` FIRST.

LOAD `{skill_gear_creator}` and FOLLOW all of its instructions, passing through whatever
parameters were given to this workflow.

That skill's own text names this repo's own reference gear and lint tool directly; prefer
`{project_paths}`'s `reference_gear_path` and `{ci_commands}`'s `dylint` entry instead when
either is empty or doesn't match what this project actually has.
