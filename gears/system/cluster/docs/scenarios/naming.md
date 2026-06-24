# Naming & Validation — Scenario Details (`SC-NAME-*`)

> Detail for §11 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. `fr-cache-storage` mandates one uniform name rule across cache
> keys, lock names, election names, and service names; the SDK exposes
> `validate_cluster_name`. (Scope *prefixes* are validated separately by
> [SC-SCOP-003](./scoping.md).)

**SC-NAME-001 — invalid cache key rejected** · L2 · ☐
- *Intent:* malformed keys fail fast rather than landing as corrupt entries.
- *Steps:* call a cache write/read with a key violating the name rule (e.g. whitespace, control chars).
- *Expected:* `Err(InvalidName { name, reason })`; a valid key succeeds.
- *Done-when:* asserts rejection of an invalid key and acceptance of a valid one.

**SC-NAME-002 — invalid election name rejected** · L2 · ☐
- *Intent:* the same rule guards election names.
- *Steps:* `elect(invalid_name)`.
- *Expected:* `Err(InvalidName)`.
- *Done-when:* asserts rejection.

**SC-NAME-003 — invalid lock name rejected** · L2 · ☐
- *Intent:* the same rule guards lock names.
- *Steps:* `try_lock(invalid_name, ttl)`.
- *Expected:* `Err(InvalidName)`.
- *Done-when:* asserts rejection.

**SC-NAME-004 — invalid service name rejected** · L2 · ☐
- *Intent:* the same rule guards service names (metadata keys/values are *not* subject to it — see SC-DISC-008).
- *Steps:* `register(ServiceRegistration { name: invalid_name, .. })`.
- *Expected:* `Err(InvalidName)`.
- *Done-when:* asserts rejection of the service name while metadata with the same characters is accepted.

**SC-NAME-005 — rule is uniform across primitives** · L2 · ☐
- *Intent:* the whole point of `fr-cache-storage`'s uniform convention — consumers learn one rule, not four.
- *Steps:* take a small set of names spanning the valid/invalid boundary; submit each to cache key, election, lock, and service name.
- *Expected:* every name's validity verdict is identical across all four primitives.
- *Done-when:* asserts no primitive accepts a name another rejects (or vice versa).
