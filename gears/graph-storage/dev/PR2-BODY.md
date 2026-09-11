# Closing what the gear owed its own documentation

*(Draft PR body for landing 2. `dev/` itself is not part of the PR.)*

## The documentation changes come first, because they are the point

Everything below was found by reading the approved documents against the
code and making the difference disappear — in one direction or the other.
Where the documents were right and the code was not, the code moved. Where
the documents were silent about something the code had to decide, the
documents gained the decision.

**`docs/DESIGN.md`**

| change | why |
| --- | --- |
| `GET /api/graph-storage/v1/edges/{edge_key}` added to the REST surface, and `GraphStoreV1::get_edge` to the store port | `fr-audit-envelope` asks for the envelope on every returned node **and edge**, and no surface returned an edge as an element. The requirement could not be observed, so nothing tested it |
| § API element envelope: a paragraph saying both halves are now reachable, and that topology references stay references | the alternative reading — that `AdjacencyEntry` was supposed to grow an envelope — is the wrong one |
| `GET /source-namespaces` and `POST /source-namespaces/{namespace}/owner` added to the REST surface | `fr-source-ownership` calls a source namespace an enforced boundary with an audited transfer flow. A boundary nobody can read is a boundary nobody can operate |
| § 3.7: the `source_namespace_owner` table, specified | the prose named it and left the columns to the reader |
| § 3.7: `gts_type` gains `revision` and `updated_at` | ADR-0006's in-place update needs a revision to move |
| the readiness matrix rows, wired to real probes | they were normative and unimplemented |

**`docs/PRD.md`**

| change | why |
| --- | --- |
| `fr-audit-envelope` gains a "found while building" note recording that the edge half had no surface to be met through, and now has one | the general form is worth stating once: a requirement that cannot be observed is a requirement nothing checks |
| `fr-type-registration` amendment (ADR-0006) | unchanged from the previous landing; repeated here because this PR carries the implementation |

**`docs/ADR/0006-…-type-evolution.md`** — accepted, with the review composition
and date.

## What the code now does that it did not

| requirement | before | now |
| --- | --- | --- |
| `fr-source-ownership` | not implemented and not recorded. `source_namespace` written `None`, `owner_principal` written `""`, nothing read either | claimed by the first writer, re-authorized on every update and materialization, transfer as an audited admin flow, both endpoints live |
| `fr-readiness` | `GET /health/ready` answered 404 | the matrix's rows, each from a real probe, per-capability healthy/degraded/unhealthy |
| `fr-scope-replace` | the fence was written and **nothing was removed** | stale static content removed inside the fenced transaction; analysis edges and their endpoints survive; and the fence itself is now a fence (below) |
| `fr-audit-envelope` | met for nodes | met for edges too, through the read that makes it observable |
| `fr-reference-nodes`, `fr-edge-provenance` | implemented, never exercised | both families ingested, read back and re-run byte-identically (PRD § 9 criterion 1) |
| `nfr-tenant-zero-leak` | the store and the hop had cases | every read surface has one, under a fixture built to expose a leak rather than to be absent from one |
| `nfr-code-coverage` | never measured | 86.77 % regions / 87.02 % lines, against the 85 % threshold |

## Three defects the tests found while being written

1. **The scope fence was not a fence** (D-035). "Replacements of one scope
   serialize through a lock held to commit, and the highest accepted
   generation is compared and updated atomically under it" was implemented as
   read, decide, write. Two concurrent replacements both read the old
   generation, both passed, and the loser's *lower* generation overwrote the
   winner's. The compare is now the write (`ON CONFLICT DO UPDATE SET
   generation = GREATEST(stored, offered)`), which also takes the row lock for
   the rest of the transaction. Worth saying plainly: this is not a clever
   alternative to a lock, it is the only lock available — the secure ORM
   exposes no row-locking surface at all, which belongs on the platform's list.

2. **A type whose schema cannot compile registered successfully** (D-037) and
   then failed every write of that type. Registration validated the chain from
   the identifier and never compiled the body, so the producer got a success
   for the act that was wrong and a failure for every act that was right.

3. **The fake numbered nodes per tenant** (D-036), so the one case that
   hydrates by a foreign internal id — the surface where a missing tenant
   predicate does not show up as a key collision — was asserting nothing.

## Tests

| lane | before | now |
| --- | --- | --- |
| unit | 75 | 86 |
| conformance, in-memory | 37 | 50 |
| conformance, PostgreSQL 19 | 44 | 57 |
| domain service | — | 14 |
| REST | — | 8 |

Plus an opt-in performance lane (`tests/perf.rs`) that seeds the reference
graph and times the four § 6.1 scenarios.

## The § 6.1 numbers

On 100 000 nodes and 500 000 edges, release build, warm indexes (developer
hardware, not the CI reference profile):

| scenario | measured | budget |
| --- | --- | --- |
| hybrid narrowing, arm limit 50, query embedding excluded | p95 **41 ms** | 500 ms |
| criteria table, filter on a declared payload path | p95 **4 ms** | — |
| bounded depth-3 traversal, edge-type filtered | p95 **511 ms** | 1 s |
| depth-3 UI neighborhood, 1 000-node budget, hydrated | p95 **396 ms** | 1 s |
| 10 000 nodes + 20 000 edges, embedding excluded | **19.7 s** | 60 s |

PRD § 9 criterion 3 is met. One thing the run showed that no criterion asks
about, and that belongs in the record rather than in a footnote: **edge
ingest slows markedly as the edge table grows** — roughly a thousand rows a
second at the start of the seeding and under a hundred around the 450 000
mark. The criterion's own batch is on a fresh tenant and is nowhere near
that part of the curve, so nothing published is at risk; a producer
re-syncing into an already-large graph is. `dev/PERF-6.1.md` names the two
likely causes.
