# Watch Auto-Restart Combinator — Scenario Details (`SC-REST-*`)

> Detail for §7 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. Exercises `*Watch::auto_restart(RetryPolicy)` →
> `RestartingWatch<W>` (DESIGN §3.9). A scripted/mock watch source that emits chosen
> `Closed(_)` payloads keeps these at L2 (no real backend needed).

**SC-REST-001 — retryable close reconnects with `Reset`** · L2 · ☐
- *Intent:* transient terminal causes should self-heal transparently and tell the consumer to re-read.
- *Steps:* wrap a watch in `auto_restart(policy)`; have the source emit `Closed(Provider { kind: ConnectionLost })` (also `Timeout`, `ResourceExhausted`).
- *Expected:* the combinator reconnects after backoff and surfaces a `Reset` to the consumer on each successful resubscribe; no `Closed` reaches the consumer.
- *Done-when:* asserts a `Reset` (not `Closed`) for each retryable kind.

**SC-REST-002 — non-retryable close propagates** · L2 · ☐
- *Intent:* causes that retrying cannot fix must reach the consumer unchanged.
- *Steps:* emit `Closed(Provider { kind: AuthFailure })`, `Closed(Shutdown)`, `Closed(CapabilityNotMet { .. })`, and `Closed(Provider { kind: Other })`.
- *Expected:* each is propagated to the consumer as `Closed(err)` verbatim; the combinator does not retry.
- *Done-when:* asserts the consumer observes the original `Closed` for every non-retryable cause.

**SC-REST-003 — backoff honors `RetryPolicy`** · L2 · ☐
- *Intent:* reconnect timing must follow the configured policy and stop at the retry cap, preventing thundering-herd storms.
- *Steps:* configure `RetryPolicy { initial_backoff, max_backoff, jitter_factor, max_retries: Some(n) }`; emit repeated retryable closes under `tokio::time` control.
- *Expected:* backoff grows from initial toward max within the jitter band; after `n` retries the last `Closed(err)` is propagated unchanged.
- *Done-when:* asserts the backoff schedule (within jitter) and propagation on exhaustion.

**SC-REST-004 — uniform across all three watch types** · L2 · ☐
- *Intent:* one `RetryPolicy` type drives the combinator for cache, leader, and service-discovery watches — one canonical pattern.
- *Steps:* construct `RestartingWatch` over `CacheWatch`, `LeaderWatch`, and `ServiceWatch` with the same policy; run SC-REST-001's retryable-close case on each.
- *Expected:* identical reconnect/`Reset` behavior across all three.
- *Done-when:* asserts the same outcome for each watch type under one policy.
