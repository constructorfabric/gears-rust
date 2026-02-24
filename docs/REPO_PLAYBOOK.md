# Cyber Fabric — Repository Playbook

Purpose: one concise map of repository artifacts that improve developer + AI productivity, with implementation coverage and planned gaps.

## Notation

- Status: `[x]` implemented, `[ ]` planned, `N/A` out of scope
- Phase: `p1` foundation, `p2` standardize, `p3` harden, `p4` scale, `p5` optimize
- ID format: `rpb-<category>-<slug>`

---

## 1) Product & Strategy

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Project vision | [x] `p1` | [README.md](../README.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) | Keep aligned with module-level PRDs |
| Goals | [x] `p1` | [README.md](../README.md) | Add measurable annual goals in roadmap artifact |
| Non-goals | [x] `p1` | [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) | Link each non-goal to ADR when changed |
| Design principles | [x] `p1` | [README.md](../README.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) | Consolidate into single “principles” page |
| Engineering philosophy | [x] `p1` | [README.md](../README.md), [guidelines/README.md](../guidelines/README.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) | Add explicit "how to choose correctness vs speed" rubric |
| Preferred trade-offs | [x] `p1` | [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) and [docs/spec-templates/cf-sdlc/ADR/template.md](./spec-templates/cf-sdlc/ADR/template.md) | Track per-domain trade-offs in ADR index |
| Decision criteria | [x] `p2` | Via [docs/spec-templates/cf-sdlc/ADR/template.md](./spec-templates/cf-sdlc/ADR/template.md) | Add repository-wide decision criteria section |

## 2) Architecture & System

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| System overview | [x] `p1` | [README.md](../README.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) | Keep synced with modules inventory |
| Architecture overview | [x] `p1` | [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md), [docs/MODULES.md](./MODULES.md) | Add “current vs target” split blocks |
| System diagrams | [x] `p1` | [docs/img/](./img), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md), [docs/MODULES.md](./MODULES.md) | Add ownership + update cadence per diagram |
| Component responsibilities | [x] `p1` | [docs/MODULES.md](./MODULES.md), [docs/modkit_unified_system/README.md](./modkit_unified_system/README.md) | Add per-module responsibility cards |
| Module boundaries | [x] `p1` | [docs/MODULES.md](./MODULES.md), [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md), [dylint_lints/README.md](../dylint_lints/README.md) | Expand lint coverage for boundary rules |
| Technology choices | [x] `p1` | [README.md](../README.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md), [guidelines/DEPENDENCIES.md](../guidelines/DEPENDENCIES.md) | Add technology decision registry page |
| Data flow | [x] `p2` | [docs/MODULES.md](./MODULES.md) and [docs/spec-templates/cf-sdlc/DESIGN/template.md](./spec-templates/cf-sdlc/DESIGN/template.md) | Add dedicated sequence-diagram doc set |

## 3) Repository, Structure & Naming

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Repository structure | [x] `p1` | [README.md](../README.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md), [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Keep in sync with workspace changes |
| Folder conventions | [x] `p1` | [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Add root-level `REPO_STRUCTURE.md` |
| Naming conventions | [x] `p1` | [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md), [scripts/validate_module_names.py](../scripts/validate_module_names.py), [dylint_lints/](../dylint_lints) | Expand naming rules beyond modules |
| Code organization rules | [x] `p1` | [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md), [docs/modkit_unified_system/README.md](./modkit_unified_system/README.md) | Add short “golden-path skeleton” page |
| Dependency policies | [x] `p1` | [guidelines/DEPENDENCIES.md](../guidelines/DEPENDENCIES.md), [docs/security/SECURITY.md](./security/SECURITY.md) | Add explicit approval policy for new deps |
| File naming rules | [x] `p2` | [docs/spec-templates/README.md](./spec-templates/README.md) (ADR/feature naming), module file layout in [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Add global naming matrix |

## 4) Coding Standards & Static Quality

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Coding standards | [x] `p1` | [guidelines/README.md](../guidelines/README.md), [CONTRIBUTING.md](../CONTRIBUTING.md) | Add short one-page standards index |
| Style guide | [x] `p1` | clippy rules in [clippy.toml](../clippy.toml) and [Cargo.toml](../Cargo.toml), `cargo fmt` in [Makefile](../Makefile), dylint rules in [dylint_lints/README.md](../dylint_lints/README.md) | Expand language-agnostic style section |
| Lint rules | [x] `p1` | [dylint_lints/README.md](../dylint_lints/README.md), [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py) | Add lint policy matrix by layer |
| Formatting rules | [x] `p1` | [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py) | Add editor setup snippets |
| Documentation standards | [x] `p1` | [docs/spec-templates/README.md](./spec-templates/README.md), [docs/checklists/README.md](./checklists/README.md) | Add docs style/lint enforcement rules |
| Static analysis rules | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [dylint_lints/](../dylint_lints), [.github/workflows/codeql.yml](../.github/workflows/codeql.yml) | Add local static-analysis quickstart |
| Code complexity rules | [x] `p2` | Clippy `cognitive_complexity` (threshold: 20) in workspace [Cargo.toml](../Cargo.toml), [clippy.toml](../clippy.toml) | Add per-module complexity budget |
| Commenting rules | [ ] `p3` | Partial conventions in existing guidelines | Add explicit comment policy document |
| README standards | [ ] `p3` | Implicit via module QUICKSTART guidance in [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Add README template + required sections |

## 5) Git Workflow & Reviews

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Commit conventions | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Add commit-lint gate |
| Branch strategy | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Add long-lived branch policy details |
| Pull request process | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md), [docs/pr-review/README.md](./pr-review/README.md) | Add machine-readable PR checklist |
| Merge rules | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Add branch-protection policy doc |
| Code review guidelines | [x] `p2` | [docs/pr-review/README.md](./pr-review/README.md), [docs/checklists/CODING.md](./checklists/CODING.md) | Add severity SLA for findings |

## 6) Governance & Roadmap

> Governance, roadmap, and maintainer processes are controlled in a separate global repository to cover cross-repo dependencies.

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Governance model | N/A | Managed in external global governance repository | Controlled externally for cross-repo dependencies |
| Decision authority | N/A | Managed in external global governance repository | Controlled externally for cross-repo dependencies |
| Ownership rules | N/A | Managed in external global governance repository | Controlled externally for cross-repo dependencies |
| Maintainer process | N/A | Managed in external global governance repository | Controlled externally for cross-repo dependencies |
| Maintenance procedures | N/A | Managed in external global governance repository | Controlled externally for cross-repo dependencies |
| Roadmap overview | N/A | Managed in external global governance repository | Controlled externally for cross-repo dependencies |

## 7) Release, Versioning & Change Management

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Release workflow | [x] `p1` | [docs/RELEASING.md](./RELEASING.md), `.github/workflows/release-plz.yml` | Add release rollback drills |
| Versioning policy | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md), [docs/RELEASING.md](./RELEASING.md) | Add semver check gate per crate type |
| Changelog rules | [x] `p1` | [docs/RELEASING.md](./RELEASING.md), [CHANGELOG.md](../CHANGELOG.md) | Add changelog quality checklist |
| Backward compatibility policy | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) (SemVer + breaking definitions) | Add API compatibility test automation |
| Deprecation policy | [x] `p2` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Add deprecation timeline template |

## 8) Local Development & Tooling

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Local development setup | [x] `p1` | [README.md](../README.md), [CONTRIBUTING.md](../CONTRIBUTING.md), [docs/QUICKSTART_GUIDE.md](./QUICKSTART_GUIDE.md) | Add one-command bootstrap doc |
| Environment setup | [x] `p1` | [README.md](../README.md), [CONTRIBUTING.md](../CONTRIBUTING.md) | Add env matrix by scenario |
| Tooling setup | [x] `p1` | [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py) | Add tool version pin policy |
| Required software | [x] `p1` | [README.md](../README.md), [CONTRIBUTING.md](../CONTRIBUTING.md) | Add OS-specific install scripts table |
| Build process | [x] `p1` | [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py) | Add build profile selection guide |

## 9) CI & Quality Pipeline

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| CI/CD pipeline | [x] `p1` | [.github/workflows/](../.github/workflows), [scripts/ci.py](../scripts/ci.py) | Add pipeline architecture diagram |
| Required checks | [x] `p1` | [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py), [.github/workflows/](../.github/workflows) | Add required-checks policy doc |
| Failure handling | [x] `p2` | [docs/modkit_unified_system/05_errors_rfc9457.md](./modkit_unified_system/05_errors_rfc9457.md), [docs/ARCHITECTURE_MANIFEST.md](./ARCHITECTURE_MANIFEST.md) | Add incident response playbook |
| Retry policies | [x] `p2` | ModKit HTTP ADR in [docs/modkit_unified_system/README.md](./modkit_unified_system/README.md) | Add shared retry policy matrix |

## 10) Testing & Quality Gates

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Testing strategy | [x] `p1` | [README.md](../README.md), [CONTRIBUTING.md](../CONTRIBUTING.md), [docs/security/SECURITY.md](./security/SECURITY.md) | Add single consolidated testing policy |
| Unit testing rules | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md), [scripts/ci.py](../scripts/ci.py) | Add unit-test layout examples |
| Integration testing rules | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md), `ci.py all` flow | Add integration test standards page |
| End-to-end testing rules | [x] `p1` | [README.md](../README.md), [scripts/ci.py](../scripts/ci.py), `.github/workflows/e2e.yml` | Add e2e flakiness policy |
| Coverage expectations | [x] `p1` | [README.md](../README.md), [CONTRIBUTING.md](../CONTRIBUTING.md), [Makefile](../Makefile) | Enforce threshold gates in CI |
| Test structure | [x] `p2` | [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md), [dylint_lints/AGENTS.md](../dylint_lints/AGENTS.md) | Add repository-wide test taxonomy |
| Test data management | [ ] `p3` | Not centralized | Add test-fixture lifecycle guide |

## 11) Debugging, Logging & Observability

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Debugging guidelines | [x] `p2` | [CONTRIBUTING.md](../CONTRIBUTING.md) env hints, [docs/TRACING_SETUP.md](./TRACING_SETUP.md) | Add debugging playbook by failure type |
| Logging standards | [x] `p2` | [README.md](../README.md) config examples, [docs/security/SECURITY.md](./security/SECURITY.md) | Add log field schema standard |
| Monitoring practices | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), workflow checks | Add SLO/SLA dashboard specs |
| Observability setup | [x] `p2` | [docs/TRACING_SETUP.md](./TRACING_SETUP.md), ModKit docs | Add production observability runbook |
| Metrics definitions | [ ] `p3` | Not centralized | Add canonical metrics dictionary |

## 12) Security, Access & Data Protection

> Dedicated security coverage (phase-ordered) is tracked here. See [docs/security/SECURITY.md](./security/SECURITY.md) for full implementation detail.

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Rust language safety baseline | [x] `p1` | [docs/security/SECURITY.md](./security/SECURITY.md), workspace Rust/clippy settings | Keep baseline aligned with toolchain policy |
| Authentication & authorization architecture | [x] `p1` | [docs/security/SECURITY.md](./security/SECURITY.md), [docs/arch/authorization/](./arch/authorization/) | Add cross-module authz test matrix |
| Security practices | [x] `p1` | [SECURITY.md](../SECURITY.md), [guidelines/SECURITY.md](../guidelines/SECURITY.md), [docs/security/SECURITY.md](./security/SECURITY.md) | Expand secure coding examples |
| Dependency security rules | [x] `p1` | [docs/security/SECURITY.md](./security/SECURITY.md), [Makefile](../Makefile), `cargo deny` | Add allow/deny decision log |
| Vulnerability response | [x] `p1` | [SECURITY.md](../SECURITY.md) | Add incident severity matrix |
| Secure ORM tenant scoping | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [docs/modkit_unified_system/06_secure_orm_db_access.md](./modkit_unified_system/06_secure_orm_db_access.md) | Add security-context propagation verification checks |
| Static security linting (Clippy + Dylint) | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [dylint_lints/README.md](../dylint_lints/README.md), [clippy.toml](../clippy.toml) | Expand security-focused lint set |
| Secrets handling | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [docs/pr-review/README.md](./pr-review/README.md) token guidance | Add repository-wide secrets policy doc |
| Data protection rules | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), secure ORM docs | Add data classification policy |
| Access policies | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), auth architecture docs | Add policy authoring guide |
| Security scanners in CI | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [.github/workflows/](../.github/workflows) | Add scanner findings triage runbook |
| Continuous fuzzing | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [fuzz/](../fuzz) | Expand fuzz target coverage and schedules |
| Security in PRD/DESIGN SDLC templates | [x] `p2` | [docs/security/SECURITY.md](./security/SECURITY.md), [docs/spec-templates/cf-sdlc/PRD/template.md](./spec-templates/cf-sdlc/PRD/template.md), [docs/spec-templates/cf-sdlc/DESIGN/template.md](./spec-templates/cf-sdlc/DESIGN/template.md) | Add explicit security checklists in templates |


## 13) Performance & Benchmarking

> Basic performance requirements are part of the NFR (non-functional requirements) in every PRD and DESIGN.

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Basic performance requirements | [x] `p1` | [docs/spec-templates/cf-sdlc/PRD/template.md](./spec-templates/cf-sdlc/PRD/template.md), [docs/spec-templates/cf-sdlc/DESIGN/template.md](./spec-templates/cf-sdlc/DESIGN/template.md) | Keep NFR performance criteria mandatory in every feature spec |
| Optimization guidelines | [x] `p2` | Rust + clippy guidance in [README.md](../README.md), [docs/security/SECURITY.md](./security/SECURITY.md) | Add hotspot optimization playbook |
| Caching strategies | [ ] `p3` | Scattered examples only | Add standard caching guidance |
| Performance standards | [ ] `p3` | Partially in architecture manifest/perf checks | Add explicit performance SLO policy |
| Performance budgets | [ ] `p4` | Not formalized | Add endpoint/module budgets |
| Benchmarking rules | [ ] `p4` | Not centralized | Add benchmark harness + reporting standard |
| Token / compute budgets | [ ] `p4` | Not formalized | Add GenAI token/compute governance |

## 14) Agents, Prompts & AI Automation

> Agents, prompts, and AI workflows are managed by **Cypilot** — see [.cypilot/](../.cypilot) (skills, scripts, templates, workflows) and [.cypilot-adapter/](../.cypilot-adapter) (project-specific configuration, `artifacts.json`, `AGENTS.md`).

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Scripts usage | [x] `p1` | [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py) | Add script catalog document |
| Automation rules | [x] `p2`| [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py), workflow files | Add automation safety policy |
| Task automation guidelines | [x] `p2` | [scripts/ci.py](../scripts/ci.py), [Makefile](../Makefile) | Add “when to automate/not automate” guide |
| Bot behavior rules | [x] `p2` | [docs/pr-review/README.md](./pr-review/README.md), workflow configs | Add standardized bot comment protocol |
| Agents overview | [x] `p2` | [.cypilot/](../.cypilot), [.cypilot-adapter/](../.cypilot-adapter), [docs/pr-review/README.md](./pr-review/README.md) | Add central "AI operations" document |
| Prompt guidelines | [x] `p2` | [.cypilot/](../.cypilot), [docs/checklists/README.md](./checklists/README.md) | Add universal prompt design guide |
| Prompt templates | [x] `p2` | [.cypilot/](../.cypilot), `docs/pr-review/` templates | Add non-PR prompt template library |
| Agent responsibilities | [x] `p2` | [.cypilot-adapter/AGENTS.md](../.cypilot-adapter/AGENTS.md), [docs/checklists/README.md](./checklists/README.md) | Add explicit role split per bot/agent |
| Agent boundaries | [ ] `p3` | Implicit in review workflows | Add hard boundaries + escalation policy |
| Agent input/output contracts | [ ] `p3` | Templates exist for PR/status outputs | Add formal contract schema |
| Agent lifecycle | [ ] `p3` | Not formalized | Add lifecycle/run-states doc |
| Agent orchestration rules | [ ] `p3` | Partial through Cypilot workflows in [.cypilot/](../.cypilot) | Add orchestration and precedence rules |
| Prompt patterns | [ ] `p3` | Not centralized | Add prompt pattern catalog |
| Prompt anti-patterns | [ ] `p3` | Not centralized | Add anti-pattern checklist |
| Prompt safety rules | [ ] `p3` | Partially implied in security/review process | Add explicit prompt safety controls |
| Prompt evaluation rules | [ ] `p3` | Not formalized | Add eval rubric and benchmark flow |

## 15) Templates, Examples & Checklists

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Templates (overall) | [x] `p1` | [docs/spec-templates/README.md](./spec-templates/README.md), [docs/checklists/README.md](./checklists/README.md), [docs/pr-review/README.md](./pr-review/README.md) | Expand reusable template index |
| PR checklist | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md), [docs/checklists/CODING.md](./checklists/CODING.md) | Add enforceable checklist bot |
| Documentation templates | [x] `p1` | [docs/spec-templates/README.md](./spec-templates/README.md) and template files | Add docs template quick-selector |
| Examples | [x] `p1` | [examples/](../examples), [docs/QUICKSTART_GUIDE.md](./QUICKSTART_GUIDE.md) | Expand reference examples per module type |
| Code templates | [x] `p2` | [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) module skeletons/patterns | Add dedicated starter templates folder |
| PR templates | [x] `p2` | [CONTRIBUTING.md](../CONTRIBUTING.md) PR description template, [docs/pr-review/code-review-template.md](./pr-review/code-review-template.md) | Add `.github/PULL_REQUEST_TEMPLATE.md` |
| Reference implementations | [x] `p2` | [examples/modkit](../examples/modkit), [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Curate “golden reference modules” list |
| Good examples | [x] `p2` | Lint/module examples in [dylint_lints/README.md](../dylint_lints/README.md), [examples/](../examples) | Add explicit tagged good examples index |
| Bad examples | [x] `p2` | Dylint bad patterns in [dylint_lints/README.md](../dylint_lints/README.md) + UI tests | Add cross-domain anti-pattern examples |
| Release checklist | [ ] `p3` | Partial in [docs/RELEASING.md](./RELEASING.md) | Add explicit release checklist doc |
| Debug checklist | [ ] `p3` | Not formalized | Add debug triage checklist |
| Issue templates | [ ] `p3` | Not found in `.github` | Add GitHub issue templates |

## 16) API, Data, Config & Error Contracts

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| API guidelines | [x] `p1` | [guidelines/README.md](../guidelines/README.md), [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md), ModKit docs | Add API design quick-reference |
| API versioning | [x] `p1` | Dylint DE0801 in [dylint_lints/README.md](../dylint_lints/README.md), [CONTRIBUTING.md](../CONTRIBUTING.md) | Add auto-check for docs/version sync |
| Contract rules | [x] `p1` | Dylint DE01xx/DE02xx/DE03xx in [dylint_lints/README.md](../dylint_lints/README.md) | Expand contract lint set |
| Error handling standards | [x] `p1` | [docs/modkit_unified_system/05_errors_rfc9457.md](./modkit_unified_system/05_errors_rfc9457.md), [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Add repository-wide error taxonomy |
| Configuration management | [x] `p1` | [README.md](../README.md) config section, [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Add config schema validation policy |
| Data model conventions | [x] `p2` | [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md), [docs/modkit_unified_system/02_module_layout_and_sdk_pattern.md](./modkit_unified_system/02_module_layout_and_sdk_pattern.md) | Add canonical model naming matrix |
| Schema rules | [x] `p2` | GTS + OData + OpenAPI references in ModKit docs and dylint DE09xx | Add schema compatibility checklist |
| Migration rules | [x] `p2` | Secure ORM and module infra patterns in ModKit docs + [guidelines/NEW_MODULE.md](../guidelines/NEW_MODULE.md) | Add explicit DB migration policy doc |
| Environment configs | [x] `p2` | [README.md](../README.md) env overrides, [docs/TRACING_SETUP.md](./TRACING_SETUP.md) | Add per-environment config matrix |
| Feature flags | [ ] `p3` | Mentioned as target in architecture docs | Add standard feature-flag framework |

## 17) Deployment & Operations

> Cyber Fabric is a collection of libraries and frameworks, not a standalone deployable component. Deployment, rollback, cost management, and resource limits are the responsibility of downstream applications that consume these libraries.

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Infrastructure overview | N/A | Out of scope for this library repository | Downstream responsibility |
| Deployment process | N/A | Out of scope for this library repository | Downstream responsibility |
| Migration guides | N/A | Out of scope for this library repository | Downstream responsibility |
| Rollback procedures | N/A | Out of scope for this library repository | Downstream responsibility |
| Deployment checklist | N/A | Out of scope for this library repository | Downstream responsibility |
| Cost management | N/A | Out of scope for this library repository | Downstream responsibility |
| Resource limits | N/A | Out of scope for this library repository | Downstream responsibility |

## 18) Documentation IA, Onboarding, Knowledge Base

| Item | Status / Phase / ID | Implemented (where) | Planned |
|---|---|---|---|
| Documentation structure | [x] `p1` | [docs/](./), [docs/spec-templates/README.md](./spec-templates/README.md), [guidelines/README.md](../guidelines/README.md) | Add docs navigation index page |
| Onboarding guide | [x] `p1` | [README.md](../README.md), [docs/QUICKSTART_GUIDE.md](./QUICKSTART_GUIDE.md), [CONTRIBUTING.md](../CONTRIBUTING.md) | Add role-based onboarding tracks |
| First contribution guide | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Add “first good issue” process |
| Contribution guidelines | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Keep aligned with CI/review changes |
| Contributor expectations | [x] `p1` | [CONTRIBUTING.md](../CONTRIBUTING.md) | Add expected turnaround/SLA guidance |
| Decision records (ADR) | [x] `p1` | [docs/spec-templates/cf-sdlc/ADR/template.md](./spec-templates/cf-sdlc/ADR/template.md), [docs/adrs/](./adrs) | Add ADR index by domain |
| Design documents | [x] `p1` | [docs/spec-templates/cf-sdlc/DESIGN/template.md](./spec-templates/cf-sdlc/DESIGN/template.md), module docs | Add quality gates for design docs |
| Common workflows | [x] `p2` | [Makefile](../Makefile), [scripts/ci.py](../scripts/ci.py), [docs/pr-review/README.md](./pr-review/README.md) | Add workflow cookbook |
| Anti-patterns | [x] `p2` | [docs/checklists/](./checklists), [dylint_lints/README.md](../dylint_lints/README.md) | Add unified anti-pattern catalog |
| Common mistakes | [x] `p2` | [dylint_lints/AGENTS.md](../dylint_lints/AGENTS.md) pitfalls, checklists | Add “top mistakes” short guide |
| Support / escalation paths | [x] `p2` | [SECURITY.md](../SECURITY.md), [CONTRIBUTING.md](../CONTRIBUTING.md) | Add general (non-security) escalation flow |
| Proposal process | [x] `p2` | Spec-driven flow in [docs/spec-templates/README.md](./spec-templates/README.md) | Add formal RFC/proposal workflow |
| Glossary | [ ] `p3` | Not centralized | Add glossary document |
| Terminology | [ ] `p3` | Partial in architecture/spec docs | Add terminology canon |
| Acronyms | [ ] `p3` | Scattered only | Add acronyms appendix |
| Known limitations | [ ] `p3` | Partially implied in architecture notes | Add limitations register |
| Known issues | [ ] `p3` | Not in docs (tracked externally) | Add known-issues doc or link policy |
| Maintainer responsibilities | [ ] `p3` | Partially implied in existing docs | Add explicit maintainer responsibilities |
| FAQ | [ ] `p3` | Not centralized | Add FAQ page |

---

## Coverage Summary

- Implemented (`[x]`): strong coverage for architecture, standards, CI, testing, release, security, examples, Cypilot-managed AI automation.
- Planned (`[ ]`): most gaps are performance benchmarking, AI prompt governance, glossary/FAQ.
- Out of scope (`N/A`): deployment, rollback, cost/resource limits — downstream responsibility.
- Highest-value next phase (`p2`/`p3`) additions:
  1. Performance budgets + benchmark policy
  2. AI prompt/agent boundary contracts
  3. Central glossary/FAQ/known-issues pages
