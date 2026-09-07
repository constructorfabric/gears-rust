# Settings Service sandbox

A local environment for building a frontend against the Settings Service:
the example server with fifteen demo declarations in four categories, a set of
test identities, and a throwaway page that drives every endpoint so the
request flows can be watched raw.

## Run it

Two terminals, from anywhere in the repository:

```sh
gears/settings-service/sandbox/run.sh server   # builds and starts the API on http://127.0.0.1:8087
gears/settings-service/sandbox/run.sh ui       # serves http://127.0.0.1:8090/ and proxies /settings-service/* to 8087
```

Open <http://127.0.0.1:8090/>. The page is one HTML file and one script, no
build step; the proxy is a standard-library Python script. The API alone is on
8087; its OpenAPI document is at <http://127.0.0.1:8087/openapi.json>.

State lives in `~/.cf-gears/settings-service/settings-service.db` (SQLite).
Delete that directory for a fresh start; the demo declarations register again
on the next boot.

## Identities

Static bearer tokens from `config/e2e-local.yaml`, sent as
`Authorization: Bearer <token>`.

| Token | Who | Tenant |
|---|---|---|
| `e2e-token-tenant-a` | platform administrator, interactive user | `00000000-df51-5b42-9538-d2b56b7ee953` (`e2e-root`, the root) |
| `e2e-token-tenant-a-reviewer` | a second platform administrator | root |
| `e2e-token-hierarchy-root` | tenant administrator | `00000000-0000-0000-0000-000000000001` (`hierarchy-root`) |
| `e2e-token-hierarchy-l1a` | tenant administrator | `00000000-0000-0000-0000-000000000002` (`hierarchy-l1a`) |
| `e2e-token-hierarchy-l1b` | tenant administrator | `00000000-0000-0000-0000-000000000005` (`hierarchy-l1b`) |
| `e2e-token-tenant-b` | a tenant outside the tree | `bbbbbbbb-…` (expect 403 or 404) |

The hierarchy tokens carry no `subject_type`, so the write path treats them as
service principals: they may write every demo setting except `api_token`,
which requires step-up and refuses a service principal with 403.

Authorization is a static policy that allows everything for these tokens; the
subtree rules below are enforced by the service itself.

## Tenants

```text
e2e-root                              00000000-df51-5b42-9538-d2b56b7ee953   platform scope
└── hierarchy-root                    00000000-0000-0000-0000-000000000001
    ├── hierarchy-l1a                 00000000-0000-0000-0000-000000000002
    │   └── hierarchy-l2b             00000000-0000-0000-0000-000000000004
    └── hierarchy-l1b                 00000000-0000-0000-0000-000000000005
        └── hierarchy-l2c             00000000-0000-0000-0000-000000000006
```

`tenant` is a query parameter on every value endpoint. Omitted, it is the
caller's own tenant; for the platform administrator that is the root, which is
platform scope. A caller may target its own tenant or a descendant. An
ancestor, a sibling or a standalone tenant answers 403. There is no tenant
list endpoint yet: the tree above has to be known to the client.

## Data model in one paragraph

A **category** groups **declarations**. A declaration has a key (a GTS type
id such as `gts.cf.core.settings.setting_type.v1~cf.settings_demo.network.proxy_enabled.v1~`),
a `value_type_id` naming one of fifteen value types (`bool_flag`, `string`,
`text`, `secret_string`, `integer`, `number`, `port`, `duration_seconds`,
`url`, `hostname`, `ipv4`, `email`, `cron`, `regex`, `json`), a `scope_class`
(`global`: one value at platform scope; `cascading`: inherited down the tree,
nearest override wins; `local`: per tenant, never inherited), a Schema Default
and `traits` for rendering. A **value** at a scope is an override; reading a
setting at a scope resolves the **effective value** with its `source`
(`own_override`, `inherited`, `schema_default`), `source_scope` and the
inheritance trail from the root down to the scope. Keys contain `~` and `.`;
URL-encode them in paths.

## The flows

Categories: `GET /settings-service/v1/categories` (OData `$filter` and
`$orderby` over `key`, `name`, `domain_affinity`; `limit` and `cursor`).

Declarations of a category:
`GET /settings-service/v1/declarations?$filter=category_id eq <uuid>` — gives
`value_type_id` and `traits`, which pick the widget.

Effective values of a category at a tenant:
`GET /settings-service/v1/settings?tenant=<uuid>&$filter=category_id eq <uuid>`
— one entry per setting with its own `outcome` (`resolved`, `not_found`,
`retired`, …) and, when resolved, `effective` carrying `value`, `source`,
`source_scope`, `traits`, `inheritance_trail`, `last_change_at`,
`data_classification`, `masked`, the review pair and `etag`. Also accepted:
`$filter=key in ('k1','k2')` and `$filter=needs_review eq true`.

One setting: `GET /settings-service/v1/settings/{key}?tenant=` — the same
shape, and the `ETag` header equals `effective.etag`.

Set: `PUT /settings-service/v1/settings/{key}/value?tenant=` with body
`{"value": …}` and header `If-Match: <etag>`; the `etag` is the one the read
returned, or the literal `absent` when the scope has no row yet. The response
carries `old_value`, `new_value`, the new `etag` and `change_set_id`. Then
re-read, or use the response.

Validate without storing: `POST /settings-service/v1/settings/{key}/validate?tenant=`
with `{"value": …, "limit": 20}` — `valid`, field-level `violations`, the
current `effective` value and, for a cascading setting, the `impact` on
descendants. Impact alone: `GET …/{key}/impact?tenant=&value=<json>&limit=`.

Revert (fall back to the ancestor or the default):
`POST …/{key}/value/revert?tenant=` with `If-Match`. Remove the row:
`DELETE …/{key}/value?tenant=` with `If-Match`. Both answer with the change
and what the scope resolves to now.

Clone: `POST …/{key}/value/clone?tenant=<to>` with `{"from": "<tenant uuid>"}`
and `If-Match` for the target.

Batch: `POST /settings-service/v1/settings/batch` with
`{"changes": [{"key", "tenant", "value", "if_match"}, …]}` — at most 500,
one `committed` or `rejected` entry per change.

History: `GET …/{key}/history?tenant=&limit=&cursor=` — newest first.

## Errors

Every error is an RFC 9457 problem document: `type`, `title`, `status`,
`detail`, `instance`, and for a validation failure `violations[]` with
`field`, `code` and `message`.

Tenant access restriction of one setting at one tenant:
`GET /settings-service/v1/settings/{key}/permissions?tenant=<uuid>` — returns
`effective` (`access`: `overridable`, `read_only` or `hidden`, plus
`supplied_by`, the tenant whose row wins), `stored` (the row at that tenant,
absent when none) and `etag`; the `ETag` header repeats it, `absent` when no
row exists. `PUT` with `{"access": "read_only" | "hidden"}` and `If-Match`
creates or changes the row; `DELETE` with `If-Match` removes it; both answer
with the new readout. `overridable` is not a stored state, so `PUT` refuses
it with 400: clear the row instead. The caller must be an administrator above
the target: a tenant cannot restrict itself, a sibling or an ancestor (403).
A row set on a tenant applies to its whole subtree; the strictest row on the
chain wins. `GET …/permissions/all` lists every row in the caller's subtree.
`hidden` makes the setting answer 404 to that tenant on every read and browse.

Declarations carry `default_value`, `data_classification`, `has_secret_trait`,
`requires_step_up`, `anonymous_exposable`, `source`, `last_change_at` and
`etag`; `GET /settings-service/v1/declarations/{id}` sets the `ETag` header.

| Status | When |
|---|---|
| 400 | malformed key or `tenant`, invalid value (with `violations`), unsupported OData, over 500 batch changes |
| 401 | step-up required and not proven — `WWW-Authenticate: Bearer error="insufficient_user_authentication", max_age=300` |
| 403 | not authorized, target outside the subtree, a service principal on a step-up declaration, a tenant whose own access is `read_only` or `hidden` |
| 404 | no declaration at the key, or hidden from the caller |
| 409 | a tenant-scoped write to a `global` setting |
| 410 | the declaration is retired |
| 412 | `If-Match` stale: the value moved since it was read |
| 428 | `If-Match` missing |
| 503 | a dependency cannot answer; a write to a secret setting, since no secret store is bound yet |

## What is not there yet

- Administrative creation of declarations: they come from gears through the
  SDK. The demo gear is the only contributor.
- Secret values cannot be set (503) until the Secret Manager lands; render
  secret settings read-only.
- Step-up needs an identity provider configured under `step_up` in the gear's
  config; the sandbox has none, so any declaration with `requires_step_up`
  answers 401 on write. The demo declarations opt out, except `api_token`.
- CORS is off on the example server. Serve the frontend from the same origin
  or proxy `/settings-service/*` in the dev server, as this sandbox does.
- No "who am I" endpoint: the client knows its tenant from how it logged in.
