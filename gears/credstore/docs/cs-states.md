# CredStore (PR #5071): fields, read, write, create and delete rules

The index row in PG carries a nullable timestamp **`write_intent_at`**, the *write intent* mark.
`NULL` means no write in progress: the index matches Vault.
A value means a write has started writing this key to Vault and has not yet confirmed it in PG.
Its age feeds metrics and decides when a fencing repair starts (W.7.3). It never decides correctness.

`write_intent_at` is never compared for equality and never identifies a writer. Several writes may start in the same instant, even the same nanosecond: they all condition their store write on the same version, and the store lets only one of them land (W.4.2). The intent is then cleared by version (W.5), not by its value. So the timestamp does not need extra precision, and no random operation identifier is needed. `now()` of PG is enough.

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

## R — Read (point read, `get_secret`, listing)

- **R.1** **Query PG** for the key's rows along the ancestor chain: own private, own non-private, ancestors. If PG is unavailable, return `unavailable`.
- **R.2** **Pick the decisive record** from the index data. If none is decisive, the whole chain is the candidate set.
- **R.3** **Read from Vault every row with `write_intent_at` that is not farther from the requester than the decisive record.**
  - **R.3.1** Vault is newer than the index: resolve by the Vault data and raise the index `WHERE version < vault`. Leave the intent alone.
  - **R.3.2** Vault holds the same version (the writer is still in flight, or died before reaching Vault): resolve by the Vault data, which is the old state. Leave the intent alone.
  - **R.3.3** Vault holds no record (a create that never reached Vault): treat the record as absent and continue upward.
  - **R.3.4** The intent is older than the configured threshold: run a fencing repair (W.7.3).
  - **R.3.5** Vault is unavailable: return `unavailable`.
- **R.4** **If the decisive record changed after R.3**, repeat R.3 for the new decisive record.
- **R.5** **Always read the decisive record from Vault.**
  - **R.5.1** The version matches the index: check visibility, type and expiry against the Vault data. A tombstone (F.13) counts as absent.
  - **R.5.2** Vault is newer: raise the index `WHERE version < vault`. If the record is no longer decisive (tombstone, `shared` → `tenant`, secret removed under `inherit`, expired), continue upward and go back to R.3.
  - **R.5.3** Vault is unavailable: return `unavailable`.
- **R.6** **Reply.** The inheritance status is computed during resolution. If nothing resolves, return not-found.

## W — Write (replace and patch of an existing record)

C (create) and D (delete) follow these steps and describe only where they differ.

- **W.1** **Read PG.** Fetch the key's row in the addressed class: version `v1` and `write_intent_at`.
  - **W.1.1** No row, or the row is a tombstone: return not-found. Patch never creates, and a create-only replace goes through C.
  - **W.1.2** PG is unavailable: return `unavailable` and stop. Nothing is written to Vault.
- **W.2** **Checks before any side effect.**
  - **W.2.1** Client precondition: `Matches` is compared with `v1`, and a mismatch returns conflict. `Exists` takes `v1` as is.
  - **W.2.2** PDP on the record's type: a denial returns not-found or forbidden; PDP unavailable returns `unavailable`.
  - **W.2.3** Type traits and immutability (F.2, F.5, the F.6 class): a violation returns a validation error.
- **W.3** **Set the intent in PG:** `UPDATE … SET write_intent_at = now() WHERE key AND version = v1`. There is no `write_intent_at IS NULL` condition, so a foreign intent is overwritten.
  - **W.3.1** 0 rows (the version moved): return conflict and stop.
  - **W.3.2** PG is unavailable or times out: return `unavailable` and stop. Nothing is written to Vault.
- **W.4** **Conditional write to Vault:** `put(IfVersion(v1))` of the whole record (secret and every visibility field).
  - **W.4.1** **Success at `v2`:** go to W.5.
  - **W.4.2** **Conflict** (Vault is no longer at `v1`):
    - **W.4.2.1** re-read the record from Vault;
    - **W.4.2.2** `UPDATE PG SET fields, version = vault WHERE version < vault`, leaving the intent alone;
    - **W.4.2.3** return conflict and stop.
  - **W.4.3** **Timeout or broken connection, outcome unknown:** re-read the record from Vault.
    - **W.4.3.1** The stored record equals what this write sent: treat the write as landed at the stored version `v2`, and go to W.5. If another writer stored identical content, the outcome is the same state, so this is harmless.
    - **W.4.3.2** The stored record differs, or is still at `v1`: return `unavailable` and stop. The intent stays.
    - **W.4.3.3** Vault is unavailable for the re-read as well: return `unavailable` and stop. The intent stays.
  - **W.4.4** **Definite error** (the write certainly did not land, for example access denied or store-side validation): return the error and stop. The intent stays.
- **W.5** **Confirm in PG:** `UPDATE … SET fields, version = v2, write_intent_at = NULL WHERE key AND (version < v2 OR version IS NULL)`. Every intent recorded at a version below `v2` belongs to a writer whose store write can no longer land, so clearing it is safe. The value of `write_intent_at` is not compared.
  - **W.5.1** 1 row: the intent is cleared and the index matches Vault.
  - **W.5.2** 0 rows (someone, such as a conflicting writer or a read repair, already raised the index to `v2` or beyond): do nothing. The intent stays until W.7.2 or W.7.3 clears it, and it is harmless meanwhile (R.3).
  - **W.5.3** PG is unavailable or times out: do nothing. The intent stays, so reads go to Vault (R.3).
- **W.6** **Reply: success with the new validator `v2`** whenever the write reached W.5, even if W.5 itself failed.
- **W.7** **Who clears `write_intent_at`.**
  - **W.7.1** The write whose store write landed, in W.5. It clears every intent recorded at a lower version, its own and any concurrent writer's, because none of those writers can land any more.
  - **W.7.2** Any later successful write to the same reference. It sets its own intent over the old one (W.3) and clears it in its own W.5.
  - **W.7.3** **Fencing repair**, started by a read (R.3.4) or a rebuild when the intent is older than the configured threshold:
    - **W.7.3.1** read the record from Vault at version `vk`;
    - **W.7.3.2** `put(IfVersion(vk))` with **the same data**, producing `vk+1`; if Vault holds no record, write a tombstone with `IfAbsent`;
    - **W.7.3.3** `UPDATE PG SET fields, version = vk+1, write_intent_at = NULL WHERE version < vk+1`.

    Once the version has advanced, no write conditioned on an earlier version can land, so the repair is correct whenever it runs. The threshold only avoids overtaking live slow writers; an overtaken writer gets a conflict and retries. The repair changes no secret or visibility field.
  - **W.7.4** Nothing else clears the intent: not a plain read, an index repair or a rebuild without W.7.3, and never age alone.

## C — Create

Steps follow W. Only the differences are listed.

- **C.1** Read PG as in W.1, except:
  - **C.1.1** No row: the base is "key absent".
  - **C.1.2** The row is a tombstone: the base is the tombstone's version.
  - **C.1.3** The row is a create that never finished (`version` is NULL, intent set): the base is "key absent".
  - **C.1.4** A live own row of the same class: return conflict (create-only) and stop.
  - **C.1.5** PG is unavailable: as W.1.2.
- **C.2** Checks as W.2. F.5 type is required. W.2.1 does not apply: create is create-only.
- **C.3** Set the intent:
  - **C.3.1** No row: `INSERT` a row with the request's fields, `version` NULL and `write_intent_at`. A unique-key violation (a concurrent create) returns conflict and stops.
  - **C.3.2** A row exists (C.1.2, C.1.3): as W.3, conditioned on the base (`version = tombstone version`, or `version IS NULL`).
  - **C.3.3** PG is unavailable: as W.3.2.
- **C.4** Conditional write to Vault as W.4, with `IfAbsent` for C.1.1 and C.1.3, and `IfVersion(tombstone version)` for C.1.2. A conflict means the record already exists: handle as W.4.2. Unknown outcomes and errors are handled as W.4.3 and W.4.4.
- **C.5–C.6** As W.5–W.6.

## D — Delete

Steps follow W. Only the differences are listed.

- **D.1** Read PG as in W.1, addressing the caller's own row in the class: the caller's own record for `private`, the tenant's record otherwise. No row or a tombstone: return not-found.
- **D.2** Checks as W.2, with the PDP delete action.
- **D.3** Set the intent as W.3.
- **D.4** Conditional write to Vault as W.4, but the record written is a **tombstone** (F.13): no secret and no visibility fields, only the deletion marker. The version continues to `v2`.
- **D.5** Confirm as W.5: the PG row stays with `tombstone = true`, `version = v2` and `write_intent_at = NULL`.
- **D.6** As W.6.
- **D.7** **State after delete:**
  - **D.7.1** Vault: the key remains, and its latest version is the tombstone. Older versions holding the secret remain in the KV v2 history until the p2 "no retained previous secrets" requirement is in place; they are not reachable through the gear.
  - **D.7.2** PG: the row remains as a tombstone for as long as the tombstone exists. It never expires on its own.
  - **D.7.3** Reads treat the tombstone as absent (R.5.1) and continue up the chain. Use suppression (F.7 `none`) instead of delete to stop inheriting.
  - **D.7.4** Re-creation goes through C.1.2, so versions never repeat and validators from before the delete never match.
  - **D.7.5** The key is removed entirely (Vault purge of all versions, PG row delete) only by tenant or owner offboarding.
- **D.8** **Failure between D.4 and D.5:** the PG row still looks live at `v1` with the intent set. R.3 reads Vault, finds the tombstone and treats the record as absent.
