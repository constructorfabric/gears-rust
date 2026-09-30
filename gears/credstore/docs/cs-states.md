# CredStore (PR #5071): fields, read, write, create and delete rules

The index row in PG carries a nullable timestamp **`write_intent_at`**, the *write intent* mark.
`NULL` means no write in progress: the index matches Vault.
A value means a write has started writing this key to Vault and has not yet confirmed it in PG.
Its age feeds metrics and decides when a fencing repair starts (W.7.3). It never decides correctness.

`write_intent_at` is never compared for equality and never identifies a writer. Several writes may start in the same instant, even the same nanosecond: they all condition their store write on the same version, and the store lets only one of them land (W.4.2). The intent is then cleared by version (W.5), not by its value. So the timestamp does not need extra precision, and no random operation identifier is needed. `now()` of PG is enough.

Every "raise the index" condition in this document is NULL-safe: `version IS NULL OR version < x`. A row whose create has not been confirmed holds `version = NULL`, and `NULL < x` is not true in SQL, so a plain `version < x` would never repair it and its intent would never be cleared.

**Audit is best-effort.** Every read that returns a secret, every write and every fencing repair publishes an audit event when the event broker is available. A failed or rejected publication is counted by a metric and never blocks, fails or rolls back the operation.

## F — Fields

The record lives in Vault as one atomic value. The PG index holds every field except F.4 secret, plus the Vault `version` and `write_intent_at`.

- **F.1 tenant** — UUID; the owning tenant, taken from the security context (`subject_tenant_id`). Never set by the caller.
- **F.2 reference** — string `[a-zA-Z0-9_-]`, 1–255 characters; the caller-chosen name of the credential within the tenant. **Set only at creation**, immutable.
- **F.3 owner** — UUID; present only in the `private` class, taken from the security context (`subject_id`). Never set by the caller.
- **F.4 secret** — string over REST (UTF-8 only), bytes over the in-process client for types that allow binary; nullable. **Set at creation, changeable**: set, replace or remove (`null`). Never stored in PG.
- **F.5 type** — a GTS type id derived from `gts.cf.core.credstore.credential.v1~`, stored as a UUID; required at creation. **Set only at creation**, immutable.
- **F.6 sharing mode** — enum `private` | `tenant` | `shared`, default `tenant`. **Set at creation, changeable** only `tenant` ↔ `shared`; `private` ↔ non-private is rejected.
- **F.7 fallback policy** — enum `inherit` | `none`, default `inherit`. **Set at creation, changeable**; `null` is rejected.
- **F.8 descendant block** — boolean, default `false`; only on non-private records. **Set at creation, changeable** (whether it stays a boolean or becomes a descendant-policy enum is still open in PRD §13).
- **F.9 expiry** — RFC 3339 timestamp, nullable; only for expirable types. **Set at creation, changeable**: set, change or clear.
- **F.10 lifecycle status** — `declared` (no secret) / `active` (has a secret); derived from F.4. Never set by the caller.
- **F.11 creating subject** — UUID; set by the gear.
- **F.12 last-update time** — timestamp; set by the gear.
- **F.13 tombstone** — deletion marker without a secret; written by D. Never set by the caller.

Caller-settable: **F.2 and F.5 at creation only; F.4, F.6, F.7, F.8 and F.9 at creation and later through replace or patch.**

The physical layout below is a proposal for DESIGN. The PRD fixes only behaviour.

- **F.14 Secret store record** (Vault / OpenBao KV v2, or any `CredStorePluginClientV2` backend). One key per record; the value is opaque to the backend and serialized by the gear.
  - **F.14.1 Key** — `<installation prefix>/<tenant>/<reference>/t` for the non-private class, `<installation prefix>/<tenant>/<reference>/o-<owner>` for the private class. It encodes F.1, F.2 and F.3, which are therefore not repeated in the value.
  - **F.14.2 Value** — one JSON object, written atomically:

    | Field | Type | Source |
    |---|---|---|
    | `format` | integer | record format version, starting at 1 |
    | `id` | UUID | record identity, new on every create; kept for `CredStoreClientV1` (`GetSecretResponse.id`, `Matches{id, version}`) |
    | `secret` | base64 bytes, absent when declared or tombstone | F.4 |
    | `type` | UUID | F.5 |
    | `sharing` | `private` / `tenant` / `shared` | F.6 |
    | `fallback` | `inherit` / `none` | F.7 |
    | `descendant_block` | boolean | F.8 |
    | `expires_at` | RFC 3339 timestamp or absent | F.9 |
    | `created_by` | UUID | F.11 |
    | `created_at` | RFC 3339 timestamp | creation time, kept for `CredStoreClientV1` compatibility |
    | `updated_at` | RFC 3339 timestamp | F.12 |
    | `tombstone` | boolean | F.13; when `true`, only `format`, `tombstone` and `updated_at` are present |

  - **F.14.3 Store-managed** — `version`: the KV v2 current version (or the backend's version counter). It is assigned by the store, never part of the value, and used for every conditional write (W.4).
  - **F.14.4 Not stored** — F.10 lifecycle status, derived from the presence of `secret`, and the inheritance status, computed during resolution.
  - **F.14.5 Linearizable access** — every read and conditional write in this document assumes per-key linearizability: an acknowledged write is visible to the next read. With Vault Enterprise performance standbys or replicas, the gear **must** read and write only on the active node or use Vault's consistency headers (`X-Vault-Index`); a stale read would make R.3/R.5 resolve by an old state and W.4.3 misjudge an unknown outcome.
  - **F.14.6 Empty data at a live version** — if the store returns a live version without data (KV v2 `delete_version_after`, or a soft delete done outside the gear), the gear **must** treat it as **E.10** `503 Service Unavailable`, never as "record absent". The startup configuration check (PRD p2) **must** reject a mount with `delete_version_after` enabled.

- **F.15 Postgres index row** (`credstore_index`). It is derived from F.14, holds no secret, and is rebuildable from the store.

  | Column | Type | Source / purpose |
  |---|---|---|
  | `tenant_id` | UUID NOT NULL | F.1 |
  | `reference` | TEXT NOT NULL | F.2 |
  | `owner_id` | UUID NOT NULL | F.3; nil UUID for the non-private class |
  | `id` | UUID NULL | F.14.2 `id`; NULL for a tombstone |
  | `type_uuid` | UUID NULL | F.5 |
  | `sharing` | SMALLINT NULL | F.6 |
  | `fallback` | SMALLINT NULL | F.7 |
  | `descendant_block` | BOOLEAN NOT NULL DEFAULT false | F.8 |
  | `expires_at` | TIMESTAMPTZ NULL | F.9 |
  | `has_secret` | BOOLEAN NOT NULL | F.10 (`declared` / `active`), without the secret itself |
  | `tombstone` | BOOLEAN NOT NULL DEFAULT false | F.13 |
  | `created_by` | UUID NULL | F.11 |
  | `created_at`, `updated_at` | TIMESTAMPTZ | F.14.2 |
  | `version` | BIGINT NULL | F.14.3; NULL while a create has not been confirmed (C.3.1) |
  | `write_intent_at` | TIMESTAMPTZ NULL | write intent (W.3, W.5, W.7); NULL means the index matches the store |

  Indexes:
  - **F.15.1** unique `(tenant_id, reference, owner_id)` — one row per store key; also serializes concurrent creates (C.3.1);
  - **F.15.2** `(reference, tenant_id)` — one query over the whole ancestor chain (R.1);
  - **F.15.3** partial index `WHERE write_intent_at IS NOT NULL` — metrics on open intents and the threshold check for W.7.3.

  Not in PG: F.4 secret.

## E — Replies (REST and SDK)

Codes and reasons are the ones the current `CredStoreClientV1` REST surface already uses (`/credstore/v1/secrets`, canonical `Problem` body). The audit is best-effort and never produces an error reply.

- **E.1** `200 OK` — read (`GET /credstore/v1/secrets/{ref}`). Body: `value` (the secret) and `metadata`: `id`, `sharing`, `inheritance` (`own` / `inherited` / `overridden`, replacing today's `is_inherited`), `version`, `secret_type`, `expires_at`, and for an own record also `fallback` and `descendant_block`. `owner_tenant_id` is no longer returned. Headers: `ETag: "<id>.<version>"` (opaque for an inherited result, and rejected as a write precondition), `Cache-Control: no-store`. SDK: `Ok(Some(GetSecretResponse))`.
- **E.2** `201 Created` — create (`POST /credstore/v1/secrets`). No body. Headers: `Location: /credstore/v1/secrets/{ref}`, `ETag` of the new record. SDK: `Ok(())`.
- **E.3** `204 No Content` — replace or patch (`PUT /credstore/v1/secrets/{ref}`). No body. Header: `ETag` of the new version, which is new compared with today, where `PUT` returns no `ETag`. SDK: `Ok(())`.
- **E.4** `204 No Content` — delete (`DELETE /credstore/v1/secrets/{ref}`). No body, no `ETag`. SDK: `Ok(())`.
- **E.5** `400 Bad Request` — category `INVALID_ARGUMENT` (or `FAILED_PRECONDITION` for a class change). Reasons: `INVALID_SECRET_REF`, `IF_MATCH_REQUIRED`, `INVALID_IF_MATCH`, the violated type trait's reason, `UNSUPPORTED_TRANSITION`. SDK: `InvalidRef`, `TypeViolation`, `UnsupportedTransition`, or an internal precondition error.
- **E.6** `403 Forbidden` — category `PERMISSION_DENIED`, reason `ACCESS_DENIED`. The caller lacks the action (write, delete, or read at all) for its own tenant. SDK: `AccessDenied`.
- **E.7** `404 Not Found` — category `NOT_FOUND`. A read resolves nothing, or its type is denied by the PDP; both look the same (no enumeration). A delete of a reference with no own record. SDK: `get` → `Ok(None)`, `delete` → `NotFound`.
- **E.8** `409 Conflict` — category `ALREADY_EXISTS`. A create over a live record, or a lost concurrent create. SDK: `Conflict`.
- **E.9** `409 Conflict` — category `ABORTED`, reason `OPTIMISTIC_LOCK_FAILURE`. The `If-Match` validator does not match, the version moved, the store's conditional write failed, or an update targets a missing record (an update never creates). The caller re-reads and retries. SDK: `Conflict`.
- **E.10** `503 Service Unavailable` — category `UNAVAILABLE`, with `Retry-After` when known. PG, Vault, the PDP, `tenant-resolver` or `types-registry` is unavailable, or a write's outcome is unknown. SDK: `ServiceUnavailable { retry_after }`.
- **E.11** `500 Internal Server Error` — category `INTERNAL`. An unexpected store error, for example the gear's own store identity is rejected. SDK: `Internal`.

## R — Read (point read, `get_secret`, listing)

- **R.1** **Query PG** for the key's rows along the ancestor chain: own private, own non-private, ancestors. If PG is unavailable, return **E.10** `503 Service Unavailable`.
  - **R.1.1** Listing: any SQL filter on index fields (type, sharing, fallback, expiry, tombstone) **must not** exclude rows with `write_intent_at`. Those rows are always selected, read from Vault in R.3, and the filter is applied to the Vault data. The same holds for any SQL-side optimisation that skips tombstones or expired rows.
- **R.2** **Pick the decisive record** from the index data. If none is decisive, the whole chain is the candidate set.
- **R.3** **Read from Vault every row with `write_intent_at` that is not farther from the requester than the decisive record.**
  - **R.3.1** Vault is newer than the index: resolve by the Vault data and raise the index `WHERE version IS NULL OR version < vault`. Leave the intent alone.
  - **R.3.2** Vault holds the same version (the writer is still in flight, or died before reaching Vault): resolve by the Vault data, which is the old state. Leave the intent alone.
  - **R.3.3** Vault holds no record (a create that never reached Vault): treat the record as absent and continue upward.
  - **R.3.4** The intent is older than the configured threshold: run a fencing repair (W.7.3).
  - **R.3.5** Vault is unavailable: return **E.10** `503 Service Unavailable`.
- **R.4** **If the decisive record changed after R.3**, repeat R.3 for the new decisive record.
- **R.5** **Always read the decisive record from Vault.**
  - **R.5.1** The version matches the index: check visibility, type and expiry against the Vault data. A tombstone (F.13) counts as absent. The PDP (`authz-resolver`) decision is taken on the type read from Vault, never on the index type, so a stale or half-finished index row can never change an authorization decision. A denial answers **E.7** `404 Not Found`, exactly like an absent record; **E.6** `403 Forbidden` is returned only when the caller has no read permission at all.
  - **R.5.2** Vault is newer: raise the index `WHERE version IS NULL OR version < vault`. If the record is no longer decisive (tombstone, `shared` → `tenant`, secret removed under `inherit`, expired), continue upward and go back to R.3.
  - **R.5.3** Vault is unavailable: return **E.10** `503 Service Unavailable`.
- **R.6** **Reply.** The inheritance status is computed during resolution. A resolved record: **E.1** `200 OK` with the secret, the metadata and the `ETag`. Nothing resolves: **E.7** `404 Not Found` (SDK `Ok(None)`).

## W — Write (replace and patch of an existing record)

C (create) and D (delete) follow these steps and describe only where they differ.

- **W.1** **Read PG.** Fetch the key's row in the addressed class: version `v1` and `write_intent_at`.
  - **W.1.1** No row, or the row is a tombstone: return **E.9** `409 Conflict` (`OPTIMISTIC_LOCK_FAILURE`), because an update never creates (today's `CredStoreClientV1` behaviour). A create goes through C.
  - **W.1.2** PG is unavailable: return **E.10** `503 Service Unavailable` and stop. Nothing is written to Vault.
  - **W.1.3** The row carries `write_intent_at`: before using `v1`, read the record from Vault and raise the index to it (`WHERE version IS NULL OR version < vault`, leaving the intent alone), then take the Vault version as `v1`. Without this, a client holding the validator returned by W.6 (the Vault version) is rejected in W.2.1 whenever W.5 failed and the index still shows the older version. If Vault is unavailable, return **E.10** `503 Service Unavailable` and stop.
- **W.2** **Checks before any side effect.**
  - **W.2.1** Client precondition: `Matches` is compared with `v1`, and a mismatch returns **E.9** `409 Conflict` (`OPTIMISTIC_LOCK_FAILURE`). `Exists` takes `v1` as is. A missing or malformed `If-Match` returns **E.5** `400 Bad Request` (`IF_MATCH_REQUIRED` / `INVALID_IF_MATCH`).
  - **W.2.2** PDP on the record's type: a denial returns **E.6** `403 Forbidden` (`ACCESS_DENIED`); PDP unavailable returns **E.10** `503 Service Unavailable`.
  - **W.2.3** Type traits and immutability (F.2, F.5, the F.6 class): a violation returns **E.5** `400 Bad Request` (the trait's reason, or `UNSUPPORTED_TRANSITION` for a change between `private` and non-private).
- **W.3** **Set the intent in PG:** `UPDATE … SET write_intent_at = now() WHERE key AND version = v1`. There is no `write_intent_at IS NULL` condition, so a foreign intent is overwritten.
  - **W.3.1** 0 rows (the version moved): return **E.9** `409 Conflict` (`OPTIMISTIC_LOCK_FAILURE`) and stop.
  - **W.3.2** PG is unavailable or times out: return **E.10** `503 Service Unavailable` and stop. Nothing is written to Vault.
- **W.4** **Conditional write to Vault:** `put(IfVersion(v1))` of the whole record (secret and every visibility field).
  - **W.4.1** **Success at `v2`:** go to W.5.
  - **W.4.2** **Conflict** (Vault is no longer at `v1`):
    - **W.4.2.1** re-read the record from Vault; if Vault holds no record at all (the key was purged while PG still holds a tombstone), treat the base as "key absent": a create continues with `IfAbsent` (C.4), an update returns **E.9** `409 Conflict` (`OPTIMISTIC_LOCK_FAILURE`), and a delete returns **E.7** `404 Not Found`;
    - **W.4.2.2** `UPDATE PG SET fields, version = vault WHERE version IS NULL OR version < vault`, leaving the intent alone;
    - **W.4.2.3** for an `Exists` (last-writer-wins) write, restart once from W.1 with the repaired index; otherwise, or if the restart conflicts again, return **E.9** `409 Conflict` (`OPTIMISTIC_LOCK_FAILURE`) and stop.
  - **W.4.3** **Timeout or broken connection, outcome unknown:** re-read the record from Vault.
    - **W.4.3.1** The stored record equals what this write sent: treat the write as landed at the stored version `v2`, and go to W.5. If another writer stored identical content, the outcome is the same state, so this is harmless.
    - **W.4.3.2** The stored record differs, or is still at `v1`: return **E.10** `503 Service Unavailable` and stop. The intent stays.
    - **W.4.3.3** Vault is unavailable for the re-read as well: return **E.10** `503 Service Unavailable` and stop. The intent stays.
    - **W.4.3.4** Known behaviour: if this write landed as `v2` and another write replaced it with different content before the re-read, the caller gets **E.10** `503 Service Unavailable` although its write landed. No data is lost; a retry with `Exists` overwrites the newer content (last writer wins), and a retry with `Matches` gets a conflict.
  - **W.4.4** **Definite error** (the write certainly did not land, for example access denied or store-side validation): return **E.5** `400 Bad Request` when the store rejects the record as invalid (for example over its size limit), otherwise **E.11** `500 Internal Server Error`, and stop. The intent stays.
- **W.5** **Confirm in PG:** `UPDATE … SET fields, version = v2, write_intent_at = NULL WHERE key AND (version < v2 OR version IS NULL)`. Every intent recorded at a version below `v2` belongs to a writer whose store write can no longer land, so clearing it is safe. The value of `write_intent_at` is not compared.
  - **W.5.1** 1 row: the intent is cleared and the index matches Vault.
  - **W.5.2** 0 rows (someone, such as a conflicting writer or a read repair, already raised the index to `v2` or beyond): do nothing. The intent stays until W.7.2 or W.7.3 clears it, and it is harmless meanwhile (R.3).
  - **W.5.3** PG is unavailable or times out: do nothing. The intent stays, so reads go to Vault (R.3).
- **W.6** **Reply:** **E.3** `204 No Content` with `ETag` = the new validator `v2`, whenever the write reached W.5, even if W.5 itself failed.
- **W.7** **Who clears `write_intent_at`.**
  - **W.7.1** The write whose store write landed, in W.5. It clears every intent recorded at a lower version, its own and any concurrent writer's, because none of those writers can land any more.
  - **W.7.2** Any later successful write to the same reference. It sets its own intent over the old one (W.3) and clears it in its own W.5.
  - **W.7.3** **Fencing repair**, started by a read (R.3.4) or a rebuild when the intent is older than the configured threshold (default 60 s):
    - **W.7.3.0** **Vault is ahead of the index** (Vault has the key and the row's `version IS NULL OR version < vault`): no store write. A single PG update `UPDATE … SET fields, version = vault, write_intent_at = NULL WHERE key AND (version IS NULL OR version < vault)` completes the repair. Every intent recorded at a lower version belongs to a writer whose conditional write can no longer land, so clearing it is safe. Rewriting the store in this case would be wrong: a late W.5 of the writer that landed could then clear the intent while the index lags behind the rewritten version (found by the TLA+ model in `tla/`).
    - **W.7.3.1** Otherwise (the index is at the Vault version, or Vault has no key): read the record from Vault at version `vk`;
    - **W.7.3.2** `put(IfVersion(vk))` with **the same data**, producing `vk+1`; if Vault holds no record, write a tombstone with `IfAbsent`;
    - **W.7.3.3** `UPDATE PG SET fields, version = vk+1, write_intent_at = NULL WHERE version IS NULL OR version < vk+1`.

    Once the version has advanced, no write conditioned on an earlier version can land, so the repair is correct whenever it runs. The threshold only avoids overtaking live slow writers; an overtaken writer gets a conflict and retries. The repair changes no secret or visibility field.

    **Threshold and clock.** The age check uses the PG clock (`write_intent_at < now() - threshold`), never the application clock, so clock skew between replicas cannot start a repair early. The threshold defaults to **60 s** and is configurable; it **must** be greater than the store request timeout plus a safety margin (for example a store request timeout of 10–15 s); a repair that overtakes a live slow create (writing a tombstone with `IfAbsent`) only turns that create into a conflict.

    **Concurrent repairs.** Two repairs of the same key both read `vk` and both write `IfVersion(vk)`; one gets `vk+1`, the other a conflict. The loser **must** not fail the read that started it: it re-reads Vault and raises the index as in R.3.1.

    **Same data.** "The same data" means byte for byte, including `id`, `created_at` and `updated_at`; only the store version changes.

    **Hot keys.** An open intent on a `shared` record near the root makes every read in its whole subtree pay one extra Vault read until W.7.2 or W.7.3 clears it. The threshold bounds that window; open intents are exposed as a metric, and those on records with a large subtree are worth alerting on.
  - **W.7.4** Nothing else clears the intent: not a plain read, an index repair or a rebuild without W.7.3, and never age alone.

## C — Create

Steps follow W. Only the differences are listed.

- **C.1** Read PG as in W.1, except:
  - **C.1.1** No row: the base is "key absent".
  - **C.1.2** The row is a tombstone: the base is the tombstone's version.
  - **C.1.3** The row is a create that never finished (`version` is NULL, intent set): the base is "key absent".
  - **C.1.4** An own row of the same class whose expiry (F.9) has passed: the create succeeds over it, and the base is that row's version.
  - **C.1.5** A live, unexpired own row of the same class: return **E.8** `409 Conflict` (`ALREADY_EXISTS`) and stop.
  - **C.1.6** PG is unavailable: as W.1.2.
- **C.2** Checks as W.2. F.5 type is required. W.2.1 does not apply: create is create-only.
- **C.3** Set the intent:
  - **C.3.1** No row: `INSERT` a row with the request's fields, `version` NULL and `write_intent_at`. A unique-key violation (a concurrent create) returns **E.8** `409 Conflict` (`ALREADY_EXISTS`) and stops.
  - **C.3.2** A row exists (C.1.2, C.1.3, C.1.4): as W.3, conditioned on the base (`version = base version`, or `version IS NULL`), and also overwrite the row's fields with the request's fields, so no field of a dead earlier create survives in the index.
  - **C.3.3** PG is unavailable: as W.3.2.
- **C.4** Conditional write to Vault as W.4, with `IfAbsent` for C.1.1 and C.1.3, and `IfVersion(base version)` for C.1.2 and C.1.4. A conflict means the record already exists: repair the index as W.4.2.1–W.4.2.2 and return **E.8** `409 Conflict` (`ALREADY_EXISTS`). Unknown outcomes and errors are handled as W.4.3 and W.4.4.
- **C.5** Confirm as W.5.
- **C.6** Reply **E.2** `201 Created` with `Location` and the new `ETag` whenever the create reached C.5, even if C.5 itself failed.

## D — Delete

Steps follow W. Only the differences are listed.

- **D.1** Read PG as in W.1, addressing the caller's own row in the class: the caller's own record for `private`, the tenant's record otherwise. No row or a tombstone: return **E.7** `404 Not Found`.
- **D.2** Checks as W.2, with the PDP delete action.
- **D.3** Set the intent as W.3.
- **D.4** Conditional write to Vault as W.4, but the record written is a **tombstone** (F.13): no secret and no visibility fields, only the deletion marker. The version continues to `v2`.
- **D.5** Confirm as W.5: the PG row stays with `tombstone = true`, `version = v2` and `write_intent_at = NULL`.
- **D.6** Reply **E.4** `204 No Content`, no body, whenever the delete reached D.5, even if D.5 itself failed.
- **D.7** **State after delete:**
  - **D.7.1** Vault: the key remains, and its latest version is the tombstone. Older versions holding the secret remain in the KV v2 history until the p2 "no retained previous secrets" requirement is in place; they are not reachable through the gear.
  - **D.7.2** PG: the row remains as a tombstone for as long as the tombstone exists. It never expires on its own.
  - **D.7.3** Reads treat the tombstone as absent (R.5.1) and continue up the chain. Use suppression (F.7 `none`) instead of delete to stop inheriting.
  - **D.7.4** Re-creation goes through C.1.2, so versions never repeat and validators from before the delete never match.
  - **D.7.5** The key is removed entirely only by tenant or owner offboarding, in this order: first the PG row, then the Vault purge of all versions. If the purge fails, the re-run is idempotent; nobody reads records of a removed tenant or owner meanwhile. Offboarding runs only after the tenant retention period, which is far longer than any write timeout, so a write started before soft deletion cannot land after the purge.
- **D.8** **Failure between D.4 and D.5:** the PG row still looks live at `v1` with the intent set. R.3 reads Vault, finds the tombstone and treats the record as absent.

## S — State space of one key

One key is one Vault key plus its PG index row (F.14.1, F.15.1). Writers in flight are not part of the state: a crashed or slow writer leaves only what it has already written. Notation:

- **PG:** `∅` means no row; `(⊥, i)` means an unconfirmed create (`version` NULL, intent set); `live@n` / `tomb@n` means a live or tombstone row at version `n`; `, i` means `write_intent_at` is set.
- **Vault:** `∅` means no key; `live@m` / `tomb@m` means a live record or a tombstone at store version `m`.

| # | PG | Vault | Meaning | A read returns |
|---|---|---|---|---|
| **S.1** | `∅` | `∅` | never written, or fully offboarded | not found |
| **S.2** | `(⊥, i)` | `∅` | create in flight, or it died before reaching Vault | not found (R.3.3) |
| **S.3** | `(⊥, i)` | `live@m` | create landed, confirm (C.5) not done | the Vault record (R.3) |
| **S.4** | `(⊥, i)` | `tomb@m` | a fencing repair wrote a tombstone over a dead create; its PG step not done | not found |
| **S.5** | `live@n` | `live@n` | steady state | the record |
| **S.6** | `live@n, i` | `live@n` | update or delete in flight, or died before Vault; or an intent left after a read repair | the Vault record, i.e. the old state (R.3.2) |
| **S.7** | `live@n, i` | `live@m`, m > n | update landed (or a fencing repair rewrote), confirm not done | the Vault record at `m` (R.3.1) |
| **S.8** | `live@n, i` | `tomb@m`, m > n | delete landed, confirm not done | not found (R.3.1) |
| **S.9** | `tomb@n` | `tomb@n` | deleted | not found |
| **S.10** | `tomb@n, i` | `tomb@n` | create over the tombstone in flight, or died before Vault | not found |
| **S.11** | `tomb@n, i` | `live@m`, m > n | create over the tombstone landed, confirm not done | the Vault record (R.3.1) |
| **S.12** | `tomb@n, i` | `tomb@m`, m > n | a fencing repair rewrote the tombstone; its PG step not done | not found |
| **S.13** | `∅` | `live@m` or `tomb@m` | offboarding removed the PG row; the Vault purge is not done (D.7.5) | not found; allowed only for a removed tenant or owner |

An expired live record (F.9) is a live state for S; a read treats it as absent (R.5.1).

**Unreachable states.** Each of these violates an invariant (section I), so the protocol never enters it:

- **S.X1** — PG row without intent, but PG ≠ Vault (version, kind or fields). Violates I.1.
- **S.X2** — PG version above the Vault version, or a PG row while Vault has no key, outside S.2. Violates I.2.
- **S.X3** — Vault has the key, PG has no row, outside offboarding (S.13). Violates I.3.
- **S.X4** — `(⊥)` without intent. Violates I.6.

## I — Invariants

They hold in every reachable state, S.1–S.13.

- **I.1 No intent means in sync.** A PG row without `write_intent_at` equals Vault: same version, kind and fields. No PG row means no Vault key, except in S.13.
- **I.2 The index is never ahead.** PG `version` ≤ Vault version, where NULL counts as below every version and a missing Vault key counts as 0.
- **I.3 No hidden record.** If Vault has the key, PG has a row (S.13 excepted). This holds because the intent row is written before the store (W.3, C.3.1).
- **I.4 No secret in PG.** PG never holds F.4.
- **I.5 Versions never repeat.** The Vault version of a key only grows and is never reused until offboarding purges the key.
- **I.6 An unconfirmed create has an intent.** A PG row with `version` NULL always carries `write_intent_at`.
- **I.7 Reads are correct.** A read of any state answers exactly what the Vault state implies, as in the "A read returns" column of S. Together, I.1 and R.3 guarantee this.
- **I.8 One winner per base.** Of any writes conditioned on the same base version, at most one lands in Vault. This is the store's CAS guarantee (F.14.5).

I.1 (strict, including version equality), I.2, I.3, I.5, I.6 and I.7 are model-checked in `tla/CredStoreIntent.tla`.

## T — Transitions (state × event)

Events that change state:

- **T.a** set intent (W.3 / C.3 / D.3);
- **T.b** a store write lands (W.4 / C.4 / D.4);
- **T.c** confirm (W.5 / C.5 / D.5);
- **T.d** read repair raises the index (R.3.1, R.5.2, W.1.3, W.4.2.2);
- **T.e** fencing repair: the PG-only repair when Vault is ahead (W.7.3.0), otherwise the store rewrite (W.7.3.2);
- **T.f** fencing repair, PG step (W.7.3.3);
- **T.g** offboarding deletes the PG row;
- **T.h** offboarding purges the Vault key.

A crash, a timeout, an unavailable dependency and a rejected request (E.5–E.11) change nothing: the state stays where it was. `—` means the event cannot occur in that state, for the reason given in the notes.

| State | T.a intent | T.b store write lands | T.c confirm | T.d read repair | T.e fence (store) | T.f fence (PG) | T.g off PG | T.h off Vault |
|---|---|---|---|---|---|---|---|---|
| **S.1** | create → S.2 | — ¹ | — ¹ | — ² | — ³ | — ³ | S.1 | S.1 |
| **S.2** | create → S.2 | create → S.3 | — ¹ | — ² | → S.4 | — ⁴ | S.1 | — |
| **S.3** | via T.d | — ⁵ | → S.5 | → S.6 | PG-only → S.5 | — ⁴ | S.13 | — |
| **S.4** | via T.d | — ⁵ | — ¹ | → S.10 | PG-only → S.9 | → S.9 | S.13 | — |
| **S.5** | update / delete / create over expired → S.6 | — ⁶ | — ¹ | — ² | — ³ | — ³ | S.13 | — |
| **S.6** | any → S.6 | update → S.7, delete → S.8 | — ¹ | — ² | → S.7 | → S.5 | S.13 | — |
| **S.7** | via T.d | — ⁵ | → S.5 | → S.6 | PG-only → S.5 | → S.5 | S.13 | — |
| **S.8** | via T.d | — ⁵ | → S.9 | → S.10 | PG-only → S.9 | — ⁴ | S.13 | — |
| **S.9** | create → S.10 | — ⁶ | — ¹ | — ² | — ³ | — ³ | S.13 | — |
| **S.10** | create → S.10 | create → S.11 | — ¹ | — ² | → S.12 | → S.9 | S.13 | — |
| **S.11** | via T.d | — ⁵ | → S.5 | → S.6 | PG-only → S.5 | — ⁴ | S.13 | — |
| **S.12** | via T.d | — ⁵ | — ¹ | → S.10 | PG-only → S.9 | → S.9 | S.13 | — |
| **S.13** | — ⁷ | — ⁷ | — ⁷ | — ⁷ | — ⁷ | — ⁷ | — | S.1 |

Notes:

1. Nothing has landed in the store for this state's intent, so there is nothing to confirm. In S.1, S.5 and S.9 there is no intent at all.
2. PG already equals Vault (or Vault is empty in S.2, where R.3.3 answers "absent"), so there is nothing to raise.
3. There is no intent, so there is nothing to repair (W.7.3 starts only on an intent).
4. The PG step of a fencing repair runs only after its own store rewrite, which happens only in S.2, S.6 and S.10 (leading to S.4, S.7 and S.12). Where Vault is already ahead, T.e is the PG-only repair and there is no separate PG step.
5. The PG base is behind Vault, so a conditional write from it fails (W.4.2) and only repairs the index. A new writer first raises the index (W.1.3, i.e. T.d) and then sets its intent from the raised state.
6. A store write needs an intent first (I.3). The write always goes through T.a before T.b.
7. The tenant or owner is removed. Requests are rejected before any side effect, and offboarding finishes with T.h.

Every non-`—` cell leads to a state in S.1–S.13; no event leads to an S.X state. The model in `tla/` checks this mechanically under all interleavings and crash points.
