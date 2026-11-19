# Private ancestor configuration never reaches a descendant

`private` keeps a field with its owner. An ancestor's upstream that shares some field is visible to its descendants, and its routes serve them, but only with the fields it shares: its `private` auth, rate limit, plugins and CORS are neither applied to nor returned for a descendant's request. This holds when the descendant has no upstream of its own (the alias resolves to the ancestor's upstream) and when it binds the alias itself, with the field left out or set with `sharing=inherit`. Headers have no sharing mode; where they go is limited by the owner's endpoints only.

Throughout, the parent's upstream also has one shared field (example: `auth.sharing=inherit` with the noop plugin, or `rate_limit.sharing=inherit` with a high limit), so it is visible to the child.

## Scenario A: private auth

Parent upstream: apikey auth with `sharing=private` and a `secret_ref`; route `POST /echo`.

Expected:
- The parent's request to `/echo` reaches the upstream with the injected credential.
- The child's request reaches the upstream with no `Authorization` header, without an upstream of its own and after binding the alias without auth.

## Scenario B: private rate limit

Parent upstream: `rate_limit.sharing=private`, 1/min; route `GET /v1/models`.

Expected:
- The parent's second request in the minute returns `429`.
- The child's requests are not limited by it (`200`, `200`).

With an ancestor limit the child does get: root `rate_limit.sharing=inherit`, 1000/min, the parent's private limit 1/min, both `scope=global`. Requests parent, child, parent:
- `200`, `200`, `429`: the child's request, under the root's limit on the parent's upstream, does not reset the parent's bucket.

## Scenario C: private plugins

Parent upstream: `plugins.sharing=private` with the required-headers guard requiring `x-tenant-guard`; route `POST /echo`.

Expected:
- The parent's request without `x-tenant-guard` is rejected (`400`, `REQUIRED_HEADER_MISSING`).
- The child's request without it passes (`200`), without an upstream of its own and after binding the alias with its own `inherit` plugins.

## Scenario D: private settings of an ancestor's route

Parent upstream shares its auth; its route `POST /echo` has `plugins.sharing=private` (required-headers guard requiring `x-tenant-guard`) and `rate_limit.sharing=private`, 1/min.

Expected:
- The parent's request without `x-tenant-guard` is rejected (`400`, `REQUIRED_HEADER_MISSING`).
- The child's requests through that route are neither guarded nor limited by it (`200`, `200`), and the route the SDK returns to the child carries neither.
