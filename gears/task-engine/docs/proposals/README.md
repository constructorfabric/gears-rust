# Task Engine design proposals

Proposed changes to the Task Engine design ([PRD](../PRD.md), [DESIGN](../DESIGN.md), [ADRs](../ADR/), [GTS schemas](../schemas/)), one file per topic. Nothing here is the design yet: the PRD, DESIGN and ADRs stay the source of truth until a proposal is accepted and applied.

## How to review

Each topic file starts with the **problem** it solves, then the proposal, what it gives up, any open questions, and the doc edits it would need. Open questions are kept to the few that need reviewers' input; edge cases and smaller questions are worked through later, when the scenarios are written.

Every item carries one label:

| Label | Meaning | What reviewers are asked for |
|---|---|---|
| `Proposed` | The author has taken a position. | Approve, or object with a reason. |
| `Open` | The author has not decided. | Input: an adopter that needs it, a failure case, a preference between the listed options. |

Please comment on the item, not the file, so each one can be settled on its own.

## Topics

| # | Topic | The problem in one line | Proposed | Open |
|---|---|---|---|---|
| 01 | [Task mutability](01-task-mutability.md) | Changing a task's dependencies, waits or children after creation races with its claim and needs locks and transitions that exist only for that | 3 | 0 |
| 02 | [Claiming and ordering](02-claiming-and-ordering.md) | A claim across several queues reads every queue, collides with other claimers and lets one backlog take every claim | 2 | 1 |
| 03 | [Heartbeats and leases](03-heartbeats-and-leases.md) | A PDP outage of 30 s expires every lease on the platform, and a dropped task holds its slot for hours | 5 | 3 |
| 04 | [Limits and counters](04-limits-and-counters.md) | Every create and claim locks a per-tenant backlog row | 1 | 0 |
| 05 | [Domains](05-domains.md) | Moving a queue to another domain stops its producers and consumers | 2 | 0 |
| 06 | [Queue operations](06-queue-operations.md) | There is no way to stop work from starting without cancelling it | 1 | 1 |
| 07 | [Waits](07-waits.md) | Event waits are the costliest feature and depend on unfinished broker work; answering an input wait needs a broad permission | 3 | 0 |
| 08 | [Dedup and supersession](08-dedup-and-supersession.md) | One dedup mode hashes the wrong rights, and the create transaction calls the PDP | 2 | 0 |
| 09 | [API reads and responses](09-api-reads-and-responses.md) | Several reads return lists that can grow without limit | 2 | 0 |

### How the topics depend on each other

- **Task mutability → Waits.** Narrowing `update` to tag changes is what gives `resolve` its own permission.
- **Task mutability → Queue operations.** With the consumer's park gone, the queue hold is the only way to stop work without cancelling it.
- **Limits and counters → Queue operations.** A held queue still admits creates, so depth limits are what push back on producers.

## Lifecycle

1. **Draft.** The file is open for review in a pull request.
2. **Accepted.** Every `Proposed` item is approved or changed. Open questions are answered, or split into their own file.
3. **Applied.** A follow-up pull request makes the edits listed under "Doc changes", writes or amends the ADR, and removes the proposal file. From then on the ADR records why.

| Topic | Expected ADR outcome |
|---|---|
| Task mutability | New ADR. Touches ADR-0002 (state machine), ADR-0004 (who sets waits) and ADR-0016 (actions) |
| Claiming and ordering | Amends or supersedes ADR-0008 |
| Heartbeats and leases | Amends ADR-0016 and ADR-0017 |
| Limits and counters | Amends ADR-0005 and the lock order in ADR-0008 |
| Domains | Amends ADR-0010 |
| Queue operations | PRD and DESIGN only |
| Waits | PRD and DESIGN; ADR-0004 and ADR-0007 only if the model changes |
| Dedup and supersession | Amends ADR-0012 |
| API reads and responses | PRD and DESIGN only |

## Line references

`PRD.md:653` means line 653 of [`../PRD.md`](../PRD.md), and `ADR-0008:74` line 74 of the ADR with that number. Every line reference was checked against `main` at `6efb0f856`; the Task Engine docs are unchanged on `main` since then. Once a proposal is applied, later proposals' line numbers may drift; use the GitHub permalink for `6efb0f856` to read them as written.
