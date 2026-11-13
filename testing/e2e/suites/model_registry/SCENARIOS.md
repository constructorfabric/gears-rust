# Model Registry E2E scenarios

Black-box REST scenarios for the `model-registry` gear. Expected results come from the gear's [PRD](../../../../gears/model-registry/docs/PRD.md), [DESIGN](../../../../gears/model-registry/docs/DESIGN.md) and [ADRs](../../../../gears/model-registry/docs/ADR/), not from its source. A failing test means the code and the docs disagree.

Run with `make e2e-local SUITE=model-registry`. The local launcher uses SQLite; PostgreSQL behavior is covered by the `model-registry-pg` integration lane.

## Environment

The suite uses the static tenant tree and tokens of `config/e2e-local.yaml`:

| Tenant | Token | Role in the PRD use cases |
|---|---|---|
| hierarchy-root | `e2e-token-hierarchy-root` | Root |
| hierarchy-l1a (child of root) | `e2e-token-hierarchy-l1a` | Partner / Customer |
| hierarchy-l2b (child of l1a) | `e2e-token-hierarchy-l2b` | Team |
| hierarchy-l1b (child of root) | `e2e-token-hierarchy-l1b` | Other partner |
| hierarchy-sm (child of root, self-managed) | `e2e-token-hierarchy-sm` | Self-managed tenant |
| hierarchy-sm-child (child of sm) | `e2e-token-hierarchy-sm-child` | Tenant below the barrier |

Two provider types derived from `gts.cf.genai.model.provider.v1~` are seeded in Types Registry: `…~cf.e2e._.open.v1~` leaves `provider_settings` unconstrained, and `…~cf.e2e._.strict.v1~` requires a string `deployment` and no other key.

The static PDP permits every request of a known tenant, so denial is covered only by the nil-tenant token (MR-R08). CredStore v2 is not linked, which is the documented state in which a new `secret_ref` fails closed (MR-M10).

Every test creates providers under a unique slug and deletes them on teardown. List assertions filter by the test's own provider, name or `family`.

## Providers — [test_providers.py](test_providers.py)

Source: `cpt-cf-model-registry-fr-provider-management`, DESIGN §3.1 Provider, §3.3 REST Endpoints, §4 GTS Checks.

| ID | Scenario | Expected |
|---|---|---|
| MR-P01 | Create with `slug`, `name`, `gts_type` only | 201, `Location`, `ETag "1"`; UUIDv7 id; `enabled=true`, `sharing_mode=tenant`, `managed=false`; audit fields name the caller |
| MR-P02 | Get and list an own provider | 200 with `ETag`; `inherited=false`, `shadowed=false`, `available_for_eval=true`; metadata returned unchanged |
| MR-P03 | PATCH `name` and `enabled`, then `metadata: null` | Present fields replace, absent fields stay, null clears; version and ETag increment |
| MR-P04 | Delete, read again, delete again; read an unknown id | 204, then 404 provider not found each time |
| MR-P05 | Create a second provider with the same slug | 409 `already_exists` in the same tenant; allowed in a sibling tenant |
| MR-P06 | Slug outside `[a-z0-9-]{1,64}`; name over 255 characters | 400 with `INVALID_FORMAT` on `slug`; `TOO_LARGE` on `name` |
| MR-P07 | `gts_type` that does not parse, is not a provider type, or is not registered | 400 on `gts_type` with `INVALID_GTS_TYPE`, `NOT_A_PROVIDER_TYPE`, `GTS_TYPE_NOT_REGISTERED`; nothing created |
| MR-P08 | PATCH `slug`, `tenant_id`, `version`, `created_at`, or an unknown field | 400 `IMMUTABLE_FIELD` on the field; unknown field is `INVALID_FORMAT` on `body`; row unchanged |
| MR-P09 | Delete a provider that has a model | 204; the model is 404 afterwards |

## Models — [test_models.py](test_models.py)

Source: `cpt-cf-model-registry-fr-model-identity`, `-fr-model-management`, `-fr-model-cost`, `-fr-model-settings-validation`, DESIGN §3.1 Model and Model Info, ADR-0002, ADR-0004.

| ID | Scenario | Expected |
|---|---|---|
| MR-M01 | Create a model | 201, `Location`, `ETag "1"`; `canonical_id = slug::name`; tenant and `gts_type` copied from the provider; approval `approved` decided by the caller; documented info defaults; `base_url` absent |
| MR-M02 | Get and list an own model | Flags `inherited=false`, `shadowed=false`, `provider_enabled=true`, `available_for_eval=true` |
| MR-M03 | Create with all five cost rates, one rate, no rates | Rates returned as sent; absent rates stay absent |
| MR-M04 | PATCH `cost` and `capabilities` with partial objects; PATCH optional fields to null; PATCH `display_name` to null | Nested objects replaced whole; absent fields unchanged; null clears `base_url` and `description`; null on a required field is 400 |
| MR-M05 | PATCH `name`, `provider_id`, `canonical_id`, `gts_type`, `tenant_id`, `version`, or an unknown field; change `provider_model_id` | 400 `IMMUTABLE_FIELD`; unknown field is `INVALID_FORMAT` on `body`; `provider_model_id` is mutable, may repeat, and leaves `canonical_id` unchanged |
| MR-M06 | Create a second model with the same name under the provider | 409 `already_exists`; the name is free under another provider |
| MR-M07 | Create under an unknown provider id | 404 provider not found |
| MR-M08 | Invalid `name`, `base_url`, `icon`, `provider_model_id`; over-long `description`, `display_name`; unknown key in `cost` or `context_window` | 400 with one violation on the field or its JSON pointer, reason `INVALID_FORMAT` or `TOO_LARGE`; nothing created |
| MR-M09 | Settings against the strict type, the open type, and a non-object | Valid settings stored; a mismatch is `SCHEMA_MISMATCH` on `provider_settings` on create and update; open type accepts any object; a non-object is rejected |
| MR-M10 | Create or update with a new `secret_ref`; clear it; malformed reference | 503 `service_unavailable` and nothing written; clearing succeeds; malformed is `INVALID_FORMAT` |
| MR-M11 | Delete, read again, delete again | 204, then 404 model not found; the name is free again |

## Concurrency — [test_concurrency.py](test_concurrency.py)

Source: ADR-0004, DESIGN §3.3 REST conventions. Each scenario runs for a provider and for a model.

| ID | Scenario | Expected |
|---|---|---|
| MR-C01 | PATCH without `If-Match` | 400 `IF_MATCH_REQUIRED` on `If-Match`; row unchanged |
| MR-C02 | PATCH with a stale validator | 409 `aborted`, `OPTIMISTIC_LOCK_FAILURE`; never 412; row unchanged |
| MR-C03 | PATCH with `W/"1"`, `"01"`, or two validators | 409 for a differently spelled validator; 400 `INVALID_FORMAT` for two |
| MR-C04 | PATCH with the current ETag, then the old one, then `*` | 200 and ETag `"2"`; 409; 200 and ETag `"3"` |
| MR-C05 | DELETE with a stale, a current, and no `If-Match` | 409 and the row remains; 204; 204 |

## Sharing, shadowing and isolation — [test_catalog.py](test_catalog.py)

Source: `cpt-cf-model-registry-fr-provider-sharing`, `-fr-provider-shadowing`, `-fr-model-sharing`, `-fr-effective-catalog`, `-fr-tenant-isolation`, `-usecase-share-and-shadow-provider`, ADR-0001, DESIGN §3.6 effective catalog.

| ID | Scenario | Expected |
|---|---|---|
| MR-T01 | A tenant-mode provider with a shared model, read by parent, sibling and child | 404 on point reads, absent from lists, model not found on resolve |
| MR-T02 | Root's shared provider and shared model, read by child and grandchild | Both visible with `inherited=true` and the full record (`cost`, `provider_settings`, `base_url`); the model resolves |
| MR-T03 | Shared provider with a tenant-mode model | Descendants see the provider, not the model |
| MR-T04 | Child updates or deletes an inherited provider or model, or creates a model under it | 403 `INHERITED_READ_ONLY`; rows unchanged |
| MR-T05 | Sibling writes to an invisible provider or model | 404 not found; rows unchanged |
| MR-T06 | l1a creates a tenant-mode provider with Root's slug | l1a resolves its own model only; Root's provider and models are listed for l1a as `shadowed`; l2b, l1b and Root keep Root's catalog |
| MR-T07 | l1a's shadowing provider is `shared` | l2b resolves l1a's model and loses Root's other models; l1b keeps Root's |
| MR-T08 | l1a's shared shadowing provider is disabled | l1a and l2b get `PROVIDER_DISABLED` for its model and not found for Root-only models; l1b unaffected |
| MR-T09 | l1a deletes its shadowing provider | Root's provider and models resolve again for l1a and l2b |
| MR-T10 | Root switches a model, then the provider, to `tenant` | The child loses each on its next request; Root is unaffected |
| MR-T11 | Self-managed tenant and its child read Root's shared entries; the child reads the self-managed tenant's shared entries; Root reads into the subtree | Nothing inherited across the barrier; the child inherits from the self-managed tenant; Root sees nothing below the barrier |
| MR-T12 | Child sets `approval_state` on an inherited model | 403 `INHERITED_READ_ONLY`; the model still resolves |
| MR-T13 | Root revokes a shared model its descendants resolved, then approves it again; Root creates a shared model as `pending` | Descendants get `MODEL_NOT_APPROVED` and lose it from the available list on the next request; the management read still shows it with `available_for_eval=false`; approval restores it |
| MR-T14 | l1a creates an empty provider with Root's slug, then adds a model with an inherited name and one with a new name | The empty provider already hides Root's models of the slug; the inherited name resolves to l1a's model with l1a's settings and cost; the new name resolves; un-overridden inherited names stay not found; l1b keeps Root's models |
| MR-L07 | Sibling lists with `$filter=tenant_id eq <other tenant>` | Empty page: a filter cannot widen visibility |

## Resolution and available models — [test_resolution.py](test_resolution.py)

Source: `cpt-cf-model-registry-fr-resolve-model`, `-fr-list-available-models`, `-fr-provider-disable`, `-fr-model-approval`, `-usecase-resolve-model`, `-usecase-approve-model`, `-usecase-list-available-models`.

| ID | Scenario | Expected |
|---|---|---|
| MR-R01 | Resolve by percent-encoded canonical id | 200 with the full model and no entry flags; equal to the management read minus flags |
| MR-R02 | Unknown model, unknown slug, missing separator, empty name | 404 model not found |
| MR-R03 | Disable the provider, then enable it; pending model of a disabled provider | 403 `PROVIDER_DISABLED` and absent from the available list while disabled; the management list shows `provider_enabled=false`; restored on enable; disabled is reported before approval |
| MR-R04 | Create as `pending`, `rejected`, `revoked`; approve; resend `approved`; revoke | 403 `MODEL_NOT_APPROVED` until approved; each request with a state records `decided_by` and a new `decided_at`; revoke stops resolution on the next call |
| MR-R05 | l1a's catalog with inherited, own, shadowed, unshared, unapproved and disabled-provider models | The available list holds exactly the canonical ids that resolve |
| MR-R06 | `$filter=vision eq true` over a vision model, a model without vision, and one with vision disabled | Only the first matches, on the available list and the management list |
| MR-R07 | Filter that matches nothing | 200 with an empty page |
| MR-R08 | No token; nil-tenant token | 401; 403 `NOT_PERMITTED` |
| MR-R09 | Root, l1a and l2b each own models; l1a has one tenant-mode model | l2b's available list is Root's shared, l1a's shared and its own models, each with its owner's `tenant_id`; l1a's list has Root's and its own; Root's has only its own |

## Listing — [test_listing.py](test_listing.py)

Source: `cpt-cf-model-registry-fr-management-api-conventions`, DESIGN §3.3 OData on lists, [OData guide](../../../../docs/toolkit_unified_system/07_odata_pagination_select_filter.md).

| ID | Scenario | Expected |
|---|---|---|
| MR-L01 | List without options | Models by `canonical_id asc`, providers by `slug asc`; `page_info.limit` is 20 |
| MR-L02 | Walk five models with `limit=2` and the cursor; `$top` and `$skiptoken` | Every row once, in order; aliases behave the same |
| MR-L03 | Filter on `approval_state`, `family`, `sharing_mode`, `name`, `sort_order`, `api_*`; provider `enabled`, `sharing_mode`, `id` | Exactly the matching rows |
| MR-L04 | `$orderby` on non-nullable fields; on a filter-only field; `$filter` on `cost` or an entry flag | Ordered result; 400 `invalid_argument` for the rest |
| MR-L05 | `$select=id,canonical_id` | Items carry only the selected fields |
| MR-L06 | `$top=0`, `$top` with `limit`, `$skip`, `$count`, bad cursor, malformed filter, `$orderby` with a cursor | 400 `invalid_argument` on all three lists |

## Type migration — [test_type_migration.py](test_type_migration.py)

Source: `cpt-cf-model-registry-fr-provider-management`, DESIGN §3.6 provider write path, §4 GTS Checks.

| ID | Scenario | Expected |
|---|---|---|
| MR-G01 | PATCH the provider's `gts_type` to the strict type when every model fits; resend the stored type | Provider and every model carry the new type; ids and canonical ids kept; every version increments. An equal type updates the provider only |
| MR-G02 | Migrate when one model does not fit; migrate to an unregistered type | 400 with one `SCHEMA_MISMATCH` violation on `gts_type` naming the model's canonical id, or `GTS_TYPE_NOT_REGISTERED`; no row changes |

## Not covered

The docs leave these points open, so no test asserts them:

- Whether `limit` over 100 is clamped or rejected.
- The reason code for an over-long `slug` or model `name`.
- The order of `IF_MATCH_REQUIRED` against 403 and 404.
- `Cache-Control: no-store` (required by ADR-0003, absent from DESIGN).
- Unknown keys in `additional_info` (free map in the field table, rejected in the REST conventions).
- The problem returned for an unknown top-level field in a create body.
- `create_model` under a shadowed inherited provider.

Out of reach of this environment: PDP row constraints and per-action denial, dependency outages other than CredStore, hierarchy changes and the chain cache TTL, `PROVIDER_TYPE_CHANGED` races, and `SECRET_REF_NOT_FOUND` / `SECRET_REF_NOT_OWNED`.
