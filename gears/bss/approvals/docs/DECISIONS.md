# BSS Approvals — Decision Register

**Status:** the facade decisions for the inbox. The gears' own registers stay pricing D-470 and products P-D-227; their sources are pricing D-490 and products P-D-250.

<!-- toc -->

- [Register](#register)
- [Entries](#entries)
  - [AP-D-1 The inbox is a facade and has no authorization resource](#ap-d-1-the-inbox-is-a-facade-and-has-no-authorization-resource)
  - [AP-D-2 The merge, the cursor, the narrowing and book_id](#ap-d-2-the-merge-the-cursor-the-narrowing-and-book_id)
  - [AP-D-3 Grants and owner resolution](#ap-d-3-grants-and-owner-resolution)
  - [AP-D-4 Votes and idempotency](#ap-d-4-votes-and-idempotency)
  - [AP-D-5 A down source is omitted and the walk does not resume it](#ap-d-5-a-down-source-is-omitted-and-the-walk-does-not-resume-it)
  - [AP-D-6 The inbox requires the caller's Idempotency-Key](#ap-d-6-the-inbox-requires-the-callers-idempotency-key)
  - [AP-D-7 The inbox unit carries whether the caller may reject or withdraw it](#ap-d-7-the-inbox-unit-carries-whether-the-caller-may-reject-or-withdraw-it)
  - [AP-D-8 The inbox publishes `$orderby` through the toolkit](#ap-d-8-the-inbox-publishes-orderby-through-the-toolkit)
  - [AP-D-10 The list and the counts answer 304](#ap-d-10-the-list-and-the-counts-answer-304)
  - [AP-D-11 The inbox names its submitters and voters](#ap-d-11-the-inbox-names-its-submitters-and-voters)

<!-- /toc -->

## Register

| ID | Priority | Decision | Status / source |
| --- | --- | --- | --- |
| AP-D-1 | H | The inbox is a facade and has no authorization resource | DECIDED 2026-10-01 |
| AP-D-2 | H | The merge, the cursor, the narrowing and `book_id` | DECIDED 2026-10-01 · amended by Run 2 (pricing D-490, products P-D-250); amended by AP-D-5, AP-D-8; amended 2026-10-02 (source names, kind counts); extended by AP-D-10 |
| AP-D-3 | H | Grants and owner resolution | DECIDED 2026-10-01 · amended by AP-D-5 |
| AP-D-4 | H | Votes and idempotency | DECIDED 2026-10-01 · amended by Run 2 (pricing D-490, products P-D-250); amended by AP-D-6 |
| AP-D-5 | H | A down source is omitted and the walk does not resume it | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 60); amends AP-D-2, AP-D-3 |
| AP-D-6 | H | The inbox requires the caller's Idempotency-Key | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 61); amends AP-D-4 |
| AP-D-7 | M | The inbox unit carries whether the caller may reject or withdraw it | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 63); extended by AP-D-11 |
| AP-D-8 | M | The inbox publishes `$orderby` through the toolkit | DECIDED 2026-10-02 · amends AP-D-2 |
| AP-D-10 | M | The list and the counts answer 304 | DECIDED 2026-10-03 · Owner, 2026-10-03 (asks 56, 57); extends AP-D-2; amended 2026-10-04 (`no-cache`, not `no-store`) |
| AP-D-11 | M | The inbox names its submitters and voters | DECIDED 2026-10-03 · Owner, 2026-10-02 (ask 32: names on the server, through AM); extends AP-D-7, AP-D-10; amended 2026-10-03 (one lookup per card, declared system actors); amended 2026-10-04 (a resolved name is reused for five minutes per caller) |

## Entries

### AP-D-1 The inbox is a facade and has no authorization resource

The units stay in the gear that writes them. The inbox reads and routes. It does not implement a database capability and it does not call `db_required`.

The gateway authenticates the caller. The inbox does not judge a grant. Each source door authorizes with its own resource. The problem type on an inbox refusal names the error envelope, not an authorization resource.

### AP-D-2 The merge, the cursor, the narrowing and book_id

The shared order is the facade's merge key: `submitted_at` as an instant, then the unit id in the same direction. That amends the per-gear order only by being the key the inbox merges on. Each gear's own door is unchanged.

Every source is asked on every page after its own key. The key becomes the last unit taken from that source, or stays. There is no exhausted state. The next cursor is present when any source had more or returned a unit that was not taken.

The cursor carries its order outside the narrowing hash. `$orderby` with a cursor is 400 `ORDER_WITH_CURSOR`. A changed narrowing is 400 `FILTER_MISMATCH`.

**Amended by AP-D-8 (2026-10-02).** `$orderby` is published with `.with_odata_orderby`, so the served contract carries `x-odata-orderby` for `submitted_at asc` and `submitted_at desc`. The door still accepts only that field.

A kind outside a gear's closed set is an empty page and zero counts, computed in the source. `book_id` on products is that same empty set. `book_id` on pricing remains the alias of `ref_id`: it keeps `prices` units of that book and no `plan_revision`, whose reference is the revision. A state or id the door would refuse is that refusal for the whole read.

A source added to the configuration later starts from an empty key. A removed source's key is ignored.

AP-D-10 extends this entry: the list and the counts answer a weak `ETag` of their JSON and `304` on a matching `If-None-Match`.

Run 2 amends this entry (pricing D-490, products P-D-250). Each real source builds the pager's own `CursorV1` from its key and reads through its gear's list read, so the keyset is the pager's column compare. A kind that no gear records, such as `bogus`, is therefore an empty page and zero counts from every source: the inbox answers 200 with no unit and every readable source named `ok`, never 400.

**Amended 2026-10-02.** A configured source name is non-blank and unique. A duplicate or a blank name fails the boot, so a merge cannot see one source twice and a card cannot treat one gear as two owners. A source counts body names only the closed kind set. A kind field that is absent is 0. A kind field outside the set does not decode, so a renamed kind fails the read instead of being counted as zero.

### AP-D-3 Grants and owner resolution

On the list and the counts, a source that answers 403 is omitted and named `forbidden`. When every source answers 403, the read is 403 and the body names no gear. A source that answers 503, or is not registered, is 503 `SOURCE_UNAVAILABLE` naming it. `total` counts the readable sources only.

**Amended by AP-D-5 (2026-10-02).** A down or unregistered source is named `unavailable` and omitted. The read fails with 503 only when every configured source is down.

On the card and the vote, every source is asked. One hit wins. Two hits are 500 naming both. Otherwise any 503 or missing source is 503 `SOURCE_UNAVAILABLE`. Otherwise any 403, with the rest missing, is 403 whose body names no gear. Otherwise the unit is 404.

### AP-D-4 Votes and idempotency

The inbox resolves the owner with the card's rule, then calls that source's single `vote` with the request body bytes and the `Idempotency-Key` as received. The source calls its own vote door, under that door's grant, with that door's own path as the idempotency endpoint. The facade and the direct door therefore share one idempotency row.

The door's answer is returned unchanged: status, headers and body. The success body is the door's receipt. A refusal keeps the door's body, including `generation` on `GENERATION_MISMATCH` and `UNIT_STALE`.

Run 2 amends this entry (pricing D-490, products P-D-250). Each source sends the vote to its gear's vote door through that gear's router, under the gear's enforcer and the platform's error layer, with the body bytes as `application/json`. A refusal therefore leaves the door with its `instance` naming the door's path. The inbox marks its answer as a passthrough (`ForeignPassthrough`), so the platform's error layer around the inbox does not rewrite a refusal the door already shaped. A refusal's `trace_id` is the one the door's layer derived: the source forwards no trace header. A facade vote and a direct vote with the same key and body replay once in both gears, and the same key with another body is the door's 409 `IDEMPOTENCY_CONFLICT`, byte for byte.

Declared codes: 400 `GENERATION_REQUIRED`, `GENERATION_MISMATCH`, `UNIT_STALE`, `NOTE_REQUIRED`, `NOTE_TOO_LONG`, `BODY_UNEXPECTED`; 403 for the grant and `SOD_VIOLATION`; 404; 409 `DUPLICATE_VOTE`, `UNIT_ALREADY_DECIDED`, `IDEMPOTENCY_CONFLICT`; 503. These doors do not answer 412. `InboxUnit` has no version.

**Amended by AP-D-6 (2026-10-02).** The inbox no longer forwards a vote that arrived without `Idempotency-Key`.

### AP-D-5 A down source is omitted and the walk does not resume it

**Status:** DECIDED 2026-10-02.

The list and the counts answer the sources that answered. A source that answers 503, or is not registered, is named `unavailable` in `sources` and contributes nothing. `InboxSourceStatusDto` is `ok`, `forbidden` or `unavailable`. The counts carry `sources` with that status.

- When every configured source is unavailable, the read is 503 `SOURCE_UNAVAILABLE` naming them, as before.
- When every configured source is forbidden, the read is 403 and the body names no gear, as before.
- A mix of forbidden and unavailable, with no source that answered, is 200 with an empty page or zero counts and both statuses.
- The card and the vote are unchanged: one owner, and that owner's 503 stays 503.

The cursor records the sources that were unavailable when it was cut. A continuation does not ask those sources, and names them `unavailable`, even when they would answer. A source that goes down on a later page is recorded on that page's cursor and stays omitted after it. A client re-reads from the first page to include a source that was down. Newest-first holds because a source is never inserted into a walk that started without it.

The narrowing hash is the canonical JSON of `book_id`, `kind`, `ref_id` and `state`, so an absent value and an empty string do not share a hash. The cursor version is 2. A token cut before this deploy is 400 `INVALID_CURSOR`.

The sources of one list or one counts are asked concurrently. `sources` stays in configuration order.

**Source:** Owner, 2026-10-02 (ask 60, "все ок"). Amends AP-D-2 and AP-D-3.

### AP-D-6 The inbox requires the caller's Idempotency-Key

**Status:** DECIDED 2026-10-02.

Approve, reject and withdraw on the inbox require `Idempotency-Key`. The header is required in the served spec. A missing or empty key is 400 `IDEMPOTENCY_KEY_REQUIRED` on the field `Idempotency-Key`. A value that is not text is 400 `IDEMPOTENCY_KEY_INVALID` on that same field. The inbox forwards the key it received and never mints one. The products and pricing vote doors are unchanged: a direct call still follows that door's own rule.

A query the inbox cannot parse is 400 `INVALID_QUERY_PARAMS` on the field `query`, carrying the parser's text. It is not an `INVALID_FILTER`.

**Source:** Owner, 2026-10-02 (ask 61, "все ок"). Amends AP-D-4.

### AP-D-7 The inbox unit carries whether the caller may reject or withdraw it

**Status:** DECIDED 2026-10-02.

`InboxUnit` and `InboxUnitDto` carry `caller_can_reject` and `caller_can_withdraw` beside `caller_can_approve`. The owning gear judges them (products P-D-255, pricing D-497). The inbox copies the door's values and does not judge a grant of its own.

AP-D-11 extends this entry: the unit also carries `submitted_by_name`, and each decision `actor_name`, resolved by the inbox itself.

**Source:** Owner, 2026-10-02 (ask 63, "все ок"). Extended by AP-D-11.

### AP-D-8 The inbox publishes `$orderby` through the toolkit

**Status:** DECIDED 2026-10-02.

The list declares `$orderby` with `.with_odata_orderby::<InboxOrderField>()`. The served contract therefore carries `x-odata-orderby` for `submitted_at asc` and `submitted_at desc`. The door still accepts only `submitted_at`, ascending or descending, and refuses any other order with 400 `INVALID_ORDERBY_FIELD`. The query struct does not rename a field to `$orderby`.

**Source:** Phase 9 review fix (architecture lints DE0802 and DE0803). Amends AP-D-2.

### AP-D-10 The list and the counts answer 304

**Status:** DECIDED 2026-10-03.

- **The two reads.** `GET /bss-approvals/v1/approval-units` and `GET /bss-approvals/v1/approval-units/counts` answer a weak `ETag` of the JSON body they serve: `W/"` plus 22 base64url characters of its SHA-256. They also send `Cache-Control: private, no-cache`: the browser keeps the answer and must revalidate it.
- **The comparison.** `If-None-Match` matches that tag by weak comparison (RFC 9110), including `*` and a comma-separated list. A match is `304` with an empty body, the same `ETag` and the same `Cache-Control`. Only a `200` is turned into a `304`. A refusal, such as `403` when every source forbids the caller or `503 SOURCE_UNAVAILABLE`, passes through unchanged and carries no tag.
- **The tag is the caller's own body.** The body includes `sources`, so a source that goes from `ok` to `forbidden` or `unavailable` changes the tag even when no unit changed. The units are the ones the caller's sources answered, so a `304` never gives one caller the view of another caller.
- **What stays.** The card and the three votes are not conditional. The votes still return the owning gear's answer unchanged (AP-D-4).
- **The tests.** `api/rest/doors_tests.rs`: `the_list_and_the_counts_answer_304_until_a_source_status_changes`, `a_refused_list_is_not_conditional` and `the_list_and_the_counts_declare_the_conditional_get`.

AP-D-11 extends this entry: the list's body carries the names of its submitters and voters, so a renamed user changes the tag.

**Amended 2026-10-04 (branch review): `no-cache`, not `no-store`.** A `304` needs the copy the browser stored: the browser sends that copy's tag, and a match tells it to use the copy. `no-store` forbids the browser to keep any copy, so under it every read would be a full `200` and the tag would save nothing. These reads therefore keep `private, no-cache`, and do not follow the rule that asks `no-store` of an API answer with per-user data (RUST-SEC-002). The trade-off is accepted for these two reads only:

- **What may stay in the browser.** A per-user body may sit in the browser's private cache, on disk, after the session ends: an inbox page with the units this caller's sources answered, their notes, and the names of their submitters and voters (AP-D-11), and the counts.
- **What still holds.** `private` keeps every answer out of shared caches: a proxy or a CDN stores none of them. `no-cache` makes the browser ask the server before each use of its copy, so the caller is authenticated and every source is asked again as the caller. The tag is the caller's own body, so a `304` confirms only a copy that this caller would be served now, never another caller's view.
- **What stays.** The card and the votes keep the headers they had.

**Source:** Owner, 2026-10-03 (asks 56 and 57). Extends AP-D-2. The shared helper is `cf-gears-bss-rest` (pricing D-518, products P-D-261). Extended by AP-D-11. The 2026-10-04 amendment: the owner's answer to the branch review's question ("ok" to the recommendation).

### AP-D-11 The inbox names its submitters and voters

**Status:** DECIDED 2026-10-03.

The inbox showed `submitted_by` and each decision's `actor` as ids. A reviewer had no way to name the person without
an Account Management read per id of its own.

- **The fields.** `InboxUnitDto.submitted_by_name` and `InboxDecisionDto.actor_name`, a string or null, beside the
  ids, which stay. A products unit's `subject_live`, the live SKU, carries its creator's `created_by_name` too. The
  SDK models (`InboxUnit`, `InboxDecision`) are unchanged: the names are resolved in the REST layer.
- **The source.** `bss_rest::actor_names` reads AM's public user read, `list_users` with an id-set filter in the
  caller's own tenant, with the caller's own context. AM decides which profiles the caller may see; the inbox adds no
  permission. The label is the display name, then first and last name, then the username. AM is a soft dependency:
  the client is found in the client hub at each lookup, and the inbox declares no gear dependency on it.
- **One lookup per answer.** The list collects every actor id of the merged page, across its sources, and resolves
  them once; the card does the same for its unit. The ids are deduplicated and read in chunks of 200, at most four
  chunks at once, inside one 2 s budget.
- **Null.** A name is null when it is not available now: AM refused the profile to this caller, found no such user,
  failed, did not answer within the budget, or is not deployed. The read never fails because of AM.
- **System.** The nil id of the platform's system context reads `"System"` without a lookup. The owning gears refuse
  their own system actors at every door, so none submits or votes on a unit the inbox lists.
- **Votes.** A vote returns the owning gear's answer unchanged (AP-D-4), and a gear's write answer names nobody
  (pricing D-519, products P-D-262).
- **Caching.** The names are part of the list's body, so its weak `ETag` covers them (AP-D-10): a rename changes the
  tag once the name is read again. The card is not conditional. Nothing is stored. Amended 2026-10-04: the resolver (`cf-gears-bss-rest` `actor_names`, through `ActorNames::from_hub`) keeps a resolved name in process memory for five minutes, for the caller AM gave it to and nobody else: the key is the caller's tenant and subject and the actor. A refused, absent or failed lookup is not kept, and nothing is kept for an anonymous caller. The cache holds at most 10,000 names. A list or card that a caller reopens within five minutes asks AM nothing.
- **The tests.** `api/rest/doors_tests.rs`: `the_inbox_names_its_submitters_and_voters_in_one_lookup`,
  `an_unavailable_directory_leaves_the_names_null_on_a_200` and `a_renamed_submitter_changes_the_list_tag`.

**Amended 2026-10-03 (branch review).** The inbox is the only one that names a unit it serves.

- **One lookup per card.** A source answers its card unnamed, so one card read makes one Account Management lookup,
  not one in the source and one here.
- **The live subject.** Each actor of a live subject that carries a `*_name` key beside it is named: a products SKU's
  `created_by`, and its `archived_by` while it is archived (products P-D-263). A subject without the key gains none.
- **Declared system actors.** `ApprovalSourceV1` gains `system_actors()`, none by default: the actors the source's
  gear records that are not people. The list and the card read the declared actors of every configured source that is
  registered, and name them `"System"` without a lookup, beside the nil id. Products declares the nil id and pricing's
  system actor (products P-D-262), so the inbox and products name them alike. The SDK adds no dependency on any gear.
- **The tests.** `api/rest/doors_tests.rs`: `a_system_actor_a_source_declares_reads_system_and_is_never_asked`, and
  the archiver in `the_inbox_names_its_submitters_and_voters_in_one_lookup`. Products:
  `actor_names_tests::an_inbox_card_read_makes_one_lookup`.

**Source:** Owner, 2026-10-02 (ask 32: "there was already code that resolves the names through AM; do it that way on
the server"). Twin of pricing D-519 and products P-D-262. Extends AP-D-7 and AP-D-10. Amended by the branch review,
2026-10-03.
