Created:  2026-10-01 by Virtuozzo International GmbH
Updated:  2026-10-02 by Virtuozzo International GmbH

# DESIGN — BSS Approvals Inbox

<!-- toc -->

- [1. Architecture Overview](#1-architecture-overview)
  - [1.1 Architectural Vision](#11-architectural-vision)
  - [1.2 Architecture Drivers](#12-architecture-drivers)
  - [1.3 Architecture Layers](#13-architecture-layers)
- [2. Principles & Constraints](#2-principles--constraints)
  - [2.1 Design Principles](#21-design-principles)
  - [2.2 Constraints](#22-constraints)
- [3. Technical Architecture](#3-technical-architecture)
  - [3.1 Domain Model](#31-domain-model)
  - [3.2 Component Model](#32-component-model)
  - [3.3 API Contracts](#33-api-contracts)
  - [3.4 Internal Dependencies](#34-internal-dependencies)
  - [3.5 External Dependencies](#35-external-dependencies)
  - [3.6 Interactions & Sequences](#36-interactions--sequences)
  - [3.7 Database schemas & tables](#37-database-schemas--tables)
- [4. Additional context](#4-additional-context)

<!-- /toc -->

## 1. Architecture Overview

### 1.1 Architectural Vision

The inbox is a facade. Units stay in the gear that writes them. The facade asks each configured source as the caller, merges the keyset pages, sums the counts, and routes a vote to the source that holds the unit.

### 1.2 Architecture Drivers

| Requirement | Design response |
| --- | --- |
| `cpt-cf-bss-approvals-fr-one-inbox` | One list, one count, one card and one vote. The merge key is `(submitted_at, id)`. A 403 source is omitted and named forbidden. A 503 or missing source is named unavailable and omitted; the read is 503 only when every source is down (AP-D-5). The card asks every source. The vote answer is forwarded unchanged. |
| `cpt-cf-bss-approvals-nfr-no-store` | The gear implements REST only. It does not implement a database capability and it never requests a database. |

### 1.3 Architecture Layers

Two crates live under `gears/bss/approvals`:

- `approvals-sdk` (package `cf-gears-bss-approvals-sdk`, library `bss_approvals_sdk`) is the source port.
- `approvals` (package `cf-gears-bss-approvals`, library `bss_approvals`) is the gear `bss-approvals`.

The SDK has no REST types. The gear owns the HTTP doors, the merge and the cursor.

## 2. Principles & Constraints

### 2.1 Design Principles

- [ ] `p1` - **ID**: `cpt-cf-bss-approvals-principle-units-stay`

A unit is created, applied and decided in the gear that owns the aggregate. The inbox never copies that write.

### 2.2 Constraints

- [ ] `p1` - **ID**: `cpt-cf-bss-approvals-constraint-no-database`

The gear has no tables. Startup does not call `db_required`. A missing configuration object skips route registration instead of failing the process for lack of a database.

## 3. Technical Architecture

### 3.1 Domain Model

`InboxUnit` is the unit the inbox serves. It has no version. Concurrency is the generation in the vote body. `subject_live` is the products card's live SKU head; pricing serves null. `impact` is the live impact, or null when the caller skips it.

The kind is the closed set `prices`, `plan_revision`, `sku_publish`, `sku_change`, `sku_retire`.

A `SortKey` is `(submitted_at, id)`. The id order is the lower-case hex order.

### 3.2 Component Model

- [ ] `p1` - **ID**: `cpt-cf-bss-approvals-component-facade`

The facade reads `sources` from its configuration, resolves each name as a scoped `ApprovalSourceV1`, and merges what the caller can read. It is the only component in this gear.

### 3.3 API Contracts

`ApprovalSourceV1` has `page`, `counts`, `get`, one `vote`, and `system_actors`: the actors its gear names "System", none by default (AP-D-11). `page` takes the narrowing, the order, the limit, the source's own key and whether to fill impact. `vote` takes the body bytes and the idempotency key and returns status, headers and body.

The HTTP doors are:

- `GET /bss-approvals/v1/approval-units`
- `GET /bss-approvals/v1/approval-units/counts`
- `GET /bss-approvals/v1/approval-units/{id}`
- `POST /bss-approvals/v1/approval-units/{id}/approve`
- `POST /bss-approvals/v1/approval-units/{id}/reject`
- `POST /bss-approvals/v1/approval-units/{id}/withdraw`

`limit` defaults to 50 and is clamped at 200. The list defaults to newest first. `$orderby` beside a cursor is 400 `ORDER_WITH_CURSOR`. A changed narrowing is 400 `FILTER_MISMATCH`.

The list and the counts answer a weak `ETag` of the JSON body they serve, `sources` included, and `Cache-Control: private, no-cache`. An `If-None-Match` that matches it is 304 with an empty body and the same two headers. A source that changes status, for example from `ok` to `unavailable`, changes the body and so the tag. The card and the votes are not conditional (AP-D-10).

The list and the card name each unit's submitter (`submitted_by_name`) and each decision's actor (`actor_name`), and a products unit's live SKU names its creator and, while it is archived, its archiver. The inbox is the only one that names them: a source answers its card unnamed, so a card read makes one lookup. The nil id and the actors the configured sources declare read "System" without a lookup. The names come from Account Management's user read, under the caller's own rights, in one lookup per answer; a name is null when it is not available now, and the read never fails because of it. The names are part of the list's body, so a rename changes its tag (AP-D-11).

Declared vote codes: 400 `GENERATION_REQUIRED`, `GENERATION_MISMATCH`, `UNIT_STALE`, `NOTE_REQUIRED`, `NOTE_TOO_LONG`, `BODY_UNEXPECTED`; 403 for the grant and `SOD_VIOLATION`; 404; 409 `DUPLICATE_VOTE`, `UNIT_ALREADY_DECIDED`, `IDEMPOTENCY_CONFLICT`; 503. The doors do not answer 412.

### 3.4 Internal Dependencies

The gear depends on the SDK, ClientHub, the canonical error types and the OData error codes `ORDER_WITH_CURSOR`, `FILTER_MISMATCH`, `INVALID_CURSOR`, `INVALID_ORDERBY_FIELD` and `INVALID_LIMIT`.

### 3.5 External Dependencies

Each configured source is another gear's `ApprovalSourceV1`, registered under `ClientScope` equal to that gear's name. A source name is non-blank and unique; a duplicate or a blank name fails init. A source counts body names only the closed kind set: an absent kind is 0, and a kind field outside the set does not decode. Pricing registers `PricingApprovalSource` as `pricing` (pricing D-490) and products registers `ProductsApprovalSource` as `products` (products P-D-250). Each calls its gear's own doors: the list's read with a `CursorV1` built from the source's key, the counts on the plain connection, the card door, and the vote door through the gear's router. This gear's own tests register fake sources; products' tests run the facade over both real gears, on `SQLite` and on Postgres.

### 3.6 Interactions & Sequences

- [ ] `p1` - **ID**: `cpt-cf-bss-approvals-seq-list-page`

1. The caller sends the list query. `$orderby` with a cursor is refused before the token is read.
2. The facade decodes the cursor, checks the narrowing hash, and asks every configured source for up to `limit` units after that source's key.
3. A 403 source is omitted and named `forbidden`. A 503 or a missing registration is named `unavailable` and omitted (AP-D-5). The read is 503 `SOURCE_UNAVAILABLE` only when every configured source is down, and 403 only when every configured source is forbidden. Any other door error is returned as that error. A cursor records the sources that were down when it was cut, and a continuation does not ask them.
4. The remaining pages are merged by `(submitted_at, id)`. Each source's key becomes the last unit taken from it, or stays. The next cursor is present when any source had more, or returned a unit that was not taken.

The card asks every source. One `Some` wins. Two `Some`s are 500 naming both. Otherwise a 503 or a missing source is 503. Otherwise a 403 is 403 whose body names no gear. Otherwise the card is 404. A vote uses that same resolution and then calls `vote` on the owner. The caller sends `Idempotency-Key`; the inbox refuses the vote without it and never mints a key (AP-D-6).

### 3.7 Database schemas & tables

The gear has no tables and no migrations. The cursor is an opaque token in the response, not a row.

## 4. Additional context

Decisions AP-D-1 through AP-D-4 are in `DECISIONS.md`. A kind outside a gear's own set, and `book_id` on products, are an empty page decided in the source, not a 400 the facade rewrites. `book_id` on pricing remains that gear's alias of `ref_id`: prices of the book, and no plan revision.
