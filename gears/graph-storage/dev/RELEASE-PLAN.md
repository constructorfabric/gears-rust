# Releasing the graph-storage gear: where it stands and how it lands

Written 2026-09-11, after PR #4523 merged the gear's documentation into
`constructorfabric/gears-rust` main. The question this answers: move the fork's
implementation upstream as it is, or close gaps first — and what "the official
version" has to mean.

Nothing here argues for a rewrite. The implementation was written *against*
these documents, by walking them requirement by requirement; it is 6 100 lines
over 31 files with 158 tests, run against a live PostgreSQL 19 stand carrying
531 251 nodes. The only real question is sequencing.

## 1. Where the two sides actually are

| | upstream `main` | fork `feature/graph-storage-v2` |
| --- | --- | --- |
| `gears/graph-storage` | **documentation only** — PRD, DESIGN, 5 ADRs, SPIKE, alternatives, 9 base schemas + 3 examples (21 files) | the same docs *amended*, plus 4 crates, 158 tests, `dev/` |
| distance | 14 commits ahead of us on other paths | 60 commits ahead, of which 17 are this month's type-evolution work |

Facts worth knowing before planning anything:

- **We hold every upstream documentation commit.** Nothing was changed in
  review after we branched, so there is no reconciliation to do in that
  direction.
- **Our branch amends the approved docs**: PRD `fr-type-registration` and its
  § 12 risk row, DESIGN § 2/§ 3.3/§ 3.7/§ 4, ADR-0003's annotation consequence,
  and a new ADR-0006 (326 lines). That is a *contract* change to a document
  someone signed off, and it needs its own review conversation.
- **Our branch deletes `docs/schemas/*.json`** (12 files) because the crate
  carries its own copy — `cargo package` ships nothing outside the crate
  directory (D-026). Upstream, those files are what a producer reads. A silent
  delete in a code PR is the wrong way to settle that.
- Outside the gear we touch exactly three things, all legitimate parts of a
  contribution: the workspace members and dependency aliases, the example
  server's gear registration, and `.github/workflows/ci.yml`.
- All four crates are `publish = false` at 0.1.0–0.1.2. Upstream release-plz
  publishes the whole workspace, so landing in main is also what publishes the
  gear — and retires the fork tag, the `[patch]` block and the `weftgraph`
  code-name plan in one move.

## 2. Conformance against the merged documentation

PRD § 9 is the checklist. `dev/DEVIATIONS.md` already walks it; this table is
that walk collapsed to p1, plus one row the walk missed.

### Met, and covered

`fr-type-registration` (and beyond it — ADR-0006), `fr-type-constraints`,
`fr-type-filtering`, `fr-stable-identity`, `fr-bulk-ingest`, `fr-soft-delete`,
`fr-node-read`, `fr-tabular-projection` (payload paths too, D-030),
`fr-lexical-search`, `fr-vector-search`, `fr-hybrid-search`,
`fr-graph-traversal`, `fr-neighborhood-projection`, `fr-embedding-pipeline`,
`fr-embedding-dim-guard`, `fr-snapshot-identity`, `fr-revision-signal`,
`fr-tenant-isolation`, `fr-access-control`, `fr-rest-api`, `fr-sdk-client`.

### The p1 gaps, by who can close them

| requirement | state | owner |
| --- | --- | --- |
| **`fr-source-ownership`** | **not implemented, and not recorded anywhere.** `node.source_namespace` is written `None` and `owner_principal` is written `""` on every path; nothing reads either. The docs call a source namespace "an enforced ownership boundary" with immutable owner provenance and per-update re-authorization. Today it is not a boundary at all. | ours |
| `fr-readiness` | not built at all (D-109). `GET /health/ready` answers 404; DESIGN's 14-row matrix is normative. The *inputs* exist — migrations, capability probe, embedding-space identity, dimension check — with nothing exposing them. | ours |
| `fr-scope-replace` | half. Generation fencing is real and covered; the replacement **removes nothing** (`fence_and_clear_scope` writes the fence and returns `(0, 0)`), so PRD § 9 criterion 2 — "removes stale static content and preserves analysis edges" — has nothing to preserve them from. | ours |
| `fr-reference-nodes`, `fr-edge-provenance` | implemented and **never exercised**: no conformance case and no stand run ingests a reference node or an analysis edge. | ours (cheap) |
| `fr-audit-envelope` | met for nodes; **no read surface returns an edge**, so half of it is unexercised (D-022). | ours (cheap) |
| `nfr-tenant-zero-leak` | partial: the store and the hop have adversarial cases; search, projection, node read and traversal are scoped by construction with no case of their own. | ours (cheap) |
| `nfr-code-coverage` | unknown. `cargo llvm-cov` has never been run against this gear; the threshold is 85 %. | ours (cheap to measure) |
| `nfr-ingest-throughput`, `nfr-search-latency`, `nfr-traversal-latency`, `nfr-response-bound` | unverified against § 6.1. Everything measured so far was ad hoc on a stand, not the seeded reference graph the criteria name — and we now have the loader that can seed it. | ours |
| `fr-read-consistency` | the built-in store **declines** the one-snapshot obligation (D-007): the sealed runner cannot hold a transaction across trait calls. The fake honours it, so the asymmetry is asserted rather than assumed. | platform (a caller-held snapshot API) or a doc amendment |
| `fr-index-admission` | not built (D-104): no activation lifecycle, no capacity admission. Equality is served by one static GIN; range and order read the expression. The index itself needs a DDL surface a gear may call — filed as gears-rust #4721 with the 250k-row measurement. | platform |
| `nfr-tenant-fairness` | the per-request bounds subset only (D-105): no queues, no fair dispatch, no reserved connections. | ours, but large |

Two obligations of the suite's own contract are also open: single-writer
serialization per scope identity has **no** test (two concurrent replacements
never race), and the PostgreSQL lane does not run in CI at all, because
`test-containers` has no PostgreSQL 19 + pgvector image (D-003).

### p2 and p3

Deferred by agreement before implementation, each with an entry and room left
in the API: chunking and heavy-content offload (D-100), labels (D-101), change
events (D-102), tenant offboarding (D-106), analytics topology and metric
annotation (D-107), the PG16 matrix (D-108), the observability contract
(D-110). None of them blocks a release that says so.

## 3. What the gap list means for the decision

The gaps sort into four kinds, and only the first two are ours to close:

1. **Cheap and entirely ours** — readiness, the coverage number, adversarial
   cases per endpoint, exercising reference nodes and analysis edges, an edge
   read surface, the scope-serialization race test. Days, not weeks, and every
   one of them is something a reviewer holding the merged docs will ask about.
2. **Real work and ours** — source-namespace ownership (a p1 boundary that does
   not exist), scope replacement's removal half, the timed § 6.1 scenarios.
3. **Platform-owned** — the DDL surface for declared paths (#4721), a
   caller-held snapshot handle, a PG19 + pgvector test image. We cannot close
   these; waiting for them means never landing.
4. **Agreed cuts** — everything p2/p3 above.

So "contribute only when it fully matches the documentation" is not reachable
by us: three p1-adjacent items need platform APIs. The choice is therefore
about *which* honest state we contribute, not whether we contribute an honest
one.

## 4. Decision

**Land it upstream in two PRs, and close the group-1 and group-2 gaps before
the code PR — except tenant fairness, which stays a recorded cut.**

- Not "as-is now": three of the gaps (source ownership, readiness, scope
  replacement) are things the merged documentation states as MUST and a
  reviewer will find in an afternoon. Finding them ourselves first is the
  difference between a contribution and a correction.
- Not "wait for full conformance": the platform-owned items make that
  unreachable, and the gear is already carrying a real consumer.
- The documentation amendment goes **first and separately**. It is a contract
  change (ADR-0006 narrows a normative MUST) and it wants the eyes that
  approved #4523, not a diff buried under 6 000 lines of Rust.

## 5. Plan

### Landing 1 — the documentation amendment (0.5 day)

Rebase onto current main, take only `docs/`: PRD amendment note, DESIGN § 2 /
§ 3.3 / § 3.7 / § 4, ADR-0003's amendment, ADR-0006. Resolve the
`docs/schemas` duplication deliberately: keep the published copy and add a test
asserting it is byte-identical to the crate's, rather than deleting what
producers read. PR body carries the conformance table from § 2 above, so the
reviewers see the gap list before the code arrives.

### Landing 2 — closing what is ours (≈ 11–14 days)

| item | estimate | done when |
| --- | --- | --- |
| `fr-source-ownership`: namespace bound to a producer principal, owner recorded on first create, re-authorized on every update and materialization, transfer as an audited admin flow | 3 d | a second producer writing another's namespace is refused, on both stores |
| `fr-readiness`: `GET /health/ready` over the inputs that already exist, per-capability healthy/degraded/unhealthy, the matrix's rows | 2 d | a degraded capability refuses exactly its own operations and readiness says which |
| `fr-scope-replace`: remove scope-managed static content not re-supplied, preserve analysis edges, inside the fenced transaction | 3 d | PRD § 9 criterion 2 passes on both stores; a re-sync drops a stale edge and keeps a provenance-bearing one |
| reference nodes + analysis edges exercised; an edge read surface | 1.5 d | criterion 1 and the second half of `fr-audit-envelope` are covered |
| adversarial multi-tenant case per endpoint | 1 d | every endpoint has a case that asserts its own fixture first |
| the scope single-writer race | 1 d | two concurrent replacements serialize, and the suite stops claiming what it did not test |
| coverage measured, then raised to 85 % | 1–3 d | `cargo llvm-cov -p cf-gears-graph-storage` reports ≥ 85 % |
| § 6.1 scenarios timed on a seeded reference graph (the loader already builds one) | 2 d | four scenarios with numbers against the thresholds, recorded |

**Progress.** The § 2 table above describes the state this sprint started
from and is left as it was, because that is the state the PR argues against.
What is closed so far, each with the case that closes it:

| item | state | what asserts it |
| --- | --- | --- |
| `fr-source-ownership` | done | `a_source_namespace_is_claimed_by_its_first_writer`, `writing_under_another_producers_namespace_is_forbidden`, `a_transfer_moves_the_namespace_and_records_who_moved_it`, `an_owned_nodes_source_field_claims_no_namespace` |
| `fr-readiness` | done | `readiness_reports_every_capability_and_only_some_block_service`, on the fake and against a real server |
| `fr-scope-replace` (removal half) | done | `scope_replacement_removes_what_the_batch_no_longer_names`, `scope_replacement_preserves_analysis_edges_and_their_endpoints` |
| the scope single-writer race | done — and it found the fence was not one (D-035) | `two_replacements_of_one_scope_serialize`, on a multi-threaded runtime |
| reference nodes, analysis edges, the edge read | done | `both_node_families_and_both_edge_families_round_trip`, `an_edge_read_carries_the_envelope`, `an_edge_whose_endpoint_is_hidden_is_not_readable`; the key rule itself in `domain::identity` unit tests |
| adversarial case per endpoint | next | — |
| coverage to 85 % | pending | — |
| § 6.1 timings | pending | — |

Left as recorded cuts, with entries: `nfr-tenant-fairness` (D-105),
`fr-index-admission` (D-104 + #4721), the snapshot obligation (D-007), the PG16
matrix (D-108), and every p2/p3 above.

### Landing 3 — the gear (≈ 2 days of preparation)

Rebase onto main; flip `publish` on all four crates and pick versions; keep the
workspace wiring, the example-server registration and the CI entry; add a
`test-graph-storage-pg` make target gated on the image, and file the
`test-containers` PG19 + pgvector ask so the strongest lane runs in CI rather
than only on a developer's machine. `dev/` does **not** go upstream (§ 6).

### After it merges

release-plz publishes the four crates. studio-web then drops the fork tag and
the `[patch]` block, takes `cf-gears-graph-storage` from crates.io, and the
`weftgraph` code-name publish is retired unused — the name it would have spent
stays Constructor Fabric's.

## 6. What stays out of the official repository

`gears/graph-storage/dev/` is ours, not the platform's: it carries the Studio
domain-model deck, the type-update plan with stand numbers and Studio entity
names, DEVIATIONS' 660 lines of narrative, and the PG19 test Dockerfile. It is
kept locally and on the fork branch, and excluded from every upstream landing.

What upstream *does* need from it travels as documentation instead: the
conformance table of § 2 in the PR body, and the deviations that are genuinely
about the gear's contract folded into DESIGN/ADR notes as they already are.
