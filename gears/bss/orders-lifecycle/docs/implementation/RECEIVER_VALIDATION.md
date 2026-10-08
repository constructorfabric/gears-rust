# S5-01 local verification

Recorded 2026-10-06 in this workspace. No deployment, owner conformance run, commit or PR.
The [delivery and semantic matrix](RECEIVER_CONTRACTS.md) describes the scope and limitations.

| Command | Observed result |
|---|---|
| `cargo +1.98.1 test -p cf-gears-bss-orders-lifecycle -p cf-gears-bss-orders-lifecycle-sdk` | 39 tests passed, including PostgreSQL hash round trips, role/capability proofs and trybuild negatives |
| `python3 gears/bss/orders-lifecycle/docs/implementation/process/check_contracts.py` | 19 semantic operations; 164 reference-model cases; mandatory 13-event corpus present |
| `python3 gears/bss/orders-lifecycle/docs/implementation/contracts/contract_checks.py` | 73 contract cases and 8 golden envelopes passed |
| `python3 gears/bss/orders-lifecycle/docs/implementation/conformance/build_vectors.py --check` | 36 independent vectors verified; all 27 prior inputs/preimages/hashes compared byte-for-byte unchanged |
| `python3 gears/bss/orders-lifecycle/docs/implementation/conformance/check_fixtures.py` | 36 hashes, 16 native preimages, 13 scenario recipes and 16 fault contracts verified |
| `python3 gears/bss/orders-lifecycle/docs/implementation/commercial/check_contracts.py` | 36 cases, 5 proposed profiles and 19-node acyclic call graph verified |
| `python3 gears/bss/orders-lifecycle/docs/implementation/readiness/check_readiness.py` | 44 upstream requirements, 70 packages, 41 acyclic dispatch units and 18 actual Cargo packages checked; no production readiness asserted |

Additional checks passed: Clippy for both crates/all targets with warnings denied; Rust formatting;
`cargo gears lint --dylint -P cf-gears-bss-orders-lifecycle-sdk -P cf-gears-bss-orders-lifecycle`;
contract/SDK catalog regeneration checks; receiver/readiness render checks; documentation links
and unchanged public table/state/event inventory; coverage drift checks; `git diff --check`.

Audit v3 is a normative format plus independent reference encoders/fixtures. The production writer
remains S2-06. The internal rebuild operation is registered in the contract catalog, not a callable
SDK implementation. Boolean proof inputs in the process reference model do not prove receiver
transactions, peer authority or the complete forced-exit guard ordering. S5-02/07/08/15 must run
these cases against their actual adapters, storage and authenticated identities.

Next assignment: S2-02 full schema inventory and secure repositories. Include audit observation
shape/version checks, grant audit linkage, the three registered grant refusal reasons and the
separate internal audit token. Preserve all 25 public routes, 24 tables, 29 public transition rows,
21 public triggers, 11 states and 11 events. The error registry now has 89 reasons.
