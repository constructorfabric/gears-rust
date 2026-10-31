# Internal pass contract, version 1

The orchestrator supplies `work`, `module`, `phase` (draft/closure), and the frozen
context SHA256 from manifest.json. The model is inherited. All sources are pinned Git
objects: use `python tools/scripts/toolkit-pr-review/review.py source --work-dir WORK
--side head --path PATH` (or `--side base`). `--search TEXT [--directory SUBDIR]` searches the pinned tree,
up to 100 hits; narrow searches when truncated. The checkout is not review evidence. context.stages/post_stages name state files: absent means pending; checked records bind outputs and source hashes.
Check toolchain/MSRV and CI lint enforcement from pinned head rust-toolchain.toml and Cargo.toml (plus any relevant crate manifest), rather than assuming the checkout versions. An absent Git tree entry can prove absence; empty search results and reader errors cannot.
The script never invokes a model. The agent returns one JSON object, not an array.

Both phases return all these fields:

```json
{
  "schema": "toolkit.review-pass.v1",
  "context_sha256": "FROM_MANIFEST",
  "head_sha": "FROM_CONTEXT",
  "module": "async",
  "status": "complete",
  "notes": [],
  "findings": [],
  "boundary_obligations": []
}
```

Each finding has exactly: candidate_id (`<module>:<integer>`), path, line, side,
subject (`line`/`file`), kind (`defect`/`coverage_gap`/`maintainability`), rule_id,
severity, issue, evidence, impact, fix. All four analytic text fields are nonempty,
at most 8192 characters. No comment prose yet. `"comment"` is produced only at format.
`"issue"` and `"fix"`, like evidence/impact, must be limited to the supported claim.
Candidate IDs are stable within the run. LEFT lines refer to removed base code, RIGHT
to added head code; only changed ranges in context.json are allowed. For renamed
files the finding path is the new diff path, base source uses old_path. subject=file
requires line=null, side=null and a deleted file or RUST-ARCH-001. Do not move anchors
to make validation pass. Zero findings still require a complete envelope and receipt.

Draft: walk every changed file one at a time and all assigned criteria, with no finding
quota. Save all supported candidates and concrete unresolved boundary hypotheses as
boundary_obligations: [{id:"<module>:chain:<integer>",hypothesis,next_edge}]. Keep
suspicions even if no finding can yet be emitted. A completed draft means the discovery
walk finished; obligations are work for closure, not proof that the final review is done.

Closure: a separate invocation reads the checked draft artifact and frozen policy.
Repeat boundary_obligations exactly. Retained IDs, coordinates, kind and rule stay fixed;
narrow all four analytic fields to confirmed introduced parts. Fresh, independently
discovered assigned-module claims may receive fresh IDs. Every omitted draft ID requires
dispositions: [{candidate_id,status:unsupported|preexisting|uncertain,reason,evidence:[anchor]}].
preexisting requires base and head evidence, uncertain makes the pass partial. In notes
separately identify confirmed, rejected and unresolved parts of compound assertions;
refuting one subcase never disproves another. Unverified/preexisting parts cannot survive
in the recommendation. Incomplete or unavailable substantial source chains require
status=partial, never silent downgrade or []. status=failed retains diagnostic notes.

For each retained claim include scope_assessments:
[{candidate_id,status:"introduced",reason,evidence:[baseAnchor,headAnchor]}].
Compare observable changed behavior/exposure, not the age of missing tests. Coverage
names the changed path, a concrete plausible wrong implementation, assertions that
would/might detect it, generated wiring, feature gates and meaningful consequence.
Unchanged behavior with the same exposure is preexisting; insufficient comparison is
uncertain. Runtime defects and their regression protection remain separate requirements.

Closure additionally includes boundary_audit:

```json
{
  "schema": "toolkit.boundary-audit.v2",
  "head_sha": "FROM_CONTEXT",
  "module": "async",
  "chains": [],
  "severity_checks": []
}
```

Every draft obligation must occur once in chains, preserving id and hypothesis.
Additional chain IDs may be appended. Each chain contains id,hypothesis,status
(supported/rejected/unresolved), entry,boundary,dispatch,sink (anchor or null),
explanation,branches. Supported requires all four source roles; rejected at least two
and a concrete rejecting/control path. Roles can share an anchor only if it implements
both roles. Every chain needs three branch entries, kinds main_transport, auxiliary_io,
configuration, each with status (examined/not_applicable/unresolved), explanation and
1–8 source anchors in evidence. A rejecting main path does not reject side I/O or
supported alternative configurations. An unresolved chain/branch makes the pass partial.

An anchor is {side:"head"|"base",path,line,quote}; quote is an exact short snippet
starting on that 1-based line (at most 2048 characters). {side,path,absent:true} is allowed
only when the pinned tree lacks the path. Default side is head. For sinks, inspect the
actual production caller, trait/factory dispatch and concrete send, not just symbol
references or copied context/span values. Verify branch prerequisites and normal controls.

severity_checks has one entry per retained candidate:
{candidate_id,severity,impact_class,consequence,secondary_effect,evidence:[anchor]}.
impact_class is observability_only/meaningful_behavior/substantial_failure/critical_breach.
HIGH/CRITICAL requires either last class and an independently evidenced substantial
secondary_effect. Lost correlation alone is not HIGH. MEDIUM needs meaningful realistic
harm; confidence or deterministic reproduction does not determine severity. No prior
grades, expectations or historical reference comments enter either source invocation.

The orchestrator writes original responses under raw/ and runs validate-pass. Two
technical attempts maximum per phase; no semantic reroll for a desired grade. Invalid
JSON is an error. Partial can be completed with a preserved history, but cannot collect
or publish. Completed artifacts are immutable. Python checks wiring/quotes, not semantic
truth. No claim of model equivalence or proven semantic accuracy follows from validation.
