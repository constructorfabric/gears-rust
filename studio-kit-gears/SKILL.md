---
name: cf-gears-router
description: "Artifacts: ADR, CODEBASE, DECOMPOSITION, DESIGN, FEATURE, PR-CODE-REVIEW-TEMPLATE, PR-REVIEW, PR-STATUS-REPORT-TEMPLATE, PRD, UPSTREAM_REQS; Workflows: doc-prd, doc-upstream-reqs, doc-adr, doc-design, decompose, doc-feature, implement, change-impact-analysis, pr-review, pr-status"
---

# Constructor Studio Skill — Kit `gears`

Kit `gears` skill extensions.

## Authoring & Implementation Workflows

Each Gears artifact has a thin preset workflow that delegates to a core engine
(`cf-write-docs` for documents, `cf-coding` for code) while binding the artifact
KIND and injecting that artifact's gears rules, template, checklist, and example.

| Skill | Artifact KIND | Engine | Workflow |
|-------|---------------|--------|----------|
| `cf-gears-doc-prd` | PRD | cf-write-docs | `{workflow_doc_prd}` |
| `cf-gears-doc-upstream-reqs` | UPSTREAM_REQS | cf-write-docs | `{workflow_doc_upstream_reqs}` |
| `cf-gears-doc-adr` | ADR | cf-write-docs | `{workflow_doc_adr}` |
| `cf-gears-doc-design` | DESIGN | cf-write-docs | `{workflow_doc_design}` |
| `cf-gears-decompose` | DECOMPOSITION | cf-write-docs | `{workflow_decompose}` |
| `cf-gears-doc-feature` | FEATURE | cf-write-docs | `{workflow_doc_feature}` |
| `cf-gears-implement` | CODE | cf-coding | `{workflow_implement}` |

ALWAYS route to the matching preset workflow WHEN the user intent is authoring,
revising, or implementing the corresponding Gears artifact:

- `cf-gears-doc-prd` - write/revise a PRD (`generate PRD`, `write the PRD`)
- `cf-gears-doc-upstream-reqs` - write/revise UPSTREAM_REQS (`generate upstream requirements`, `write UPSTREAM_REQS`)
- `cf-gears-doc-adr` - write/revise an ADR (`generate ADR`, `record a decision`)
- `cf-gears-doc-design` - write/revise a DESIGN (`generate DESIGN`, `design the gear`)
- `cf-gears-decompose` - write/revise a DECOMPOSITION (`decompose`, `break into features`)
- `cf-gears-doc-feature` - write/revise a FEATURE (`generate FEATURE`, `spec the feature`)
- `cf-gears-implement` - implement a FEATURE in code (`implement`, `write the code`)

When routed to a preset workflow:
1. Read the matched workflow file and follow it.
2. The preset binds the artifact KIND + gears references, then delegates the full
   author/coder -> deterministic-gate -> semantic-review loop to its core engine.

## Analysis Workflows

| Skill | Engine | Workflow | Output |
|-------|--------|----------|--------|
| `cf-gears-change-impact-analysis` | cf-analyze | `{workflow_change_impact_analysis}` | `.change-impact/{id}/report.md` |

ALWAYS route to `cf-gears-change-impact-analysis` WHEN the user intent is to
analyze downstream impact of an upstream Gears artifact change (`impact of
changing UPSTREAM_REQS`, `what breaks if I change this DESIGN`, `trace affected FEATUREs`).
The workflow is read-only; modes are `cascade-tracking` and
`release-readiness-estimation`; thresholds live in `{change_impact_config}`.

## UPSTREAM_REQS

### UPSTREAM_REQS Commands
- `cfs validate --artifact <UPSTREAM_REQS.md>` - validate UPSTREAM_REQS structure and IDs
- `cfs list-ids --kind upreq` - list all upstream requirements
- `cfs where-defined --id <id>` - find where an upstream requirement ID is defined
- `cfs where-used --id <id>` - find where an upstream requirement ID is referenced downstream
### UPSTREAM_REQS Workflows
- **Generate UPSTREAM_REQS**: create requirements from existing modules toward a future module with guided prompts per section
- **Analyze UPSTREAM_REQS**: validate structure (deterministic) then semantic quality (checklist-based)

## ADR

### ADR Commands
- `cfs validate --artifact <ADR.md>` — validate ADR structure and IDs
- `cfs list-ids --kind adr` — list all ADRs
- `cfs where-defined --id <id>` — find where an ADR ID is defined
- `cfs where-used --id <id>` — find where an ADR ID is referenced in DESIGN
### ADR Workflows
- **Generate ADR**: create a new ADR from template with guided prompts per section
- **Analyze ADR**: validate structure (deterministic) then semantic quality (checklist-based)

## CODEBASE

### CODE Commands
- `cfs validate --artifact <code-path>` — validate code traceability and quality
- `cfs where-defined --id <id>` — find where an ID is defined in artifacts
- `cfs where-used --id <id>` — find where an ID is referenced in code via `@cpt-*` markers
### CODE Workflows
- **Generate CODE**: implement FEATURE design with optional `@cpt-*` traceability markers
- **Analyze CODE**: validate implementation coverage, traceability, tests, and quality

## DECOMPOSITION

### DECOMPOSITION Commands
- `cfs validate --artifact <DECOMPOSITION.md>` — validate DECOMPOSITION structure and IDs
- `cfs list-ids --kind feature` — list all features
- `cfs list-ids --kind status` — list status indicators
- `cfs where-defined --id <id>` — find where a feature ID is defined
- `cfs where-used --id <id>` — find where a feature ID is referenced in FEATURE artifacts
### DECOMPOSITION Workflows
- **Generate DECOMPOSITION**: create feature manifest from DESIGN with guided prompts
- **Analyze DECOMPOSITION**: validate structure (deterministic) then decomposition quality (checklist-based)

## DESIGN

### DESIGN Commands
- `cfs validate --artifact <DESIGN.md>` — validate DESIGN structure and IDs
- `cfs list-ids --kind component` — list all components
- `cfs list-ids --kind principle` — list all design principles
- `cfs where-defined --id <id>` — find where a DESIGN ID is defined
- `cfs where-used --id <id>` — find where a DESIGN ID is referenced downstream
### DESIGN Workflows
- **Generate DESIGN**: create a new DESIGN from template with guided prompts per section
- **Analyze DESIGN**: validate structure (deterministic) then semantic quality (checklist-based)

## FEATURE

### FEATURE Commands
- `cfs validate --artifact <FEATURE.md>` — validate FEATURE structure and IDs
- `cfs list-ids --kind flow` — list all flows
- `cfs list-ids --kind algo` — list all algorithms
- `cfs list-ids --kind state` — list all state machines
- `cfs list-ids --kind dod` — list all definitions of done
- `cfs where-defined --id <id>` — find where a FEATURE ID is defined
- `cfs where-used --id <id>` — find where a FEATURE ID is referenced in code
### FEATURE Workflows
- **Generate FEATURE**: create a new FEATURE from template with guided CDSL prompts
- **Analyze FEATURE**: validate structure (deterministic) then semantic quality (checklist-based)

## PR-REVIEW

## PR Review & Status (Shortcut Routing)

ALWAYS re-fetch and re-analyze from scratch WHEN a PR review or status request is detected — even if the same PR was reviewed earlier in this conversation. Previous results are stale the moment a new request arrives. NEVER skip fetch or reuse earlier analysis.

ALWAYS run `python3 {scripts}/pr.py list` WHEN user intent matches PR list patterns:
- `list PRs`, `list open PRs`, `cf list PRs`
- `show PRs`, `show open PRs`, `what PRs are open`
- Any request to enumerate or browse open pull requests

AVOID use `gh pr list` directly — ALWAYS use `pr.py list` for listing PRs.

ALWAYS route to the `cf-gears-pr-review` workflow WHEN user intent matches PR review patterns:
- `review PR {number}`, `review PR #{number}`, `review PR https://...`
- `cf review PR {number}`, `PR review {number}`
- `code review PR {number}`, `check PR {number}`

ALWAYS route to the `cf-gears-pr-status` workflow WHEN user intent matches PR status patterns:
- `PR status {number}`, `cf PR status {number}`
- `status of PR {number}`, `check PR status {number}`

### PR List (Quick Command)

When routed to list PRs:
1. Run `python3 {scripts}/pr.py list`
2. Present the output to the user (respects `.prs/config.yaml` exclude list)
3. No Protocol Guard or workflow loading required — this is a quick command

### PR Review Workflow

When routed to PR review:
1. **ALWAYS fetch fresh data first** — run `pr.py fetch` even if data exists from a prior run
2. Read `{workflow_pr_review}` and follow its steps
3. Use `python3 {scripts}/pr.py` as the script
4. When target is `ALL` or no PR number given, run `pr.py list` first to show available PRs
5. Select prompt and checklist from `{cf-studio-path}/config/pr-review.toml` → `prompts`
6. Load prompt from `prompt_file` and checklist from `checklist` in matched entry
7. Use templates from `{pr_code_review_template}` and `{pr_status_report_template}`

### PR Status Workflow

When routed to PR status:
1. **ALWAYS fetch fresh data first** — `pr.py status` auto-fetches, but never assume prior data is current
2. Read `{workflow_pr_status}` and follow its steps
3. Use `python3 {scripts}/pr.py` as the script
4. When target is `ALL` or no PR number given, run `pr.py list` first to show available PRs

## PRD

### PRD Commands
- `cfs validate --artifact <PRD.md>` — validate PRD structure and IDs
- `cfs list-ids --kind fr` — list all functional requirements
- `cfs list-ids --kind actor` — list all actors
- `cfs where-defined --id <id>` — find where a PRD ID is defined
- `cfs where-used --id <id>` — find where a PRD ID is referenced downstream
### PRD Workflows
- **Generate PRD**: create a new PRD from template with guided prompts per section
- **Analyze PRD**: validate structure (deterministic) then semantic quality (checklist-based)
