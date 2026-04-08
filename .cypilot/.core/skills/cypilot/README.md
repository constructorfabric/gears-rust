# Cypilot Skill Engine

Deterministic agent tool for structured workflows, artifact validation, traceability, kit management, and multi-agent integration.

## Commands

### Setup & Configuration

| Command | Description |
|---------|-------------|
| `init` | Initialize Cypilot config directory (`.core/`, `.gen/`, `config/`) and root `AGENTS.md` |
| `update` | Update `.core/` from cache, regenerate `.gen/` from user blueprints, ensure `config/` scaffold |
| `info` | Discover Cypilot configuration and show project status |
| `generate-agents` | Generate agent-specific entry points for supported agents (Windsurf, Cursor, Claude, Copilot, OpenAI) |

### Validation

| Command | Description |
|---------|-------------|
| `validate` | Validate artifacts against templates (structure, IDs, cross-references) and code traceability markers (pairing, coverage, orphans) |
| `validate-kits` | Validate kit configuration, blueprint markers, and constraints |
| `validate-toc` | Validate Table of Contents in Markdown files (anchors, coverage, staleness) |
| `self-check` | Validate kit examples against their own templates (template QA) |
| `spec-coverage` | Measure CDSL marker coverage in codebase files (coverage %, granularity score) |

### Search & Navigation

| Command | Description |
|---------|-------------|
| `list-ids` | List all Cypilot IDs from registered artifacts (filterable by pattern, kind) |
| `list-id-kinds` | List ID kinds that exist in artifacts with counts and template mappings |
| `get-content` | Get content block for a specific Cypilot ID from artifacts or code |
| `where-defined` | Find where a Cypilot ID is defined |
| `where-used` | Find all references to a Cypilot ID |

### Kit Management

| Command | Description |
|---------|-------------|
| `kit install` | Install a kit from source directory (copies blueprints, scripts, generates resources) |
| `kit update` | Update kit reference copies from cache and regenerate `.gen/` outputs |
| `kit migrate` | Three-way merge of kit blueprints with interactive per-marker decisions |
| `generate-resources` | Regenerate `.gen/` outputs from user blueprints |

### Utility

| Command | Description |
|---------|-------------|
| `toc` | Generate or update Table of Contents in Markdown files |

### Migration

| Command | Description |
|---------|-------------|
| `migrate` | Migrate Cypilot v2 projects to v3 (adapter → blueprint, JSON → TOML) |
| `migrate-config` | Convert legacy JSON config files to TOML format |

### Legacy Aliases

| Alias | Maps to |
|-------|---------|
| `validate-code` | `validate` |
| `validate-rules` | `validate-kits` |

## Usage

### Via global CLI (recommended)

```bash
cpt init
cpt validate
cpt validate --artifact architecture/PRD.md
cpt spec-coverage
cpt kit migrate
cpt generate-agents --agent windsurf
cpt update
```

### Via direct script invocation

```bash
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py validate
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py validate --artifact architecture/PRD.md
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py spec-coverage --min-coverage 80
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py list-ids --pattern "-actor-"
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py where-defined --id cpt-myapp-fr-auth
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py kit migrate --dry-run
python3 {cypilot_path}/.core/skills/cypilot/scripts/cypilot.py toc architecture/DESIGN.md
```

All commands output JSON to stdout.

> **Note**: `cypilot auto-config` and `cypilot configure` are **AI prompts** (typed in the IDE chat), not CLI commands. They route through the `generate.md` workflow.

## Testing

```bash
make test
make test-coverage
```

## Documentation

- `SKILL.md` — complete skill definition with mandatory instructions, workflow routing, and command reference
- `cypilot.clispec` — CLI specification (commands, flags, output formats)
