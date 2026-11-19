# Descendant override permissions

## Setup

Parent upstream shares configs with `sharing=inherit`:
- `auth.sharing=inherit`
- `rate_limit.sharing=inherit`
- `plugins.sharing=inherit`

## Scenario A: child lacks override_auth permission

Child attempts to override auth config.

Expected:
- Update/binding rejected or effective config ignores child override (lock expected behavior).

## Scenario B: child lacks override_rate permission

Child sets a custom `rate_limit`.

Expected:
- Rejected or ignored.

## Scenario C: child lacks add_plugins permission

Child attempts to append plugins.

Expected:
- Rejected or ignored.

## Scenario D: override refused under an enforcing grand-ancestor

Three tenants bind the same alias: top → middle → leaf.

- Middle: `auth.sharing=inherit`, `cors.sharing=inherit` (bound before the top enforced anything).
- Top: `auth.sharing=enforce`, `cors.sharing=enforce`.

The leaf binds with its own `auth` or its own `cors`.

Expected:
- `400 Bad Request`, `invalid_argument`, `context.format` = `cannot override auth: ancestor upstream has sharing mode 'enforce'` (same for `cors`). The closest ancestor's `inherit` does not lift the top's `enforce`.
- Binding without overrides is accepted (`201`).

## Scenario E: rate limit allocated under an enforcing grand-ancestor

Three tenants bind the same alias: top → middle → leaf.

- Middle: `rate_limit.sharing=inherit`, 500/min, `budget={mode: allocated, total: 500}` (bound before the top enforced anything).
- Top: `rate_limit.sharing=enforce`, 2/min.

The leaf binds with `rate_limit` 100/min (`inherit`), then updates it to 200/min. Under an allocated parent the leaf must set a limit, so the top's `enforce` does not refuse it: a rate limit override is refused only when the closest ancestor enforces it.

Expected:
- Create `201`, update `200`; the budget allocation check against the middle still applies.
- The leaf's proxy requests are held to the top's 2/min: two `200`, then `429`. The merge keeps the top's enforced limit as the floor down the chain.
