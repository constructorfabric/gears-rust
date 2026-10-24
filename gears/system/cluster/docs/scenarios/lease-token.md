# Lease Tokens — Scenario Details (`SC-LTOK-*`)

> Detail for §13 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Implemented in `cluster-conformance/src/lease_token.rs`.
> SC-LTOK-LOCK-003 and SC-LTOK-LEAD-002 need a stored deadline; holder-only backends (k8s) run
> them `#[ignore]`d (DESIGN §3.19.1).

**SC-LTOK-LOCK-001 — lock token round trip** · L2 · ☑
- *Intent:* the token half acquires, renews and releases one lease.
- *Steps:* A `acquire`; B `acquire` the same name; A `renew`, `release`; B `acquire`.
- *Expected:* B's first acquire is `LockContended`; A's renew and release are `Ok`; B's second acquire succeeds.
- *Done-when:* asserts each outcome.

**SC-LTOK-LOCK-002 — release is idempotent; a released token is dead** · L2 · ☑
- *Intent:* a retried release is harmless, and a released token cannot be revived.
- *Steps:* `acquire`, `release` twice, then `renew`.
- *Expected:* both releases `Ok`; renew `LockExpired`.
- *Done-when:* asserts the second release and the renew.

**SC-LTOK-LOCK-003 — renew after a lapse** · L2 · ☑
- *Intent:* the token-path twin of SC-LOCK-005: a lapsed lease must not be renewed, even if nobody took it.
- *Steps:* `acquire` with a short TTL; let it lapse; `renew`.
- *Expected:* `LockExpired`.
- *Done-when:* asserts `LockExpired`. Ignored on holder-only backends.

**SC-LTOK-LOCK-004 — stale token after a steal** · L2 · ☑
- *Intent:* fencing: a successor's lease is untouchable by the previous token.
- *Steps:* A `acquire` with a short TTL; let it lapse; B steals; A `renew`, A `release`; C `acquire`; B `renew`.
- *Expected:* B's `(owner, fence)` differs from A's; A's renew `LockExpired`; A's release `Ok` and changes nothing; C `LockContended`; B's renew `Ok`.
- *Done-when:* asserts each outcome.

**SC-LTOK-LOCK-005 — guard and token share one lease** · L2 · ☑
- *Intent:* `try_lock` and `acquire` contend on the same lease.
- *Steps:* guard holds the name; `acquire`; release the guard; `acquire`; `try_lock`.
- *Expected:* acquire under the guard and try_lock under the token are both `LockContended`.
- *Done-when:* asserts both contentions.

**SC-LTOK-LEAD-001 — leader token round trip** · L2 · ☑
- *Intent:* the token half joins, renews and resigns one claim.
- *Steps:* A `join`; B `join`; A `renew`, `resign` twice, `renew`; B `join`.
- *Expected:* B's first join `None`; A's renew `Ok`; both resigns `Ok`; A's last renew `LockExpired`; B's second join wins.
- *Done-when:* asserts each outcome.

**SC-LTOK-LEAD-002 — leader renew after a lapse** · L2 · ☑
- *Intent:* the election twin of SC-LTOK-LOCK-003.
- *Steps:* A `join` with a short TTL; let it lapse; `renew`.
- *Expected:* `LockExpired`.
- *Done-when:* asserts `LockExpired`. Ignored on holder-only backends.

**SC-LTOK-LEAD-003 — stale leader token after a takeover** · L2 · ☑
- *Intent:* fencing for elections.
- *Steps:* A `join` with a short TTL; let it lapse; B joins on the default 30 s TTL; A `renew`, A `resign`; B `renew`; C `join`.
- *Expected:* A's renew `LockExpired`; A's resign `Ok` and changes nothing; B's renew `Ok`; C's join `None`.
- *Done-when:* asserts each outcome.
