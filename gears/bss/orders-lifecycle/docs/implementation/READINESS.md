# S1-07 — Execution queue and deployment-readiness ledger

Status: **local execution handoff and readiness inventory implemented**. All 44 declared upstream requirements have an owner/profile, existing-versus-missing capability assessment, affected paths, implementation packages, required conformance scenario and explicit deployment evidence fields. All 70 numbered packages are dispatched through 41 acyclic workstreams, including the two early/later package splits. This finishes the local S1 planning/fixture deliverables; provider agreements, runtime implementations and production acceptance remain open. Business operations remain disabled. No deployment, commit or PR was performed.

Inspect the [upstream matrix](readiness/UPSTREAM.md) and [execution queue](readiness/QUEUE.md). Their reviewed sources are [ledger.json](readiness/ledger.json), [queue.json](readiness/queue.json) and the [Cargo inventory](readiness/cargo-snapshot.json). The inventory was collected using `cargo +1.98.1 metadata --no-deps --format-version 1`; it contains actual package versions, paths, targets and required features, not proposed Cargo names.

## What the inventory establishes

| Finding | Consequence |
|---|---|
| Pricing reads, receipt acceptance, holds and public native models exist | Integrate their supported SDKs and deploy the required seller service grants; do not recreate these APIs |
| Complete nonbinding assessment and seller-policy discovery are missing | Pricing S3-03/05 must deliver those APIs/profiles; existing acceptance is not a Preview read |
| Subscriptions, Workflow, Rating, Contracts and Payments commercial seams are not delivered runtimes in this workspace | Their owner packages remain real implementation work; local fixtures cannot satisfy them |
| Account Management tenant/metadata and Approvals inbox APIs exist | Payer commercial schema, delegated buying proof and order-approval policy still require explicit owner implementations |
| Ledger records postings/settlement | It does not supply Payments authorization/status or Workflow pending recovery |
| Event Broker runtime, explicit envelope tenancy and initial-cursor transient retry already exist | Wire and verify them; do not file another fix for the superseded cursor-retry defect |
| ToolKit dead-letter replay claims records | Authenticated event republication, broker acknowledgement and operator recovery remain separate work |
| ToolKit can express the tested secure ORM constraints | S2-03 built the runtime adapter ([AUTHZ](AUTHZ.md)) and a configurable rules PDP plugin used for local real-provider matrices; the bundled static provider still fails closed. Production provider/policy provisioning remains a blocker |
| Gateway supports identity/IP throttle keys | Implement subject-plus-order support or the documented bounded REST-edge fallback, with deployment/capacity evidence |

The 14 provider profiles distinguish existing crate/API versions from unagreed schema/profile identifiers. Shared SDK references in an absent owner's profile identify reusable types only: for example Pricing BillingTerms is not a Subscriptions resolver. Provider configuration, authenticated principal, deployed grants/revision and real conformance run are null when unverified. No placeholder credential, guessed root tenant or default provider is inserted.

Every requirement records a concrete owner acceptance scenario and applicable release path. Its owner conformance command is null because a complete Orders-specific real seam suite has not been established. Separately recorded local commands target existing files and Cargo targets and have known limits. The JSON also names actual existing owner tests where useful, marked metadata-verified and not run in S1-07. Test targets are distinguished from examples and benchmarks; required feature flags are checked. Source-content hashes and the inspected workspace revision disambiguate unreleased `0.0.0` crates from deployed versions. The presence of upstream generic test targets is not a claim that they prove the required Orders scenario.

## Next assignment and execution sequence

**Next: S2-02 — implement the full schema inventory and secure repositories.** S2-01 is already
delivered. [S5-01](RECEIVER_CONTRACTS.md) has reconciled the normative receiver contracts and
fixtures under D-201: internal grant replacement, force-request observation with audit v3,
separate activation clocks, flat Workflow plan and explicit failure mapping. Its local contract
prerequisite is complete; provider implementation/deployment acceptance remains open.

S2-02 can now finalize grant/control and audit DDL alongside the remaining 24-table inventory.
It alone owns Orders migrations. Carry the audit observation checks, frozen v1/v2 decoding,
v3 writer election, grant audit FK and separate internal writer token into schema tests.
Commercial port/profile work (S3-01) remains independently startable. The queue does not launch
workers or establish external owner agreement.

After that, build secure repositories/PEP, then independent idempotency/audit/claims/event collaborators and compose the transition engine. Draft authoring, dates, maintenance and current-parent reads/access logging form the first usable milestone, S2-12. Start real commercial owner delivery alongside the foundation: terms and occupancy need a small Subscriptions provider, not its complete receiver; approval/Payments need owner infrastructure, not completed end-to-end Workflow.

The queue separates **start dependencies** from **completion dependencies** and preserves these critical splits:

- S2-04 can start against S1 contribution types; its completed integration waits for S2-05–08. Those collaborators do not wait for completed S2-04.
- S4-08a supplies pure submit consent before S3-12. S4-08b adds standalone recording and integrated history later.
- S5-06a supplies canonical key/occupancy before gate integration. S5-06b supplies later transactional receiver admission/fencing.
- S6-01/04 draft reads/logging start alongside draft authoring; full projections wait for their producers. S4-06 base history and S4-08b's acceptance extension must not create a circular prerequisite.
- S4-11 can build a payment continuation on selected durable infrastructure before full S5-08. Q-10 engine selection remains visible. Final integration needs the actual process and provider.
- S3-13/14, S4-12 and S5-16 are integration joins. They can produce evidence before S6 production acceptance; no provider waits for its own integration milestone.

Workstream bundles are dispatch aids, not indivisible serial tasks. The original 70 package plans retain detailed acceptance and earlier slice opportunities. No duration estimate or percent-complete claim is derived from their count.

## Release conditions and explicit exclusions

`production_ready` remains false. A local fixture check passing never fills a deployed provider/grant field. Before enabling an affected flow, its owner must record the concrete SDK/schema/profile combination, implementation/configuration revision, trusted identities/grants, supported commercial scope, evidence freshness rules and real conformance command/run. The actual deployment still needs authenticated negative tests, migrations/roles, producer recovery, operational alerts, capacity/latency and restore evidence.

Requirements apply to their declared paths and conditions. The ledger is not an instruction to demand every optional feature for every milestone:

- Contract status and live consent declarations are required when a contract is referenced; absence of an optional contract differs from an unavailable referenced contract.
- Interim Products SKU reads remain required until complete authoritative assessment replaces them; successful receipt creation alone does not retire the read.
- D-103 identity lifecycle is a shared follow-up, not a newly invented Orders-specific release gate. A confirmed unsafe deployment identity still requires resolution.
- Safe revision cleanup remains disabled with references retained; only enabling cleanup invokes the full S6-08 all-writer closure/drain/usage proof. Optional external audit anchoring stays disabled while local audit/checkpoints remain mandatory.
- Only `new_sale` is admitted. Change/add-on/renewal profiles, cross-seller transfer and expanded payment collection/SCA/refund/credit-scoring scope remain excluded.
- Q-07 commercial-history deletion is deferred; separately selected refusal/diagnostic retention is still implemented. No broad destructive retention policy is inferred.
- Partner launch needs real delegation and customer acceptance access, or an explicitly promoted acceptance-not-required commercial policy. Missing prerequisites do not authorize automatic scope reduction.
- Broader quantity policy, tax ownership, durable Workflow substrate and missing performance budgets remain named owner work. Governing latency targets are not silently relaxed.

See all nine [scope dispositions](readiness/UPSTREAM.md#conditional-and-deferred-scope). Source-wide and product-sensitive requirements remain routed by [COVERAGE](COVERAGE.md) and [REVIEW](REVIEW.md); S6-09 must close every applicable source unit with real evidence. This S1 ledger does not label all 2557 source units implemented.

## Verification and maintenance

```sh
python3 gears/bss/orders-lifecycle/docs/implementation/readiness/check_readiness.py
python3 gears/bss/orders-lifecycle/docs/implementation/readiness/render_readiness.py --check
python3 gears/bss/orders-lifecycle/docs/implementation/validate_docs.py
python3 gears/bss/orders-lifecycle/docs/implementation/build_coverage.py --check
```

The checker validates exact upstream coverage and source hashes, all source package titles/dependency text, split-package routing, both dependency edge types for cycles, source/report paths, current Cargo versions/targets, existing local command names and disabled production claims. It does not connect to deployment, inspect secrets or execute owner integration suites. Render without `--check` only after reviewing intentional JSON changes; refresh Cargo/source snapshots deliberately when upstream code changes.

Recorded S1-02–06 results remain linked evidence from those earlier runs, including 38 Rust tests at S1-06. This documentation/metadata step does not claim to rerun them or to have closed their production blockers.

Observed on 2026-10-06: the readiness checker passed against freshly collected Cargo metadata for 44 upstream IDs, 70 packages, 41 acyclic dispatch units and 18 existing packages. Rendered tables matched the reviewed JSON. Documentation validation passed 4432 local links; coverage retained 27 source documents, 2557 units and 44 upstream requirements. New-file syntax/JSON/whitespace checks and `git diff --check` passed. No Rust or external-provider test suite was rerun for this documentation/metadata-only step.
