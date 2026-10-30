# Gear Maturity Model

A **gear** is a component of the platform. A gear has a crate with `#[toolkit::gear]`, an SDK crate
and documents (see [docs/GEARS.md](../docs/GEARS.md)). A **consumer** is a gear, a product, a vendor
plugin, or a deployment that uses a gear.

Each gear declares a maturity level. The level shows how far the gear is proven: whether
production use confirmed its requirements, how complete its specifications are, and whether its
implementation is settled. The level does not change the rules for what consumers build against
and for stored data (§1): they are the same at every level.

## 1. Parts of a gear

A gear has three kinds of parts. Each kind has its own rule for change:

- A **public contract** is a part that a consumer builds against: an SDK trait or type, a REST or
  gRPC API, an MCP tool, an event, a GTS type, or a configuration. A part is a public contract
  as soon as a release contains it or a DESIGN declares it.
- The **persisted data** is the data that the gear stores and its database schema. No consumer
  reads another gear's tables, so the schema is not a public contract. But stored data must not be
  lost.
- The **internals** are the rest of the gear: its implementation and its internal design. They can
  change in any way that keeps the public contracts and the persisted data intact.

**Public contracts.** A consumer of a public contract can always move from the previous version to
the new one. A change to a public contract is additive and keeps backward compatibility, or it is
a breaking change that ships with a **migration path**: a new major version of the contract,
migration notes that tell a consumer what to change, and the scripts, tooling, or compatibility
wrappers that move consumers and data. The rule covers every version of a contract.

The rule binds each part of a public contract (an operation, a field, a type, a key) from the
first release that contains it. Before that, the part follows the DESIGN that declares it, and a
change to the DESIGN is checked for compatibility like a change to a released contract. A 
change to a released part is breaking even when the same pull request updates every caller 
in this repository: consumers outside the repository, for example, vendor plugins and 
external deployments, also use the contract. 

The previous version of a contract keeps working after a breaking change. This repository does not
plan its removal: an operator removes it only when it decommissions the old version of the
component after the consumers moved to the new version, and that happens outside this repository.

**Persisted data.** Once any environment stores data, a schema change ships with a forward
migration, and the previous release keeps working on the migrated schema. Dropping stored data is
data loss, not a new major version.

A mistake in a public contract or in persisted data is expensive: each fix of a contract is a new
major version served next to the previous one and a migration for every consumer. So these parts
are specified and reviewed in full at every maturity level (§6). A mistake in the internals costs
one more pull request. The maturity levels exist to spend design effort on the internals only where
the gear is proven enough to need it: the team may build and deploy a `preview` gear before its
internals are settled, then refine. Security and tenant isolation are costly to change at any level.

## 2. Statuses

Each gear declares two statuses in `gear.gdl`. The statuses are independent:

- The **maturity level** shows how far the gear is proven.
- The **implementation status** shows whether the gear exists only as documents, is partly built,
  or is complete.

A `stable` gear can still be in progress: production use confirmed what is built, and the team
adds more features. A `preview` gear can be complete: all its features work, but production use
has not confirmed its requirements yet, and its internals can still be reworked.

The maturity level sets two items:

1. Which specifications the gear has and whether they must match the code, and the least strict
   implementation flow the gear can use (§6).
2. How finding severity is calibrated for gaps in completeness (see [REVIEW.md](REVIEW.md)).

The implementation status shows which parts of the specifications describe code that exists, and
which parts describe the plan.

### Maturity level

| Level        | Meaning                                                                                                                      | Who may use it                                                                   |
|--------------|------------------------------------------------------------------------------------------------------------------------------|----------------------------------------------------------------------------------|
| `preview`    | Built for its first consumers; their feedback confirms or changes the requirements. The internals can still be reworked.     | Any consumer. Production use is allowed. The maturity card lists the known gaps. |
| `stable`     | Proven in production: the requirements are confirmed, and the internals are settled.                                         | Any consumer, without extra risk.                                                |
| `deprecated` | Replaced or retired. Names the replacement and the migration path to it, if any. Its public contracts keep working as before | Existing consumers only.                                                         |

The level is the `maturity` field in `gear.gdl` (see the [GDL reference](https://github.com/constructorfabric/gearbox/blob/main/docs/gdl.md)).
Gearbox, the tool that composes products from gears, requires the field on every gear with code. It
warns when a product includes a `deprecated` gear and informs when it includes a `preview` gear.

### Implementation status

The status is the `implementation` field in `gear.gdl`.

| Status        | Means                                                                 | Specifications and code                               |
|---------------|-----------------------------------------------------------------------|-------------------------------------------------------|
| `design`      | Documents, and possibly an SDK. No crate. A product cannot include it | Nothing that the specifications describe is built     |
| `in-progress` | Code exists, and work continues                                       | Part of what the specifications describe is built     |
| `done`        | All planned features are built                                        | Everything that the specifications describe is built  |

A `design` gear has no maturity level in `gear.gdl`: nothing is built that production use could
prove. The public contracts that its DESIGN declares already follow §1.
The maturity card names its implementation flow, one that the `preview` level allows (§6).
Without a card, it follows `design-first`.

For a consumer, the two statuses answer different questions. The implementation status shows what
works today. The maturity level shows how proven it is.

## 3. Dimensions per level

Each level sets five dimensions: requirement confidence, scope, accepted trade-offs, internals,
and specifications. Public contracts and persisted data follow §1 at every level. A `deprecated`
gear keeps the dimensions and the implementation flow of the level it had before.

### `preview`

| Dimension               | Value                                                                                                                                                      |
|-------------------------|------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Requirements confidence | Each `p1` use case has a named consumer. Feedback from the consumers confirms or changes the requirements                                                  |
| Scope                   | The smallest set that serves the first consumers. The implementation plan in the maturity card lists what comes later. Additions go through the gear owner |
| Accepted trade-offs     | Recorded in the PRD and DESIGN                                                                                                                             |
| Internals               | Can still be reworked                                                                                                                                      |
| Specifications          | Maturity card, short PRD and DESIGN that match the code                                                                                                    |

### `stable`

| Dimension               | Value                                                                                                                                                                   |
|-------------------------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Requirements confidence | Production use confirmed the requirements                                                                                                                               |
| Scope                   | Changes go through the gear owner and a design review                                                                                                                   |
| Accepted trade-offs     | Recorded in the PRD and DESIGN. A trade-off that is expensive to undo has an ADR                                                                                        |
| Internals               | Settled and proven in production                                                                                                                                        |
| Specifications          | Full PRD and DESIGN. ADRs for decisions that are expensive to undo. Other documents when consumers need them, for example an integration guide or an operations runbook |

## 4. Where each fact is declared

The gear owner writes the facts a reviewer needs about a gear once, instead of being
guessed again in every pull request. Each fact has one place:

| Fact                                     | Where                                                                  | Why there                                                                                                                                                                                                                            |
|------------------------------------------|------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Maturity level                           | `gear.gdl`: `maturity = "<level>"`                                     | Gearbox defines the field and its values and uses for level checks when building a product.                                                                                                                                          |
| Implementation status                    | `gear.gdl`: `implementation = "<status>"`                              | Gearbox defines the field and its values.                                                                                                                                                                                            |
| Consumers                                | Maturity card                                                          | Gearbox knows only the links between the gears in its catalogue, and it cannot yet list the gears that depend on a given gear. It does not know consumers outside the catalogue, for example vendor plugins or external deployments. |
| Contract versions                        | Rust attributes (`#[toolkit::contract(version = ...)]`) and REST paths | Already there                                                                                                                                                                                                                        |
| Scope, known gaps                        | Maturity card (markdown, below)                                        | Prose with issue links; GDL states facts for the resolver, not text for reviewers                                                                                                                                                    |

**Maturity card**: a `## Maturity` section at the top of the gear's `README.md` (or of its PRD when
there is no README):

```markdown
## Maturity

- **Implementation flow**: `iterative` (see Implementation flows below)
- **Consumers**: llm-gateway, product X deployment
- **In scope now**: <the use cases this level covers>
- **Known gaps**: <the list of known temporary gaps of the current level, each with an issue link
  when applicable, e.g. "list endpoints have no pagination — #1234", "no rate limit on file
  upload — #1240">
- **Implementation plan**: <the order of the planned work and the action items to reach the next level, each
  with an issue link when applicable, e.g. "1. pagination for list endpoints — #1234; 2. first
  production deployment in product X; 3. contract tests for the public SDK — #1251">
```

The card contains only the fields that tell something about this gear. An empty field is deleted,
not filled with "none". For example, a `stable` gear that the team considers complete has no implementation plan.

When the level changes, the same pull request changes `gear.gdl` and the card. A gear without a
card follows `design-first` and has no declared gaps. These conservative defaults apply until the
gear owner adds the card.


## 5. Promotion

The gear owner proposes a promotion in a pull request that updates `gear.gdl` and the card.
One maintainer approves it, as for any pull request.

A gear stays in `preview` until real consumers use it in production and their feedback confirms
the requirements. This is a recommendation, not a fixed time limit. The implementation plan 
in the card shows the way to `stable`.

## 6. Implementation flows

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
section that the document can have. The maturity level decides which documents are written (§3).
The flow decides which of their sections are filled and how much detail each has; the subsections
below give these rules. When both give a rule for the same thing, the stricter one applies. An empty section is deleted, not filled with a
placeholder.

The flows go from the less strict to the more strict: `iterative`, `design-first`.
The maturity level sets the least strict flow that the gear can use. The author can always choose
a stricter flow and writes the chosen flow in the maturity card:

| Flow           | `preview`             | `stable` |
|----------------|-----------------------|----------|
| `iterative`    | Yes, the usual choice | No       |
| `design-first` | Yes ¹                 | Required |

¹ Required at every level for system gears, and for authentication, authorization, tenant
isolation, and secret management in any gear. ToolKit libraries have no maturity level and always
follow `design-first` (see [Design first approach](#design-first-approach-design-first)).

**Requirement priority.** The PRD template gives each requirement a priority tier: `p1`, `p2`, `p3`
and so on. `p1` is the highest tier. The flows differ in how much detail each tier gets, as the
sections below show. A reviewer does not ask for more detail than the chosen flow requires.

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
