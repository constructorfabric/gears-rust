# Gear Maturity Model

A **gear** is a component of the platform. A gear has a crate with `#[toolkit::gear]`, an SDK crate
and documents (see [docs/GEARS.md](../docs/GEARS.md)). A **consumer** is a gear, a product, a vendor
plugin, or a deployment that calls the public contract of a gear. A **public contract** is a part of
a gear that a consumer builds against, or that stays after a release. Public contracts are SDK
traits and types, REST and gRPC APIs, events, GTS types, configuration keys and the persisted
database schema.

Each gear declares two statuses in `gear.gdl`. The statuses are independent:

- The **maturity level** shows what the gear promises to its consumers.
- The **implementation status** shows whether the gear exists only as documents, is partly built,
  or is complete.

A `stable` gear can still be in progress: consumers can rely on it, and the team adds more
features. An `experimental` gear can be complete: all its features work, but its contracts can
still change.

The maturity level sets three items:

1. Which specifications the gear has and whether they must match the code, and the least strict
   implementation flow the gear can use (§5).
2. How review expectations and finding severity are calibrated (see [REVIEW.md](REVIEW.md)).
3. The compatibility promise: the parts of the gear that a consumer can rely on.

The implementation status shows which parts of the specifications describe code that exists, and
which parts describe the plan.

The goal is to spend design effort where a mistake is expensive. An `experimental` or `preview`
gear may change its public contracts within the compatibility rules of its level, so their shape
can evolve from implementation and consumer feedback. The team may build and deploy before every
design detail is settled, then refine the specifications as it learns. This does not make every
change inexpensive: security, tenant isolation, persisted data, and behavior already used by
consumers can be costly to change at any level. Consumers of an `experimental` or `preview` gear
accept the contract changes allowed by that level. Consumers of a `stable` gear rely on backward
compatibility.

## 1. Statuses

### Maturity level

| Level          | Meaning                                                                                                                   | Who may use it                                                                                                |
|----------------|---------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------|
| `experimental` | Built to learn. Use cases are known; detailed requirements are not.                                                       | Named consumers that accept the risk. Not in production unless the product owner accepts the risk in writing. |
| `preview`      | Requirements for the main use cases are confirmed by at least one real consumer. Contracts may still change, with notice. | Any consumer, at its own risk. Production use is allowed. The consumer plans for migrations.                  |
| `stable`       | Proven in production. Contracts change only by the versioning rules.                                                      | Any consumer, without extra risk.                                                                             |
| `deprecated`   | Replaced or retired. Names the replacement and the removal release.                                                       | Existing consumers only.                                                                                      |

The level is the `maturity` field in `gear.gdl` (see the [GDL reference](https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md)).
Gearbox, the tool that composes products from gears, requires the field on every gear with code. It
warns when a product includes an `experimental` or `deprecated` gear and informs when it includes a
`preview` gear.

### Implementation status

The status is the `implementation` field in `gear.gdl`.

| Status        | Means                                                                 | Specifications and code                               |
|---------------|-----------------------------------------------------------------------|-------------------------------------------------------|
| `design`      | Documents, and possibly an SDK. No crate. A product cannot include it | Nothing that the specifications describe is built     |
| `in-progress` | Code exists, and work continues                                       | Part of what the specifications describe is built     |
| `done`        | All planned features are built                                        | Everything that the specifications describe is built  |

A `design` gear has no maturity level in `gear.gdl`: a gear with no code promises nothing yet.
The maturity card names its planned level and implementation flow. Without a card, it follows
`design-first`.

For a consumer, the two statuses answer different questions. The implementation status shows what
works today. The maturity level shows how stable it is.

## 2. Dimensions per level

Each level sets six dimensions: requirement confidence, scope, accepted trade-offs,
compatibility promise, stability, and specifications.

### `experimental`

| Dimension               | Value                                                                                                                |
|-------------------------|----------------------------------------------------------------------------------------------------------------------|
| Requirements confidence | Low. Use cases and requirements are hypotheses. Feedback from the first consumer confirms or changes them            |
| Scope                   | The smallest set that serves the first consumer. The implementation plan in the maturity card lists what comes later |
| Accepted trade-offs     | Recorded in the PRD, or in the DESIGN when there is one.                                                             |
| Compatibility promise   | None. Any release can break any contract                                  |
| Stability               | Expect contract changes in most releases                                                                             |
| Specifications          | Maturity card, a short PRD and a short DESIGN                                                                        |

### `preview`

| Dimension               | Value                                                                                                                                                                              |
|-------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Requirements confidence | A real consumer confirmed the requirements of the main use cases. Other requirements can be hypotheses                                                                             |
| Scope                   | The scope of the main use cases is fixed. Additions go through the gear owner                                                                                                      |
| Accepted trade-offs     | Recorded in the PRD and DESIGN.                                                                                                                                                    |
| Compatibility promise   | No backward compatibility. Each breaking change has migration notes and a notice to named consumers before merge. Persisted data always gets a forward migration |
| Stability               | Contract changes are rare and announced                                                                                                                                            |
| Specifications          | Short PRD and DESIGN that match the code                                                                                                                                           |

### `stable`

| Dimension               | Value                                                                                                                                                                   |
|-------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Requirements confidence | Production use confirmed the requirements                                                                                                                               |
| Scope                   | Changes go through the gear owner and a design review                                                                                                                   |
| Accepted trade-offs     | Recorded in the PRD and DESIGN. A trade-off that is expensive to undo has an ADR                                                                                        |
| Compatibility promise   | Full rules of [CONTRIBUTING.md](../CONTRIBUTING.md): SemVer, `/vN` REST versions, deprecation for at least one MINOR release                                            |
| Stability               | Contract changes are additive, except in a new major version                                                                                                            |
| Specifications          | Full PRD and DESIGN. ADRs for decisions that are expensive to undo. Other documents when consumers need them, for example an integration guide or an operations runbook |

**The persisted schema is always the strictest surface.** Once any environment stores data, a
schema change ships with a forward migration, at every level. Dropping data is not a "breaking
change allowed in experimental"; it is data loss.

## 3. Where each fact is declared

The gear owner writes the facts a reviewer needs about a gear once, instead of being
guessed again in every pull request. Each fact has one place:

| Fact                                     | Where                                                                  | Why there                                                                                                                                                                                                                            |
|------------------------------------------|------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Maturity level                           | `gear.gdl`: `maturity = "<level>"`                                     | Gearbox defines the field and its values and uses for level checks when building a product.                                                                                                                                          |
| Implementation status                    | `gear.gdl`: `implementation = "<status>"`                              | Gearbox defines the field and its values.                                                                                                                                                                                            |
| Consumers                                | Maturity card                                                          | Gearbox knows only the links between the gears in its catalogue, and it cannot yet list the gears that depend on a given gear. It does not know consumers outside the catalogue, for example vendor plugins or external deployments. |
| Contract versions                        | Rust attributes (`#[toolkit::contract(version = ...)]`) and REST paths | Already there                                                                                                                                                                                                                        |
| Scope, known gaps, compatibility details | Maturity card (markdown, below)                                        | Prose with issue links; GDL states facts for the resolver, not text for reviewers                                                                                                                                                    |

**Maturity card**: a `## Maturity` section at the top of the gear's `README.md` (or of its PRD when
there is no README):

```markdown
## Maturity

- **Planned level**: <only for a gear with `implementation = "design"`: the level the gear gets
  with its first code>
- **Implementation flow**: `iterative` (see Implementation flows below)
- **Consumers**: llm-gateway, product X deployment
- **In scope now**: <the use cases this level covers>
- **Known gaps**: <the list of known temporary gaps of the current level, each with an issue link
  when applicable, e.g. "list endpoints have no pagination — #1234", "no rate limit on file
  upload — #1240">
- **Compatibility details**: <the maturity level sets whether the gear keeps backward
  compatibility. Write here only the details, e.g. "the job submit and status endpoints
  (`POST /v1/jobs`, `GET /v1/jobs/{id}`) are frozen and keep backward compatibility; other
  endpoints can still change">
- **Implementation plan**: <the order of the planned work and the action items to reach the next level, each
  with an issue link when applicable, e.g. "1. pagination for list endpoints — #1234; 2. first
  production deployment in product X; 3. contract tests for the public SDK — #1251">
```

The card contains only the fields that tell something about this gear. An empty field is deleted,
not filled with "none". For example, only a gear with `implementation = "design"` has a planned
level, and a `stable` gear that the team considers complete has no implementation plan.

When the level changes, the same pull request changes `gear.gdl` and the card. A gear without a
card follows `design-first` and has no declared gaps. A gear with code but
without a card is also reviewed at the level in `gear.gdl`, as if it has consumers. These
conservative defaults apply until the gear owner adds the card.


## 4. Promotion

The gear owner proposes a promotion in a pull request that updates `gear.gdl` and the card.
One maintainer approves it, as for any pull request.

A gear stays in `preview` until real consumers use it and their feedback confirms the contracts.
This is a recommendation, not a fixed time limit. But a gear that stays in `preview` for a long time
never gives its consumers a compatibility promise. The implementation plan in the card shows the
way to `stable`.

## 5. Implementation flows

An **implementation flow** sets how detailed each section of the specifications is and whether it
is agreed before the implementation is accepted. It does not prescribe whether documents and code are changed in the same pull request or
in separate ones. A flow is the path by which a gear gets to `stable`. The author chooses it while
the gear is built. A `stable` gear follows `design-first`: a change to a requirement, a public
contract or persisted data is specified and reviewed before its implementation is accepted. For
other changes, such as fixes, refactoring, or performance work, the author decides whether the
specifications need an update. They are updated when the change requires alignment or adds a
decision that a future reader needs.

Each kind of specification document has its own template: PRD, DESIGN, ADR, and others (see
[studio-kit-gears/artifacts](../studio-kit-gears/artifacts/README.md)). A template lists every
section that the document can have. The maturity level decides which documents are written (§2).
The flow decides which of their sections are filled and how much detail each has; the subsections
below give these rules. When both give a rule for the same thing, the stricter one applies. An empty section is deleted, not filled with a
placeholder.

The flows go from the least strict to the most strict: `explore`, `iterative`, `design-first`.
The maturity level sets the least strict flow that the gear can use. The author can always choose
a stricter flow and writes the chosen flow in the maturity card:

| Flow           | `experimental`                                                  | `preview`             | `stable` |
|----------------|-----------------------------------------------------------------|-----------------------|----------|
| `explore`      | Yes, the usual choice                                           | No                    | No       |
| `iterative`    | Yes                                                             | Yes, the usual choice | No       |
| `design-first` | Yes, but rarely useful: the requirements are still hypotheses ¹ | Yes ¹                 | Required |

¹ Required at every level for system gears, and for authentication, authorization, tenant
isolation, and secret management in any gear (see [Design first approach](#design-first-approach-design-first)).

**Requirement priority.** The PRD template gives each requirement a priority tier: `p1`, `p2`, `p3`
and so on. `p1` is the highest tier. The flows differ in how much detail each tier gets, as the
sections below show. A reviewer does not ask for more detail than the chosen flow requires.

### Explore flow (`explore`)

- **PRD**: Overview, Actors, Use Cases, Scope (in and out), Open Questions. Requirements are not
  enumerated one by one.
- **DESIGN**: short, describes only meaningful constraints, boundaries, principles:
  - the component boundaries: what the gear owns and what it leaves to other gears;
  - the dependencies on other gears;
  - the kinds of public contracts (for example, REST API, SDK, events) and the main principles
    behind them, without the full list of operations or fields;
  - the persisted data;
  - how the gear does authentication, authorization, and tenant isolation.

  Sequences, schemas, and state machines are added only when the experiment is about them.
- **ADR**: only for an exception to a platform rule.
- An open question is closed by a spike (time-boxed throwaway code that answers one question) or
  by the first consumer, not by more review rounds.

### Iterative flow (`iterative`)

The author starts with short specifications and makes them more precise from decisions made during
implementation and from feedback from the first consumers. The cycle repeats until the gear is
ready for `stable`.

- **PRD**: short and high-level.
  - `p1`: clear use cases and requirements, each with a named consumer.
  - `p2` and lower: a statement of what the gear needs to support or plans to support. Use cases
    and detailed requirements are optional.
- **DESIGN**: short; detailed only where a mistake is expensive: public contracts, persisted data,
  state ownership, tenant isolation, authorization, and failure of acknowledged work.
- **ADR**: for a decision that is expensive to undo: the shape of a public contract, the model of
  persisted data, or a technology or dependency that is hard to replace. Also for an exception to
  a platform rule. The author writes the ADR when the decision is made. Internal decisions that
  the code can change later need no ADR.
- Implementation may refine the DESIGN when it shows a better way. Keep the DESIGN aligned with
  the decisions this flow requires it to cover; implementation details that can change later do
  not need to be specified in advance.

### Design first approach (`design-first`)

The specifications are comprehensive: the requirements of all priority tiers and the design are
agreed before the implementation is accepted. Unlike the `iterative` flow, decisions that the
specifications are required to cover are not deferred to implementation.

This flow is required where a mistake is very expensive because many consumers depend on the
result: ToolKit libraries, system gears that other gears call, and authentication, authorization,
tenant isolation, or secret management in any gear. These parts follow `design-first` at every
maturity level. The rule also applies to ToolKit libraries, which have no `gear.gdl` and no
maturity level. A `stable` gear always follows this flow.

- Full template set, as used today.
- A change to a requirement, a public contract, or persisted data is documented and reviewed as a
  design decision before the implementation is accepted. For other changes, the author decides
  whether the specifications need an update.
