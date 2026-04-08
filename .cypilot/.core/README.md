# .core — Cypilot Core Files

**Do NOT edit files in this directory.**

These files are copied from the Cypilot cache (`~/.cypilot/cache/`) during
`cpt init` or `cpt kit install`. They are the read-only reference copies of:

- `skills/` — Cypilot skill scripts and CLI entry points
- `workflows/` — workflow definitions
- `requirements/` — validation requirements
- `schemas/` — JSON schemas for configuration files
- `architecture/specs/` — traceability, CDSL, CLI, and kit specifications

To update these files, run `cpt init --force` or `cpt kit update`.
Any manual changes **will be overwritten** on the next update.
