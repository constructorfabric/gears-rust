# Review Principles

This document gives the rules for every review of a pull request: the design review, the code
review, and the automated (LLM) review. It gives the principles of how to review and the checks for
code. [DESIGN_REVIEW.md](DESIGN_REVIEW.md) tells what to check in design documents.

A **finding** is an evidenced defect with a location and a concrete consequence.
The gear's **maturity level** and **maturity card** are defined in [MATURITY.md](MATURITY.md).

## 1. Review parameters and context

Severity depends on two kinds of inputs: the parameters of the gear and the factors of each
finding. Each input answers one question.

### Gear parameters

Before the first finding, the reviewer (human or tool) determines the parameters of the gear. They are
the same for every finding in the review.

**Maturity level: what does the gear promise to its consumers?**
For a gear with code, `experimental`, `preview`, `stable` or `deprecated`, from `gear.gdl`. For a
`design` gear, use the planned level from its maturity card. If a `design` gear has no card, apply
no maturity-based severity cap.

**Implementation flow: how complete must the specifications be?**
`explore`, `iterative` or `design-first`, from the maturity card. If the card is missing, use
`design-first`. The flow sets which documents, sections, and detail the reviewer can require
(see [MATURITY.md](MATURITY.md) §5).

**Implementation status: how much of the gear is built?**
`design`, `in-progress` or `done`, from `gear.gdl`. If the field is missing, use `done`. A feature 
that is not built yet is not reported as missing code. A document or SDK comment that presents 
it as working is a finding. For a built part of an `in-progress` or `done` gear, a mismatch between
the code and a specified behavior, public contract, or material design decision is a finding when
it has a concrete consequence under sections 2–4. Differences in wording or in implementation 
details that the specifications do not constrain are not findings.

**Consumers: who calls the gear today?**
The maturity card ([MATURITY.md](MATURITY.md)) either names the consumers or omits the field when
there are none. If the card itself is missing, assume that consumers exist: do not apply the
no-consumer severity cap. For a `preview` gear with a card and no consumers, findings about the
shape of a public contract are MEDIUM at most. An `experimental` gear has this cap with or without
consumers (§5). `stable` and `deprecated` gears have no such cap: their compatibility promise
covers consumers that the card does not name. With named consumers, their use cases decide what is
`p1`.

### Finding factors

For each finding, the reviewer assesses these factors separately. They can differ between the
findings of one review.

**Blast radius: who is affected by the defect?**
`gear` (only the gear's internals), `consumers` (the gear's consumers) or `platform` (every gear or
client). The reviewer sees it from what the defect touches, the component type, and the consumers in
the maturity card. A wide blast radius raises the severity of the same defect.

**Reversibility: what does undoing it cost?**
A decision is two-way when a later pull request can fix it. A decision is one-way when undoing it
needs a migration or breaks a consumer, or when data is already lost or leaked. The reviewer
decides this from what the defect touches. One-way decisions can be HIGH or CRITICAL. Two-way
decisions are usually MEDIUM or lower, but reversibility does not cap severity: a reversible defect
can be HIGH when it causes severe or widespread harm before a fix can be delivered.

**Likelihood: how realistic is the failure scenario?**
Confirmed, realistic or theoretical, from the reviewer's evidence. A theoretical scenario usually
lowers the severity. The reviewer decides how much, from the consequence: a theoretical scenario
with a severe consequence, for example cross-tenant access, can still be a finding. HIGH and
CRITICAL usually need a confirmed or realistic scenario.

## 2. Scope

A review looks at the change in the pull request: what it adds or modifies in its gear, at the
maturity level that the gear declares. Problems outside this change can be real, but usually they
are not the author's job in this pull request.

By default, these items are outside the scope of a pull request:

- Debt that existed before the change if the change does not make it worse
- Problems in other gears, in their code or documents
- Requirements that are not in the PRD
- Gaps, out-of-scope items, and trade-offs that the PRD, DESIGN, an ADR, or the maturity card
  already declares if the maturity level allows them
- Detail that the author can decide during implementation without a change to a public contract
- Style and wording that do not change the meaning

The reviewer can mention such an item as a suggestion or a question, or propose a follow-up issue.
It is not a finding in this review. The exception is an item that the change makes worse or depends
on. Then the reviewer writes it as a finding, with its consequence. For a new requirement, the gear
owner decides whether and when it enters the scope of the gear.

## 3. Findings and evidence

A finding states:

- **Consequence**: what breaks, for whom and in which scenario, whether a later pull request can
  still fix it, and what happens if the fix waits.

A finding is only as strong as its evidence.

- **Read before judging.** Read the change and the code or documents that it touches and check
  each claim against the source.
- **Point to the place.** Give `file:line` or a document section, with a short quote. A finding
  without a location is hard to act on and easy to dispute.
- **Ask when unsure.** When unsure, write a question: say what looks wrong and what would confirm
  it. A suspicion becomes a finding only when the evidence supports it.
- **Say what was not checked** and why, for example, no build, no running environment, or no access.
  Then the author knows which risks are still open.

## 4. Severity

Severity follows from what the finding describes, not from the area it belongs to. The main factor
is the consequence. A finding about security is not CRITICAL by itself, and a typo in a log line
is not LOW by itself.

Determine the severity in this order:

1. Base severity from the consequence, by the definitions below. A check in another guideline,
   for example [DESIGN_REVIEW.md](DESIGN_REVIEW.md), may give the base severity.
2. Adjust it by the finding factors (§1): blast radius, reversibility, and likelihood.
3. Apply the caps: maturity (§5) and no consumers (§1). A cap wins over steps 1–2.
4. The items that §5 excludes from the cap stay uncapped.

The distinction between HIGH and MEDIUM depends on both the cost of undoing the change and the harm
caused while the defect remains. A later pull request being technically possible does not by itself
limit a finding to MEDIUM.

- **CRITICAL:** The harm cannot be undone, or the foundation is wrong: data or money loss,
  cross-tenant access, an authentication or authorization bypass, or an existing `stable` contract
  (present in the base branch) broken without a migration path.
- **HIGH:** A defect with severe or widespread impact, or a decision that is expensive to undo: a
  supported path that fails for many consumers or across the platform, a public contract that will
  have to break, a persisted schema that will need a destructive migration, a duplicated platform
  capability, or a decision that lets two engineers build incompatible implementations.
- **MEDIUM:** A realistic reliability, operations, or compatibility problem with bounded impact
  that a later pull request can fix without breaking anyone or leaving a severe failure in place.
- **LOW:** A local defect with limited impact.

Suggestions and questions are not findings. Label them as such.

Many CRITICAL findings in one review are a signal. Either the change is in a very bad state, or
the severity is inflated. It is a reason for the reviewer to check the findings again or to ask the
author for an explanation.

## 5. Maturity calibration

The maturity level sets how strict the review is. A lower level gives more freedom to the shape of
the gear, but never to security or data. The calibration lowers severity; it does not remove a
finding. Whether a gap is a finding is decided by the scope (§2) and the implementation flow (§1).

At every level, cross-tenant access, an authentication or authorization bypass, a secret leak, and
the loss of persisted data are findings whose severity is not capped by maturity. A change to the
persisted schema needs a forward migration as soon as any environment stores data, also in an
`experimental` gear.

An `experimental` gear exists to learn fast. It promises no compatibility, so a problem in the
shape of a public contract is MEDIUM at most, with or without consumers. The cap covers the shape
of the persisted schema too, but not a missing forward migration or the loss of persisted data.
Incomplete requirements and NFRs, missing design detail, and missing operability (metrics, limits,
rollback) are findings when the flow requires them, usually LOW or MEDIUM. A `p2` or lower feature
that has not been promised yet is not a finding. Once a pull request adds or changes a
feature, defects in the changed behavior are reviewed regardless of the feature's priority.

A `preview` gear has its first consumers, and their adoption can still change its public
contracts. An intentional contract change is not a defect merely because it is breaking when it
follows the level's compatibility promise. A defect in a public contract is usually HIGH. Context
from the maturity card, an ADR, or the pull request description can change the severity by showing
the actual consumers, likelihood, reversibility, and migration path; it does not excuse the defect.
Security and persisted data follow the full severity rules. Everything else, for example, minor
gaps in requirements, design details, or operability, is usually MEDIUM or HIGH, depending on the
consequence and the finding factors.

## 6. Code review focus

These are the areas that usually matter in code review, not an exhaustive checklist. Apply the
ones relevant to the change together with the [Rust](DNA/languages/RUST.md),
[REST API](DNA/REST/API.md), [ToolKit](../docs/toolkit_unified_system/README.md),
[security](SECURITY.md), and [dependency](DEPENDENCIES.md) guidelines. A check is not a finding by
itself: report a finding only when the evidence shows a defect and a concrete consequence under
sections 2–5.

Maturity changes the required specification detail and compatibility promise. It does not relax
correctness within the declared scope, security or tenant isolation, or protection of persisted
data.

- **Correctness.** Check all behavior affected by the change, starting with `p1` paths, against the
  PRD, DESIGN, and public contracts. Include successful results, failures, empty inputs, and
  relevant boundary cases. Use the pull request description to understand the intended scope and
  check that it describes the change accurately. A mismatch with the pull request description does
  not make the implementation incorrect by itself.
- **Specification alignment.** Follow the implementation flow in [MATURITY.md](MATURITY.md). In the
  `explore` flow, the DESIGN covers only boundaries, dependencies, contract kinds, persisted data,
  and security; code may go beyond it. In the `iterative` flow, the DESIGN may be short and 
  implementation may refine it. In the `design-first` flow, the required design decisions are 
  comprehensive and agreed before the implementation is accepted. In every flow, update the DESIGN 
  when the change would make it inaccurate or introduces a decision that future maintainers need
  to understand.
- **Tests.** A test must make a meaningful assertion about the behavior it names and fail when that
  behavior breaks. A test does not provide coverage when it never exercises the relevant code,
  checks only its own setup or the test harness, or derives the expected result from the same logic
  as the implementation.
- **Compatibility.** Determine how every public contract change affects existing consumers. Do not
  assume that an additive change is compatible: a new required field, enum variant, or trait method
  can be breaking. Apply the gear's compatibility promise and the versioning rules in
  [CONTRIBUTING.md](../CONTRIBUTING.md).
- **Migrations.** Treat migrations as compatibility for persisted data. Check a fresh database and
  the upgrade paths promised at the current maturity level. Once data exists, a migration must
  preserve it or transform it deliberately. Do not edit a migration that may already have run; add
  a new one. When old and new versions can run at the same time, verify that both work during the
  rollout.
- **Dependency failure.** Check how the code behaves when a gear or external system it calls is
  unavailable, slow, or returns an error. Consider timeouts, retries, idempotency, isolation of
  unrelated work, and whether the path should fail open or fail closed. Treat these as evidence,
  not automatic findings; judge the behavior by its contract and consequence.
- **Security.** This area is relevant when the change affects authentication, authorization,
  tenant isolation, sensitive data, or secrets. Review it against [SECURITY.md](SECURITY.md) and
  the [ToolKit auth rules](../docs/toolkit_unified_system/06_authn_authz_secure_orm.md).
- **Concurrency.** Assume that two replicas can run the same operation at the same time. Check
  check-then-act and read-modify-write code, in-memory caches, and background jobs for lost
  updates, duplicate work, and assumptions that only one process runs the code.

## 7. Comments

Start every comment with a label, so the author knows what kind of feedback it contains:

- `finding (CRITICAL|HIGH|MEDIUM|LOW):` — an evidenced defect
- `suggestion:` / `question:` — optional

## 8. Automated (LLM) review

Automated review comes in two forms. In both, the tool follows these guidelines.

Before the first finding, the tool determines the gear parameters of §1 from `gear.gdl`, the maturity
card, and the diff. When a source is missing, the tool applies the defaults of §1. The output
starts with the parameters and their sources, and marks each default.

**An automated tool without human verification.** No person checks its comments before they
appear. Each such comment is visibly marked as a tool output. It must not post false or
useless comments that distract the author and the reviewers. An approval from such a tool does not
count as an approval of the pull request.

**An automated tool with human verification.** An agent reviews the pull request, and a person
verifies its findings before they are posted. The person posts only the findings that hold up and
owns each of them and its severity. The usual review rules apply.
