# Pricing — Decision Register (PriceBook)

**Replaces:** the charge-line/market-price register on `bss/products-backup` (`3a38f0b28`), ending at D-383.
Historical decisions remain there and in git history; only living rules restated below govern the new model.
**Authority:** `docs/superpowers/specs/2026-09-24-pricebook-model-design.md`, especially §2, §2.2 and §13,
then this register, then code, then descriptive prose. D-399 is the explicit phase-plan deviation; D-403 overrides
the spec's 422 wording; D-407 (items as a sub-resource) and D-413 (a copied reference attaches after its write) are
the phase 3 plan's deviations; the owner defers promotions (D-409), migration requests and retirement (D-410),
and the sold-as bundle and grants (D-411), and drops quote and the Studio wiring (D-415).
**Status:** model decisions accepted; implementation remains unchecked in the FEATUREs.

<!-- toc -->

- [Register](#register)
- [Entries](#entries)

<!-- /toc -->

## Register

| ID | Priority | Decision | Status / source |
| --- | --- | --- | --- |
| D-384 | H | Books own currency and validity | DECIDED 2026-09-25 · §2 decision 5; §5; §8 |
| D-385 | H | One registered dimension, independent value chains | DECIDED 2026-09-25 · §2 decision 4; §5; §14 |
| D-386 | H | SKU type defines the entry key and allowed models | DECIDED 2026-09-25 · §2 decision 5; §5; amended by D-427 |
| D-387 | H | Tier bands are half-open | DECIDED 2026-09-25 · §5 tier bands; §10 |
| D-388 | H | Minimum fee is per price per subscription per period | DECIDED 2026-09-25 · §2 decision 13; §5; amended by D-467 |
| D-389 | H | Descriptors bind from durable SKU versions | DECIDED 2026-09-25 · §2 decision 14; §2.2; §7.1 |
| D-390 | H | Windows normalize per chain and preserve usage structure | DECIDED 2026-09-25 · §2 decision 16; §5; amended by D-520, D-521 |
| D-391 | H | Temporary changes keep pair identity or resume fallback | DECIDED 2026-09-25 · §5 temporary pairs; amended by D-443 |
| D-392 | H | Publish changes is a selected book batch | DECIDED 2026-09-25 · §2 decision 7; §6; §8 |
| D-393 | H | One unit engine, quorum and generations | DECIDED 2026-09-25 · §2 decision 8; §2.2; §6; extended by D-459; amended by D-520, D-521 |
| D-394 | H | Plans are versioned structure bound to one book | DECIDED 2026-09-25 · §5 plans; §6; §8; amended by D-450, D-467 |
| D-395 | H | Promotions are versioned and migrations are requests | DECIDED 2026-09-25 · §5; §6; §11 phase 3 |
| D-396 | H | One replay store and optimistic conditional writes | DECIDED 2026-09-25 · §2.2; §3 items 23 and 27; §7.2 |
| D-397 | H | Consumer pins replace cohorts and catalog versions | DECIDED 2026-09-25 · §2 decision 6; §7.1; §12 |
| D-398 | H | Reference reservation closes the lifecycle race | DECIDED 2026-09-25 · §2 decision 17; §13 |
| D-399 | H | No SkuChanged listener or local SKU cache in phase 2 | DECIDED 2026-09-25 · Phase 2 plan, Global Constraints; deviation from spec §7.3 and §11 |
| D-400 | H | Toolkit outbox and broker TypedEvent own event delivery | DECIDED 2026-09-25 · Phase 2 plan Task 2a.2 and 2c.8; Products phase 1 pattern |
| D-401 | H | Reference work is a durable op written before reserve | DECIDED 2026-09-25 · Phase 2 plan 2c.1/2c.6; plan review findings 2, 3; amended 2026-10-02 |
| D-402 | H | The pair guard compares SKU metering as of each price's start | DECIDED 2026-09-25 · Phase 2 reconciliation matrix row 24; spec §5 pair guard; supersession-continuity family |
| D-403 | M | Validation refusals are 400 with a code; no wire 422 | DECIDED 2026-09-25 · toolkit canonical error mapping; Products phase 1 behaviour; deviation from spec §2 decision 16 and §6 |
| D-404 | H | A draft belongs to its author | DECIDED 2026-09-25 · Phase 2 review (chains MEDIUM-1, docs F2); spec §6 "author ≠ approver" (finding 8) |
| D-405 | M | Publish changes completes a selected pair | DECIDED 2026-09-25 · Phase 2 plan and reconciliation row 23; Phase 2 review (docs F4) |
| D-406 | H | A temporary window is not crossed | DECIDED 2026-09-25 · Phase 2 second review (behaviour MEDIUM-2) |
| D-407 | H | Plan items are reserved references; items are a sub-resource | DECIDED 2026-09-25 · Phase 3 plan rev 2; plan review HIGH 2; owner, 2026-09-25 (kind plan_item only); deviation from spec §7.2; amended by D-467, D-512 |
| D-408 | H | Plan checks read every SKU fresh; descriptors are information, never content | DECIDED 2026-09-25 · Phase 3 plan rev 2; plan review MEDIUM 6; phase 2 "owed to phase 3"; amended by D-453, D-465, D-482, D-516, D-522; extended by D-466 |
| D-409 | H | Promotions are deferred (owner, 2026-09-25) | DECIDED 2026-09-25 · Owner, 2026-09-25, during Run 3.1; Phase 3 plan rev 3; spec §2.4 |
| D-410 | H | Migration requests and plan retirement are deferred (owner, 2026-09-25) | DECIDED 2026-09-25 · Owner, 2026-09-25, during Run 3.1; spec §11 phase 3 |
| D-411 | H | The sold-as bundle and plan grants are deferred (owner, 2026-09-25) | DECIDED 2026-09-25 · Owner, 2026-09-25, during Run 3.1; spec §5 plan_revision |
| D-412 | M | The phase 2 schema is edited in place until the first deployment | DECIDED 2026-09-25 · Phase 3 plan rev 2; plan review LOW 15; closed by D-427 |
| D-413 | H | A copied item attaches its reference after the write | DECIDED 2026-09-25 · Phase 3 plan rev 2; plan review HIGH 1; deviation from D-401's reserve before write; amended by D-451, D-467, D-512 |
| D-414 | H | A revision's references outlive it | DECIDED 2026-09-25 · Phase 3 plan rev 2; plan review LOW 14 |
| D-415 | H | Quote is not built; the Studio is not wired to the API (owner, 2026-09-25) | DECIDED 2026-09-25 · Owner, 2026-09-25, during Run 3.1; deviation from spec §7.1 and §11 phase 4 |
| D-416 | H | Descriptors are read best-effort; the reads a rule needs stay hard | DECIDED 2026-09-26 · Phase 3 review, fix run 7 (plans F2, surface S-1, docs F1 and F2) |
| D-417 | M | The last revision of a never-published plan takes the plan with it | DECIDED 2026-09-26 · Phase 3 review, fix run 7 (plans F1, surface S-2) |
| D-418 | H | A plan revision is submitted under plan:submit | DECIDED 2026-09-26 · Phase 3 review, fix run 7 (surface S-4); corrects the run 3.4 brief |
| D-419 | H | Resolve answers one revision on one date, with the caller's pins | DECIDED 2026-09-26 · Phase 4 plan rev 3 (Run 4.2); spec §7.1; plan review M4 b, L4, L7; amended by D-454, D-467 |
| D-420 | H | The matrix and the walk: a binding is always in force | DECIDED 2026-09-26 · Phase 4 plan rev 3 (Run 4.2); owner, 2026-09-26 (no promotion rule; rule 4 as recommended); spec §2.4, §5, §7.1; plan review H2, M4, L1; amended by D-467, D-512 |
| D-421 | H | The binding carries resolved invoice inputs with their source | DECIDED 2026-09-26 · Phase 4 plan rev 3 (Run 4.2); PRD AC #13; plan review H3, L7; amended by D-467 |
| D-422 | H | The pinned price read serves approved money forever | DECIDED 2026-09-26 · Phase 4 plan rev 3 (Run 4.2); spec §7.1; plan review H4; amended by D-520 |
| D-423 | H | Both gears refuse a legacy or stale schema at boot | DECIDED 2026-09-26 · Phase 4 plan rev 2 (Run 4.1); plan review H1, M1, M2, L6 |
| D-424 | H | Resolve reads SKU versions as pricing's system actor | DECIDED 2026-09-26 · Phase 4 review, fix run 8 (docs M1); amends D-421; amended 2026-10-02 (phase 9 review F1) |
| D-425 | H | The binding says where it ends for its holder | DECIDED 2026-09-26 · Phase 4 review, fix run 8 (contract C-1, docs M2); amends D-420 |
| D-426 | H | An entry's invoice line is locked once the entry carries money | DECIDED 2026-09-26 · Owner, 2026-09-26 (option 1 of three); amends D-421 |
| D-427 | H | The model belongs to the entry, fixed for its life, and is part of its key | DECIDED 2026-09-26 · Owner, 2026-09-26; phase 5 plan rev 2; closes D-412; amends D-386, D-390, D-391, D-401, D-402 |
| D-428 | H | Entries and SKUs report their usage | DECIDED 2026-09-26 · Owner, 2026-09-26; phase 5 plan rev 2; amended by D-440, D-453 |
| D-429 | M | The replay store's mechanics (twin of products P-D-198) | DECIDED 2026-09-27 · Carried from D-142 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27 |
| D-430 | M | A tier ladder's top band is open | DECIDED 2026-09-27 · Carried from D-17 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27 |
| D-431 | M | The request's correlation id is minted at the authoring edge | DECIDED 2026-09-27 · Carried from D-178 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27; amended 2026-10-02 (phase 9 review F1b) |
| D-432 | M | If-Match on every write to a versioned row, and on a draft price's DELETE | DECIDED 2026-09-27 · Carried from D-141 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27; extends D-396 |
| D-433 | M | The audit log is append-only with a reserved sealing seam (twin of products P-D-200) | DECIDED 2026-09-27 · Carried from P-D-08, P-D-28, P-D-46, P-D-118 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27 |
| D-434 | M | Where a SKU is priced and sold: its entries across books, the plans that name it, one plan item | DECIDED 2026-09-27 · Owner, 2026-09-27; phase 6 plan rev 2; amended by D-440, D-453, D-460, D-461, D-472, D-483, D-484, D-485, D-486 |
| D-435 | M | An approval-policy override can be reset; the default cannot be deleted (twin of products P-D-216) | DECIDED 2026-09-27 · Owner, 2026-09-27; phase 6 plan rev 2 |
| D-436 | M | Dimension values edit one at a time and show their use | DECIDED 2026-09-27 · Owner, 2026-09-27; phase 6 plan rev 2 |
| D-437 | M | The default rounding is one of five modes; a tenant with no settings rounds half_even | DECIDED 2026-09-27 · Owner, 2026-09-27; phase 6 plan rev 2; the half_even default, Owner, 2026-09-28 |
| D-438 | M | The settings offer currencies and say who changed them | DECIDED 2026-09-27 · Owner, 2026-09-27; phase 6 plan rev 2; extended by D-519 |
| D-439 | M | Closed sets are enums on the responses; requests keep strings and their codes (twin of products P-D-217) | DECIDED 2026-09-27 · Owner, 2026-09-27; phase 6 plan rev 2; amended by D-467 and the phase 9 review (C, fix run 9.5d-1) |
| D-440 | M | An entry's prices, its price in force and its approved prices by date | DECIDED 2026-09-28 · Owner, 2026-09-28; phase 7 plan rev 2; amends D-428, D-434; extended by D-456; amended by D-472, D-473 |
| D-441 | M | Every book read carries its stats | DECIDED 2026-09-28 · Owner, 2026-09-28; phase 7 plan rev 2; amended by D-453 |
| D-442 | M | The book list pages on the toolkit's OData pager, searched by q and sku_id | DECIDED 2026-09-28 · Owner, 2026-09-28; phase 7 plan rev 2; extended by D-522 |
| D-443 | M | A temporary draft's dates move, and its pair follows | DECIDED 2026-09-28 · Owner, 2026-09-28; phase 7 plan rev 2; amends D-391 |
| D-444 | M | A book has a description, and an unused book can be deleted | DECIDED 2026-09-28 · Owner, 2026-09-28; phase 7 plan rev 2; amended by D-453, D-522 |
| D-445 | L | An approval unit carries its submitter's note (twin of products P-D-219) | DECIDED 2026-09-28 · Owner, 2026-09-28; phase 7 plan rev 2; amended by D-464 |
| D-446 | M | A plan revision can be stored scheduled: the state, its index and its migration | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 |
| D-447 | M | A scheduled revision takes effect on its date: the effective state is derived | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 |
| D-448 | M | The storage writes of a scheduled revision: schedule, switch, unschedule and the due scan | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 |
| D-449 | M | An approval before the sale date schedules the revision | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 (run 8.2) |
| D-450 | M | The switch job persists a due switch on its date and announces it once | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 (run 8.2); plan review H1, M4, L2, L7 |
| D-451 | M | The copy, clone and unschedule doors catch a due switch up; one scheduled revision at a time | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 (run 8.2); plan review M1; amended by D-463 |
| D-452 | M | A scheduled revision can be withdrawn to a draft | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 (run 8.2); plan review M5 |
| D-453 | M | Every read derives the effective state; the counts read the stored state | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 (run 8.2); plan review M2, L5; amended by D-460, D-461, D-484, D-485 |
| D-454 | M | Resolve serves a scheduled revision from its sale date | DECIDED 2026-09-29 · Owner, 2026-09-28; phase 8 plan rev 2 (run 8.2); plan review M3 |
| D-455 | M | The outbox wakes its sequencer after the commit | DECIDED 2026-09-29 · Main sync of 2026-09-29 (toolkit-db 2bfc76aec); phase 8 plan rev 2 (run 8.2b) |
| D-456 | M | A plan names only a book its author may read | DECIDED 2026-09-29 · Whole-branch review PS-08 (fix run W1a); extends D-440; extended by D-463, D-468 |
| D-457 | M | Every text a request writes has an explicit length cap | DECIDED 2026-09-29 · Whole-branch review PS-09, PS-10, X-01 (fix run W1a); twin of a products decision in W1b; extended by D-468; amended 2026-10-02 (phase 9 review F1) |
| D-458 | M | The approval-unit list pages and reads its page set-based | DECIDED 2026-09-29 · Owner, 2026-09-29 (dispositions O2, "2"); whole-branch review PS-13 (fix run W1a); amended by D-470 |
| D-459 | M | One approve-eligibility predicate for the engine and its readers | DECIDED 2026-09-30 · Phase 9 plan rev 2 (W2, binding; plan review W2, L5); extends D-393; amended by the phase 9 review (E, fix run 9.5d-1) |
| D-460 | M | The plans list names each plan's current revision and the one in effect | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 1); phase 9 plan rev 2 (decision 1; plan review M6, M7, W3, L4); amends D-434, D-453; amended by D-461, D-480, D-482, D-485, D-516; extended by D-519 |
| D-461 | M | A revision says who made it and when it was submitted and approved | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 7); phase 9 plan rev 2 (decision 2; plan review M6, M7, L1, L9); amends D-434, D-453, D-460; extended by D-519 |
| D-462 | M | A pending revision shows its vote progress under plan read | DECIDED 2026-09-30 · Owner, 2026-09-30 (O-9a, "yes"); phase 9 plan rev 2 (decision 4; plan review M7, L5); amended by the phase 9 review (E, fix run 9.5d-1) |
| D-463 | M | A plan's sale date on create and clone | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 7); phase 9 plan rev 2 (decision 3; plan review L10); amends D-451, extends D-456 |
| D-464 | L | A plan submit and a publish-changes carry the submitter's note | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 6); phase 9 plan rev 2 (decision 5; plan review L3); amends D-445; amended by the phase 9 review (fix run 9.5d-2) |
| D-465 | M | A revision may carry again a deprecated SKU its plan sells | DECIDED 2026-09-30 · Owner, 2026-09-30 (O-9b, "yes"); phase 9 plan rev 2 (decision 6; plan review L2); amends D-408 |
| D-466 | M | Each check row names its items and its blocking prices | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 5); phase 9 plan rev 2 (decision 7); extends D-408; amended by the phase 9 review (fix run 9.5d-2) |
| D-467 | H | A plan item is a SKU and its entry: no treatment, no included quantity, no minimum quantity | DECIDED 2026-09-30 · Owner, 2026-09-30 (the included quantity, then the treatment, then qty_min removed); phase 9 plan rev 2 (run 9.2); amends D-388, D-394, D-407, D-413, D-419, D-420, D-421, D-439; amended by the phase 9 review (G, fix run 9.5d-1); amended by D-512 |
| D-468 | M | A new plan's code follows a declared rule | DECIDED 2026-09-30 · Owner, 2026-09-30 (ask 39, "do it"); phase 9 run 9.2 scope addition; extends D-456, D-457 |
| D-469 | M | The served contract declares every door's 503, every ETag it sets and the refusals of the plan doors | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 2); phase 9 plan rev 2 (M1 and W1, binding; decisions 8 and 9; L8); extended by D-470; amended by the phase 9 review (fix run 9.5d-2); amended by D-512; extended by D-518 |
| D-470 | M | The approval units are counted by state and kind, list newest first on request, and skip the live impact on request | DECIDED 2026-09-30 · Owner, 2026-09-30 (the approvals option 1, "ok"); phase 9 plan rev 2 (decision 10; plan review M3, M4, L11); amends D-458, extends D-469; amended by the phase 9 review (C, R32; fix run 9.5d-1; I, fix run 9.5d-2); amended by D-490 |
| D-471 | M | A unit says whether its reader may approve it | DECIDED 2026-09-30 · Owner, 2026-09-30 (validation 3 item 4, "ok"); phase 9 plan rev 2 (decision 11; W2; plan review M2); amended by the phase 9 review (E, fix run 9.5d-1) |
| D-472 | M | An entry names its next price | DECIDED 2026-10-01 · Owner, 2026-09-30 (validation 3 item 7, "ok"); phase 9 plan rev 2 (decision 12; plan review M5, L7); amends D-434, D-440; amended by D-473 |
| D-473 | M | The book's entries list reads its prices on a date | DECIDED 2026-10-01 · Owner, 2026-09-30 (validation 3 item 7, "ok"); phase 9 plan rev 2 (decision 13; plan review M5, L6); amends D-440, D-472; amended by the phase 9 review R1 (fix run 9.5d-1), D-483 |
| D-480 | M | A revision read carries its entries, sale-date prices and reservation state | DECIDED 2026-10-01 · phase 9 plan rev 4 (run 9.6, asks 48, 49, 50); amends D-442, D-460, D-462 |
| D-481 | M | The quorum a submit needs is on the checks and on an effective-policy read | DECIDED 2026-10-01 · phase 9 plan rev 4 (run 9.6, asks 54, 55); amends D-435, D-462; amended 2026-10-02 (phase 9 review F1) |
| D-482 | M | The checks read their context as a set, and many revisions in one read | DECIDED 2026-10-01 · phase 9 plan rev 4 (run 9.7, ask 47); amends D-408, D-460 |
| D-483 | M | A book's entries page on the toolkit's pager, in the order (sku_id, charge_kind, model, id) | DECIDED 2026-10-01 · Owner, 2026-10-01 (asks v4, 51); phase 9 plan rev 4 (run 9.8; review H3, A4); amends D-434, D-473 |
| D-484 | M | A plan stores time-stable list facts; selling and change are derived from the day | DECIDED 2026-10-02 · Owner, 2026-10-01 (#31, form B); phase 9 plan rev 4 (run 9.8b; review N1, N2); amends D-453 |
| D-485 | M | The plans list pages on the stored summary and counts the derived axes | DECIDED 2026-10-02 · Owner, 2026-10-01 (#31, form B); phase 9 plan rev 4 (run 9.8b; review N1, N7); amends D-434, D-460, D-453; amended 2026-10-02 (phase 9 review F1b); amended by D-515 |
| D-486 | M | A SKU's entries narrow, order and page in memory | DECIDED 2026-10-01 · Owner, 2026-10-01 (#25, form A); phase 9 plan rev 4 (run 9.8c; N5); amends D-434; amended by D-517 |
| D-490 | M | Pricing's approval units answer the approvals inbox through pricing's own doors | DECIDED 2026-10-01 · Owner, 2026-10-01 (option A, "yes, A, agreed", then "write the plan"; Run 2 started before 9.5d-2); approvals inbox plan rev 2 (Run 2; design 1 and 3; plan review H1, H3, H4, M1, M5, L1); amends D-470; amended by D-496 |
| D-491 | M | An entry op stores its policy reference as a named object | DECIDED 2026-10-02 · phase 9 review F1b |
| D-496 | M | The inbox source judges `state` before a foreign empty page | DECIDED 2026-10-02 · phase 9 review; amends D-490 |
| D-497 | M | A unit says whether its reader may reject or withdraw it, and approve includes the grant | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 63); amends D-471 |
| D-501 | H | Authorized SDK reads share the frozen preview snapshot and canonical JSON digests | DECIDED 2026-09-30 · Pricing Seam Contracts Task 1; amended 2026-10-02 (phase 9 review F1) |
| D-502 | H | Immutable usage policies belong to entries and their semantic key | DECIDED 2026-10-01 · Pricing Seam Contracts Task 2; amends D-386, D-401, D-427; amended by D-513, D-514 |
| D-503 | H | Exact meter evidence gates usage publication and stays out of historical reads | DECIDED 2026-10-01 · Pricing Seam Contracts Task 3; amended 2026-10-01 by the owner (E1a raw and E1b derived meters; products P-D-229 and rating T-D-39 on branch `bss/pricebook-meters`); amended 2026-10-01 by products P-D-233 (E1b provided by Products, E1a still external); amended by D-514 |
| D-504 | H | Pure new-sale terms validate a bounded commercial profile and snapshot integrity | DECIDED 2026-10-01 · Pricing Seam Contracts Task 4; amended 2026-10-02 (phase 9 review F1b); amended by D-514 |
| D-505 | H | Durable commercial receipt storage | DECIDED 2026-10-01 · Pricing Seam Contracts Task 5a |
| D-506 | H | Authorized commercial provider boundary | DECIDED 2026-10-01 · Pricing Seam Contracts Task 5b |
| D-507 | H | Atomic acceptance and durable authenticated command replay | DECIDED 2026-10-01 · Pricing Seam Contracts Task 5c |
| D-508 | H | Frozen first holds and fresh original-binding eligibility | DECIDED 2026-10-01 · Pricing Seam Contracts Task 6 |
| D-509 | H | Executable Pricing seam fixtures and transport boundary | DECIDED 2026-10-01 · Pricing Seam Contracts Task 7 |
| D-510 | H | Database parity and provider handoff | DECIDED 2026-10-01 · Pricing Seam Contracts Task 8; E1 restated as E1a and E1b per the D-503 amendment; E1b delivered by products P-D-233 |
| D-511 | H | Commercial commands enforce scoped prices and nonempty activation windows | DECIDED 2026-10-01 · Pricing Seam Contracts review fix run |
| D-512 | H | A plan item may wait for its entry in a draft | DECIDED 2026-10-02 · Owner, 2026-10-02; amends D-407, D-413, D-420, D-467, D-469 |
| D-513 | M | A usage policy's single-valued fields default on input | DECIDED 2026-10-02 · Owner, 2026-10-02; amends D-502; amended by D-514 |
| D-514 | H | A usage rating policy is its rating rules and the entry stores the SKU revision | DECIDED 2026-10-02 · Owner, 2026-10-02; amends D-502, D-503, D-504, D-513 |
| D-515 | M | A plan row's book carries its id and validity | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 67); amends D-485 |
| D-516 | M | A named book carries its identity beside its id | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 54); amends D-460, D-408 |
| D-517 | M | Entries can be read by id | DECIDED 2026-10-02 · Owner, 2026-10-02 (ask 68); amends D-486; amended 2026-10-03 (the filter's length and its declaration) |
| D-518 | M | The plan list, the plan counts, the book list and the settings answer 304 | DECIDED 2026-10-03 · Owner, 2026-10-03 (asks 56, 57); extends D-469; amended 2026-10-04 (`no-cache`, not `no-store`) |
| D-519 | M | Every actor id a read shows carries its current name | DECIDED 2026-10-03 · Owner, 2026-10-02 (ask 32: names on the server, through AM); extends D-438, D-460, D-461 |
| D-520 | H | A scheduled price is cancelled through the prices unit | DECIDED 2026-10-03 · Owner, 2026-10-03; asks 19, 58a; amends D-390, D-393, D-422; amended 2026-10-03 (the event, the reads, the binding guard, the pairing CHECKs); amended 2026-10-04 (the pinned price's one-value status; the binding guard's scan noted as deferred) |
| D-521 | H | A live price is ended through the prices unit | DECIDED 2026-10-03 · Owner, 2026-10-03; ask 58a; amends D-390, D-393; amended 2026-10-03 (the event) |
| D-522 | H | A finished book can be archived, and archiving it releases its entries' SKU references (twin of products P-D-263) | DECIDED 2026-10-03 · Owner, 2026-10-03 ("archived"; ask 58b); amends D-408, D-444; extends D-407, D-442; amended 2026-10-03 (the submit's 409, the door's drive, the unarchive's answer, the op's reason, the mark's pairing); amended 2026-10-04 (the unarchive waits for open reference work; the plan revisions' references noted as deferred) |

## Entries

#### D-384 [H] Books own currency and validity

One currency book owns its SKU × charge kind × period entries. Book code is unique within the tenant. Revisions select a book; every plan reading it shares its money. A plan-specific exception uses another book or SKU, not variant. Book export is a single read-only JSON GET (decision 16). See ADR-0001.

**Source:** §2 decision 5; §5; §8.

#### D-385 [H] One registered dimension, independent value chains

A tenant-level dimension_key registry, initially region, supplies values. Each entry selects at most one key; null dim_value is the default chain. Resolve chooses an in-force value price before default. A default is optional and coverage is judged per value. A value with prices cannot be removed; dimension_key changes only while no price has a value. See ADR-0002.

**Source:** §2 decision 4; §5; §14.

#### D-386 [H] SKU type defines the entry key and allowed models

The book key is (sku_id, charge_kind, normalized period, model); model joined it with D-427. There is no plan, phase, variant, cohort, region or currency axis within that key. Recurring uses month/year and flat or per_unit; one_time has no period and flat or per_unit; usage has no period and per_unit, graduated, volume or package. The model is the entry's, chosen at its create and fixed for its life (D-427); a model the charge kind does not allow is MODEL_KIND_CHARGEKIND_MISMATCH. Bundle SKUs cannot be priced. A charge kind that does not match the SKU type is CHARGE_KIND_SKU_TYPE.

**Source:** §2 decision 5; §5.

#### D-387 [H] Tier bands are half-open

All tier bands use [from, to). Quantity 1000 belongs to the band beginning at 1000, including volume cliffs. Correct the prototype comparison qty <= upTo when porting; the surviving tier-boundary goldens are the arithmetic oracle. Do not copy the prototype boundary bug.

**Source:** §5 tier bands; §10.

#### D-388 [H] Minimum fee is per price per subscription per period

Aggregate every bound value and slice rated by the same price, deduct included quantities, then apply its floor prorated by the fraction of the period covered, before promotions. Two values sharing a default price share one floor. Separate valued prices carry separate floors. No plan cap or plan minimum survives. Pricing stores and validates min_fee; Rating applies the floor (D-415).

D-467 amends this entry: no plan carries an included quantity any more, so no quantity is deducted before the floor; the floor and proration stay Rating's.

**Source:** §2 decision 13; §5. Amended by D-467.

#### D-389 [H] Descriptors bind from durable SKU versions

Prices do not freeze GL, tax, invoice descriptors, metering or timing. Consumers bind the SKU version in force at the period start via versions?as_of; equal effective dates choose the highest published_version. An earlier pin keeps its descriptors forever. A descriptor change creates no refreeze price or pricing approval unit.

**Source:** §2 decision 14; §2.2; §7.1.

#### D-390 [H] Windows normalize per chain and preserve usage structure

On approval, sort approved prices within each (price_book_entry_id, dim_value), set predecessor effective_to to successor effective_from, and enforce one approved start per chain. The default tail stays open; a value tail may explicitly end and resume default fallback. Re-read chains transactionally, with serializable Postgres isolation. Usage successors cannot change package size or SKU metering as of each price's start (D-402): CHAIN_MODEL_CHANGED at submit, revalidated at apply. The model cannot change on a chain at all: it is the entry's (D-427).

**Amended by D-520 and D-521.** A chain is its `set` rows: a `cancel` or `end` row is never in it, and only a `set` row takes an approved start. A cancelled price leaves the chain, so the end of the price before it is recomputed onto the next start that remains, or left open. An end written by D-521 is explicit: it is kept, and a successor that starts inside it still closes it at that start.

**Source:** §2 decision 16; §5. Amended by D-520 and D-521.

#### D-391 [H] Temporary changes keep pair identity or resume fallback

On an existing chain, temporary_until creates a promo price and a return price in one unit. The return copies the money versionAt would apply at the end and inherits dim_value. Common-date shifts preserve duration; a shift that would carry a temporary price across the start of another price of its chain, where apply would cut it short, is refused TEMPORARY_SPANS_A_CHANGE (D-406). A return to an explicitly closed price (a temporary nested in a value's closed price) ends explicitly at that price's end, which a common-date shift does not move, so the value falls back to the default after it. Submit and apply re-derive that copy from the approved chain as it stands, on the shifted end: a return that no longer names the price in force there, or no longer carries its price and min_fee (the model is the entry's, D-427), and a single closed price whose own chain now has a price in force on its end, are refused PAIR_RETURN_STALE (400 at submit, APPLY_REFUSED at apply); the author re-drafts. When the price in force on that end is itself a price of the same unit, the return is right only if that price is another pair's return naming the same restored price with the same price and min_fee (two pairs on one chain, both drafted against it); any other price of the unit there makes it stale. A value with no own chain gets one closed price and no synthetic return copy of default. A temporary that ends exactly where the chain's next approved price starts is the promo price alone: that price already ends it. Pair edits, selection and submission cannot orphan a companion.

Amended by D-443: the temporary half of a draft takes new dates, and the PATCH builds its pair again over them in the same transaction — the return re-derived in place, deleted or created as the new end calls for — so a moved pair is never left for submit to refuse as stale.

**Source:** §5 temporary pairs.

#### D-392 [H] Publish changes is a selected book batch

List all draft book prices with full predecessor, proposed content and impact, pre-select all, then submit the operator-selected atomic prices and optional common_effective_date as one prices unit; a ticked half of a temporary pair brings its partner (D-405). Book money and plan structure remain independently approved. A rejected revision cannot undo an approved repricing that also affects the old revision.

**Source:** §2 decision 7; §6; §8.

#### D-393 [H] One unit engine, quorum and generations

bss-approval owns the shared engine shape; pricing owns prefixed tables and subjects. Quorum is tenant policy with kind overrides and fail-safe one when * is absent. No materiality. Submitter and all item authors are excluded from approval; an item's author is its price's creator, the only principal who may edit it (D-404). Votes carry generation; drift commits refreshed items/snapshot/hash, increments generation and marks prior decisions stale (UNIT_STALE). Conditional unit version yields UNIT_CONTENDED on lost races. Quorum zero, reject and withdraw all write terminal audit and ApprovalUnitDecided. See ADR-0003.

**Amended by D-520 and D-521.** A prices unit's item is still a price row, and its `change_kind` says what it asks: `set` (a price, as before), `cancel` (cancel the approved price `target_price_id` names) or `end` (end that price at the row's `effective_to`). A unit may mix them, and one unit names a price once. Every kind is submitted, withdrawn, rejected and approved the same way, under the same separation of duties, quorum and generations. Withdraw and reject leave the named price untouched; apply cancels or ends it and approves the row as its record.

D-459 extends this entry: the engine's approve rules (the terminal state, the separation of duties, the duplicate vote and which votes count) are one exported function, approve_eligibility. The engine judges a vote through it, and so does every read that shows a unit's vote count or whether its caller may approve.

**Source:** §2 decision 8; §2.2; §6. Extended by D-459.

#### D-394 [H] Plans are versioned structure bound to one book

Phase 3 publishes immutable revisions containing one book, paid/optional/included items, minimal Grants and optional sold-as bundle. Validate recurring frequency, meter uniqueness, usage-only included quantities, SKU lifecycle, foreign-book entries, book validity and coverage per value. blocked_by is computed from pending price units. Clone makes a draft; publishing a revision never moves existing pins, and neither does the switch of a scheduled revision on its date (D-450). Grants and the sold-as bundle are deferred (D-411), and so is retirement (D-410).

D-467 amends this entry: a revision's items are SKUs, each with its entry in the plan's book; there are no paid, optional or included treatments and no included or minimum quantity, so neither "usage-only included quantities" nor any check keyed on a treatment remains.

**Source:** §5 plans; §6; §8. Amended by D-450, D-467.

#### D-395 [H] Promotions are versioned and migrations are requests

Phase 3 forbids overlapping plan promotions; [from_date, to_date) applies by period start. Approved edits increment promotion version and bindings pin id/version. An approved migration to a published revision persists its preview and emits SubscriptionMigrationRequested; Subscriptions executes movement and confirms retirement in its separate plan. Pricing never reports a request as an executed move. Both halves are deferred by the owner: promotions (D-409) and migration requests (D-410).

**Source:** §5; §6; §11 phase 3.

#### D-396 [H] One replay store and optimistic conditional writes

All pricing POSTs require Idempotency-Key. The 24-hour store is keyed by tenant, concrete endpoint and client_key; payload hash guards replay. The hash is over canonical JSON (object keys sorted at every depth), so the order a client sends keys in never turns a retry into IDEMPOTENCY_CONFLICT. Check it before reservations or unit work and claim/respond in the mutation transaction. Approval units have no idempotency column, despite the superseded §6 sketch. If-Match protects PATCH/PUT; conditional pending ownership and unit versions replace row locks. Persist audit and domain events with the mutation.

**Source:** §2.2; §3 items 23 and 27; §7.2.

#### D-397 [H] Consumer pins replace cohorts and catalog versions

Phase 4 resolve returns a full per-item chain matrix, versioned descriptors and promotion inputs (deferred with promotions, D-409), never totals. Renewals walk all successors from their pin and stop before the first new price; signup selects the in-force price. Approved prices remain readable by id forever, including keep_for_bound predecessors. Usage binds lazily per value, slices at a binding's ends_on (D-425) and resolves again with the pin on that date. Quote is the Studio totals preview, not the consumer contract, and it is not built (D-415).

**Source:** §2 decision 6; §7.1; §12.

#### D-398 [H] Reference reservation closes the lifecycle race

Before creating an entry, reserve in Products, re-read the SKU, then write the object and receipt durably before confirming. Reserve/fence guards share the Products database. Never release because confirm timed out. Registry outage before the entry write is REGISTRY_UNAVAILABLE and writes no entry. Only entry references are built in phase 2; plan_item follows in phase 3, and sold_as is deferred (D-411). See ADR-0004; bookkeeping: see D-401. The earlier pending_release wording is superseded by D-401.

**Source:** §2 decision 17; §13.

#### D-399 [H] No SkuChanged listener or local SKU cache in phase 2

Pricing reads bss_products_sdk::ProductsClient at write time for type and lifecycle, including a re-read after reservation. Resolve binds descriptors from versions?as_of in phase 4. Phase 2 deliberately does not subscribe to SkuChanged or keep a local SKU read model. Add a cache only after measurement justifies its consistency and operational cost. This supersedes the earlier listener wording for this phase.

**Source:** Phase 2 plan, Global Constraints; deviation from spec §7.3 and §11.

#### D-400 [H] Toolkit outbox and broker TypedEvent own event delivery

Retire the gear-authored pricing_outbox without a relay in phase 2b. The new chain uses toolkit outbox migrations with prefix bss_pricing_outbox; writers accept the same scoped transaction as the state change. Events implement broker TypedEvent and retain the envelope-encoded interim sink pattern proved by Products. Only committed outbox rows dispatch. Core payloads are PricesPublished, ApprovalUnitDecided and PriceBookEntryReferenceLost; phase 3 adds plan events (promotion events are deferred, D-409; migration and retirement events, D-410).

**Source:** Phase 2 plan Task 2a.2 and 2c.8; Products phase 1 pattern.

#### D-401 [H] Reference work is a durable op written before reserve

**Status:** DECIDED 2026-09-25.

Tx A claims the Idempotency-Key, mints price_book_entry_id and inserts pricing_reference_op with kind create_entry and state reserving before reserve. Reserve is idempotent per (owner, kind, ref_id). Re-read the SKU after reserve; Tx B inserts the entry with reservation_id and reference_state = confirmation_pending and moves the op to written. The op's outcome carries the create input (sku_id, period, dimension_key, invoice_line_override and, from D-427, model), and every op of an entry rebuilds it from the entry (a rereserve, a delete); Tx B re-judges the period and the model against the SKU type the reservation froze, and a refusal there is a 400 receipt. Confirm succeeds before Tx C sets the entry to confirmed, the op to done and answers the key. A refusal after reserve moves the op to cancelling, then release, then done. A door that gets no definite answer before the write (the reserve, or the SKU re-read after it) moves the op to cancelling, releases the key claim and answers 503: a 503 never becomes an entry, and the cancellation releases any receipt. Any other error that ends the door's drive after its reserve and before its write (409 CONTENDED, a 500) cancels the create the same way before it is answered, so no answered error becomes an entry later; only a failed cancellation leaves the op to the ticker. The ticker never makes a first reservation for a user: it cancels a create still reserving without a receipt, and completes one holding a receipt only when its door is gone. Delete removes the entry and inserts a delete_entry op in releasing in one transaction. Amended 2026-10-02. A serialization failure of a create's write or of its confirm does not end the door by itself. The door tries that step again. Two creates of one entry key still end as one 201 and one 409 ENTRY_KEY_TAKEN. A create that is still reserving after those further tries is cancelled and answers 409 CONTENDED, and that answer writes nothing. A confirm that is still contended is tried again, because the reference is already written, and the door does not answer CONTENDED for it.

A ticker drives every op not done with bounded backoff and never drops one. It also reconciles confirmed entries through states(): a released reservation on a live entry is re-reserved through a rereserve_entry op when the SKU is not fenced; otherwise the entry becomes lost, new prices fail ENTRY_REFERENCE_LOST and PriceBookEntryReferenceLost is emitted. A reservation released before its confirm follows the same rule: Tx C keeps the entry confirmation_pending and starts a rereserve_entry op, so a create is never answered lost. A reservation Products answers 404 for (it does not know the id, for example after a restore from an older backup) is treated as released: at confirm it takes this rereserve path, and at release it counts as released. Only a reservation refused because the SKU is fenced, retiring or retired makes an entry lost; any other refusal of a re-reservation is retried. Reconciliation also scans lost entries and re-reserves those whose SKU admits a reservation again (a lifted fence). Never release because a confirm timed out. An op has no foreign key to the entry and outlives removal. This supersedes D-398's earlier bookkeeping. Phase 3 renames the op kinds create, delete and rereserve, adds attach (D-413), and has an op name its reference as (ref_kind, ref_id), so plan items use the same machine (D-407, D-412).

**Source:** Phase 2 plan 2c.1/2c.6; plan review findings 2, 3.

#### D-402 [H] The pair guard compares SKU metering as of each price's start

**Status:** DECIDED 2026-09-25.

On a usage chain the successor keeps package_size and the SKU's (unit, usage_type_ref) read from the SKU version in force at each price's effective_from; otherwise CHAIN_MODEL_CHANGED. The model needs no comparison: every price of a chain has its entry's model (D-427). Products freezes the SKU type while referenced but versions its metering, hence the dated read. The meter is included because the supersession-continuity family (spec §5 "asserts exactly this") rejects a meter change. Submit checks the guard and apply rechecks it. A price that starts before the SKU's first version is compared with that first version's metering, never with "no metering". A Products refusal of the dated read (for example 403 for a caller without SKU read) reaches the caller with its own status and code; only unavailability (5xx, timeout, rate limit, a lost race) is 503 REGISTRY_UNAVAILABLE.

**Source:** Phase 2 reconciliation matrix row 24; spec §5 pair guard; supersession-continuity family.

#### D-403 [M] Validation refusals are 400 with a code; no wire 422

**Status:** DECIDED 2026-09-25.

The toolkit's canonical errors have no 422: InvalidArgument and FailedPrecondition both answer 400, and Products phase 1 already answers its failed submit checks with 400. Pricing keeps its route census rule that no operation declares a 422. Therefore CHAIN_MODEL_CHANGED, PAIR_SPLIT and every other pure-rule refusal at a door or at submit are 400 with their stable code in the problem body, and no approval unit is created. Conflicts stay 409 (PRICE_LOCKED_PENDING, PRICE_NOT_DRAFT, ENTRY_REFERENCE_LOST, UNIT_CONTENDED, APPLY_REFUSED). The spec's "422 at submit" wording in §2 decision 16 and the §6 trait comment are superseded by this entry.

**Source:** toolkit canonical error mapping; Products phase 1 behaviour; deviation from spec §2 decision 16 and §6.

#### D-404 [H] A draft belongs to its author

**Status:** DECIDED 2026-09-25.

A draft belongs to its author: only its creator edits or deletes it. PATCH or DELETE of a draft price by anyone but its created_by is 403 NOT_DRAFT_AUTHOR, and a temporary pair's partner follows its creator. The entry delete honours it: DELETE /price-book-entries/{id} is 403 NOT_DRAFT_AUTHOR, naming the first such price, while a draft price of the entry was created by someone other than the caller; rejected prices are history and do not block. Separation of duties (D-393) excludes the submitter and every item's created_by; because no one else can change a draft, every number in a unit is its item author's, and an editor can never approve money they wrote under another author's name. Products applies the same rule to SKU drafts. Another author proposes a different number with a draft of their own.

**Source:** Phase 2 review (chains MEDIUM-1, docs F2); spec §6 "author ≠ approver" (finding 8).

#### D-405 [M] Publish changes completes a selected pair

**Status:** DECIDED 2026-09-25.

POST /price-books/{id}/publish-changes that ticks one half of a temporary pair adds the other half to the unit and records it in the snapshot's added_partner: a pair is never split, and an untick of one half is not refused. Submitting one half alone through POST /prices/{id}/submit stays 400 PAIR_SPLIT, and the subject's submit validation still refuses a partial pair (PAIR_SPLIT) as a safety net. A foreign-book price is refused PRICE_NOT_IN_BOOK with no unit.

**Source:** Phase 2 plan and reconciliation row 23; Phase 2 review (docs F4).

#### D-406 [H] A temporary window is not crossed

**Status:** DECIDED 2026-09-25.

On one chain (entry, dim_value), a proposed price that is neither temporary nor a pair's return may not start inside [effective_from, temporary_until) of an approved temporary price or of a temporary price in the same unit: the promo's end (its return, or its closed end) would undo it. It is refused 400 PRICE_INSIDE_TEMPORARY at the draft door (create, and a PATCH that moves the start), at submit, and at apply as APPLY_REFUSED. A temporary price whose window (effective_from, temporary_until) strictly contains the start of an approved price of its chain, or of a price in the same unit, is refused 400 TEMPORARY_SPANS_A_CHANGE at the draft door, at submit after a common-date shift, and at apply: normalisation would cut the promo at that start. A price that starts exactly on temporary_until is allowed, because it ends the promo. A nested pair's return belongs to its pair and is not refused by the first rule. The alternative, a price scheduled inside a promo that takes effect at the promo's end, needs a return re-derived after approval; it is left to the owner.

**Source:** Phase 2 second review (behaviour MEDIUM-2).

#### D-407 [H] Plan items are reserved references; items are a sub-resource

**Status:** DECIDED 2026-09-25.

An item added by POST /plan-revisions/{id}/items reserves kind plan_item with ref_id = the item id, through phase 2's create op (D-401: the op before the reserve, the SKU re-read, the write, the confirm; a 503 writes nothing). plan_item is the only new reference kind of phase 3: the sold_as kind waits with the sold-as bundle (D-411). Items are POST /plan-revisions/{id}/items and PATCH or DELETE /plan-items/{id}, not a list inside PATCH /plan-revisions/{id}. This deviates from spec §7.2 on purpose: an added item is one op with its own Idempotency-Key and its own recovery, and a removed item is one delete op with its own recovery (the DELETE takes no key).

D-467 amends this entry: the item create takes sku_id and price_book_entry_id, both required, and the item PATCH takes only price_book_entry_id; treatment, included_qty and qty_min are 400 BODY_UNEXPECTED at both doors.

D-512 amends this entry: `price_book_entry_id` on the item create is optional. Absent or null adds the SKU with no entry. The PATCH still takes only `price_book_entry_id`, and a null one is still 400 `ITEM_ENTRY_MISSING`.

**Source:** Phase 3 plan rev 2; plan review HIGH 2; owner, 2026-09-25 (kind plan_item only); deviation from spec §7.2 (items inside the revision PATCH). Amended by D-467, D-512.

#### D-408 [H] Plan checks read every SKU fresh; descriptors are information, never content

**Status:** DECIDED 2026-09-25.

Checks, submit and apply read each item's SKU through sku_for_write (D-399). A deprecated SKU may stay in a new revision of the SAME plan that carries it over from the published revision; it cannot be added, and a clone (a new plan) that carries it is red (ITEM_SKU_DEPRECATED). A draft, retiring or retired SKU is red (ITEM_SKU_UNAVAILABLE). P-D-248 amends the retiring case: there is no `retiring` lifecycle; a SKU with `retire_pending` is red the same way (`ITEM_SKU_UNAVAILABLE` on the checks, `SKU_RETIRING` on an entry or item create). A lost reference of that SKU is not re-reserved. A bundle SKU cannot be an item (ITEM_BUNDLE_SKU). An entry that a plan item names cannot be deleted (ENTRY_IN_USE). The approval snapshots of revisions and of prices carry each SKU's current descriptors for the reviewer, OUTSIDE the fingerprinted after: a GL change must not refresh every pending unit. They are gathered in collect or validate_submit and cached on the subject, because snapshot is synchronous. Being information, their read is best-effort and never refuses a submit, a vote or a reject (D-416).

D-453 amends this entry: the published revision is the one in effect today (D-447). Once a scheduled revision is due, it is its plan's published revision and its predecessor is not, before the job persists the switch and after it. So the checks of both revisions answer the same on the two sides of the persist.

D-465 amends this entry: "newly added" does not cover a re-add. The item create admits a deprecated SKU that the plan's published revision in effect carries, as the checks do, so an item removed from a draft can be added back; any other deprecated SKU stays ITEM_SKU_DEPRECATED.

D-466 extends this entry: each check row names the items that turn it red and the pending prices behind its blocked_by.

D-482 amends this entry: checks, submit and apply read their item SKUs in one `skus_for_write` (products P-D-245), not one `sku_for_write` per SKU. A SKU Products no longer knows is still left out, so the checks show it unavailable. The checks doors read the stored context on a connection, as a set (`stored_contexts`).

D-522 amends this entry: an entry whose reference is `released` (its book archived) holds no reference, so the checks count an item that names one as they count an item on a `lost` entry (`ENTRY_REFERENCE_LOST`), and no item create or PATCH names it: 409 `BOOK_ARCHIVED` while its book is archived, 409 `ENTRY_REFERENCE_RELEASED` after an unarchive could not re-reserve it.

**Source:** Phase 3 plan rev 2; plan review MEDIUM 6; phase 2 "owed to phase 3" (fresh SKU reads, current descriptors in snapshots). Amended by D-453, D-465, D-482, D-522; extended by D-466.

#### D-409 [H] Promotions are deferred (owner, 2026-09-25)

**Status:** DECIDED 2026-09-25.

The owner took promotions out of phase 3: there are no promotion tables, doors, promotion approval kind or PromotionPublished event until the owner brings them back. resolve's "active promotion (id, version)" is deferred with them (spec §2.4). The promotion DoDs in features/promotions-migrations.md stay unticked, each marked deferred by the owner; where DESIGN, slice 06 or the features describe promotion routes or tables, the text stays and is marked deferred. The promotion half of D-395 waits with them. The phase 3 plan's earlier shape for promotions (one identity row, one row per version, overlap against each other promotion's current approved and open pending version, end-today and cancel as new versions) is kept in the plan's revision 2 for their return.

**Source:** Owner, 2026-09-25, during Run 3.1; Phase 3 plan rev 3; spec §2.4. Supersedes the Phase 3 plan rev 2 D-409 (versioned promotion rows).

#### D-410 [H] Migration requests and plan retirement are deferred (owner, 2026-09-25)

**Status:** DECIDED 2026-09-25.

The owner took migration requests, their preview and plan retirement out of phase 3: there is no migration-request table, no POST /plans/{id}/migrations or /retire door, no migration approval kind, no retiring plan state, and no SubscriptionMigrationRequested or PlanRetired event until the owner brings them back. The migration and retirement DoDs stay unticked, each marked deferred by the owner; where the DESIGN, the PRD, slices 04 and 06 or the features describe these routes, tables or events, the text stays and is marked deferred. The migration half of D-395 and the retirement prerequisite of D-394 wait with them. The phase 3 plan's rev 2 shape (caller-supplied subscription periods, a request that moves nothing in pricing, retirement as a request against another plan) is kept in the plan for their return.

**Source:** Owner, 2026-09-25, during Run 3.1; spec §11 phase 3. Supersedes the Phase 3 plan rev 2 D-410 (caller-supplied periods) and D-411 (retirement's request half).

#### D-411 [H] The sold-as bundle and plan grants are deferred (owner, 2026-09-25)

**Status:** DECIDED 2026-09-25.

The owner took the sold-as bundle SKU and the revision's grants out of phase 3: pricing_plan carries no bundle_sku_id and no bundle unique index, pricing_plan_revision carries no grants, bundle_sku_id or sold_as reference columns, the BUNDLE_SKU check is not built, and the sold_as reference kind waits with them. A bundle SKU still cannot be an item (ITEM_BUNDLE_SKU). Where the DESIGN, the PRD, slice 04 or the plans feature describe sold-as or grants, the text stays and is marked deferred. The minimal Grants and optional sold-as bundle of D-394 wait with them.

**Source:** Owner, 2026-09-25, during Run 3.1; spec §5 plan_revision.

#### D-412 [M] The phase 2 schema is edited in place until the first deployment

**Status:** DECIDED 2026-09-25.

The chain has never been deployed. This phase edits m20260926_000006 (reference ops) in place in Run 3.2 and adds m20260926_000010 onward. A database migrated before keeps the old shape silently (CREATE … IF NOT EXISTS); the phase 4 legacy-history guard is the protection.

**Closed** by D-427 (2026-09-26): the chain is deployed, no shipped migration is edited again, and every schema change is a new migration after m20260926_000012.

**Source:** Phase 3 plan rev 2; plan review LOW 15.

#### D-413 [H] A copied item attaches its reference after the write

**Status:** DECIDED 2026-09-25.

POST /plans/{id}/revisions (copy) and POST /plans/{id}/clone write the revision and every copied item in ONE transaction with reference_state = unreserved and reservation_id NULL, plus one attach op per item. The attach op has the rereserve shape: reserve, then confirm; a losing refusal (a SKU fenced, retiring or retired at the reserve, or a lifecycle or type the SKU re-read refuses) marks the item's reference lost, and any other refusal, such as one of the caller's own grant for the reserve or the re-read, is retried and the ticker finishes the op as the system actor. Products authorizes the system actor to the tenant, so a refusal given to the system actor itself (a SKU Products no longer knows, say) is about the SKU and would never change: it marks the item lost, never an attach retried forever (phase 3 second review B-1). This deviates from "reserve before write" on purpose: the copied SKU is already protected by the SOURCE revision's live reference, which scheduled, published and superseded revisions never release, so no retire or type fence can slip in between. Products admits a deprecated SKU for a reservation, so a carried-over deprecated SKU attaches. The door drives the attach ops best-effort, stopping at the first that fails, and answers 201; the ticker finishes the rest. Submit and apply require every item reference to be confirmed, or confirmation_pending with a receipt; otherwise the checks show ITEM_REFERENCE_PENDING or ITEM_REFERENCE_LOST. POST …/items alone keeps D-401's create op. The source is the revision in effect: both doors switch a due scheduled revision first (D-451), and a copy is refused while a revision waits for its date.

D-467 amends this entry: a copied item is a new row, written paid with no quantity; a legacy item stored without an entry is copied without one (the column's CHECK then keeps it included), so the draft's checks show it ITEM_ENTRY_MISSING.

D-512 amends this entry: a copied or cloned item that has no entry, a draft item waiting for one or a legacy row, is copied without one. A book remap does the same, and never chooses an entry.

**Source:** Phase 3 plan rev 2; plan review HIGH 1; deviation from D-401's reserve before write. Amended by D-451, D-467, D-512.

#### D-414 [H] A revision's references outlive it

**Status:** DECIDED 2026-09-25.

Items of published and superseded revisions keep their references, fail-safe, because a pinned subscription may still be rated on them. So a SKU ever sold in a revision cannot be retired until those references are released. The release waits until Subscriptions reports that no subscription pins the revision; that report is owed to the Subscriptions integration. The delete of a draft revision releases its items' references through delete ops.

**Source:** Phase 3 plan rev 2; plan review LOW 14.

#### D-415 [H] Quote is not built; the Studio is not wired to the API (owner, 2026-09-25)

**Status:** DECIDED 2026-09-25.

GET /pricing/v1/quote is not built in this programme, and the PriceBook Studio is not wired to the API; no Studio programme follows. Quote was the Studio's preview, not a consumer contract: consumers read resolve and GET /bss-pricing/v1/prices/{id}. The minimum-fee floor arithmetic of D-388 belongs to Rating, with its acceptance example: two values rated 10 + 10 on one default price with min_fee 30 bill 30; two own prices with min_fee 30 each bill 60 (Rating T-D-38). cpt-cf-bss-pricing-fr-min-fee stays: pricing stores and validates min_fee and resolve returns it. In the PRD, DESIGN, slice 07 and features/read-contract-events.md every quote statement stays and is marked not built (D-415); the quote and minimum-fee-floor DoDs stay unticked for that reason.

**Source:** Owner, 2026-09-25, during Run 3.1; deviation from spec §7.1 (GET /pricing/v1/quote) and §11 phase 4.

#### D-416 [H] Descriptors are read best-effort; the reads a rule needs stay hard

**Status:** DECIDED 2026-09-26.

The SKU descriptors an approval unit shows (D-408) are information, so their read is best-effort: when Products cannot answer it (503) or definitely refuses it (for example 403 for a caller without products read), the snapshot records "descriptors": "unavailable" and the submit, the vote or the reject goes on. The reads that feed a RULE stay hard and are made as the caller: the plan checks (GET /plan-revisions/{id}/checks, and the plan_revision subject's validate_submit and apply) and a usage chain's dated metering (the pair guard, D-402). There only unavailability is 503 REGISTRY_UNAVAILABLE, and a refusal keeps Products' own status and code on submit, approve and reject alike, never 400 REGISTRY_REFUSED. So an approve-only reviewer votes on any unit whose rules read no SKU and rejects any unit, while a rule read needs products read too: the plan_revision submitter and its final approver (apply re-runs the checks), readers of the checks, plan item authors (the item door and its create re-read), price-book entry authors (the period rule and the create re-read), and the submitter and final approver of a prices unit on a usage chain.

**Source:** Phase 3 review, fix run 7 (plans F2, surface S-1, docs F1 and F2; controller notes 4 and 5).

#### D-417 [M] The last revision of a never-published plan takes the plan with it

**Status:** DECIDED 2026-09-26.

DELETE /plan-revisions/{id} of the last revision of a plan that was never published (published_rev IS NULL and no other revision) deletes the plan in the same transaction, with a plan.delete audit row, and frees its code; the door still answers 204. Without it such a plan was stranded: a copy answered PLAN_UNPUBLISHED, a clone CLONE_SOURCE_UNPUBLISHED, no door deletes a plan, and its code stayed taken. Nothing refers to a never-published plan: it has no published revision, no pin and no event, and an approval unit names a revision by ref_id without a foreign key. A plan with a published revision keeps its behaviour: deleting its draft leaves the plan, its code and its published revision as they are.

**Source:** Phase 3 review, fix run 7 (plans F1, surface S-2; controller note 1).

#### D-418 [H] A plan revision is submitted under plan:submit

**Status:** DECIDED 2026-09-26.

POST /plan-revisions/{id}/submit checks the plan label's own submit action, plan:submit: the label-specific analogue of price:submit on POST /prices/{id}/submit and of price_book:submit on publish-changes. The plan label's actions are read, author and submit. approval_unit:submit, which the run 3.4 brief bound to this door, stays the right to withdraw a unit; it no longer lets a prices submitter submit, or read through the receipt, a plan revision, so a tenant can withhold plan submission from its prices submitters.

**Source:** Phase 3 review, fix run 7 (surface S-4); corrects the run 3.4 brief.

#### D-419 [H] Resolve answers one revision on one date, with the caller's pins

**Status:** DECIDED 2026-09-26.

GET /bss-pricing/v1/resolve?plan_revision_id=&date=&item_id=&pins= (label plan, action read); it is spec §7.1's /pricing/v1/resolve below the gear's base. Only a published or superseded revision resolves, and a scheduled one from its sale date (D-454); a draft or pending one is 409 REVISION_NOT_PUBLISHED, and a scheduled one before its date 409 REVISION_NOT_YET_AVAILABLE. The state is the one the revision reads today (D-447). date is required, YYYY-MM-DD (400 DATE_INVALID). item_id is optional: resolve that one item only (404 if the revision has no such item); a consumer with many pins splits its request by item. pins is optional and comma-separated, each pin either price_id or price_id:dim_value: the first pins the price's own chain; the second pins a DEFAULT-chain price that the value dim_value was bound to. A pin must name an approved price of the tenant, of an entry an item of this revision names; with :dim_value it must be a default-chain price of that entry, and the value is NOT checked against today's registry (a value may have been removed since). Otherwise the whole request is 400 PIN_FOREIGN. Two pins for one (item, value) are 400 PIN_DUPLICATE; more than 1 000 pins are 400 PINS_TOO_MANY. Resolve is a read: it writes nothing (no audit row, no idempotency key, no binding) and returns no totals and no promotion (D-409, D-415).

**Source:** Phase 4 plan rev 3 (Run 4.2); spec §7.1; plan review M4 b (a removed value is not checked against the registry), L4 (the pin bound and the split by item) and L7. Amended by D-454, D-467: a resolved item carries no treatment, included_qty or qty_min.

#### D-420 [H] The matrix and the walk: a binding is always in force

**Status:** DECIDED 2026-09-26.

Per item, the chains are the default chain and one per value registered today for the entry's dimension key, plus any value a pin names (so a removed value still resolves for the subscription that holds it); an item without an entry has no chains. Per (item, value):

1. No pin (signup): the binding is the price in force on date, the value's own chain, else the default chain (domain::price::version_at); dim_used says which; neither gives uncovered: true and no binding (never an invented price, never a total).
2. With a pin: walk the pinned price's chain forward through approved successors with effective_from <= date: an all successor is taken; the walk stops before the first new successor (spec §7.1, D-397), except as rules 3 and 4 say. The last price reached is the binding; pinned_from names the pin.
3. A binding is always in force (generic; plan review H2 i and iii): if the walk ends on a price that is no longer in force on date (a closed value chain, a temporary price whose window ended, a chain with no successor), the binding is chosen as in rule 1 for that value (own chain, else default: the value reads the default again, spec §5); pinned_from still names the pin. This is also what happens to a pin on an ended temporary price. There is no promotion-specific rule: a temporary pair is walked like any other prices of its chain, and a new promo pair is NOT skipped (the owner, 2026-09-26: promo is not built now, spec §2.4 addendum); promotion-aware renewal comes back with promotions (D-409).
4. A default-chain pin for a value that later got its own chain (plan review M4 a): the binding moves to the value's own chain when the own price in force on date has eligibility all and started after the pin's effective_from (spec §7.1: a new all price changes the binding from the next period); a new own price does not move it. This is the owner's decision of 2026-09-26 (plan question 2, answered "the second question as recommended").

Read precisely: a price reached by the walk is no longer in force on date when it starts after date, or when its own end has passed. Its own end is read from the stored fields normalisation does not rewrite: temporary_until for a temporary price, else the stored end of an explicitly closed price (closed_explicitly), else none (amended by D-425, phase 4 review C-1). It is never a temporary price's stored effective_to: normalisation cuts that at the start of a pair nested inside the temporary price, and that start is a successor the walk did not take. An explicit close that a later pair cuts is stored as min(end, next) and binds until that stored end; no new column keeps the original end. The start of a successor the walk did not take does not end a price for the pin: a price stopped before a new successor stays the binding, and keep_for_bound marks exactly such a price. keep_for_bound predecessors stay readable and bindable; the resolve context carries the set of keep_for_bound price ids (the domain Price has no such field; plan review L1) and the binding reports it. Each binding reports its own end as ends_on (D-425).

D-512 amends the D-467 line below: an item with no entry, a draft waiting for one or a legacy row, has no chains. The doors do not publish a waiting item, so resolve does not serve one.

**Source:** Phase 4 plan rev 3 (Run 4.2); the owner, 2026-09-26 (Q1: no promotion-specific renewal rule; Q2: rule 4 as recommended); spec §2.4 addendum, §5, §7.1, §12; plan review H2, M4, L1. Amended by D-425 (phase 4 review, fix run 8): the own end, and ends_on. Amended by D-467: only a legacy item stored without an entry has no chains. Amended by D-512.

#### D-421 [H] The binding carries resolved invoice inputs with their source

**Status:** DECIDED 2026-09-26.

Each item's SKU version is read as of date through sku_version_as_of (the detached registry read, made as pricing's system actor after the caller passed plan:read, D-424: consumers need no products read). A registry that cannot answer is 503; Products' definite refusal keeps its own status and code; a 404 for an unknown SKU gives sku_version: null, like no version on that date, never a pass-through 404 that reads like "revision not found". The item returns, each as { value, source } with source entry, sku, tenant or null: invoice_line_template = the entry's invoice_line_override, then the SKU version's template, then the tenant template for the charge kind (settings.invoice_line_templates); gl_code = the SKU, then the tenant default_gl; tax_category = the SKU, then the tenant default_tax_category; billing_timing = the SKU, then the tenant default_timing (PRD AC #13: advance on the SKU beats arrears as the tenant default). It also returns sku_version { published_version, effective_from } and meter { usage_type_ref, unit } of that version (null without a version). The rules mirror the prototype (ui-prototype pricebook 50-rules.js, resolveInvoiceLine and the DESCRIPTORS check) and the plan checks' Defaults. Revision-level: book_id, currency, currency_minor_digits (domain::book::minor_digits), rounding_policy (the tenant default_rounding) and date.

Read precisely: settings.invoice_line_templates is keyed by SKU type (recurring, usage, one_time, bundle; PUT /settings refuses any other key). A charge kind's name is its SKU type's, so the entry's charge kind is the key; an item without an entry takes its SKU version's type, as the prototype does. A source that is absent or blank falls through to the next one; when none is left the field is { value: null, source: null } (the prototype's built-in "{sku}" line is not adopted). billing_timing always has a value: the tenant default_timing is advance until the settings are written.

**Source:** Phase 4 plan rev 3 (Run 4.2); PRD AC #13 (fr-settings); spec §7.1 (what a pin carries), decision 14; plan review H3 and L7. Amended by D-424 (phase 4 review, fix run 8): the SKU version read is made as pricing's system actor. Amended by D-467: the item returns no stored treatment or quantity.

#### D-422 [H] The pinned price read serves approved money forever

**Status:** DECIDED 2026-09-26.

GET /bss-pricing/v1/prices/{id} (label price, action read), spec §7.1's /pricing/v1/prices/{id} below the gear's base, answers an APPROVED price of the tenant whatever its window (closed, followed by a later price, keep_for_bound) with its entry's SKU, charge kind, period, book and currency. It returns only stored facts: no status or other value computed from today, and no authoring internals (version, pending_unit_id, note, created_by). A draft, pending or rejected price, an unknown id and another tenant's id are 404, with the same body.

**Amended by D-520.** A cancelled price is still answered by id, with its money as approved; it was never in force, so no pin or binding names it. It carries `status: cancelled`, a stored fact the same on every day; an approved price still carries no status (D-520's 2026-10-03 amendment). A `cancel` or `end` row is not a price and is the same 404.

**Source:** Phase 4 plan rev 3 (Run 4.2); spec §7.1; plan review H4 (no value computed from today).

#### D-423 [H] Both gears refuse a legacy or stale schema at boot

**Status:** DECIDED 2026-09-26.

Each gear's chain starts with ONE guard migration, named to sort first under the toolkit runner's name sort: m0000_pricing_refuse_a_legacy_or_stale_schema here and m0000_products_refuse_a_legacy_or_stale_schema in Products (products P-D-195). It sorts before the coordination, broker and outbox migrations (m0001_…, m001_…), so it is pending on every database that predates phase 4 and runs there once, before anything of the gear is created. It creates nothing and reads the catalog only: sqlite_master and table_info on SQLite; information_schema and pg_constraint on Postgres, tables in schema bss. It refuses (the migration fails, and boot fails naming the gear) when it finds:

- a legacy table: one of the 45 pricing_* tables that the pre-PriceBook chain creates (bss/products-backup, m20260821_000001 to m20260921_000050, 47 tables) and today's chain does not. pricing_plan and pricing_price exist in both and are not evidence. The set is a constant in the guard; a test proves it disjoint from every table a fresh chain creates.
- a stale shape, a table that today's chain edited in place (D-412) or renamed: pricing_reference_op without the column ref_kind; pricing_price_row present (the pre-rename name); pricing_price without the column price_book_entry_id (the pre-rename, entry-shaped table). Because the guard sorts first, a pre-rename database, on which the renamed migrations 000005 and 000007 are pending too, meets the guard's refusal and not a raw SQL error from 000007.
- the legacy shape of a table that both chains create: pricing_plan without the column code (the legacy revision row). A clean-up that drops only the tables a refusal names leaves it, and m20260926_000010's CREATE TABLE IF NOT EXISTS would keep it and then fail on its code index with a raw SQL error (phase 4 review F2, fix run 8).

The refusal reads: bss-pricing: this database holds a <legacy|stale> bss-pricing schema (<what was found>); PriceBook does not migrate it — start from an empty data root / empty bss-pricing tables. A fresh database passes, and so does a database migrated by today's chain: the guard is pending there once and finds nothing. The schema goldens do not change.

**Source:** Phase 4 plan rev 2 (Run 4.1); plan review H1, M1, M2 and L6. It is the phase 4 legacy-history guard that D-412 names.

#### D-424 [H] Resolve reads SKU versions as pricing's system actor

**Status:** DECIDED 2026-09-26.

Rating and Subscriptions call GET /bss-pricing/v1/resolve as system subjects (bss-rating.system, bss-subscriptions.system; PRD §2.2 lists them as system actors). The Products registry refuses every system subject except bss-pricing.system (403 REFERENCE_OWNER_MISMATCH, DESIGN §3.5), so a SKU version read made as the caller refused every consumer, whatever its grants. Resolve therefore reads each item's SKU version as of date as pricing's system actor (reference_ticker::system_actor for the caller's tenant). It does so only after the caller has passed plan:read and the revision was found in the caller's tenant. This widens what a plan reader reads: a human caller with plan:read but no products read now receives, through resolve, the SKU-version fields (sku_version, meter, and the invoice inputs whose source is sku) of the SKUs its revision names. It receives no other SKU and nothing of another tenant: the SKU ids come only from the items of a revision found under the caller's plan:read scope, and the system actor reads in the caller's tenant. A consumer needs pricing plan:read for resolve and price:read for the pinned read, and never products read. The rest of D-421 stands: a Products that cannot answer is 503 REGISTRY_UNAVAILABLE, a definite refusal keeps Products' own status and code, and a 404 gives sku_version: null. The reads that feed a rule stay the caller's (D-416).

The registry's trust of pricing's system actor is in-process only; its threat model is products P-D-222 (whole-branch review RS-02, fix run W1b). A pricing door hands the registry its caller's context (the entry and plan item doors, the plan checks' SKU reads), so no REST door of either gear serves that actor: a caller whose context carries it in either half, the subject type bss-pricing.system or the id PRICING_SYSTEM_ACTOR, is 403 SYSTEM_ACTOR_RESERVED at every door, before the PDP (support::require_authenticated, which every door calls first; the test is bss_products_sdk::is_pricing_system_actor). Before, a token carrying both halves, with a pricing grant, got the registry's tenant-wide trust through an entry create (the second review of W1b, M1; fix run W1c). Rating's and Subscriptions' system subjects are not refused: they pass the edge and the PDP judges their plan:read, as above. Resolve still builds its own system actor after the caller passed plan:read.

**Amended 2026-10-02 (phase 9 review F1).** The same actor is the contract on `PricingReadV1::resolve`. REST resolve and the SDK method both call `pricing_reads::finish`, which calls `versions_as_of`. That read builds `reference_ticker::system_actor` for the caller's tenant and does not forward the caller's context to Products. It runs only after the caller passed `plan:read` and the revision was found in the caller's tenant: the REST door's plan read, or `PricingReadProvider::scope` on the SDK path. A consumer Products does not trust would be refused if the read used the caller's context. The SKU ids still come only from the admitted revision, in the caller's tenant.

**Source:** Phase 4 review, fix run 8 (docs M1: the decision change that the review names as the alternative; the orchestrator's decision). Amends D-421. Amended by the second review of W1b, M1 (fix run W1c): the REST edge refuses pricing's system actor. Amended 2026-10-02 (phase 9 review F1): the SDK resolve path uses the same system actor.

#### D-425 [H] The binding says where it ends for its holder

**Status:** DECIDED 2026-09-26.

Each binding of GET /bss-pricing/v1/resolve carries ends_on: the binding's own end as D-420 rule 3 defines it. That is temporary_until for a temporary price, the stored end of an explicitly closed price, and null when the price has no end of its own. A consumer slices a period at ends_on, never at effective_to. effective_to stays in the binding as the stored window, for information only: the start of a successor sets it, and the start of a new successor that the pin did not take is not an end for a pinned subscription. Rule 3 reads the same own end, so a pinned temporary price binds until its temporary_until even when a pair nested inside it has cut its stored window (phase 4 review C-1).

A return to a TEMPORARY price (a pair nested in an outer pair) ends where that temporary price ends — its temporary_until, stored as an explicit close — so its binding carries that ends_on too (phase 4 second review M1, fix run 9).

**Source:** Phase 4 review, fix run 8 (contract C-1, docs M2; the orchestrator's decision). Amends D-420. Narrows spec §7.1's "Slices" bullet (a period is split at every row boundary of the bound chain): a successor's start inside a period is not a slice point for its holder; the binding's ends_on is (phase 4 second review L2).

#### D-426 [H] An entry's invoice line is locked once the entry carries money

**Status:** DECIDED 2026-09-26.

A PriceBookEntry stays outside approval: it is structure, and money reaches a customer only through an approved price (the prices kind) and a published plan revision (the plan_revision kind). The one entry field that reaches consumers directly is invoice_line_override: resolve returns it as the invoice line with source entry, ahead of the SKU version's template (D-421), while the same template on the SKU changes only through Products' sku_change approval. So once an entry has an approved or a pending price, PATCH /bss-pricing/v1/price-book-entries/{id} refuses a change of invoice_line_override (a new value or null) with 409 INVOICE_LINE_LOCKED; sending the stored value is no change. A draft or rejected price does not lock it. Another invoice line needs another entry. The prices of the entry are read tenant-scoped in the PATCH transaction.

**Source:** Owner, 2026-09-26 — asked why PriceBookEntry is not under approval, chose option 1 of three (lock the override once the entry carries money; the others: route override changes through a prices unit with an effective date, or accept the gap). Amends D-421.

#### D-427 [H] The model belongs to the entry, fixed for its life, and is part of its key

**Status:** DECIDED 2026-09-26.

A PriceBookEntry carries its model: pricing_price_book_entry.model is NOT NULL, one of flat, per_unit, graduated, volume and package, and allowed for the entry's charge kind (D-386). POST /bss-pricing/v1/price-books/{id}/entries requires model: an unknown string is 400 MODEL_INVALID, and a model that the charge kind does not allow is 400 MODEL_KIND_CHARGEKIND_MISMATCH. No PATCH changes it: the entry PATCH does not carry it and refuses an unknown field. The create is the durable op of D-401: the door judges model against a fresh SKU read before it claims anything, and Tx B judges it again against the SKU type the reservation froze; a refusal there is a 400 receipt, and the op releases its reservation. The op's persisted create input carries model, and a rereserve or delete op rebuilds it from the entry. An op stored before m20260926_000013 has no model. Such a create was posted under the key of its day (book, SKU, charge kind and period): when an entry already holds that key, Tx B gives the create that entry's model, so the insert meets the key and the op ends 409 ENTRY_KEY_TAKEN without a second entry. Otherwise it resolves to the model that the migration gives an entry without prices, the charge kind's default (flat for recurring and one_time, per_unit for usage). A rereserve or delete op does not write the model, so the entry keeps its own.

model joins the entry key: pricing_price_book_entry_key is (book_id, sku_id, charge_kind, coalesce(period, ''), model). So another model is another entry beside the old one in the same book, and a plan picks between them through its revision's items (a structural change under plan_revision approval). The create's refusal of a taken key stays 409 ENTRY_KEY_TAKEN, on the wider key. The book change of a draft revision (PATCH /plan-revisions/{id} with a new book_id) remaps an item to the new book's entry of the same (SKU, charge kind, period, model). The owner did not answer the plan review's key question (M4); model joins the key as the orchestrator's call, which the owner may overturn.

pricing_price.model is dropped. A price's price_json must match its entry's model: a mismatched shape stays 400 PRICE_MISSING. POST /price-book-entries/{id}/prices and PATCH /prices/{id} no longer carry model, and refuse it as an unknown field. The reads keep model, read-only and copied from the entry: every price of the authoring doors (PricingPriceDto) and GET /bss-pricing/v1/prices/{id}. GET /bss-pricing/v1/resolve carries model on the item (null for an item without an entry) and no longer on the binding. The pair guard compares package size and dated metering (D-402); the prices of one entry share its model, so the guard no longer compares a model, and CHAIN_MODEL_CHANGED stays for those two. A temporary pair's return no longer copies or compares a model (D-391). The fingerprinted before and after of a prices unit no longer carry model, so each prices unit that is pending at the deploy refreshes once, to a new generation, the first time it is judged.

The schema change is the forward migration m20260926_000013_model_on_the_entry, in the runner's transaction, on both dialects. On Postgres it first locks pricing_price_book_entry and pricing_price ACCESS EXCLUSIVE, so its check and its backfill judge the same prices (SQLite's writer lock already serialises). When the prices of an entry, in any state, carry two or more models, it fails, names the entries and changes nothing. Otherwise it adds the column (SQLite: NOT NULL DEFAULT 'flat' with its CHECK, then the backfill, and the default stays in the schema; Postgres: a nullable column, the backfill, SET NOT NULL and the named CHECK pricing_price_book_entry_model_check). The backfill gives each entry the one model of all its prices, or the charge kind's default when it has no price. Then the migration recreates the key index with model last and drops pricing_price.model. Its down is an explicit irreversible error. D-412 closes with this decision: no shipped migration is edited again, and every schema change is a new migration after m20260926_000012.

**Source:** Owner, 2026-09-26; phase 5 plan rev 2 (plan review H1, H2, M3, M4, L9, L11, L12). Amends D-386, D-390, D-391, D-401 and D-402; closes D-412.

#### D-428 [H] Entries and SKUs report their usage

**Status:** DECIDED 2026-09-26.

The two entry reads carry the entry's usage, for the SKUs screen and the book view. GET /bss-pricing/v1/price-book-entries/{id} and GET /bss-pricing/v1/price-books/{id}/entries answer PricingPriceBookEntryReadDto: the fields of the entry and usage = { prices: { approved, pending, draft }, plans, plans_superseded_only }. prices counts the entry's prices by state; a rejected price is not counted. plans counts the distinct plans that have a draft, pending, scheduled or published revision whose items name the entry; two revisions of one plan count once. plans_superseded_only counts the distinct plans that name the entry only through superseded revisions. Such a plan is history, but its items keep their references (D-414) and keep the entry ENTRY_IN_USE (D-408), so an entry with plans 0 can still refuse its delete. An entry counts in every reference state (confirmation_pending, confirmed, lost), and so does a plan item. The other answers that carry an entry keep the plain PricingPriceBookEntryDto without usage: the POST and PATCH answers, the stored Tx B receipt, the export and the publish-changes listing. The counts are numbers about the entry that the caller may read (price_book_entry read). They are read tenant-scoped, as the entry PATCH reads its prices, and need no price read or plan read. A request makes a fixed number of set-based reads, whatever the number of entries: the toolkit's QueryRecorder shows the same statement count for 10 and for 100 entries.

Pricing also fills the SKU usage port of Products (P-D-197): products-sdk SkuUsageV1, which pricing registers in the ClientHub at its init as dyn SkuUsageV1. For the SKU ids of one tenant it answers each distinct id once, as { sku_id, entries, currencies, prices { approved, pending, draft }, plans }. entries counts the SKU's entries in every book of the tenant, in every reference state. currencies are the distinct currencies of their books, sorted. prices adds up the counts of those entries. plans counts the distinct plans across all the SKU's entries, with the entry rule above: a plan that names two entries of the SKU counts once, where a sum of the entry counts would count it twice. An unknown id, the SKU of another tenant and a bundle SKU (it has no entry, D-386) answer zeros. The caller must hold price_book_entry read; otherwise the port answers 403, and Products shows no usage. Pricing reads only the SKU ids that it is given, and no Products data flows back into pricing.

Products P-D-212 adds the port's usage_sets(ctx, tenant): the tenant's priced SKUs (an entry in any book, in any reference state: entries above zero) and its in-plan SKUs (an entry named by a plan item of a draft, pending, scheduled or published revision: plans above zero), each sorted and distinct, under the same rule and scope, in two set-based statements whatever the number of SKUs. They are what the Products SKU list's priced and in_plan filters keep or drop.

D-440 amends the entry reads: usage.prices also counts the approved prices by where their window stands today (scheduled, active, superseded; approved is their sum), from the same grouped count, and each entry read carries current_price. The SKU usage port keeps products-sdk's PriceCounts unchanged.

D-453 amends the plan counts: they read the stored state, so a scheduled revision counts as a published one does, and a due revision's stored-published predecessor counts too until the switch is persisted. The switch job ends that over-count: it persists at most the ticker's limit (100) due revisions a cycle (a minute), oldest available_from first, so a backlog of more than 100 drains 100 a cycle, and a plan whose switch keeps failing over-counts until it is repaired (D-450).

**Source:** Owner, 2026-09-26; phase 5 plan rev 2 (the counts on the entry and on the SKU, option 1; the semantics confirmed by the owner; plan review M6, M7, L10). Amended by D-440, D-453.

#### D-429 [M] The replay store's mechanics (twin of products P-D-198)

**Status:** DECIDED 2026-09-27.

A key's row in pricing_idempotency is claimed or answered, and a CHECK ties the response pair to the state. The claim INSERT is the at-most-once gate: a door claims on the transaction that writes its act (D-396), and a reference-work door claims in Tx A with its op (D-401), so the claim commits before the reserve; when that create is cancelled before its write, the cancellation deletes the claim and frees the key. The endpoint is the concrete resource path, never the route template, and no in-flight deadline exists. The answered row stores the status and body the caller was told, so a replay reads no other row. An answer is stored only when its transaction commits: a refusal that rolls back takes the claim with it, and a committed refusal (Tx B's 400 receipt, D-401 and D-427) is stored and replays. Expiry is judged at claim time: an expired row is taken over by a compare-and-swap on the expires_at that was read, and the loser answers IDEMPOTENCY_KEY_IN_FLIGHT having executed nothing. The loser may even carry a different payload from the winner, and is still refused in-flight rather than for the mismatch, since its transaction never compared the two: it read the expired holder's digest, never the winner's. Apart from that loser, a matching live claimed row is IDEMPOTENCY_KEY_IN_FLIGHT, and a digest mismatch is IDEMPOTENCY_CONFLICT in either state. POST entry and POST plan item bind their claim to the durable reference op in Tx A (bind_op), and entity_ref holds that op's id. A bound claimed row is never taken over on expiry: a matching retry is IDEMPOTENCY_KEY_IN_FLIGHT until the op answers the key. The op's answer is kept a full 24 hours from the moment it is written, however late, so a retry after a late answer replays it. Every other door leaves entity_ref NULL. Products runs the same store (P-D-198), with two differences: products binds no op, so its entity_ref is always NULL; and its retention is configurable (24 hours to ten years), where pricing's is a fixed 24 hours.

**Source:** Carried from D-142 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27.

#### D-430 [M] A tier ladder's top band is open

**Status:** DECIDED 2026-09-27.

A graduated or volume price has at least one band (TIER_BAND_EMPTY), strictly ascending bounds below the top (TIER_BANDS_ORDER), and an open top band: a closed top band is TIER_TOP_CLOSED. No quantity above a last bound is left unrated; capping usage is not a price. The band edges themselves are D-387's.

**Source:** Carried from D-17 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27.

#### D-431 [M] The request's correlation id is minted at the authoring edge

**Status:** DECIDED 2026-09-27.

The authoring router mounts correlation::establish: each request gets one UUID v7 before any handler runs, and an inbound traceparent is not consumed. Every audit row the request writes carries it as correlation_id, so the rows of one call join. It is not the Idempotency-Key and is not derived from the payload. A handler reached without the layer answers 500 and never mints its own.

**Amended 2026-10-02 (phase 9 review F1b).** A commercial check has no authoring edge. It mints one correlation id for the check and passes that id to catch-up and to the acceptance audit row, so those rows join. A rereserve op is the exception: the reference ticker starts it, or the Tx C of a create, an attach or another rereserve op does when the reservation was released before its confirm — a Tx C the door's own drive may run inside a request — and it mints its own UUID v7 when it is created. The confirm and reference-lost audit rows its completion writes carry that id, which matches no request. Pricing never writes a NULL correlation_id. The read-contract router (resolve, the pinned price read) writes nothing and mounts none; events carry no correlation id. Products establishes no correlation and writes NULL (P-D-200).

**Source:** Carried from D-178 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27.

#### D-432 [M] If-Match on every write to a versioned row, and on a draft price's DELETE

**Status:** DECIDED 2026-09-27.

PATCH of books, entries, plans, plan revisions and plan items, PATCH /prices/{id} and DELETE /prices/{id} of a draft price, and PUT of settings, dimension keys and the approval policy require If-Match with the row's strong version: a missing or malformed header is 400, a stale one 409 STALE_REVISION. The DELETEs of an entry, a plan revision and a plan item take none, as built. This extends D-396's "If-Match protects PATCH/PUT".

**Source:** Carried from D-141 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27. Extends D-396.

#### D-433 [M] The audit log is append-only with a reserved sealing seam (twin of products P-D-200)

**Status:** DECIDED 2026-09-27.

pricing_audit refuses every DELETE by trigger and admits one UPDATE: unsealed to sealed, supplying chain_id, seq and row_hash (prev_hash NULL only on a segment head) with every record column unchanged. The gear writes seal_state = unsealed with the four seal columns NULL on every row and never seals, chains or verifies: sealing is a platform capability the columns are reserved for. The key is a surrogate audit_id, because seq is NULL until a row is sealed. No REVOKE UPDATE, DELETE is issued (a deployment role the migration does not own; SQLite has none). correlation_id is text: pricing writes its edge id (D-431), or on a rereserve op's rows the id the op minted, and never NULL; products writes NULL (P-D-200). error_code, attempted_key, session_id and ceremony_ref are carried in the DDL and written NULL. Products has the same table shape (P-D-200).

**Source:** Carried from P-D-08, P-D-28, P-D-46, P-D-118 (backup `3a38f0b28`); decisions cleanup, owner 2026-09-27.

#### D-434 [M] Where a SKU is priced and sold: its entries across books, the plans that name it, one plan item

**Status:** DECIDED 2026-09-27.

The SKUs screen shows, for one SKU, where it is priced and where it is sold (ask 8). The gear had the counts only (D-428).

- GET /bss-pricing/v1/price-book-entries?sku_id= (price_book_entry read) lists the tenant's entries of the SKU in every book and every reference state, read under the caller's entry scope, narrowed, ordered and paged as D-486 states. Each item is PricingSkuEntryDto: the entry's own fields, book_code, book_name and currency, usage (D-428) and current_price. current_price is the default chain's approved price in force today: no dimension value, effective_from on or before today and effective_to after it, chosen as resolve chooses a price in force (the latest start, then the latest version). It is null when no such price exists. A value chain's price is not the entry's current price. The money is shown only to a caller who also holds price_book read, the export's grant, judged a second time in the same request. Without that grant the entries are still listed, each with current_price null. A denial of that second judgement refuses nothing, and an unavailable policy fails the read with 503. When the grant's scope admits some books only, only their entries show money. sku_id is required. The query, its in-memory narrowing and its paging are D-486; a query D-486 does not take is still 400 QUERY_INVALID. An unknown SKU, or another tenant's, is an empty list, because pricing does not know which SKUs exist. The read makes seven statements whatever the number of entries: the entries, their books, the three usage reads, the books the grant admits and those books' default-chain approved prices. The QueryRecorder shows the same statements for 10 and for 100 entries.
- GET /bss-pricing/v1/plans?sku_id= (plan read) lists the plans that have a draft, pending, scheduled or published revision whose items name an entry of the SKU. This is the usage's plans definition (D-428; products P-D-212's in_plan): a plan that names the SKU only through superseded revisions does not count, and an included item without an entry does not count. The answer has the same schema as GET /plans (PricingPlanList, each plan with all its revision headers). The revisions, items and entries are read tenant-scoped. GET /plans itself becomes set-based: two statements whatever the number of plans (the plans, then all their revisions), where it made one revision read per plan. A malformed sku_id, or any other key, is 400 QUERY_INVALID. Before, the route took no query and ignored any key.
- GET /bss-pricing/v1/plan-items/{id} (plan read) answers PricingPlanItemReadDto: the item's fields (PricingPlanItemDto), plan_id, rev_no and state, which is its revision's state as it reads today (D-453). The ETag is the item's version, the value its PATCH takes as If-Match. An item the tenant does not hold is 404.

D-440 amends this entry: the two entry reads carry the same current_price, chosen and shown by the same functions (in_force; money_scope, the one second judgement of price_book read), and usage carries the approved prices by date from the same grouped count, so the SKU's entry list still makes seven statements.

D-453 amends this entry: the plan list, by SKU or not, and the item read render each revision's state as it reads today, derived in memory, so GET /plans keeps its two statements; which plans the SKU filter keeps is still read from the stored state.

D-460 amends this entry: each plan of GET /plans, by SKU or not, names its current revision with its item SKUs, read in one grouped statement over the listed plans' current revisions, so the list makes three statements whatever the number of plans. A current revision's sku_ids name every item, while the SKU filter keeps a plan only for an item with an entry, on the stored state: the two may differ.

D-461 amends this entry: each revision header says when it was submitted and approved, from the units the listed revisions name, read in one grouped statement more, so GET /plans makes four statements whatever the number of plans (one when there is none): the plans, their revisions, the current revisions' items and the units.

D-472 amends this entry: each entry of the SKU's list also carries next_price, chosen and shown with current_price by one function from one read, which is widened to the default chain's pending and draft prices, so the list still makes seven statements.

D-486 amends this entry: the list narrows, orders and pages in memory, and each entry carries status and changing. The read still makes seven statements.

D-483 amends this entry: GET /price-books/{id}/entries, whose entries carry this entry's current_price under the same per-book rule, pages on the toolkit's pager, 500 entries by default and at most 500, in the order (sku_id, charge_kind, model, id). The money is judged once per request on the book, so a page shows it on every entry or on none.

D-485 amends this entry: GET /plans, with or without sku_id, is one page of the toolkit pager. sku_id's stored-state EXISTS is inside the page query and the counts, not a filter over a page. The page is five statements: the page, the revisions, the items, the units and the current revisions' books.

**Source:** Owner, 2026-09-27; phase 6 plan rev 2 (ask 8; plan review L11). Amended by D-440, D-453, D-460, D-461, D-472, D-483, D-485, D-486.

#### D-435 [M] An approval-policy override can be reset; the default cannot be deleted (twin of products P-D-216)

**Status:** DECIDED 2026-09-27.

The PUT sets an override but nothing removed one, so a kind once overridden never followed the default again (ask 11b). DELETE /bss-pricing/v1/approval-policy/{kind} (config settings) removes the kind's override at the policy the caller read. If-Match carries the policy's content tag, the tag the policy PUT takes (D-432). The kind then follows the default quorum again. The answer is 200 with the policy and its new tag, as the PUT answers. The path names the default as `*` (percent-encoded or not), as the PUT's body does. The default is never deleted (400 POLICY_DEFAULT_REQUIRED): a tenant always has a quorum to fall back to, and a tenant that never stored one follows the fail-safe one. PUT changes the default and nothing removes it. The refusals are judged in this order: 403 without config settings, before any precondition; 400 for a missing or malformed If-Match; 400 POLICY_DEFAULT_REQUIRED, or POLICY_KIND_INVALID for a kind other than prices and plan_revision; 409 STALE_REVISION; 404 when the kind has no override. A reset writes one audit row, approval_policy.reset. A unit already submitted keeps the quorum it copied (D-393). Products has the same door for its kinds (P-D-216).

D-481 amends this entry: the policy is also read by two doors that are not config settings. GET /plan-revisions/{id}/checks names quorum_required, the plan_revision quorum, under plan read, at no statement more (the checks already read the policy). GET /approval-policy/{kind}/effective answers { kind, quorum_required } in one statement: prices under price_book_entry read, plan_revision under plan read. An unknown kind is 400 QUERY_INVALID. DELETE /approval-policy/{kind} is unchanged. The quorum is not money; pricing serves no price read of its own, so price read is not the grant.

**Source:** Owner, 2026-09-27; phase 6 plan rev 2 (ask 11b; plan review L8). Amended by D-481.

#### D-436 [M] Dimension values edit one at a time and show their use

**Status:** DECIDED 2026-09-27.

The Settings screen edits one key's values and shows which values are in use (ask 11c).

- The request and response shapes are split. PricingDimensions and PricingDimensionEntry stay the PUT's body only. PricingDimensionKeyPatch is the PATCH's body. Both refuse unknown fields. GET, PUT and PATCH /dimension-keys answer PricingDimensionRegistry: { items: [ { key, values: [ { value, usage: { prices } } ] } ] }. Breaking: each value is an object now, not a string, and a GET answer is no longer a PUT body.
- usage.prices counts the prices of any state whose entry names the key and whose chain is the value: draft, pending, approved and rejected. This differs from D-428's entry counts, which leave a rejected price out, because this count is the removal rule: a rejected or pending price still carries its value. It comes from ONE grouped count (the prices joined to their entries, grouped by the entry's key and the price's value). So GET makes two statements whatever the number of entries and prices (the registry, then the count). The content tag covers the stored registry only: a price written since does not move it.
- PATCH /bss-pricing/v1/dimension-keys { key, add, remove } (config settings, If-Match: the content tag the PUT takes) edits the values of one declared key: a stored key, or the seed key region while the tenant stores no registry. Keys are added and removed by the PUT only. Values are trimmed and empty ones dropped. The result keeps the key's values in their order without the removed ones, then the added ones in the order sent, and it is judged by the PUT's rule for a key. The refusals, in order: 403 without config settings; 400 for a missing or malformed If-Match; 409 STALE_REVISION; 400 DIM_NOT_DECLARED for another key; 400 DIM_VALUE_DUPLICATE for a value named twice across add and remove, or added while the key holds it; 400 DIM_VALUE_UNKNOWN for removing a value the key does not hold; 400 DIM_VALUE_INVALID or DIM_VALUES_FEW for the result; 409 DIM_VALUE_IN_USE for removing a value a price uses. The 409 names the value, for example "DIM_VALUE_IN_USE: region=us is used by 1 price". An empty patch writes nothing and answers the registry. A PATCH writes one audit row, dimension_keys.patch.
- The PUT judges its removals from the same grouped count and from one DISTINCT read of the keys that entries name, where it read every entry's prices one entry at a time. Its DIM_VALUE_IN_USE names the value and its DIMENSION_KEY_IN_USE names the key. Tests pin GET, PUT and PATCH at the same statements for 10 and for 100 entries; each key a write stores is still one write.

**Source:** Owner, 2026-09-27; phase 6 plan rev 2 (ask 11c; plan review M6).

#### D-437 [M] The default rounding is one of five modes; a tenant with no settings rounds half_even

**Status:** DECIDED 2026-09-27.

default_rounding is one of half_up, half_even, half_down, up and down. PUT /bss-pricing/v1/settings refuses any other value with 400 ROUNDING_INVALID; a blank value stays 400 ROUNDING_REQUIRED. No database CHECK is added. A stored value outside the set reads back as stored, and resolve carries it as rounding_policy. The tenant cannot save its settings again until it chooses one of the five, so the deployment has a hard pre-flight gate: `SELECT DISTINCT default_rounding FROM bss.pricing_settings` must return values inside the set, or a normalizing migration ships first. The default of a tenant that never wrote its settings is half_even, banker's rounding: it is aligned with the ledger PRD's platform default, so a tenant that sets nothing rounds its prices as the ledger rounds its postings. It was half_up until 2026-09-28. A revision drafted before the tenant's first settings write carries the default, and resolve reports it as rounding_policy. Explicit PUT bodies that name half_up stay valid.

**Source:** Owner, 2026-09-27; phase 6 plan rev 2 (ask 11d; plan review M5). The half_even default: Owner, 2026-09-28.

#### D-438 [M] The settings offer currencies and say who changed them

**Status:** DECIDED 2026-09-27.

- **The migration.** m20260927_000014_settings_currencies_and_author runs in the runner's transaction on both dialects and only adds columns. currencies is declared as invoice_line_templates is (jsonb NOT NULL DEFAULT '[]' on Postgres, text NOT NULL DEFAULT '[]' on SQLite, the entity's Json): every existing row offers any currency, so no tenant or book changes behaviour. updated_by is nullable and declared as the dialect's other uuid columns are (uuid on Postgres, text on SQLite, where the application stores a 16-byte blob). Its up skips a column that is already there, and its down drops the two columns. The upgrade tests run the gear's list without 000014, seed a settings row, apply 000014 alone and prove exactly two added columns on both dialects; the row survives, and the application reads and writes it again. The schema goldens gain exactly these two columns. m20260926_000013's upgrade tests pass with 000014 in their before state.
- **currencies.** PUT /bss-pricing/v1/settings requires currencies, a full replace; [] offers any currency, and a body without the field is 400, as any missing field is. Each code is spelled as a book's currency is (three uppercase ASCII letters; the workspace holds no ISO 4217 list, domain::book::currency_code) and appears once, else 400 CURRENCY_INVALID. The list is stored in the order sent. POST /bss-pricing/v1/price-books with a currency outside a non-empty list is 409 CURRENCY_NOT_OFFERED, judged after the book's own 400s. The settings are read tenant-scoped, so the book author needs no config grant. Existing books are untouched: the list restricts new books only.
- **Who and when.** The settings answer carries currencies, updated_at and updated_by. At version 0 (nothing written) both updated_at and updated_by are null. updated_by is also null on a row written before 000014. Every PUT stamps both: the time of the write and the caller's subject id.

Breaking: the PUT's body (currencies required), and a GET answer is a PUT body only without version, updated_at and updated_by.

D-519 extends this entry: `GET /settings` also carries `updated_by_name`, the writer's current name through Account Management, null when it is not available now. The PUT answer's is null.

**Source:** Owner, 2026-09-27; phase 6 plan rev 2 (asks 11f, 11g; plan review L6, L7). Extended by D-519.

#### D-439 [M] Closed sets are enums on the responses; requests keep strings and their codes (twin of products P-D-217)

**Status:** DECIDED 2026-09-27.

- **Responses.** Every closed set a response schema carries is an enum in the served OpenAPI. It holds exactly the tokens that the column stores and that the wire always carried, so the wire does not change and the golden contracts are not recorded again. The sets: an entry's charge_kind (recurring, usage, one_time), period (month, year), model (flat, per_unit, graduated, volume, package) and reference_state (confirmation_pending, confirmed, lost); a price's model, eligibility (all, new), state (draft, pending, approved, rejected) and display status (draft, pending, rejected, scheduled, active, superseded); an item's treatment (paid, optional, included) and reference_state (unreserved, confirmation_pending, confirmed, lost); a revision's state (draft, pending, scheduled, published, superseded; `scheduled` since D-446), and the resolved revision's (published, superseded, scheduled; `scheduled` since D-454); a reference op's kind, state and ref_kind; a unit's state, a decision (approve, reject) and a vote's outcome (pending, applied, rejected, withdrawn); the settings' default_timing (advance, arrears); a resolved input's source (entry, sku, tenant). The same sets appear in resolve (DESIGN §3.3, slice 07 §6) and in the pinned price read. Each set is one schema component with the Pricing prefix (api/rest/closed_sets.rs). A set with a domain enum maps to and from it, so a value added on one side only does not compile.
- **Stored values.** A database CHECK holds each stored set on both dialects: charge_kind, period (the entry's CHECK: month or year for a recurring entry, null for the others), model, the reference states, eligibility, the price's state, treatment, the revision's state, the reference op's three columns, the unit's state, the decision and default_timing. The read is fallible. Only a writer that goes around the CHECK can store a token outside its set, and the read answers it with CorruptRow, which names the row and the token: a 500 with the detail logged, never a panic and never a value that the enum does not hold. The display status, the vote's outcome and the source are computed from typed values.
- **Requests keep string.** A request field over the same set stays string in its schema, and its door judges it, so each field keeps its refusal code (D-403): MODEL_INVALID, ENTRY_PERIOD_INVALID, ELIGIBILITY_INVALID, TREATMENT_INVALID, TIMING_INVALID and ROUNDING_INVALID. A request enum would fail at deserialization, before the door, with a 400 that has no code. The downstream e2e asserts MODEL_INVALID, and spec-check P3 would find codes that are declared and never raised.
- **Response fields that stay string.** default_rounding and resolve's rounding_policy: no CHECK guards the column (D-437), and a legacy value reads back as stored. A normalizing migration with a CHECK is not made here: the phase takes only ADD COLUMN migrations, a CHECK on an existing SQLite column needs a table rebuild, and the deployment pre-flight (D-437) is the gate. A unit's kind and ref_type: the shared approval tables (bss_approval::ddl) have no CHECK on them, for the same reason. A check's code is a code vocabulary that the check builders write as literals, and a proposal's chain is a dimension value or default; neither is a closed set of stored values.
- **Proof.** tests/response_enums.rs reads the served spec: every listed field has its enum with the exact values in order and its nullability, no request body reaches an enum, and the fields above stay plain strings. An entry row poisoned on SQLite (the CHECK refuses the write; the test then bypasses it) reads 500, and the gear goes on serving. Unit tests pin each set's schema, wire and stored tokens as one list.

D-467 amends this entry: a plan item's treatment is no longer on any response or request, so its enum and TREATMENT_INVALID are gone; the column's CHECK stays and still holds the stored rows.

The phase 9 review's theme C amends this entry (fix run 9.5d-1): an approval unit's kind is an enum on every response, PricingApprovalKind (prices, plan_revision). No CHECK holds the column still; the repository reads it through the set, so a unit of another kind is a corrupt row (500), never served (D-470). A unit's ref_type stays a string.

**Source:** Owner, 2026-09-27; phase 6 plan rev 2 (ask 12; plan review M5). Amended by D-467 and by the phase 9 review, theme C (fix run 9.5d-1).

#### D-440 [M] An entry's prices, its price in force and its approved prices by date

**Status:** DECIDED 2026-09-28.

The Price Books screen opens a drawer per entry with its dated prices (ask 18). The only source was the whole-book export, and usage.prices.approved lumped the active, scheduled and superseded prices together.

- **The entry's prices.** GET /bss-pricing/v1/price-book-entries/{id}/prices answers PricingEntryPriceList { items }, each item a PricingPriceDto: every price of the entry in every state, each with its display status on the day of the request (domain::price::window_display: draft, pending or rejected as stored; an approved price is superseded when its window ended on or before today, scheduled when it starts after today, and active otherwise). The order is the default chain first, then each dimension value's chain in ascending order of the value; each chain by effective_from, then version_no, then id, as the export orders it. status keeps one status or several, comma-separated (status=draft,scheduled). An unknown or empty value, a repeated status and any other key are 400 QUERY_INVALID.
- **The grant.** The whole answer is money, so the grant is judged twice (plan review H1). price_book_entry read reaches the entry: 403 without it, and 404 ENTRY_NOT_FOUND for an entry the tenant does not hold, judged before the money. Then price_book read, judged a second time as D-434 judges it, must admit the entry's book: 403 PRICE_BOOK_READ_REQUIRED without the grant, or when its scope does not admit that book; 503 when the policy cannot judge. The order: 403 for entry read, 503 for the money's policy, 400 for the query, 404, then 403 for the money. The read makes three statements whatever the number of prices: the entry, its book under the grant, and its prices. The QueryRecorder shows the same statements for 10 and for 100 prices.
- **The price in force.** The two entry reads, GET /price-book-entries/{id} and GET /price-books/{id}/entries, carry current_price with D-434's value, shape and grant rule: the default chain's approved price in force today (a PricingPriceDto), or null when none is in force or when the caller's price_book read does not admit the entry's book. An unavailable policy fails the read with 503. The list judges its one book once. D-434's list and these reads share one function that chooses the price (in_force) and one that judges the grant (money_scope).
- **The approved prices by date.** usage.prices gains scheduled, active and superseded beside approved, pending and draft, so approved = scheduled + active + superseded for every entry. The tests prove it over value chains, a temporary price in force with its scheduled return, an ended gap and a price that ends today. The split comes from the same ONE grouped count that D-428 reads: per entry and state, it also sums the prices whose window ended on or before today and those not ended that start after it, in window_display's order (superseded first), with a UTC today bound once per request. So no usage read makes a statement more, and D-434's SKU entry list still makes seven. Every price the same request answers takes the same day. The split is pricing's own: products-sdk's PriceCounts, the SKU usage port, the SKU card and the SKU list are unchanged (plan review M1). The SKU's entry list (D-434) carries the same usage.
- **The frozen contract.** The golden price_book_entry_usage leaves out the three dated counts and current_price, because a frozen document holds no value of today. tests/book_reads.rs pins them.

Breaking for a consumer that compares usage as a closed object: it gains three fields, and the entry reads gain current_price. The gears-rust e2e is adapted in run 7.1; the downstream suite follows in run 7.4.

D-456 extends this entry: plan create, clone and a book-naming revision PATCH judge the same second price_book read on the book the plan names.

D-472 amends this entry: the entry reads also carry next_price. in_force became price_book_entries::headline, which chooses current_price and next_price from one read of the default chain's approved, pending and draft prices, so no read gains a statement; the golden leaves next_price out as it leaves current_price out.

D-473 amends this entry: GET /price-books/{id}/entries takes as_of, and its one day is that date instead of today. Every price the list answers takes that day: current_price, next_price, each price's status and the usage split. Its refusal order gains the query: 403 for entry read, 503 for the money's policy, 400 QUERY_INVALID then DATE_INVALID, then 404 for the book, then 403 PRICE_BOOK_READ_REQUIRED for an as_of other than today without price_book read on the book (the phase 9 review's R1).

**Source:** Owner, 2026-09-28; phase 7 plan rev 2 (ask 18; plan review H1, M1, L4). Amends D-428 and D-434. Extended by D-456. Amended by D-472, D-473.

#### D-441 [M] Every book read carries its stats

**Status:** DECIDED 2026-09-28.

The Price Books screen lists books with their prices, SKUs, plans, pending changes and last change (ask 14). One request per book could not give distinct plans or a last change.

- **The shape.** GET /bss-pricing/v1/price-books and GET /bss-pricing/v1/price-books/{id} answer PricingPriceBookReadDto: the book's fields (PriceBookDto, flattened) and stats { entries, skus, plans, plans_superseded_only, prices { draft, pending, approved, scheduled, active, superseded, rejected }, pending_units, last_change_at }. The write answers (POST, PATCH, the stored receipt), the export and publish-changes keep PriceBookDto (plan review L8). The ETag of GET /price-books/{id} is unchanged.
- **The counts.** entries counts the book's entries in every reference state, and skus their distinct SKUs. plans counts the distinct plans with a draft, pending, scheduled or published revision whose book_id is the book, from the stored state (D-453). A plan whose revisions on the book are all superseded is not counted, and two revisions of one plan count once. plans_superseded_only counts the distinct plans that name the book only through superseded revisions: the plans with a revision of any state on the book, less those plans counts. Both come from plan_revision_repo::plans_on_books, one grouped statement, which is also the one read by which the book delete judges BOOK_IN_PLAN and BOOK_IN_PLAN_HISTORY (D-444). So plans = 0 and BOOK_IN_PLAN never disagree (plan review M5), and entries, plans and plans_superseded_only are all 0 exactly when the delete succeeds (If-Match aside). plans_superseded_only was added in the phase 7 fix run (additive); it is the book's twin of D-428's entry count of the same name. prices counts the prices of the book's entries by state. Unlike D-428's entry counts, a rejected price is counted, and the approved ones are also split as D-440 splits them. pending_units counts the book's prices units in review; a prices unit's ref_id is its book, and a unit of another kind is never counted.
- **The last change.** last_change_at is the latest of the book's updated_at, its entries' and their prices' updated_at, and its prices units' submitted_at and decided_at (plan review M4). A submit, a withdraw or a reject writes no updated_at, so it moves the last change through its unit. A deleted draft or entry leaves no row, so its deletion does not move it. The instants are compared as instants. On Postgres the maximum of a timestamptz is rendered in UTC to the microsecond that Postgres keeps. On SQLite, where RFC 3339 text does not sort as time within one second (P-D-213: …00Z after …00.5Z), the maximum is taken over a fixed-width key that pads the fraction to nine digits. Pricing writes every instant in UTC.
- **The statements.** A fixed number of grouped statements, one per source and none multiplying another (plan review M3): the entries (count, distinct SKUs, latest); the prices, each joined to its one entry (by book and state, with the dated sums and the latest); the plans that name the book (the live ones and all of them, in one grouped statement); and the prices units (pending, latest). A page makes its own statement and these four, and one book's read makes the book's statement and these four. The QueryRecorder shows the same five statements for 10 and for 100 books ($top=200), half of them named only by superseded revisions, and one book's read makes the same statements whether a plan holds it or only history does. Every source is read tenant-scoped: the counts are facts of a book the caller may read (price_book read), as D-428 reads an entry's usage.

**Source:** Owner, 2026-09-28; phase 7 plan rev 2 (ask 14; plan review M3, M4, M5, L8); phase 7 review (behaviour lens: plans_superseded_only). Amended by D-453.

#### D-442 [M] The book list pages on the toolkit's OData pager, searched by q and sku_id

**Status:** DECIDED 2026-09-28.

GET /price-books took no parameter and answered every book (ask 15). It now pages on the toolkit's OData pager, as the Products SKU list does (P-D-210).

- **The query.** $filter reads id, code, name, currency, valid_from and valid_until. The two dates are nullable: eq null is a book open on that side, and they filter only, never order. id compares with eq and in; a malformed uuid is 400. $orderby reads code and name, tie-broken by id, and the default order is the code. $top (alias limit) defaults to 200 and is clamped at 500, the categories' page, so a tenant's books stay on one page (plan review M2). cursor (alias $skiptoken) comes from page_info.
- **q and sku_id.** q is a case-insensitive substring of the code or the name, matched literally (%, _ and \ are escaped). On Postgres both sides fold through the ICU root collation und-x-icu, whatever the database's locale: a C database's own lower() folds ASCII only. The deployment's Postgres must be built with ICU, as P-D-210 says. On SQLite both sides fold through lower(), ASCII only. An empty q is no search. sku_id keeps the books with an entry of that SKU, in any reference state; it is a sub-select condition of the page's own statement. The cursor carries a hash of $filter, q and sku_id, so a cursor replayed under another narrowing is 400 FILTER_MISMATCH.
- **Refusals.** Authorization is judged first (price_book read). Any other plain key, a repeated key and a malformed sku_id are 400 QUERY_INVALID, the code pricing's other reads give a key (plan review M2 left the choice). The toolkit refuses $select, $count and every other option it does not take with UNSUPPORTED_QUERY_PARAM, and a filter or an order it cannot read with INVALID_FILTER or INVALID_ORDERBY_FIELD.
- **The answer.** The list answers `Page<PricingPriceBookReadDto>`: { items, page_info { next_cursor, prev_cursor, limit } }, each item with its stats (D-441). There is no total: the toolkit Page carries none, and the stats are the pattern for counts (plan review M6). Pricing takes the toolkit-odata dependency.

Breaking: the list pages, where it was unlimited, so a tenant with more than 200 books reads the rest through next_cursor; and an unknown key, which was ignored, is 400. items keeps its place and page_info is added. The deploy notes name both.

D-480 amends this entry: `$filter` names `id` as well (`eq` and `in`). A malformed uuid is 400. The cursor hash already covers `$filter`, so a cursor replayed under another id is 400 FILTER_MISMATCH, on SQLite and on Postgres.

**Source:** Owner, 2026-09-28; phase 7 plan rev 2 (ask 15; plan review M2, M6, L3). Amended by D-480.

**Extended by D-522 (2026-10-03).** `$filter` names `archived`: an archived book is left out unless the filter asks `archived eq true`, and the page's statement count is unchanged.

#### D-443 [M] A temporary draft's dates move, and its pair follows

**Status:** DECIDED 2026-09-28.

The Price Books screen edits a temporary draft's dates (ask 21). PATCH /prices/{id} refused every date of a temporary price with TEMPORARY_PRICE_FIXED, so the author deleted the pair and drafted it again.

- **The door.** PATCH /bss-pricing/v1/prices/{id} of the temporary half of an unlocked draft (the price that carries temporary_until) takes effective_from and temporary_until, by its author and under If-Match, as before. A PATCH that sends either date runs the pair builder (domain::price::temporary) again over the new dates, in the same transaction.
- **The shapes.** The builder makes a pair when a price of the chain is in force on the end (the return restores it); the promo alone when the chain's next approved price starts exactly on the end; and one explicitly closed price when nothing of the chain is in force there. Every move between them is reconciled. Pair → pair: the return is derived again in place (its id, version_no and author stay; its start is the new end; its money and min_fee are copied again from the price it restores, and its end from that price's own end). Pair → one price: the return is deleted. One price → pair: a return is created. One price → one price: the promo's end and closed_explicitly are written again.
- **The write order.** The pair references are foreign keys. Pair → pair: the promo at its version, then the return at its own version. Pair → one price: the promo first, its paired_price_id cleared, then the return is deleted at its version, with a price.delete audit row. One price → pair: the return first, naming the promo (which exists), then the promo names it; the return has a price.create audit row. A new return takes the entry's next version_no (the highest of all the entry's prices, plus one), never promo.version_no + 1, which another price may hold (the unique (price_book_entry_id, version_no) index). A concurrent writer that takes the number first makes the PATCH try again, as the create does.
- **The judgement.** Both halves are judged as the create judges them: each price's own rules (for example WINDOW_END_INVALID, WINDOW_START_IN_PAST, WINDOW_OVERLAP) and D-406 against the approved prices and the pair itself (PRICE_INSIDE_TEMPORARY, TEMPORARY_SPANS_A_CHANGE). A refusal writes nothing.
- **The answer.** 200 with the edited price: its paired_price_id names its partner now, or null, and the ETag is its new version. A kept return's version moves too, so a client reads it again before it edits the return.
- **What stays fixed.** TEMPORARY_PRICE_FIXED (400) is narrowed to: a return's own dates (the author edits the temporary half); the chain (dim_value) of either half; temporary_until on a price that is not the temporary half; and a null temporary_until. No PATCH makes a price temporary or ends its temporariness. A pair whose other half is not an unlocked draft is 409 PRICE_NOT_DRAFT, and another author's is 403 NOT_DRAFT_AUTHOR, as the delete judges them.
- **An edited return.** A return's money stays editable, and a move of its pair copies the money again from the restored price. Submit would refuse the edited return as PAIR_RETURN_STALE anyway (D-391), so the copy loses nothing that submit would keep.

**Source:** Owner, 2026-09-28; phase 7 plan rev 2 (ask 21; plan review H2, L5). Amends D-391.

#### D-444 [M] A book has a description, and an unused book can be deleted

**Status:** DECIDED 2026-09-28.

The Price Books screen shows a book's description and deletes a book that nothing uses (ask 16). A book had no free text, and no door removed a book.

- **The description.** Migration m20260928_000015_book_description adds pricing_price_book.description, a nullable text column, by ADD COLUMN on both dialects (a book written before it reads null). POST /price-books takes an optional description; PATCH /price-books/{id} keeps it when the field is omitted, replaces it with a value and clears it with null. It holds at most 2000 characters (Unicode scalar values), judged at the door: 400 BOOK_DESCRIPTION_TOO_LONG; there is no CHECK. It is stored as sent. Every book answer carries it: PriceBookDto (the write answers, the stored receipt, the export, publish-changes) and the book reads, which flatten it (D-441). A stored POST receipt younger than a day replays the answer it recorded, without the field.
- **The delete.** DELETE /bss-pricing/v1/price-books/{id} under the book write grant (price_book author) and If-Match answers 204 and writes a price_book.delete audit row. The refusals come in this order: 403 without the grant (authorization first); 400 for a missing or malformed If-Match; 404 for a book the tenant does not hold; 409 STALE_REVISION; 409 BOOK_HAS_ENTRIES for an entry of the book in any reference state; 409 BOOK_IN_PLAN for a plan with a draft, pending, scheduled or published revision on the book (D-453); 409 BOOK_IN_PLAN_HISTORY when only superseded revisions name the book. Both are judged from plan_revision_repo::plans_on_books, the read D-441's stats.plans and stats.plans_superseded_only count, and BOOK_HAS_ENTRIES from the read stats.entries counts. So the delete succeeds exactly when stats.entries, stats.plans and stats.plans_superseded_only are all 0, and a screen knows beforehand which refusal it would meet.
- **Superseded revisions keep the book.** A revision's book_id is a foreign key, and a revision may name a book without any entry of it (a revision moved to another book, or one with only included items). A book that only superseded revisions name has stats.plans = 0 and stats.plans_superseded_only > 0, and its delete is still refused: the history of those revisions keeps it. That refusal has its own code, BOOK_IN_PLAN_HISTORY, so BOOK_IN_PLAN keeps D-441's meaning.
- **No refusal for a pending unit.** A prices unit in review holds pending prices; a pending price keeps its entry (ENTRY_PRICES_IN_USE), and BOOK_HAS_ENTRIES is judged first. A plan-revision unit names its revision, which is not superseded while it is pending (BOOK_IN_PLAN). So no unit can be pending on a book without entries, and the plan's BOOK_LOCKED_PENDING is not a refusal (plan review L1).
- **A lost race.** A row that a concurrent writer adds after the door's reads meets the book's foreign key: Postgres waits for that writer and fails the delete on the key it names, and the entry's key is 409 BOOK_HAS_ENTRIES, a revision's 409 BOOK_IN_PLAN, never a 500. On SQLite one writer holds the database for the whole transaction, so the door's reads see every row. An entry create in flight loses cleanly the other way: its Tx B finds no book (BOOK_NOT_FOUND), which cancels the op and releases the reservation.
- **What stays.** The book's audit rows stay. A decided unit (rejected or withdrawn) that named the book stays readable: its card and the unit list answer with its ref_id, its decisions and the impact of its stored items, without the book. An approved unit's prices keep their entries, so its book is never deleted. Like a deleted draft SKU's create (P-D-206), the book create's Idempotency-Key still replays its 201 for a day, naming the deleted book; the code is free again.

D-522 amends this entry: a finished book need not be deleted to leave the screen. `POST /price-books/{id}/archive` hides it and releases its entries' SKU references. A revision that is only superseded does not refuse an archive, though it still refuses the delete (`BOOK_IN_PLAN_HISTORY`). An archived book with entries is still refused its delete (`BOOK_HAS_ENTRIES`).

**Source:** Owner, 2026-09-28; phase 7 plan rev 2 (ask 16; plan review L1, L8); phase 7 review (behaviour lens: the stats predict the delete). Amended by D-453, D-522.

#### D-445 [L] An approval unit carries its submitter's note (twin of products P-D-219)

**Status:** DECIDED 2026-09-28.

The approval library's unit now carries `submit_note`, the submitter's own words, which products' submit doors take (P-D-219, ask 4b). Pricing shares the unit shape and the approval DDL, so it gets the column too.

- **The column.** Migration m20260928_000016_unit_submit_note adds pricing_approval_unit.submit_note, a nullable text column without a default or a CHECK. It uses the library's separate step, bss_approval::ddl::apply_add_submit_note: ADD COLUMN with IF NOT EXISTS on Postgres, and a catalog check first on SQLite, so it replays. The library's ddl::up() is the body of m20260926_000002 and stays as it shipped (plan review H3). down drops the column the same way. A unit written before the migration reads null.
- **The reads.** GET /approval-units, GET /approval-units/{id} and every answer that carries a unit (the submit, vote and publish-changes receipts) carry submit_note.
- **No note from pricing's doors.** POST /prices/{id}/submit, POST /price-books/{id}/publish-changes and POST /plan-revisions/{id}/submit take no note, and their bodies are unchanged. So submit_note is null on every pricing unit. A door that takes a note later passes it through the library's SubmitRequest.note.
- **Not content.** The note is not part of the snapshot or of snapshot_hash, and a stale refresh keeps it.

D-464 amends this entry: POST /plan-revisions/{id}/submit and POST /price-books/{id}/publish-changes take an optional note, which they pass through SubmitRequest.note to the unit's submit_note; POST /prices/{id}/submit still takes none. So submit_note is null on a pricing unit only when its submitter sent no note.

**Source:** Owner, 2026-09-28; phase 7 plan rev 2 (ask 4b; plan review H3). Amended by D-464.

#### D-446 [M] A plan revision can be stored scheduled: the state, its index and its migration

**Status:** DECIDED 2026-09-29.

The owner approved scheduled plan revisions (2026-09-28): a plan change is approved today and takes effect on a later date. Until that date the plan keeps selling its current revision.

- **The state.** A revision state `scheduled` joins draft, pending, published and superseded. An approved revision whose sale date (`available_from`) is after the apply's day is stored `scheduled`: its lock is cleared, `approved_by_unit_id` names the unit, and `published_at` stays null. The plan's published revision and its `published_rev` do not move. A null or past `available_from` publishes at once, as before. A revision that was published at once before this entry stays published: there is no backfill. The apply makes this choice (D-449), and the switch on the date is announced by whoever persists it (D-450, D-451).
- **Why a stored state.** The published partial unique index admits one published revision per plan. A reader that filters `state = 'published'` does not see a scheduled revision, so no reader sells the future revision early. A reader that filters `state <> 'superseded'` (the BOOK_IN_PLAN family, D-441) counts it as a holder of its book and its entries, which is correct. The alternative, "published with a null published_at", would need a second test in every revision reader, and one missed reader would sell the future revision today.
- **One per plan.** The partial unique index pricing_plan_revision_scheduled ON (plan_id) WHERE state = 'scheduled' admits one scheduled revision per plan. A second one is 409 REVISION_SCHEDULED_EXISTS. Postgres names the index; SQLite names only the column, so the write that sets the state tells the two single-column indexes apart.
- **The migration.** The chain is deployed, so the change is the forward migration m20260929_000017_revision_scheduled. Postgres drops chk_pricing_plan_revision_state and adds it again with 'scheduled' (DROP CONSTRAINT IF EXISTS, so a replay changes nothing), then creates the index. SQLite cannot change a CHECK. The toolkit runner runs each up() in a transaction, where PRAGMA foreign_keys=OFF has no effect, so the SQLite arm rebuilds the family without a PRAGMA, as products m20260925_000007 does (P-D-196). The family is pricing_plan_revision and its only child, pricing_plan_item. The steps: create both new tables (000011's and 000012's text, word for word, except the wider CHECK), copy every row, drop the child and then the parent, rename the new tables, recreate the two partial indexes with their original text, and create the new index. down() is an explicit irreversible error.
- **The proof.** Upgrade tests on both dialects go through the real runner, from a database migrated without 000017, with plans, revisions in every old state and items. Only 000017 is pending. The dump differs by the CHECK and the new index only, each table's text by the CHECK only, and every row and foreign key survives. The CHECK admits scheduled and refuses an unknown state, and the new index refuses a second scheduled revision. An upgraded database equals a fresh one, and a replay applies nothing. The schema goldens were recorded again once: only the CHECK and the index changed. The schema guard needs no change.
- **The wire.** The closed set PricingRevisionState gains scheduled, so no read answers 500 on a stored scheduled row. Run 8.1 rendered the stored state everywhere; run 8.2 derives the effective state in every read that renders one (D-453), and /resolve serves a scheduled revision from its date (D-454).
- **The counts.** The counts read the stored state: stats.plans and BOOK_IN_PLAN (D-441, D-444), the SKU's usage.plans (D-434), the entry usage (D-428) and the plans that name a SKU. They count a scheduled revision as they count a published one. Once it is due, they also count its stored-published predecessor until the switch is persisted. The switch job ends that over-count: it persists at most the ticker's limit (100) due revisions a cycle (a minute), oldest available_from first, so a backlog of more than 100 drains 100 a cycle, and a plan whose switch keeps failing over-counts until it is repaired (D-450, D-453).
- **Deploy and rollback.** An old pod refuses a stored scheduled row in its closed sets and answers 500 for that tenant's plan list, SKU usage and item reads, and its copy door can open a draft beside the waiting revision. So the deploy is not a rolling update (plan rev 2 H2). First dump the pricing and products tables, both runner history tables and both gears' outbox tables: a restore without the outboxes sends their events again. Then scale core-server to 0, set the image and scale it to 1. A plain set image back is not safe once one scheduled row exists. A rollback first finds the rows with state = 'scheduled' and unschedules each one that is not yet due. A row that is already due cannot be unscheduled: the door answers 409 REVISION_IN_EFFECT, and its catch-up rolls back with the refusal. The job switches such a row, and it is then an ordinary published revision that the old code reads. So the image goes back only when no row with state = 'scheduled' is left. The other rollback is to restore the dump.

**Source:** Owner, 2026-09-28 (scheduled plan revisions); phase 8 plan rev 2 (decisions 1, 2, 10; plan review H2, L3, L4, L5, L8); phase 8 review (behaviour B4; docs L2, L3).

#### D-447 [M] A scheduled revision takes effect on its date: the effective state is derived

**Status:** DECIDED 2026-09-29.

A scheduled revision must take effect on its date with nobody acting and before any job runs. So every read derives the effective state from the stored rows, as a price's display state is derived (domain::price::status).

- **The rule.** domain::plan::effective(revisions, today) is a pure function. A scheduled revision whose available_from is on or before today reads as published, with published_at at 00:00 UTC of available_from. The stored-published revision of its plan reads as superseded and keeps its own published_at. The plan's published_rev reads as the due revision's rev_no (domain::plan::published_rev). Every other revision reads as stored. One list may hold the revisions of many plans; plans are told apart by plan_id.
- **Today.** Today is the UTC date of now, as the checks use it (domain::plan::sale_date).
- **A revision with no date.** A scheduled revision with a null available_from is never due. The storage writes none, because the apply schedules only a future date. The persisted switch (D-448) reads such a row the same way.
- **Reads never write.** The derivation runs in memory, so a GET door writes nothing. The plan read and list and the checks derive over the revisions that they read already, so their statement count does not change. The revision read, the item read and /resolve read their plan's revisions once more, and the impact reads the revisions of its plans in one statement (D-453).
- **The persisted switch agrees.** D-448's switch writes the same states and the same published_at. Only the two revision rows' version and updated_at change when the switch is persisted; the plan row does not change (D-448).
- **Where it applies.** The plan read and list, the revision read, the item read, /resolve and the prices unit's live impact use it (D-453, D-454; plan rev 2 M2, M3). So do the checks: the published revision that a deprecated SKU may be carried from (D-408) is the one in effect today.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decision 3); phase 8 review (behaviour B2; docs L1).

#### D-448 [M] The storage writes of a scheduled revision: schedule, switch, unschedule and the due scan

**Status:** DECIDED 2026-09-29.

The doors and the job of run 8.2 call four storage functions of plan_revision_repo. Each one is conditional on the state it reads, so a lost race changes nothing.

- **schedule(tenant, id, unit, now).** A pending revision that the unit holds becomes scheduled. pending_unit_id becomes null, approved_by_unit_id names the unit, the version increases and updated_at is now. published_at stays null. Any other state, or another unit's revision, is 409 REVISION_NOT_PENDING, and a second scheduled revision of the plan is 409 REVISION_SCHEDULED_EXISTS. The write does not read the date: the apply decides between schedule and publish (D-446).
- **switch_due(tenant, plan_id, now).** This persists the plan's due switch in the caller's transaction. First it finds the due scheduled revision (state scheduled, available_from on or before the UTC date of now). Then it supersedes the stored-published revision, publishes the due one with published_at at 00:00 UTC of its date, and writes published_rev through plan_repo::advance_published. That write changes neither the plan's version nor its updated_at: published_rev is a projection, so an If-Match that a client read before the switch stays valid (plan rev 2 L6). The two revision rows increase their version and set updated_at, as every write does. The result names the superseded revision (none for a plan's first publication), the published revision, its unit, its rev_no and its book: what a PlanRevisionPublished event names. There is a result only when the update from scheduled to published changed its row. Nothing due, or a switch that another writer made first, returns nothing and writes nothing. A predecessor superseded with no revision published after it is refused with 409 STALE_REVISION and not committed. A scheduled row that names no approving unit is a corrupt row, and nothing is written.
- **unschedule(tenant, id, now).** A scheduled revision that is not yet due (its available_from is after the UTC date of now, or null) becomes an unlocked draft. approved_by_unit_id becomes null, the version increases, and its items stay. The state condition is the concurrency control; there is no If-Match (plan rev 2 M5). Any other state, or a due revision, is 409 REVISION_NOT_SCHEDULED. A draft or pending revision beside it is 409 REVISION_DRAFT_EXISTS (the open index). The applied unit stays applied in its history.
- **due_scheduled(today, limit).** This is the job's scan: the due scheduled revisions of every tenant, ordered by available_from and then id, at most limit. It reads across tenants on purpose (AccessScope::allow_all(), as the reference ticker's scans do). The job then switches each plan in its tenant's scope.
- **Now, not today.** switch_due and unschedule take the instant now and not a date. The revision rows need it for updated_at, and today is always its UTC date, so a caller cannot pass two times that disagree.
- **The callers.** The apply calls schedule (D-449). plan_revisions::catch_up calls switch_due, enqueues the event and writes the audit row plan_revision.switch under the system actor (plan rev 2 L2), for the ticker duty (D-450) and the copy, clone and unschedule doors (D-451). The unschedule door calls unschedule (D-452).
- **The tests.** Each write's from-states, an idempotent switch, a switch that races itself (two connections on SQLite, two pools on Postgres: exactly one switches), a second scheduled revision refused, the plan's version unchanged by a switch, and the due scan, on both dialects. A probe of each behaviour was armed, caught by these tests, and reverted.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decisions 4, 5, 7; plan review L2, L6, M5).

#### D-449 [M] An approval before the sale date schedules the revision

**Status:** DECIDED 2026-09-29.

A plan change can be approved today and take effect on a later date (D-446). The apply decides which.

- **The choice.** The apply of a plan_revision unit judges the checks again, as before. Then it compares the revision's available_from with the UTC day of the apply. A later date stores the revision scheduled through plan_revision_repo::schedule (D-448): nothing is superseded, the plan's published revision and published_rev do not move, and the subject's superseded() stays None. A null date, or a date on or before that day, publishes at once, as before.
- **The unit.** The unit is applied in both cases: it is approved, its ApprovalUnitDecided is enqueued, and the receipt says applied. The receipt's revision reads scheduled, with a null published_at. Quorum 0 applies at the submit with the same choice.
- **No event at the apply.** The approval door enqueues PlanRevisionPublished only for a revision that its apply published now (PlanRevisionSubject::published_now). A scheduled revision is announced at its switch (D-450).
- **The tests.** A future date schedules through an approving vote and at quorum 0; a date of today publishes at once, and the tests of a null date stay green unchanged.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decision 1, run 8.2).

#### D-450 [M] The switch job persists a due switch on its date and announces it once

**Status:** DECIDED 2026-09-29.

Every read shows a due switch at once (D-453). The job makes it exact in storage and announces it.

- **The duty.** The pricing ticker runs a switch duty first in its tick: on the first tick, then every 60 ticks (reference_ticker::SWITCH_EVERY), about once a minute at the one-second tick. The period is fixed in the code, as the ticker's other knobs are; BssPricingConfig has no key for it (plan rev 2 M4).
- **Its own error handling.** The duty never fails the tick. A failed scan, or a failed plan, is a warning, and the other plans go on. It runs before the reference duties, so their early returns and a failing reconciliation cannot skip it. A test pins this with a reconciliation that fails because the registry is absent. The tick count moves right after the duty and before the reference duties, so a reference scan that keeps failing cannot hold the count off the duty's period either. A second test fails the reference-op scan on 180 ticks in a row (its table is renamed), and the due revision is switched on the 60th of them.
- **The scan.** plan_revision_repo::due_scheduled on the UTC day of the ticker's clock, across tenants (allow_all), at most the ticker's limit (100), by available_from and then id. Each plan is switched once per cycle, in its own serializable transaction, in its tenant's scope. A due revision beyond the limit waits for the next cycle.
- **One transaction per plan.** plan_revisions::catch_up runs switch_due (D-448). Only when it switched does the same transaction enqueue PlanRevisionPublished and write the audit row plan_revision.switch under pricing's system actor (plan rev 2 L2). A switch that another writer made first returns nothing and announces nothing, so each switch is announced exactly once.
- **The event.** It has the apply's type and shape: the plan, the revision, its rev_no and its book, superseded_revision_id from the switch (null for a plan's first publication), and unit_id, the revision's approved_by_unit_id. actor_ref is the actor of the unit's latest approving decision that is not stale and is of the unit's current generation. At quorum 0 the unit records no decision, so actor_ref is the unit's submitter (plan rev 2 H1). It is never the system actor.
- **No second judgement.** The switch does not run the checks again. The apply judged the revision on its sale date. The reference registry fences a SKU. An approved price chain loses its coverage only to a successor. A book's dates can move under a published revision today, and GET /plan-revisions/{id}/checks shows red in both cases. A value chain that ends explicitly is the same case (plan rev 2 L7). A SKU that is deprecated between the apply and the date is not. While the revision waits, its checks show ITEM_SKU_DEPRECATED red when the published revision does not carry the SKU. From the date the revision is in effect and carries the SKU itself, so the row is ok (D-408). The checks derive the revision in effect (D-447), so the row does not change when the job persists the switch.
- **Pins.** A switch moves no subscription pin (D-394).
- **A corrupt row.** A scheduled revision whose unit is gone fails its plan's switch, which is tried again every cycle, while the reads already show the switch. It keeps its place in the due scan's order, so once it is among the 100 oldest due revisions it takes one of the limit's places in every cycle until it is repaired.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decision 5; plan review H1, M4, L2, L7); phase 8 review (behaviour B1, B2, B4).

#### D-451 [M] The copy, clone and unschedule doors catch a due switch up; one scheduled revision at a time

**Status:** DECIDED 2026-09-29.

- **Where.** POST /plans/{id}/revisions (the copy), POST /plans/{id}/clone and POST /plan-revisions/{id}/unschedule call plan_revisions::catch_up first in their transaction: after the key's claim, before they judge anything. A catch-up that switched enqueues the job's event and audit row; the audit row carries the request's correlation. When the door then refuses, the catch-up rolls back with it, and the job persists the switch later.
- **The copy.** After the catch-up, a revision that waits for its sale date refuses a new draft: 409 REVISION_SCHEDULED. Withdraw it (D-452) or wait for its date. A due revision has just been switched, so the copy is of the revision in effect. *Counter-argument:* an operator cannot prepare the next change while one waits. Accepted for now: a chain of future revisions is a separate design.
- **The clone.** The clone copies the stored published revision. After the catch-up it is the revision in effect, so from 00:00 UTC of the date the clone copies the new revision, also when the job is down.
- **Nowhere else (plan rev 2 M1).** The draft's PATCH and DELETE, the item writes, the submit and the apply never meet a scheduled sibling. The open index admits one draft or pending revision per plan, the copy refuses a new draft while a revision waits, and unschedule turns the waiting revision itself into the draft. A catch-up in those writes would be dead code.
- **The invariant.** No draft or pending revision stands beside a scheduled one. A test drives every door that opens or moves a revision (the copy, the draft's PATCH and DELETE, the submit, the item writes, the votes on the applied unit, the clone and the plan create) and then finds the plan with exactly its published and its scheduled revision.

D-463 amends this entry: the clone copies the source's sale date unless its body names another date, which overrides it, or null, which clears it.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decisions 4, 6; plan review M1). Amended by D-463.

#### D-452 [M] A scheduled revision can be withdrawn to a draft

**Status:** DECIDED 2026-09-29.

- **The door.** POST /bss-pricing/v1/plan-revisions/{id}/unschedule, under plan submit (D-418): it withdraws an approved change, which plan author alone may not do (plan rev 2 M5). It takes an Idempotency-Key with the replay store's claim and answer, as every pricing POST does (D-396), and no body. It takes no If-Match: its write is conditional on the state scheduled, and that is its concurrency control.
- **The order.** Authorization, the key's claim, the revision (404 for one the tenant does not hold), the plan's catch-up (D-451), then the state.
- **The codes, by the state after the catch-up.** A published revision, stored so or due and just switched, is 409 REVISION_IN_EFFECT. A draft, pending or superseded revision is 409 REVISION_NOT_SCHEDULED.
- **The answer.** 200 with the draft (PricingPlanRevisionDto): approved_by_unit_id null, the version moved, its items and their references kept. Its ETag is its version, which its PATCH takes. The audit row is plan_revision.unschedule, under the caller. There is no event, and the applied unit stays applied in its history.
- **After it.** The draft is edited as any draft is: D-404's author rule is unchanged, so its created_by edits it. It is submitted again as any draft is; without a date it publishes at once.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decision 7; plan review M5).

#### D-453 [M] Every read derives the effective state; the counts read the stored state

**Status:** DECIDED 2026-09-29.

- **The reads.** Every read that renders a revision's state renders the state it reads today (D-447): domain::plan::effective over its plan's stored revisions. These are the plan read and list (each revision header's state and published_at, and published_rev through domain::plan::published_rev), GET /plans?sku_id=, the revision read, the item read's state, and the prices' impact plans (the approval queue's card and list, the publish-changes preview). A prices unit's stored snapshot records the impact of its own day and stays history. /resolve is D-454. The checks (GET /plan-revisions/{id}/checks) judge a deprecated SKU against the revision in effect today, which they derive from the revisions that they read already (D-408, D-447).
- **No write, and no statement more for a list.** A read never writes. The plan list derives in memory over the revisions that it reads already, so it keeps its two statements; the QueryRecorder pins this with due revisions among the plans. The revision read, the item read and /resolve read their plan's revisions once more, and the impact reads the revisions of its plans in one statement.
- **A write answers what it wrote.** The answers of the writes (the create, copy, clone, PATCH, submit and unschedule answers) render the stored state, which is the state the write wrote. No draft or pending revision stands beside a due one (D-451), so the two agree.
- **The counts.** The counts read the stored state and filter state <> 'superseded': stats.plans and BOOK_IN_PLAN (D-441, D-444), the entry usage and the SKU usage's plans (D-428), the in-plan SKUs (products P-D-212) and the plans GET /plans?sku_id= keeps (D-434). They count a scheduled revision as they count a published one. They also count a due revision's stored-published predecessor until the switch is persisted. The job ends that over-count: it persists at most the ticker's limit (100) due revisions a cycle (a minute), oldest available_from first, so a backlog of more than 100 drains 100 a cycle, and a plan whose switch keeps failing over-counts until it is repaired (D-446, D-450). A test reads a due revision before the persist: its predecessor's book still counts the plan, and after the job's tick only history names that book. plans_superseded_only stays "only superseded".
- **Where the texts changed.** products-sdk SkuUsage.plans and SkuUsageSets.in_plan, products P-D-212, D-408, D-413, D-419, D-428, D-434, D-441, D-444, the published_rev field of PricingPlanDto, and the served texts of the entry usage, the book stats, the two entry reads, the book list and the book delete.

D-460 amends this entry: the plan list derives each plan's current revision and the one in effect in memory as well, and reads the current revisions' items in one statement more, so it makes three statements; the QueryRecorder pins them with due revisions among the plans.

D-461 amends this entry: the plan list also reads the units its revisions name, one statement more: four. A write answers what it wrote with the unit it holds: the submit receipt's revision carries its unit's instants.

D-484 amends this entry: every plan write that matches a row refreshes the stored summary, one select and one update of the summary columns only. The plan's version and updated_at stay the If-Match clock. advance_published does not refresh; switch_due refreshes once after it. delete_unpublished does not: the row is gone. A no-op catch_up adds no statement.

D-485 amends this entry: GET /plans makes five statements for a non-empty page. GET /plans/counts is one grouped statement under the list's narrowing.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decisions 3, 10; plan review M2, L5); phase 8 review (behaviour B2, B3, B4; docs M1, L2). Amended by D-460, D-461, D-484, D-485.

#### D-454 [M] Resolve serves a scheduled revision from its sale date

**Status:** DECIDED 2026-09-29.

- **The state.** GET /resolve judges the revision by the state that it reads today (D-447), not by its stored state.
- **Due.** Once due, stored or derived, a revision resolves like any published revision, on every date (D-419), and its predecessor like any superseded one. So an answer does not change when the job persists the switch; a test asks the same questions before and after the persist (plan rev 2 M3).
- **Waiting.** A revision that still waits for its date resolves on a date on or after its available_from, with the resolved state scheduled: PricingResolvedRevisionState gains scheduled, after published and superseded. A date before its available_from is 409 REVISION_NOT_YET_AVAILABLE. A scheduled revision without a date is never due (D-447) and never available.
- **Draft and pending.** They stay 409 REVISION_NOT_PUBLISHED.
- *Rejected alternative:* fence every revision by its available_from. It would change D-419 for revisions that were published at once.

**Source:** Owner, 2026-09-28; phase 8 plan rev 2 (decision 8; plan review M3).

#### D-455 [M] The outbox wakes its sequencer after the commit

**Status:** DECIDED 2026-09-29.

- **The defect.** Since the main sync, toolkit-db's Outbox::enqueue does not mark its partition dirty. It returns a Wake, which marks the partition and wakes the sequencers when it is fired, after the commit (toolkit-db 2bfc76aec). events::enqueue fired that Wake at once, inside the caller's transaction. A sequencer woken then read the partition before the commit, found nothing and cleared the flag. The committed row then waited for the cold reconciler, a minute at the default profile.
- **The handle.** events::TxOutbox is the event sink as one transaction sees it. events::enqueue takes it in place of the EventSink. It adds each event's Wake to the handle and fires nothing. The clones of a handle share it.
- **The transaction.** events::transaction runs the work in the retrying transaction, with a new handle over the gear's sink. A retried attempt first discards the wakes of the attempt before it, which rolled back. When the transaction commits, the handle fires once. When it fails, the handle is discarded. The doors call it through support::transaction_with_events, support::unit_transaction_with_events and support::unit_transaction_door_with_events. These keep the isolation, the retries and the codes of support::transaction and the unit transaction: serializable, CONTENDED or UNIT_CONTENDED.
- **The writers.** Every writer that enqueues runs in it. These are the submit doors of a price, of publish-changes and of a plan revision, and the vote door (PricesPublished, PlanRevisionPublished, ApprovalUnitDecided). They are also the copy, clone and unschedule doors and the switch job, which catch a due switch up (D-450, D-451), and the commit of the reference work, which marks a reference lost (PriceBookEntryReferenceLost, PlanReferenceLost). No approval subject of pricing enqueues in its apply: the doors enqueue after the engine returns, in the same transaction.
- **The approval engine.** bss-approval changes no signature. An effect that may act only after the commit never passes through the engine: the gear keeps it, and the ApprovalSubject doc says so.
- **The census.** A test pins that TxOutbox::new occurs in src only in events::transaction, and that no other file fires or discards a wake. The census of the unit doors also requires their _with_events transaction.
- **The tests.** tests/broker_producer.rs drives the real in-process broker over a database with four connections. With one connection the sequencer queues behind the transaction, and the race does not show. A committed enqueue is delivered at once, although its transaction goes on for 300 ms after it. A rolled-back one wakes nothing: a row committed before it with its wake discarded stays undelivered in the same partition. A retried attempt's wake is dropped. The two events of a quorum-zero price submit are delivered at once. A probe that fires the wake inside the transaction again turns the first test red.
- **The interim envelope** (whole-branch review PS-21). Without a broker, events::enqueue writes the SDK's producer-outbox envelope by hand (events::interim_envelope), with producer_mode stateless, on the queue a bound producer later reads (bss_pricing_events). Stateless is right although the bound producer registers as monotonic. The interim arm has no producer registration, so it knows no producer_id, and the SDK's processor refuses a monotonic or chained envelope without one (ProducerOutboxEnvelope::to_event). A held monotonic row would never drain. A stateless row drains once a broker is bound, published without the producer's sequence, so the broker does not deduplicate it by sequence; every row enqueued after the bind carries it. A test deserializes the interim envelope as event_broker_sdk::producer::ProducerOutboxEnvelope and serializes it back unchanged, so a change of the SDK's envelope fails that test.
- *Rejected alternative:* each enqueue returns its Wake, and every function on the path returns it to the transaction (the toolkit's outbox::in_transaction and main's gears). The engine's apply returns nothing, so the products subjects would need a second mechanism. An error after an enqueue would also drop an unfired Wake, which the toolkit logs as a leak. The handle gives both gears one design (products P-D-221).

**Source:** Main sync of 2026-09-29 (sync report, port 3; toolkit-db 2bfc76aec); phase 8 plan rev 2 (run 8.2b).

#### D-456 [M] A plan names only a book its author may read

**Status:** DECIDED 2026-09-29.

- **The defect.** POST /plans and a PATCH /plan-revisions/{id} with a book_id checked the book with a tenant scope only, and the only PDP decision of the request was plan author. POST /plans/{id}/clone named the source's book the same way. So a caller could attach any book of its tenant, one its price_book grants exclude included. Once that revision was published, GET /resolve served the book's prices under plan read, while GET /price-book-entries/{id}/prices refuses the same money without price_book read (D-440).
- **The rule.** These three doors judge price_book read a second time, as D-440 judges the money, on the book the plan names: the body's book_id for the create and the PATCH, the source's published revision's book for the clone. The grant must admit that book. A denial, or a grant whose scope does not admit the book, is 403 PRICE_BOOK_READ_REQUIRED, and nothing is written. A policy that cannot judge is 503.
- **The order.** plan author first (403), then the money's policy (503), then the body (400), the book the tenant does not hold (404), then 403 PRICE_BOOK_READ_REQUIRED. A PATCH judges the money only when it names a book, so it reads its body first: one without book_id asks nothing of the money's policy. The clone judges the book it will copy, after a due scheduled revision is switched (D-451), and the refusal rolls the switch back with the rest.
- **What does not change.** The copy (POST /plans/{id}/revisions) keeps its plan's own book and judges nothing more. Resolve still reads under plan read (D-424); from now on a revision names only a book its author could read when the book was named.
- **The tests.** tests/book_reads.rs judges the three doors under a grant narrowed to another book (403) and under a policy that cannot judge (503), with nothing written, and then under a grant that admits the book. A caller holding plan author alone is now refused POST /plans and the clone (tests/plan_doors.rs, tests/plan_clone.rs).
- **Breaking for a caller** that holds plan author without price_book read on the book: POST /plans, the clone and a book-naming PATCH now answer 403. The downstream e2e plan authors hold every pricing action (CATALOG_AUTHOR), and its narrower actors are refused plan author first, so its grants suffice; a new scenario would give one actor every grant but price_book read and expect the three 403s.

D-463 extends this entry: a malformed available_from on POST /plans or the clone is 400 DATE_INVALID, among the body's refusals: after the money's policy (503) and before the 404 of the book or of the source plan.

D-468 extends this entry: a new plan's code off the rule is 400 PLAN_CODE_INVALID, among the body's refusals, after PLAN_CODE_REQUIRED and before DATE_INVALID.

**Source:** Whole-branch review of 2026-09-29, PS-08 (fix run W1a; the orchestrator's scope decision). Extends D-440. Extended by D-463, D-468.

#### D-457 [M] Every text a request writes has an explicit length cap

**Status:** DECIDED 2026-09-29.

- **The defect.** A book's and a plan's code and name had only a blank check, and a price's and a vote's note, the settings' GL code, tax category and invoice line templates, an entry's invoice line override and the dimension keys and values had none at all. The request body limit was their only bound, while one list page repeats a book's code and name up to 500 times. Only a book's description had a cap (D-444).
- **The caps**, counted in characters (Unicode scalar values), as a description always was: a code 64 (a book's, a plan's, a new dimension key, a new dimension value); a name 200 (a book's, a plan's); a note or a description 2000 (a price's note, a vote's note, a book's description); a GL code and a tax category 64 (the settings' default_gl and default_tax_category); an invoice line template 2000 (an entry's invoice_line_override, each template of the settings). The values live in domain::caps; the note cap is the approval engine's NOTE_MAX_CHARS, and a const assertion ties the two.
- **The refusal.** 400 FIELD_TOO_LONG with the field named (a field violation on code, name, invoice_line_override, default_gl, default_tax_category, invoice_line_templates, key, values or add), the shape NOTE_TOO_LONG already has. A note keeps NOTE_TOO_LONG on note, and a description BOOK_DESCRIPTION_TOO_LONG (D-444). Each door judges its body right after it parses it, before it reads or writes anything, so a too long text is refused before a 404 or a 409. The two full-replace PUTs are the exception: the PUT of the dimension registry and the settings PUT judge their texts against the stored row, after their If-Match (below). The vote note is judged by the approval engine (bss-approval, NoteTooLong) before its first write, on approve and on reject.
- **Stored rows.** Only a write is judged, only on the fields its body carries, and only writes of new text are judged. A stored row over a cap stays readable, and a PATCH that does not carry the field leaves it as it is. A text that must name a stored row is never capped: a price's dim_value (400 DIM_VALUE_UNKNOWN otherwise), an entry's dimension_key (400 DIM_NOT_DECLARED otherwise), and the registry PATCH's key and the values it removes. The PUT of the registry caps only a key the stored registry does not hold, and only the values their key does not hold. So a key or a value stored before the caps and longer than 64 characters never locks the registry: every PUT must carry a key an entry names (else 409 DIMENSION_KEY_IN_USE) and a value a price uses (else 409 DIM_VALUE_IN_USE), and it passes; a PATCH names the key and removes a value nothing uses (the second review of W1a, L1; tests/dimension_values.rs). The settings PUT replaces the whole row, so every write sends the stored texts back: it caps only a default_gl or a default_tax_category other than the stored one, and only a template of invoice_line_templates other than the one stored for its SKU type. So settings stored before the caps with a longer GL code, tax category or template never lock the settings: a write that changes only the rounding passes, and only a changed text is refused (the second review of W1b, L2; tests/settings_doors.rs).
- **The tests.** tests/book_writes.rs sends one text over each cap, door by door, and each is 400 with its code and field, with nothing written; the caps themselves pass in two-byte characters. tests/approval_doors.rs does the same for the vote note.
- **Products** applies the same caps in its own decision, P-D-225 (fix run W1b). The downstream e2e would add one refusal: a book created with a 65-character code is 400 FIELD_TOO_LONG on code.

D-468 extends this entry: a new plan's code also follows a rule of at most 32 characters; the 64-character cap is judged first.

**Amended 2026-10-02 (phase 9 review F1).** An entry create's usage-rating policy caps its quantity strings, each 400 `FIELD_TOO_LONG` on that field, before the meter provider is asked: `usage_type_id` and `accrual_policy_version` at 512 characters (Products' cap on a SKU's `usage_type_ref`, which the meter must equal; a derived accrual version is `derived-v1:` plus 64 hex digits, 75 characters), `version` and `unit` at a code's 64. A list's `q` (the plans list, the book list, and `GET /price-book-entries`) is at most a name's 200 characters, the same code on `q`, before it becomes a pattern.

**Source:** Whole-branch review of 2026-09-29, PS-09, PS-10 and X-01 (fix run W1a; the dispositions' "Length caps"); the second reviews of W1a (L1) and W1b (L2). Extended by D-468. Amended 2026-10-02 (phase 9 review F1): the policy's quantity strings and a list's `q`.

#### D-458 [M] The approval-unit list pages and reads its page set-based

**Status:** DECIDED 2026-09-29.

- **The defect.** GET /bss-pricing/v1/approval-units answered every unit of the tenant that its filters kept, and read each unit's items, its decisions and its impact's plans with statements of their own: a tenant with many units got an unbounded answer in a number of statements that grew with it.
- **The page.** The list takes limit (200 by default, clamped at 500, the book list's rule, D-442) and cursor, the opaque continuation of a page's page_info.next_cursor, the toolkit pager's cursor as the book list's. The order stays submission order (submitted_at), with the unit id breaking a tie. The answer is { items, page_info }: items keeps its shape, and page_info (next_cursor, prev_cursor, limit) is added. The cursor carries a hash of the narrowing (state, kind and the referenced aggregate, ref_id or book_id), so a cursor replayed under another narrowing is 400 FILTER_MISMATCH. A cursor that does not read is 400, and a limit that is not a number 400 QUERY_INVALID.
- **The reads.** A page reads its units, then all their items, all their decisions and the plans their impact names, each in one statement (infra::prices::PlansReading, the four statements of plans_reading over the page's entries). The QueryRecorder shows the same statements for 10 and for 100 units, each with an item, a vote and a plan naming its entry.
- **Breaking for a caller** that reads the list whole: a tenant with more than 200 units matching its filters gets them over several pages, and must follow next_cursor. The Studio (the pricing-mfe) has to follow it. The gears-rust tests that read the list whole follow it (entry_support::Fixture::all_units); the gears-rust e2e reads no list. The downstream e2e reads it whole in its tenant-isolation checks (two reads) and its pending-queue price checks, each with far fewer than 200 units, so they pass unchanged; a downstream change would only make them follow next_cursor.
- **Products** gets the same list in its own decision, P-D-224 (fix run W1b).

D-470 amends this entry: the list also takes $orderby=submitted_at desc, newest first, while submission order stays the default, and impact=false, which answers every unit's impact as null and skips the plans' reading (infra::prices::PlansReading, four statements).

**Source:** Owner, 2026-09-29 (the dispositions' O2, answered "2"); whole-branch review PS-13 (fix run W1a, a scope addition). Amended by D-470.

#### D-459 [M] One approve-eligibility predicate for the engine and its readers

**Status:** DECIDED 2026-09-30.

- **The risk.** A read that shows whether its caller may approve a unit, or how many votes the unit has, would copy the rules of the approval library's private rules module: the terminal state, the separation of duties, the duplicate vote and which votes count. A copy drifts: the read says the caller may approve, and the vote door answers 403 SOD_VIOLATION.
- **The rule.** bss_approval exports one function, approve_eligibility(unit, items, decisions, actor), and its result ApproveEligibility { approvals, refusal }. approvals counts the approve votes of the unit's current generation that are not stale; a reject, a stale vote and a vote of an earlier generation do not count. refusal is none when the actor may approve, else the engine's refusal, in the engine's order: UNIT_ALREADY_DECIDED for a terminal unit, SOD_VIOLATION for its submitter or an item author, DUPLICATE_VOTE for an actor who voted in the current generation. The items are the unit's stored items, which are the current generation's: a stale refresh rewrites them. The rules module stays private.
- **The engine uses it.** evaluate_approve, which Engine::approve calls once it has loaded the unit at the reviewer's generation, judges through it: its refusal is the vote's error, and an eligible vote pends at approvals + 1 or applies. So the engine answers as before: the library's rule tests keep their expectations, and both gears' door tests pass unchanged.
- **The readers.** A pending plan revision's progress reads approvals through it (D-462). Products and pricing compute caller_can_approve through it in run 9.3.
- **The tests.** A table over quorum 0, 1 and 2, a stale vote, a vote of an earlier generation and a decided unit, for the submitter, an item author, a voter of each generation and a fresh reviewer: the predicate's refusal is evaluate_approve's error, and a counted vote's have is approvals + 1. An engine test drives Engine::approve through a quorum-2 unit: a refused submitter, a first vote, a duplicate, a content drift and its refresh, the first reviewer's vote on the new generation, the apply and a vote after it. Before each vote the predicate over the stored rows answers what the vote meets. A probe that counted stale votes turned three tests red.

D-393's engine keeps its rules; this entry makes them one function that the readers call.

The phase 9 review's theme E amends this entry (fix run 9.5d-1; R3, R4, R39): approve_eligibility(unit, authors, decisions, actor) takes the authors of the unit's stored items (any iterator of their ids), not the items: it judges only who authored an item, so a reader may read the authors alone. The count is its own function, counted_approvals(unit, decisions), over the decisions alone, and ApproveEligibility's approvals is that count. Its refusal is ApproveRefusal (AlreadyDecided, SodViolation, DuplicateVote), which converts into the ApprovalError the engine answers. evaluate_approve passes its items' authors and answers that error, so the engine's refusals are byte-identical: the library's rule tests keep their expectations, a test pins each refusal's code and text, and both gears' door tests pass unchanged. Pricing's progress counts the decisions alone (D-462); the flag reads the item authors alone where it does not need the items (D-471, products P-D-228). A probe that counted stale votes turned four library tests red.

**Source:** Phase 9 plan rev 2 (W2, binding: the review's alternative W2; L5 for #40's counts). Amended by the phase 9 review, theme E (fix run 9.5d-1): the authors, the count apart, a three-variant refusal.

#### D-460 [M] The plans list names each plan's current revision and the one in effect

**Status:** DECIDED 2026-09-30.

The plans screen shows, per plan, the revision being changed or waiting and the one it sells today (ask 27). The list carried only the revision headers, so the screen read every revision to count its items.

- **The rule.** GET /plans and GET /plans/{id}, and every answer of PricingPlanDto (the create, the clone and the rename), carry two objects:
  - current, PricingPlanCurrent { revision_id, rev_no, state, item_count, sku_ids, created_by }: the draft or pending revision (a plan holds at most one, D-451), else the scheduled one still waiting for its date, else the published one in effect. It is chosen over the states the revisions read today (D-447; domain::plan::current), so a due scheduled revision whose switch is not persisted is current, and reads published. item_count counts every item, an included item without an entry too; sku_ids names each item's SKU, in ascending order; created_by is the revision's author, who edits it while it is a draft (D-404), not the plan's.
  - in_effect, PricingPlanInEffect { revision_id, rev_no }: the published revision in effect today (domain::plan::in_effect), the one the plan sells.
  - current is null for a plan without revisions; in_effect is null before the first publication.
- **Not the SKU filter.** sku_ids names every item of the current revision; GET /plans?sku_id= keeps a plan for an item with an entry, judged on the stored state of its draft, pending, scheduled or published revisions (D-434). The two may differ, and the served text says so.
- **The reads.** The items come from one grouped read over the listed plans' current revisions (plan_item_repo::skus_of_revisions: revision and SKU only). GET /plans makes three statements whatever the number of plans: the plans, their revisions and the current revisions' items; one when the tenant has no plan. The grouped read runs for an empty list too, so the count does not depend on the rows. The QueryRecorder shows the same statements for 10 and for 100 plans.
- **The write answers.** A write answers what it wrote (D-453): the create answers its empty draft (item_count 0), the clone its new draft with the items it copied, and the rename the plan as its read shows it.
- **No ready flag.** The list carries no ready flag and no count of red checks: the checks read every item SKU from Products (D-408), so the list would make a Products read per SKU of every draft. The screen reads GET /plan-revisions/{id}/checks per draft, or GET /plan-revisions/checks for up to 50 drafts in one read (D-482).
- **The tests.** tests/plan_overview.rs: the current revision and the one in effect for a draft only, a pending only, a published only, a draft, a pending and a waiting scheduled revision beside the published one, a due scheduled revision whose switch is not persisted (both name it, and its stored state stays scheduled), and a plan without revisions; the plan read agrees with its list row; sku_ids against the SKU filter; the create, clone and rename answers; the statements for 10 and 100 plans. domain::plan's tests pin the choice over the effective states. Probes that chose the published revision before the scheduled one, and that read the items per plan, were caught.

D-461 amends this entry: the plan list also reads the units its revisions name, in one grouped statement more, so GET /plans makes four statements whatever the number of plans (one when there is none): the plans, their revisions, the current revisions' items and the units.

D-480 amends this entry: in_effect gains sku_ids, the item SKUs of the revision in effect, in ascending order. The grouped items read names the current revision and the one in effect, still one statement, so GET /plans stays at four. A draft beside a published revision shows the published revision's SKUs, not an empty list and not the draft's.

**Source:** Owner, 2026-09-30 (validation 3 item 1); phase 9 plan rev 2 (decision 1; plan review M6, M7, W3, L4). Amends D-434 and D-453. Amended by D-461, D-480, D-482.
D-485 amends this entry: current gains book { code, name, currency }, the book of that revision. GET /plans reads those books in one grouped statement, so a non-empty page is five statements.

D-516 amends this entry: each revision header gains book { id, code, name, currency } beside book_id. The book read is that same grouped statement, over every revision's book. A non-empty page stays five statements.

D-519 extends this entry: the plan, `current` and each revision header carry `created_by_name` beside `created_by`, resolved after the page's statements in one Account Management lookup. A non-empty page stays five statements.

**Source:** Owner, 2026-09-30 (validation 3 item 1); phase 9 plan rev 2 (decision 1; plan review M6, M7, W3, L4). Amends D-434 and D-453. Amended by D-461, D-480, D-485, D-516. Extended by D-519.

#### D-461 [M] A revision says who made it and when it was submitted and approved

**Status:** DECIDED 2026-09-30.

The plans screen shows who made each revision and when it was submitted and approved (ask 33). A header carried its state and published_at only, and a scheduled revision has a null published_at until its date.

- **The fields.** PricingPlanRevisionHeader gains created_by and created_at, the row's; the revision read already carries them. Both the header and PricingPlanRevisionDto gain submitted_at and approved_at:
  - submitted_at is the submission instant of the unit the revision names: its pending unit (pending_unit_id), or the unit that approved it (approved_by_unit_id).
  - approved_at is the approving unit's decided_at. A scheduled revision needs it, because its published_at is null until its date.
  - A draft names no unit, so both are null: also a draft back from a reject or a withdraw (its lock is cleared) or from an unschedule (approved_by_unit_id is cleared, D-452). A superseded revision keeps its own unit's instants. A unit that does not read leaves both null.
- **The reads.** The plan read and list read the units that their revisions name in ONE grouped statement (approval_repo::unit_instants: id, submitted_at and decided_at only), so GET /plans makes four statements whatever the number of plans (D-434, D-453, D-460). The revision read reads the one unit its revision names. The instants are read under plan read: two instants, no actor and no content (the owner's O-9a covers them with D-462).
- **The write answers.** A write answers what it wrote (D-453). The submit receipt's revision carries its new unit's submitted_at, and approved_at when quorum 0 applied it at once; the copy, the revision PATCH and the unschedule answer a draft, with neither.
- **The tests.** tests/plan_overview.rs: the header of a pending, a scheduled, a published and a superseded revision against its unit's own read, and of a draft back from a reject, a withdraw and an unschedule; the revision read agrees; created_by and created_at are the row's; the write answers; the list's four statements for 10 and 100 plans. Probes that took submitted_at from the approving unit only, and that dropped the instants from the receipt, were caught.

D-519 extends this entry: on a read, the header's and the revision's `created_by` carry `created_by_name`, the author's current name; the write answers' is null.

**Source:** Owner, 2026-09-30 (validation 3 item 7); phase 9 plan rev 2 (decision 2; plan review M6, M7, L1, L9). Amends D-434,
D-453 and D-460 (GET /plans makes four statements, not three). Extended by D-519.

#### D-462 [M] A pending revision shows its vote progress under plan read

**Status:** DECIDED 2026-09-30.

The plan's screen shows how far a pending revision is from its quorum (ask 40).

- **The grant (O-9a).** The owner ruled on 2026-09-30 that a plan reader may see the vote progress of a pending revision, counts only, without the approval-unit read grant. *Counter-argument:* it tells a reader that a change is in review and how close it is, which before needed the unit grant.
- **The field.** PricingPlanRevisionDto gains approval, PricingPlanApprovalProgress { unit_id, approvals, quorum_required }, while the revision reads pending, and null in every other state: on GET /plan-revisions/{id} and on every answer of the DTO. A submit receipt of a pending unit carries approvals 0; one applied at once carries null.
- **The count.** approvals is D-459's count through bss_approval::approve_eligibility over the unit, its stored items and its decisions: the approve votes of the current generation that are not stale, the votes the vote door counts. A duplicate vote adds nothing, and a stale refresh turns earlier votes stale, so they stop counting. quorum_required is the unit's own. No actor id, note or snapshot is shown; the unit's reads keep their grant.
- **The reads.** The revision read reads its unit, and for a pending one its items and its decisions: at most three statements more, only for a revision that names a unit.
- **The tests.** tests/plan_overview.rs: quorum 2 through its votes (approvals moves as the receipt's have does, a duplicate vote adds nothing, a refresh makes the first vote stale and it stops counting, the apply clears the field), quorum 1 and quorum 0 at the submit, a draft; a caller holding plan read only reads it. Probes that counted stale votes, that dropped the progress from the read and from the receipt, were caught.

The phase 9 review's theme E amends this entry (fix run 9.5d-1; R13, R45, R52): the count is bss_approval::counted_approvals over the unit's decisions alone (D-459), so the revision read reads its unit and, for a pending one, its decisions: at most two statements more (it read the unit's items too, only to discard them). The progress takes no reader: GET /plan-revisions/{id} answers the same counts to every plan reader. The submit receipt reads its new unit's item authors and its decisions once each and builds both the progress and the unit from them (it read each twice). tests/plan_overview.rs pins both: the receipt reads one items statement and one decisions statement, and the read of a pending revision none and one.

D-480 amends the revision read's statements, measured after 9.5d-2 and then with the new reads. Before: a draft is 3 (the revision, its items, its plan's revisions); a pending revision is 5 (those, its unit and its decisions); a scheduled, published or superseded revision is 4 (those three and its unit). The read adds the entries, the admission of their books and the sale-date prices, 3 on every state, and the in-effect items, 1 on a draft or a pending revision. The pins are absolute: draft 7, pending 9, scheduled 7, published 7, superseded 7, the same SQL for 10 items and for 100. reservations_settled is computed from the items in hand and adds no statement, on reads and on writes. D-481 amends the plan-read grant: it also serves quorum_required on the checks, at no statement more.

**Source:** Owner, 2026-09-30 (O-9a, "yes"); phase 9 plan rev 2 (decision 4; plan review M7, L5). Amended by the phase 9 review, theme E (fix run 9.5d-1): the count from the decisions alone, each row read once. Amended by D-480 and D-481.

#### D-463 [M] A plan's sale date on create and clone

**Status:** DECIDED 2026-09-30.

A new plan's first revision took its sale date only through a second call, the revision PATCH (ask 34). The date decides at approval whether a revision is scheduled or published (D-449).

- **The create.** POST /plans takes an optional available_from, YYYY-MM-DD: rev 1's sale date. Omitted or null means "at publish", as before.
- **The clone.** POST /plans/{id}/clone takes an optional available_from. Omitted, rev 1 keeps the source's sale date, as before (D-451). A date overrides it. Null clears it: "at publish". The clone request tells the three apart, as the revision PATCH does.
- **The refusal.** The date is judged as the revision PATCH judges it: a date that does not read is 400 DATE_INVALID on available_from, and nothing is written. It is one of D-456's body refusals: after the money's policy (503), with PLAN_CODE_REQUIRED, before the 404 of the book (the create) or of the source plan (the clone). Like the PATCH, the door refuses only a date that does not read; the checks judge the sale date (D-408).
- **The texts.** The served texts of the create and the clone name the field and DATE_INVALID.
- **The tests.** tests/plan_doors.rs: a create with a date, without one, with null, and with a malformed date on a known and on an unknown book (400 both times, nothing written). tests/plan_clone.rs: a clone that keeps, overrides and clears the source's date, and a malformed date on a known and an unknown source plan; the source is unchanged. Probes that dropped the create's date and ignored the clone's override were caught.

**Source:** Owner, 2026-09-30 (validation 3 item 7); phase 9 plan rev 2 (decision 3; plan review L10). Amends D-451; extends D-456.

#### D-464 [L] A plan submit and a publish-changes carry the submitter's note

**Status:** DECIDED 2026-09-30.

The approvals screen shows why a unit was submitted (ask 44). Products' submit doors take a note (P-D-219); pricing's took none (D-445), so a plan change or a batch of prices reached its approver without one.

- **The plan submit.** POST /plan-revisions/{id}/submit takes an optional body { note } (PricingPlanRevisionSubmitRequest; the served body is optional), products P-D-219's body rule: no body, {} and note: null carry no note, and any other key is 400 BODY_UNEXPECTED, the rule of the empty body it replaces. A note that is neither text nor null is 400, as a body that does not read. The Idempotency-Key's digest covers the body as sent ({} for none).
- **Publish-changes.** POST /price-books/{id}/publish-changes takes an optional note beside price_ids and common_effective_date; omitted or null carries none. Its body rules are otherwise unchanged: it still needs a JSON object, and a key it does not know is 400 as before.
- **The cap.** Both doors judge the note right after they parse the body, before they read anything: at most 2000 characters, counted as Unicode scalar values (bss_approval::NOTE_MAX_CHARS, D-457), else 400 NOTE_TOO_LONG on note, and nothing is written. So a revision or a book the tenant does not hold answers the 400 too. The engine's submit caps no note, so the door is its only judge.
- **Where it goes.** The note passes through the library's SubmitRequest.note onto the unit's submit_note: the submit receipt and every unit read carry it. It is not content (D-445): neither the snapshot nor the fingerprint carries it.
- **The single price.** POST /prices/{id}/submit takes no note: its body stays empty, and any key is 400 BODY_UNEXPECTED. A note for prices travels with publish-changes. Its served text says so.
- **The tests.** tests/plan_revision_approvals.rs: a note stored and read back; no body, {} and a null note; 2000 two-byte characters pass; 2001 are 400 NOTE_TOO_LONG on a revision and on an unknown one, with nothing written; a stray key alone and beside a note is 400 BODY_UNEXPECTED. The pin that the plan submit takes no body now pins that it takes nothing but a note. tests/approval_doors.rs: publish-changes stores a note and a null one, refuses one over the cap before the 404 of an unknown book, and the price submit refuses a note. Probes that dropped either door's cap were caught.

The phase 9 review (fix run 9.5d-2; R5, R18, R64) adds one parse and its tests: once its keys are judged, the plan submit's body is read into the served PricingPlanRevisionSubmitRequest, so the schema and the parser are one definition; every answer is unchanged. tests/plan_revision_approvals.rs: a note that is a number, a list, an object or a boolean is 400 as a body that does not read, with no unit written; one Idempotency-Key replays the receipt for no body then {}, {} then no body and one note sent twice, and is 409 IDEMPOTENCY_CONFLICT for another note, for {} then {"note":null} and for {"note":null} then no body: the digest covers the body as sent.

**Source:** Owner, 2026-09-30 (validation 3 item 6); phase 9 plan rev 2 (decision 5; plan review L3). Amends D-445 and products P-D-219's Pricing bullet. Amended by the phase 9 review (fix run 9.5d-2).

#### D-465 [M] A revision may carry again a deprecated SKU its plan sells

**Status:** DECIDED 2026-09-30.

The checks accept a deprecated SKU that the plan's published revision in effect carries (D-408), but the item door refused every deprecated SKU. So an item removed from a copied draft could not be added back, although the copy had carried it (ask 36).

- **The ruling (O-9b).** The owner ruled on 2026-09-30 that D-408's "newly added" does not cover a re-add. *Counter-argument:* make the check strict instead, so a deprecated SKU never re-enters a draft; not chosen.
- **The rule.** POST /plan-revisions/{id}/items admits a deprecated SKU when the plan's published revision in effect today (D-447) carries it. Any other deprecated SKU stays 400 ITEM_SKU_DEPRECATED on sku_id. A clone is a new plan with no revision in effect, so its drafts add no deprecated SKU; the SKUs it carried stay red in its checks (D-408).
- **One source.** infra::plan_revisions::published_skus (plans::published_skus before the phase 9 review's theme F moved it below the doors) gives the item SKUs of the published revision in effect, from the same function the checks read them with (stored_context). The door judges it in its admissibility read, before any claim or reservation. The create op's SKU re-read (reference_work::observe_sku) judges it on its reserving step, reading the op's revision and its plan on the clock's day, so the create answers what the door answers. A revision that stops carrying the SKU between the two is judged again by the checks at submit and at apply.
- **Unchanged.** An attach and a rereserve admit a deprecated SKU as before (D-413). A draft, retiring or retired SKU and a bundle SKU stay refused.
- **The texts.** The served text of the item create names ITEM_SKU_DEPRECATED and the rule.
- **The tests.** tests/plan_item_doors.rs: a SKU deprecated after its plan's publication, removed from the copy and added back (201, confirmed, its check green); another deprecated SKU refused; the carried SKU refused in a clone. tests/plan_item_references.rs: the create op, below the door, writes a carried deprecated SKU and refuses another with the door's answer, its op keeping SKU_DEPRECATED. A probe of the door alone turned the door test red and left the op's green; a probe of the op turned both red.

**Source:** Owner, 2026-09-30 (O-9b, "yes"); phase 9 plan rev 2 (decision 6; plan review L2). Amends D-408.

#### D-466 [M] Each check row names its items and its blocking prices

**Status:** DECIDED 2026-09-30.

The plan's items table marks the items that a failing check is about (ask 29). A check row named its items only in the prose of detail, and blocked_by named approval units only, so the screen kept a copy of the rules.

- **The fields.** Each row of GET /plan-revisions/{id}/checks, and each row of the REVISION_CHECKS_RED body (whose detail is the red rows as the checks door renders them), gains two fields. code, ok, label, detail, info and blocked_by keep their names and their meaning.
  - subjects, [PricingPlanCheckSubject { item_id, sku_id, price_book_entry_id }]: the items that turn the row red, in the revision's item order. price_book_entry_id is null for an item that names no entry. A green row names no item. A plan-wide row names none either: PLAN_NAME, PLAN_BOOK, PLAN_BOOK_VALIDITY, PLAN_ITEMS and the information rows DESCRIPTORS and APPROVAL.
  - blocked_by_prices, [PricingPlanCheckBlockingPrice { unit_id, price_id, price_book_entry_id }]: the pending prices behind blocked_by, one per price, ordered by unit and then by price. blocked_by is built as before: the units that hold a pending price of an entry that an uncovered item names, on any chain, because the default chain can cover a value. The units of blocked_by_prices are exactly blocked_by.
- **Which items.** An item-level row names the items that its detail lists. ITEM_ENTRY_MISSING, ITEM_ENTRY_SKU_MISMATCH, ITEM_ENTRY_LOST, ITEM_BUNDLE_SKU, CHARGE_KIND_SKU_TYPE, ITEM_BOOK_FOREIGN, ITEM_UNCOVERED, ITEM_SKU_DEPRECATED, ITEM_SKU_UNAVAILABLE, ITEM_REFERENCE_PENDING and ITEM_REFERENCE_LOST name each item they refuse. METER_DUPLICATE names both items of each pair that meters one usage type. FREQUENCY_MIXED names every recurring item priced in the plan's book, because each bills in one of the mixed periods. Only ITEM_UNCOVERED has a blocked_by, so only it has blocked_by_prices.
- **The reads.** No read is added: the checks' context already holds each item, the entry it names and that entry's pending prices with their units.
- **The tests.** src/domain/plan_tests.rs: a table over every code that the checks emit, enumerated from what the checks answer over the table's contexts, each context turning its code red (the information rows stay green) and naming its items; in every context a green row names no item and no price, and each row's blocked_by is exactly the units of its blocked_by_prices; an uncovered entry's pending prices, one row per price. tests/plan_item_doors.rs: ITEM_UNCOVERED names its item and the pending price of a submitted prices unit, and names neither once the unit is approved. tests/plan_revision_approvals.rs: the REVISION_CHECKS_RED body carries both fields and equals the checks door's red rows. Probes that dropped the blocking prices, the mixed periods' items and the DTO's subjects were caught.

The phase 9 review (fix run 9.5d-2; R19, R20, R55, R56, R57): the domain keeps the blocking prices alone and derives blocked_by from them (domain::plan::Check::blocked_by and ItemCoverage::blocked_by: the distinct units of the prices, in order), so the two cannot drift. The domain's Subject and BlockingPrice name their aggregates (item, sku, entry; unit, price, entry); only the DTOs carry the wire names of ask 29. The wire is unchanged. src/domain/plan_tests.rs: FREQUENCY_MIXED beside a recurring item priced in another book still names the plan book's recurring items only.

**Source:** Owner, 2026-09-30 (validation 3 item 5); phase 9 plan rev 2 (decision 7). Extends D-408. Amended by the phase 9 review (fix run 9.5d-2).

#### D-467 [H] A plan item is a SKU and its entry: no treatment, no included quantity, no minimum quantity

**Status:** DECIDED 2026-09-30.

The owner asked for how long an included quantity is included, and learned that it had no defined period, no proration and no rollover rule. The owner then removed the field for now, and with it the treatment and the minimum quantity. A plan item is now a SKU and its entry in the plan's book.

- **The API.** POST /plan-revisions/{id}/items takes sku_id and price_book_entry_id, and both are required: a missing or null entry is 400 ITEM_ENTRY_MISSING on price_book_entry_id. PATCH /plan-items/{id} takes only price_book_entry_id; a null one is 400 ITEM_ENTRY_MISSING, and so is a PATCH that leaves an item without an entry. At both doors a body that carries treatment, included_qty or qty_min is 400 BODY_UNEXPECTED on that key, the rule for a stray key, judged with the body, before any read; any other key the body does not know is refused by its parse, as before. TREATMENT_INVALID, INCLUDED_QTY_INVALID and QTY_MIN_INVALID are gone.
- **The reads.** No read shows the three fields: the item read and every item answer (PricingPlanItemDto), the revision read, GET /resolve (PricingResolveItemDto) and the snapshots of the units submitted from now on. The treatment's closed set (PricingTreatment) is gone from the served spec.
- **Storage.** There is no migration: the columns and their CHECKs stay. Every row written from now on stores treatment = 'paid', included_qty = NULL and qty_min = NULL (plan::stored_treatment): the item create op, the copy and the clone, the item PATCH and the revision PATCH's book remap, which rewrite the row they change in that shape. The repository's two writers derive that shape themselves (plan_item_repo::insert and update_draft, the phase 9 review's theme G, fix run 9.5d-1), whatever the model they are given carries: no caller sets the three columns, so a writer added later cannot keep a legacy row. insert_as_given and update_draft_as_given write a row as given; no door calls them, and they seed the suites' legacy rows and the column CHECKs' tests. tests/plan_repositories.rs: a legacy treatment and quantities passed to the insert and to the draft update are stored paid, NULL and NULL, and an entry-less item included with no quantity. A probe that wrote the given shape turned 21 tests red. The one exception is the copy of a legacy item stored without an entry: its entry stays null, so the column's CHECK keeps its treatment 'included', with no quantity. A create op stored before D-467 carries the old fields in its input; they are ignored.
- **Legacy rows.** Rows stored before D-467 keep reading, with the fields hidden. A legacy included item answers price_book_entry_id: null, and resolves with no chains. Published history is not rewritten: a published or superseded revision keeps its rows as stored.
- **The checks.** INCLUDED_QTY and every branch keyed on a treatment or a quantity go. FREQUENCY_MIXED, METER_DUPLICATE and ITEM_UNCOVERED stay. An item without an entry is ITEM_ENTRY_MISSING, whatever it was stored as, and meters nothing; so a legacy included item in a draft is ITEM_ENTRY_MISSING, and its author removes it or gives it an entry. The row's label is "Every item points at a price".
- **The units pending at the deploy.** The fingerprinted content of a plan revision is its book, its sale date and its items by SKU, each item its SKU and its entry; before D-467 it also held each item's treatment and quantities. Pricing re-derives a unit's content on every vote, so every plan_revision unit pending at the deploy finds its content changed once. Measured in tests/plan_items_legacy.rs:
  - the first approve or reject refreshes the unit at the next generation, records no vote, and answers 400 UNIT_STALE with the new generation; the votes of the older generation stop counting;
  - a unit whose revision holds only priced items (the deployed database's qty_min item) then applies on the approve of the new generation;
  - a unit whose revision holds a legacy included item is refused at the apply of the new generation, 409 APPLY_REFUSED with REVISION_CHECKS_RED: ITEM_ENTRY_MISSING, and nothing is published; the reject of the new generation returns the revision to a draft, whose author fixes it. No unit is left that can be neither approved nor rejected.
- **The frozen read contract (D-419 to D-422).** treatment, included_qty and qty_min leave each resolved item; the consumer goldens under tests/contract/ were recorded again, and they differ only by these keys. The pinned price read (D-422) is unchanged. No code outside pricing reads the three fields; the ledger's treatment is another concept, and pricing-sdk has none.
- **Cross-gear effects.**
  - No optional (add-on) and no included items remain in pricing plans.
  - Main's orders PRDs assumed optional items: the orders-lifecycle PRD's add-on selection on the order line (gears/bss/orders-lifecycle/docs/PRD.md, the submit gate's add-on bounds and the resolved open question on add-on selection) and the orders-changes PRD's "Add-On Selection on the Order Line". They are named here and not edited.
  - Rating's T-D-38 floor is applied "after included quantities", and that step has no operand now; Rating's register records the amendment (gears/bss/rating/docs/DECISIONS.md).
  - The downstream e2e sends and asserts treatment; it follows in run 9.5.
- **The tests.** src/domain/plan_tests.rs: the checks without a treatment, an entry-less item ITEM_ENTRY_MISSING and metering nothing, the check list without INCLUDED_QTY. tests/plan_item_doors.rs: each key refused at both doors with nothing reserved or written, the entry required, a null entry refused, a new row stored paid with no quantity. tests/plan_item_references.rs: the create op writes paid with no quantity. tests/plan_items_legacy.rs seeds the deployed database's shapes (an included item in a draft, in a pending revision and in a published one; a qty_min item pending and published; two pending units whose content carries the old fields) and reads, resolves, copies, checks, approves and rejects them as above. tests/response_enums.rs: no plan item schema carries the keys, and the create requires its entry. Probes that let a key through, copied the source's treatment and quantities, fingerprinted the treatment again, spared an entry-less item from ITEM_ENTRY_MISSING, kept the PATCHed row's shape and wrote a quantity from the create op were caught.

D-512 amends this entry: `POST /plan-revisions/{id}/items` takes `price_book_entry_id` as optional. Absent or null adds an entry-less item and reserves the SKU. The checks still show it `ITEM_ENTRY_MISSING` ("Every item points at a price"), and submit, the scheduled apply and publish refuse the revision with `REVISION_CHECKS_RED`, the code any red check already answers. A PATCH may set the entry; a null PATCH stays `ITEM_ENTRY_MISSING` and never clears an entry. Copy, clone and a book remap keep an entry-less item entry-less and never choose an entry. The row stores `treatment` `included`, the shape the column CHECK admits.

**Source:** Owner, 2026-09-30 (the phase 9 plan's section "plan items lose treatment, included_qty and qty_min"); phase 9 plan rev 2 (run 9.2). Amends D-388, D-394, D-407, D-413, D-419, D-420, D-421 and D-439. Amended by the phase 9 review, theme G (fix run 9.5d-1): the repository writes the row shape. Amended by D-512.

#### D-468 [M] A new plan's code follows a declared rule

**Status:** DECIDED 2026-09-30.

A plan's code had a blank check and a length cap only (D-457), stored as sent, so `PRO`, `pro` and `PRO ` could coexist under the exact unique index (ask 39). The owner asked for a rule (the scope addition to run 9.2).

- **The rule.** On POST /plans and POST /plans/{id}/clone the new plan's code matches `^[A-Z0-9][A-Z0-9_-]{0,31}$`: 1 to 32 characters (domain::plan::CODE_MAX) of upper-case ASCII letters, digits, `-` and `_`, starting with a letter or a digit (domain::plan::code_follows_the_rule). Otherwise the answer is 400 PLAN_CODE_INVALID on field code, and nothing is written.
- **As sent.** The code is judged as sent, with no trim and no case folding: `pro`, `PRO ` and ` PRO` are all invalid, and a valid code is stored as sent.
- **The order.** The length cap comes first, with the body, before any read: a code of 65 characters or more is still 400 FIELD_TOO_LONG (D-457). Then a blank code is 400 PLAN_CODE_REQUIRED, as before. Then the rule: a code of 33 to 64 characters, or any other code off the rule, is 400 PLAN_CODE_INVALID. Then the rest, in D-456's order: DATE_INVALID, the book's or the source plan's 404, PRICE_BOOK_READ_REQUIRED, and 409 PLAN_CODE_TAKEN.
- **Stored codes are grandfathered.** There is no migration. A code stored before the rule keeps reading and is never judged again: the plan reads, lists, is renamed (the plan PATCH takes a name only and judges no code) and is cloned, and only the clone's new code is judged. The deployed environment holds 113 lower-case codes such as `plan-6dfd4733` from the e2e. Uniqueness stays exact, as before; because every new code is upper-case, no two plans created from now on can differ in case alone.
- **Out of scope.** The codes of books, SKUs and categories.
- **The texts.** The served texts of both doors name the rule and PLAN_CODE_INVALID in their refusal lists.
- **The e2e.** The gears-rust e2e creates upper-case plan and clone codes. The downstream e2e (its plan helper, and its clone codes) creates lower-case codes; it is fixed in run 9.5 and must land with this image.
- **The tests.** tests/plan_codes.rs: valid codes (one character, a digit, `-` and `_`, 32 characters) stored as sent; each invalid shape refused with its code and nothing written (lower and mixed case, a trailing, a leading and an inner space, a leading `-` or `_`, a dot, a non-ASCII letter, 33 and 64 characters: PLAN_CODE_INVALID; empty and blank: PLAN_CODE_REQUIRED; 65 characters: FIELD_TOO_LONG); the order against the book's 404, a malformed date and an over-long name; the clone under the same rule, before the source's 404; a grandfathered lower-case plan that reads, is renamed and clones to a valid code, and a new upper-case code beside a stored lower-case one. The suites' plan fixtures now create upper-case codes. Probes that dropped the rule, judged a blank code by the rule, skipped the clone's code and admitted lower case were caught.

**Source:** Owner, 2026-09-30 (ask 39; backend asks validation 3, "confirm there is no charset rule"; phase 9 plan rev 2 L8 had moved it to Out as an open question). Extends D-456 (the order of the body's refusals) and D-457 (the code's cap).

#### D-469 [M] The served contract declares every door's 503, every ETag it sets and the refusals of the plan doors

**Status:** DECIDED 2026-09-30.

The generated client of the pricing-mfe did not type a 503 (ask 30): no pricing op declared one, while products declares it on all of its ops. Validation 3 also found served texts that did not say what their doors do (its contracts notes). This entry changes the served contract only: every door already answered as below.

- **The census.** The ops are the 51 that register_rest mounts: tests/rest_support/mod.rs lists them, and its harness asserts that the registered set equals the list; tests/rest_authz.rs, tests/module_test.rs, tests/skeleton_contract.rs and tests/authoring_doors.rs count the same 51.
- **503 on every op.** Every door judges its caller at the policy decision point first, and a decision point that cannot answer is 503 (support::authz_failure), so every op declares a 503 problem response, as products does. D-434, D-440 and D-456 name the 503 of a second judgement, on the money or the book.
- **REGISTRY_UNAVAILABLE on the ops that read Products hard.** The served texts name the Products dependency on exactly eight ops, measured at the base of run 9.2 (9.1 changed the item create, which still reads the SKU): GET /plan-revisions/{id}/checks, POST /plan-revisions/{id}/items, POST /plan-revisions/{id}/submit, POST /approval-units/{id}/approve (a plan revision's checks at apply, a usage chain's dated metering), POST /price-books/{id}/entries, GET /resolve, and POST /prices/{id}/submit and POST /price-books/{id}/publish-changes (a usage chain's dated metering, D-402). A reject never answers a Products 503: the stale refresh's descriptor read is best-effort (D-416), and tests/plan_revision_approvals.rs rejects under an outage. An item PATCH reads nothing from Products, and tests/plan_item_doors.rs PATCHes under one. The copy, the clone and the deletes drive their reference ops best-effort and never answer it.
- **ETag.** Every answer that sets an ETag declares the header on its success response: the eight reads that declared it before, and the nineteen write answers that set one, found by a census of the doors: POST /price-books, PATCH /price-books/{id}, PUT /settings, PUT and PATCH /dimension-keys, POST /price-books/{id}/entries, PATCH /price-book-entries/{id}, PUT /approval-policy, DELETE /approval-policy/{kind}, POST /price-book-entries/{id}/prices, PATCH /prices/{id}, POST /plans, PATCH /plans/{id}, POST /plans/{id}/revisions, POST /plans/{id}/clone, PATCH /plan-revisions/{id}, POST /plan-revisions/{id}/unschedule, POST /plan-revisions/{id}/items and PATCH /plan-items/{id}. The deletes that answer 204, the submits, publish-changes and the votes set none.
- **The texts (the contracts notes).**
  - The revision PATCH: a new book remaps each item to the new book's entry of the same SKU, charge kind, period and model (D-427); an item with no such entry keeps its own and its checks show ITEM_BOOK_FOREIGN; book_id omitted or null leaves the book unchanged.
  - The item create lists every refusal its door answers, from a census of the door: 400 BODY_UNEXPECTED (D-467), ITEM_ENTRY_MISSING, ITEM_BOOK_FOREIGN, ITEM_ENTRY_SKU_MISMATCH, ITEM_SKU_DEPRECATED, ITEM_BUNDLE_SKU and REVISION_ITEMS_TOO_MANY; 403 NOT_DRAFT_AUTHOR; 404 for the revision and ENTRY_NOT_FOUND for the entry; 409 REVISION_NOT_DRAFT, ITEM_SKU_TAKEN, IDEMPOTENCY_CONFLICT, IDEMPOTENCY_KEY_IN_FLIGHT, and SKU_FENCED, SKU_RETIRING or SKU_DRAFT from its create op; Products' own refusal of the SKU read; 503 REGISTRY_UNAVAILABLE. D-512 amends this census: the create no longer answers `ITEM_ENTRY_MISSING`. That code stays the checks row for an item with no entry.
  - The revision delete names STALE_REVISION, the answer of a concurrent write that changed the revision or one of its items first.
- **The tests.** tests/served_contract.rs reads the served spec: a 503 problem on each of the 51 ops; REGISTRY_UNAVAILABLE in the text of exactly the eight; the item create's, the revision PATCH's and the revision delete's texts, and the plan code rule's (D-468). tests/module_test.rs pins the ETag on the success answers of the eight reads and the nineteen writes, and nowhere else. Probes that dropped one op's 503, named the registry on the reject, dropped the unschedule's ETag and dropped STALE_REVISION from the delete's text were caught.

D-470 extends this entry: the approval-unit counts are one op more, so the census, the 503 on every op and the tests count 52 ops, not 51.

The phase 9 review (fix run 9.5d-2; R27, R28): tests/read_contract.rs measures the writes as it measures the reads. Each of the 30 write ops is called on a live fixture with what its success needs, and exactly those that declare an ETag on a success answer set one; module_test's census is no longer the only check of the writes. tests/served_contract.rs fails on the text of an op the spec does not serve, so a check that a text does not name a code reads a text that is there.

D-518 extends this entry: the plan list, the plan counts and the book list declare a weak ETag of their JSON body on their 200 and their 304, and the settings read declares its version ETag on its 304 too. module_test counts 34 ETag declarations, not 27.

**Source:** Owner, 2026-09-30 (validation 3 item 2, approved); phase 9 plan rev 2 (M1 and W1, binding: 503 on every op; decisions 8 and 9; L8: the contracts notes correct served texts to match the code). Extended by D-470. Amended by the phase 9 review (fix run 9.5d-2). Extended by D-518.

#### D-470 [M] The approval units are counted by state and kind, list newest first on request, and skip the live impact on request

**Status:** DECIDED 2026-09-30.

The approvals screen of the pricing-mfe merges pricing's and products' units (the owner's option 1: two per-gear methods that the UI merges). It shows a badge per state and per kind, the newest units first, and does not need the live impact on the queue (ask 42).

- **The counts.** GET /bss-pricing/v1/approval-units/counts answers PricingApprovalUnitCounts { by_state { pending, approved, rejected, withdrawn }, by_kind { prices, plan_revision }, total }: every state and every kind is named, 0 when none, and total is the number of units the list pages through under the same narrowing.
  - It takes the list's whole narrowing (plan review L11): state, kind, and the referenced aggregate, ref_id or its alias book_id. The list and the counts read one condition (approval_repo::UnitListFilter), so the badge and the list count the same set.
  - A narrowing the list refuses is refused the same way: 400 UNIT_STATE_INVALID on state, 400 QUERY_INVALID on kind for a kind pricing does not record (below), 400 QUERY_INVALID on book_id for a ref_id and a book_id that differ, 400 QUERY_INVALID on query for a malformed id. It takes nothing but the narrowing: limit, cursor, $orderby, impact and any other key are 400 QUERY_INVALID, whose violation names the rejected key, on the counts as on the list (the phase 9 review's R43, fix run 9.5d-1).
  - It counts in ONE grouped statement (approval_repo::count_units: state, kind, count, grouped by state and kind), whatever the number of units. A stored kind pricing does not record is a corrupt row (500), as on every unit door.
  - It reads that statement on the plain connection, outside any transaction (the phase 9 review's R32, fix run 9.5d-1): one statement is its own snapshot, and the doors' serializable transaction would take read locks over the scanned units that can push concurrent submits and votes into serialization failures. tests/book_reads.rs pins the statement outside any transaction.
- **The kind is a closed set (the phase 9 review's theme C, fix run 9.5d-1).** The list and the counts take a kind pricing records, prices or plan_revision: any other kind, an empty one or another case included, is 400 QUERY_INVALID on kind, judged with the narrowing, before the filter or the cursor's hash is built. Before, any text was taken, of any length, and counted zero. The repository reads a stored unit's kind through the same set (every unit read, and count_units' rows: approval_repo::UnitCount), and PricingApprovalUnitDto.kind is the enum PricingApprovalKind (D-439). So the list, the card, the receipts, the votes and the counts refuse a unit of another kind alike, with a 500 that does not name it. The cursor's hash is unchanged: it hashes the kind's stored name, as before (a1a21e85af067d2d for kind=prices). tests/book_reads.rs: kind=promotion, an empty kind, PRICES and a kind of 5000 characters are refused alike by the list and the counts, on kind. tests/approval_kinds.rs: a unit of an unknown kind is 500 on the list (bare, by state, with impact=false), on the counts (bare and by state) and on the card, while a narrowing that does not keep it serves; a state written around its CHECK is 500 on the list, the counts and the card. tests/response_enums.rs: the kind is an enum.
  - It is authorized as the list is: approval_unit read. The route census holds 52 ops (D-469's 51 and the counts), and the counts declare 503 as every op does.
- **The descending order.** The list takes $orderby=submitted_at desc, newest first, and submitted_at asc (or submitted_at alone), the submission order of D-458 and still the default. The unit id breaks a tie in the same direction. Any other $orderby is the toolkit's 400 INVALID_ORDERBY_FIELD (an order on another field, a second key, a direction other than asc or desc). Its violation names the key it refuses: the other field, or "only one key, submitted_at, is accepted" for a second submitted_at key; it named the whole order before, which called submitted_at unsupported (the phase 9 review's R67 and its twin here, fix run 9.5d-1).
  - The order is not part of the narrowing's hash (plan review M4). The cursor carries its order (CursorV1.s: `+submitted_at,+id` or `-submitted_at,-id`), and a continuation follows the cursor's order, so every cursor minted before this decision still continues ascending, never 400 FILTER_MISMATCH.
  - $orderby beside a cursor is the toolkit's 400 ORDER_WITH_CURSOR, judged first, as its OData extractor judges it: a continuation sends only its cursor.
  - **Declared through the toolkit** (the phase 9 review's theme I: R41, R42, R36; fix run 9.5d-2). The list declares its order with .with_odata_orderby::<UnitOrderField>(), a one-field enum (submitted_at), so the served contract lists submitted_at asc and submitted_at desc under x-odata-orderby, and $orderby is that one parameter (its text is the toolkit's; the op's text keeps the default, the tie-break and the refusals). The door accepts exactly the declared field. It keeps its own parse rather than take the toolkit's OData extractor, which keeps the three rules above but answers other requests differently: it refuses limit=0, of which the list reads one unit as the house pager does (D-486 states the same for a SKU's entries), with 400 INVALID_LIMIT, and its refusal of a cursor that does not read drops the cause (invalid cursor, not invalid cursor: malformed JSON). The served behaviour is unchanged. The order has one source: without a cursor the door puts it on the page's query (approval_repo::submission_order: submitted_at, then the id, in one direction), and approval_repo::page_units reads it from there alone, ascending when the query names none; a continuation follows its cursor's order. The page takes no separate direction any more. tests/served_contract.rs pins the declared fields and the one $orderby; tests/book_reads.rs pins limit=0 in both orders.
- **The merge contract** (shared with products P-D-227). A client merging the two gears' pages compares submitted_at as an instant, never as text: the RFC 3339 rendering trims trailing zeros of the fraction and writes UTC as Z, so as strings `…:00Z` sorts after `…:00.5Z` (the P-D-213 trap). It then compares the unit id as lower-case hex, in the same direction.
- **impact=false** (plan review M3). The list takes impact=false to skip the live impact read: every unit answers impact: null, and no plan is read. The page still reads its units, their items and their decisions; only the plans' reading (infra::prices::PlansReading, four statements) is skipped. impact=true is the default; a value that is not a boolean is 400 QUERY_INVALID. Pricing has no impact_live: the field is impact.
- **The texts.** The served texts of the list and the counts name the order, the merge contract, impact=false and their refusals.
- **The tests.** tests/book_reads.rs: the counts against the list's own pages under nine narrowings, and the list's refusals refused alike by the counts; the counts in one grouped statement for 10 and 100 units; the newest-first order and every page size over a three-way tie; a cursor minted before this decision (its narrowing hash `a1a21e85af067d2d` for kind=prices, pinned as a literal), each order's cursor with the same hash and its own order, ORDER_WITH_CURSOR and INVALID_ORDERBY_FIELD; impact=false reading the units, their items and their decisions and no plan, the items otherwise equal to the full read's. tests/served_contract.rs: the counts op's 503, parameters, schema and text, and the list's. The route censuses (tests/rest_support/mod.rs, rest_authz.rs, module_test.rs, skeleton_contract.rs, authoring_doors.rs, served_contract.rs, read_contract.rs) count 52 ops.

D-490 amends this entry: the shared order is also the merge key of the approvals inbox (`bss-approvals`, AP-D-2). The inbox merges the two gears' pages by `(submitted_at, id)` itself, so the merge contract above is now the inbox's as well as a client's. The pricing source reads its page through this list's own read and pager, with a cursor it builds from the inbox's key.

**Source:** Owner, 2026-09-30 (the approvals option 1, "ok": two per-gear methods, per-gear counts and a descending order); phase 9 plan rev 2 (decision 10; plan review M3, M4, L11). Amends D-458 (the order and the impact); extends D-469 (52 ops, the counts' 503). Amended by the phase 9 review, theme C and R32 (fix run 9.5d-1): the kind is a closed set, and the counts read outside any transaction. Amended by the phase 9 review, theme I (fix run 9.5d-2): the order is declared through the toolkit and has one source. Amended by D-490: the order is the approvals inbox's merge key.

#### D-471 [M] A unit says whether its reader may approve it

**Status:** DECIDED 2026-09-30.

The approvals screen shows the Approve action only to a reviewer the vote door would take (ask 28). The rule is not a grant alone: separation of duties excludes the submitter and every author of the unit's items, and a reviewer votes once per generation. No unit DTO carried the item authors, so the screen could not judge it.

- **The field.** Every PricingApprovalUnitDto carries caller_can_approve, a boolean: on GET /approval-units, GET /approval-units/{id} and every answer that carries a unit (the submit receipts of a price, a plan revision and publish-changes, and the vote receipts). It is true when the caller may approve the unit now.
- **One rule (W2, D-459).** It is bss_approval::approve_eligibility(unit, items, decisions, caller).refusal.is_none(): the predicate Engine::approve judges through, over the unit's STORED items (the current generation's: a stale refresh rewrites them, as Engine::approve reads them) and its decisions. So it is false for a decided unit (UNIT_ALREADY_DECIDED), for the submitter and every item author (SOD_VIOLATION; an item's author is its price's creator, or the plan revision's), and for a caller who voted in the current generation (DUPLICATE_VOTE); a vote that a refresh made stale does not stop its voter. The flag and the door cannot drift: they are one function.
- **Approve only (plan review M2).** Engine::reject judges no separation of duties, only a duplicate vote, so the submitter and an item's author may reject a unit whose flag is false. The field's text says so.
- **Not the grant.** The flag does not judge approval_unit approve: without it the vote door still answers 403. The field's text says so. The caller's own identity, for the screen's other rights, comes from account-management's GET /account-management/v1/me.
- **What it does not see.** A vote may still meet a refresh (400 UNIT_STALE) when the content drifted since the last read, and an apply may be refused by its rules (409 APPLY_REFUSED); the flag judges the eligibility of the vote, not its outcome.
- **The reads.** No statement is added to the list: its page already reads its units' items in one statement (D-458) and their decisions in one more, and with impact=false it still reads the items, for the flag (D-470). The card reads the unit's items and decisions once each, as before. A receipt reads the unit's items and decisions (two statements).
- **The tests.** tests/approval_doors.rs: a prices unit at quorum 3, authored by one user and submitted by another, a vote in generation 1, a refresh to generation 2, a vote in generation 2; for an item's author, the submitter, the voter of this generation, a fresh reviewer and the voter of the earlier generation, the flag on the card and in the list (which agree) is exactly whether the vote door answers 200 (403 SOD_VIOLATION twice, 409 DUPLICATE_VOTE, then pending and applied); on the decided unit it is false for everyone and the door answers 409 UNIT_ALREADY_DECIDED; the submit and vote receipts answer false for their caller. The submitter's reject answers 200 while the submitter's flag, and the author's, is false. tests/served_contract.rs: the field is a required boolean whose text says Approve only and 403, and the list's and the card's texts name it.

The phase 9 review's theme E amends the reads (fix run 9.5d-1; R44, R46): where only the flag needs a unit's items, their authors alone are read (approval_repo::item_authors_of_units: each item's unit and author, one statement): the list with impact=false and the vote receipt. The list with its impact and the card read the items whole, which the impact needs, and take the authors from them. Every receipt reads its unit's items, or their authors, and its decisions once each: the receipt of publish-changes and of a price submit builds its prices and its unit from one read of the items (it read them twice), and the plan submit's receipt its progress and its unit (D-462). tests/book_reads.rs pins the light list's projection and the publish-changes receipt's reads: the items and the decisions once each under quorum 1, twice under quorum 0, where the apply's event and the decided event read them once more.

**Source:** Owner, 2026-09-30 (validation 3 item 4, "ok"); phase 9 plan rev 2 (decision 11; W2, binding; plan review M2). Uses D-459. Amended by the phase 9 review, theme E (fix run 9.5d-1): the flag reads the item authors alone, and each receipt reads each row once.

#### D-472 [M] An entry names its next price

**Status:** DECIDED 2026-10-01.

The Price Books screen shows, beside each entry's price in force, the price that comes next (ask 26). It read that price one entry at a time, from each entry's prices list.

- **The value.** The two entry reads, GET /price-book-entries/{id} and GET /price-books/{id}/entries, and the SKU's entry list, GET /price-book-entries?sku_id= (D-434), carry next_price beside current_price, a PricingPriceDto or null. It is the default chain's earliest scheduled price: an approved price that starts after the day the answer is judged on, today. Else it is the default chain's newest draft or pending price, else null.
  - "Earliest" is the lowest effective_from. Two approved prices of one chain never share a start (the pricing_price_approved_start index); if they did, the highest version_no would win, as it wins in force.
  - "Newest" is the highest version_no, then the latest created_at, then the highest id (plan review L7). version_no is unique per entry (UNIQUE (price_book_entry_id, version_no)), so the later keys only make the order total.
  - A draft or pending price is named whatever its effective_from, even a start that has passed.
  - A rejected price is never the next price, and neither is a dimension value's price: next_price is the default chain's, as current_price is (D-434).
  - Its status is its display status on the same day: scheduled for an approved price, draft or pending otherwise.
- **The money.** next_price follows D-434's rule, as current_price does. It is shown only when the caller's price_book read, judged a second time, admits the entry's book, and it is null otherwise. An unavailable policy fails the read with 503.
- **One read (plan review L7).** The read that current_price is chosen from is widened, not doubled. price_repo::default_chain (approved_default_chain before) reads the default chain's approved, pending and draft prices of the entries in the same ONE statement, where it read the approved ones. So no entry read gains a statement; only the rows read grow, by the chains' drafts and pending prices.
  - The book's entry list makes seven statements whatever the number of entries: the book, its entries, the book under the money's grant, the three usage reads (the price counts, the plan items that name the entries, and their revisions, a read skipped when no item names an entry) and the default chain.
  - The SKU's entry list keeps D-434's seven statements.
  - price_book_entries::headline (in_force before, D-440) chooses both prices. It decodes only the approved rows, which alone can be in force; domain::price::next_of, beside own_version_at, chooses the next price over a typed view of the chain (moved out of the door by the phase 9 review's theme F).
- **The frozen contract.** The contract goldens leave next_price out, as they leave current_price out (D-440): a frozen document holds no value of today. tests/book_reads.rs pins it.
- **The tests.** tests/book_reads.rs:
  - Twelve chain shapes: a scheduled price after the current one, stored before an earlier scheduled one; only drafts, where the higher version wins though it was written earlier and starts earlier; only pending prices; drafts and pending prices beside a rejected one; nothing after the current price; no price; only a rejected price; a scheduled price with nothing in force; value chains beside the default; a value chain's scheduled price beside the default's draft; a temporary pair in force, whose return is next; and a temporary pair to come, whose temporary price is next. For each, the book's list, the single read and the SKU's entries agree, and each headline price is the very price the entry's prices list answers.
  - The money: next_price is null, never absent, with entry read alone and with a price_book grant on another book, on each of the three reads.
  - The book's entry list in seven statements for 10 and for 100 entries.

  tests/served_contract.rs: the three texts name next_price, and its schema is current_price's. tests/entry_doors.rs: a new entry reads next_price null. tests/postgres_plans.rs: default_chain reads the default chain's approved and draft prices, and no value chain's or rejected price.

Breaking for a consumer that compares an entry read as a closed object: it gains next_price.

D-473 amends this entry: on GET /price-books/{id}/entries the day next_price is judged on is the list's as_of, today by default, so "scheduled" means an approved price that starts after as_of (plan review M5).

**Source:** Owner, 2026-09-30 (validation 3 item 7, "ok"; ask 26); phase 9 plan rev 2 (decision 12; plan review M5, L7). Amends D-434 and D-440. Amended by D-473.

#### D-473 [M] The book's entries list reads its prices on a date

**Status:** DECIDED 2026-10-01.

The Price Books screen shows a book's prices on a date the user picks: what was in force last month, or what will be in force after a scheduled change (ask 37). GET /price-books/{id}/entries took no query and judged every price on today.

- **The parameter.** GET /bss-pricing/v1/price-books/{id}/entries takes as_of, a YYYY-MM-DD date. Without it the day is today (UTC), as before.
- **One day per answer (D-440, plan review M5).** Every price the list answers is judged on as_of, never partly on today:
  - current_price is the default chain's approved price in force on as_of;
  - next_price is the default chain's earliest approved price that starts after as_of, else its newest draft or pending price (D-472). With an as_of after a scheduled start, that price is current_price and next_price is the one after it, or a draft or pending price;
  - each price's status is its display status on as_of (window_display): the current price is active on it and an approved next price is scheduled;
  - usage.prices splits the approved prices into scheduled, active and superseded on as_of, from the same grouped count bound to as_of. The plans counts read the stored state and are not dated (D-453).
- **Outside the book's validity.** An as_of before the book's valid_from, or on or after its valid_until, is not refused: the list answers with the prices in force on that day. domain::book::valid_on says whether the book allows a sale on a date, and the list does not judge it. So a price the list answers on such a day is not sellable, and the served text says so.
- **Refusals (plan review L6).** list_entries parsed no query and ignored every key. It now reads only as_of: any other key, and as_of twice, is 400 QUERY_INVALID, the house rule of pricing's reads (D-440, D-442). An as_of that is not a YYYY-MM-DD calendar date, an empty one included, is 400 DATE_INVALID on as_of, as resolve's date is. The order is D-440's, with the query in its place: 403 without price_book_entry read; 503 when the policy cannot judge the money; 400 QUERY_INVALID, then 400 DATE_INVALID; 404 for a book the tenant does not hold; then 403 PRICE_BOOK_READ_REQUIRED for another day without the money's grant (below).
- **Another day is money (phase 9 review R1, fix run 9.5d-1).** The usage split moves with the start and the end of every approved price, so a caller who could step as_of could bisect for the dates of every approved price of the book, which the prices list refuses it (D-440). An as_of other than today (UTC) therefore takes price_book read on the book, judged as D-434 judges the money: without the grant, or under a grant whose scope does not admit the book, the list is 403 PRICE_BOOK_READ_REQUIRED. It is judged after the 404 of the book, as D-440 and D-456 judge the money after the book, and before any price or usage is read. No as_of, and an as_of of today, answer as before: the usage on today, and both prices null without the grant. A caller with the grant reads any day. tests/book_reads.rs: an entry reader and a grant narrowed to another book read without as_of and on today (200, no money) and are refused three other days (403); a grant that admits the book reads all five; an unknown book is 404 on another day. tests/served_contract.rs: the list's text names the refusal.
- **Only the list.** The plan names the book's list only (decision 13). GET /price-book-entries/{id} takes no as_of: it still reads no query and stays dated on today. GET /price-book-entries?sku_id= stays dated on today and answers as_of with 400 QUERY_INVALID, as any other key (D-434).
- **The statements.** as_of changes only the bound date: the list makes the same seven statements with it as without it, for 10 and for 100 entries.
- **The tests.** tests/book_reads.rs: one entry with four approved default-chain prices, a pending and a draft price, and a value chain's price, read on six days — before every price, in the past, today, the day a price ends, a day after a scheduled start, and after the last scheduled start. On each day current_price, next_price, their statuses and the usage split are the ones of that day, the answer without as_of equals the answer with today's as_of, and the single read with an as_of still answers today. A book valid from today+10 to today+40, read before its valid_from, inside, on its valid_until and after it, answers the prices in force on each day. The refusals: seven malformed dates (DATE_INVALID on as_of) and five other queries (QUERY_INVALID), then the order 403, 503, 400 and 404. The seven statements with and without as_of for 10 and for 100 entries. tests/served_contract.rs: the as_of query parameter, not required, and the list's text naming as_of, DATE_INVALID, QUERY_INVALID, valid_from, valid_until and "not sellable".

Breaking for a caller that sends GET /price-books/{id}/entries a query key: the key was ignored and is now 400 QUERY_INVALID. The deploy notes name it.

D-483 amends this entry. The list takes as_of, limit (alias $top), cursor (alias $skiptoken) and $filter, so limit, cursor, $top and $skiptoken are no longer 400 QUERY_INVALID; any other plain key, or one given twice, still is. The cursor carries the day, so every page of a read is judged on its one as_of, and a cursor replayed on another day is 400 FILTER_MISMATCH. PRICE_BOOK_READ_REQUIRED is judged before any entry is read: at the base of run 9.8 the entries were read first, though no price or usage. The seven statements hold per page.

**Source:** Owner, 2026-09-30 (validation 3 item 7, "ok"; ask 37); phase 9 plan rev 2 (decision 13; plan review M5, L6). Amends D-440 and D-472. Amended by the phase 9 review, R1 (fix run 9.5d-1): an as_of other than today takes price_book read on the book (D-440, "every price is money"). Amended by D-483.

#### D-480 [M] A revision read carries its entries, sale-date prices and reservation state

**Status:** DECIDED 2026-10-01.

The plan page read a revision, then each entry, then each price (asks 48, 49, 50). GET /plan-revisions/{id} now answers what that page shows, and the plans list names the SKUs the revision in effect sells.

- **The answer.** GET /bss-pricing/v1/plan-revisions/{id} answers PricingPlanRevisionReadDto: PricingPlanRevisionDto flattened, plus sale_date, entries and carried_sku_ids. sale_date is domain::plan::sale_date: available_from, or today when the revision is sold from its publication. A past available_from answers that past date. entries is one PricingPlanEntrySummary per distinct entry the items name, in entry id order: the entry id, its book, charge kind, period, model, dimension key, and price_on_sale_date. price_on_sale_date is the default chain's approved price in force on sale_date, chosen by price_book_entries::headline (D-440, D-472), the same price the entries list would headline with as_of = sale_date. It is null when only value chains price the entry (its coverage check may still be green), when none is in force, or when the caller's price_book read does not admit the entry's book (D-434). carried_sku_ids is present on a draft or a pending revision and null otherwise: the SKU ids of the plan's published revision in effect today (D-447), [] when none is in effect. That is the set a re-add of a deprecated SKU judges (D-465).
- **Money.** One grouped admission of the entries' books under the caller's price_book read. An entry of a book that is not admitted has price_on_sale_date null. The money's 503 sits where D-440 places it: after the plan-read 403, before the 404.
- **Writes.** PATCH, copy, unschedule and the plan submit receipt keep serving PricingPlanRevisionDto. They gain no statement, no PDP call and no money. The replay body stays keyed by tenant, endpoint and key, not by caller (D-429).
- **reservations_settled.** PricingPlanRevisionDto gains reservations_settled, true when no item is unreserved or confirmation_pending. lost counts as settled, so settled is not a green check. It is computed from the items in hand on every answer, read and write alike (D-453), at no statement.
- **The reservations read.** GET /bss-pricing/v1/plan-revisions/{id}/reservations answers { items: [{ item_id, reference_state, reservation_id }], settled } under plan read, in two statements: the revision's find (404 when the tenant does not hold it) and its items. It declares 404 and 503. A page re-read to watch reservations settle is this one read.
- **in_effect.sku_ids.** PricingPlanInEffect gains sku_ids, the item SKUs of the revision in effect, in ascending order. The plans list's grouped items read takes the current revision and the one in effect, still one statement, so GET /plans stays at four (D-460).
- **The statements.** Measured after 9.5d-2, then pinned absolute in tests/revision_reads.rs for 10 items and for 100, the same SQL: draft 7, pending 9, scheduled 7, published 7, superseded 7. The reservations read is 2 for 10 and for 100.
- **The book list.** $filter names id (D-442).

**Source:** Phase 9 plan rev 4 (run 9.6, asks 48, 49, 50; A1, M1, M3, M7). Amends D-442, D-460 and D-462.

#### D-481 [M] The quorum a submit needs is on the checks and on an effective-policy read

**Status:** DECIDED 2026-10-01.

The book page and the plan page showed a quorum the submit would need only by reading the whole policy (asks 54, 55).

- **The checks.** PricingPlanChecksDto gains quorum_required, the plan_revision quorum stored_context already reads. It costs no statement, under plan read. The checks already show it as the APPROVAL info row. The revision gains no quorum field.
- **The effective policy.** GET /bss-pricing/v1/approval-policy/{kind}/effective answers { kind, quorum_required }: the kind's override, or the tenant default. One statement, approval_repo::read_policy. kind is parsed through the closed set of kinds pricing records; an unknown kind is 400 QUERY_INVALID. prices is read under price_book_entry read, the grant the book page's entry and price lists need. plan_revision is read under plan read. The path keeps DELETE /approval-policy/{kind} as it is.
- **Not price read.** The quorum is not money. Pricing serves no price read of its own (GET /prices/{id} is a price, D-440's list is price_book_entry read plus the money's second judgement), so price read is not this door's grant.

**Amended 2026-10-02 (phase 9 review F1).** The caller's grant stays the admission. `read_policy` then runs under `AccessScope::for_tenant`, as `stored_contexts` already does. `pricing_approval_policy` is scoped on `kind`, a text column. A grant constrained by `RESOURCE_ID` was compiled onto that column: Postgres answered 500 (`operator does not exist: text = uuid`) and SQLite matched no row, so quorum 3 or 0 was answered as the fail-safe 1. The checks door already read this policy under the tenant scope, so the two doors agree for this grant too.

**Source:** Phase 9 plan rev 4 (run 9.6, asks 54 and 55; A3). Amends D-435 and D-462. Amended 2026-10-02 (phase 9 review F1): the effective read uses the tenant scope after the grant.

#### D-482 [M] The checks read their context as a set, and many revisions in one read

**Status:** DECIDED 2026-10-01.

The plan page read each revision's checks on its own, and each check read its entries and prices one at a time (ask 47). Fifty revisions of twenty entries were thousands of statements in one serializable transaction.

- **The context.** `stored_contexts` reads, once for the whole set, the revisions, their plans, their items, the entries, the prices, the books, the dimensions, the plans' revisions, the in-effect items, the policy and the settings. Eleven statements whatever the number of revisions and entries, once any revision is held. `stored_context` is that read for one id, so the single checks and the batch are the same function. Submit and apply keep calling it inside their transaction. The checks doors read it on a connection, not under a serializable transaction.
- **The SKUs.** One `skus_for_write` over the union (D-408, products P-D-245). A left-out SKU is unavailable. An answer that is all missing makes no Products call.
- **The batch.** GET /bss-pricing/v1/plan-revisions/checks?revision_ids= takes 1 to 50 distinct ids and answers { items: [{ revision_id, checks }], missing }. Each checks is byte-identical to GET /plan-revisions/{id}/checks. A revision the tenant does not hold, or one outside the caller's plan-read scope, is missing, which is that door's 404. GET /plan-revisions/{id} is unchanged. The plans list gains no ready flag (D-460).
- **Refusals.** 400 QUERY_INVALID for an empty list, more than 50, a repeated id, a repeated revision_ids key, a malformed id, or any other key. 503 REGISTRY_UNAVAILABLE. Products' definite refusal as it gave it (D-416). Under plan read.
- **The statements.** tests/plan_checks_batch.rs pins 11 for one revision, for 5 revisions of 1 entry and for 50 of 20, the same SQL, none of them inside a transaction.

**Source:** Phase 9 plan rev 4 (run 9.7, ask 47; review H2, A2, L5). Amends D-408 and D-460.

#### D-483 [M] A book's entries page on the toolkit's pager, in the order (sku_id, charge_kind, model, id)

**Status:** DECIDED 2026-10-01.

GET /price-books/{id}/entries answered every entry of the book on one page, sorted in memory (ask 51). The Price Books screen pages a large book and narrows it to the SKUs the user searched for.

- **The pager.** The list pages on the toolkit's OData pager, as the book list does (D-442). limit (alias $top) defaults to 500 and is clamped at 500; a limit of 0 is the pager's 400. cursor (alias $skiptoken) continues from page_info.next_cursor. $filter takes sku_id (eq, ne, in), charge_kind, model and reference_state. The three closed sets compare (eq, ne, in) with one of their values only, else 400; the filter is judged before any read. The door declares the vocabulary with .with_odata_filter, as theme I declares the unit lists' order. $orderby, $select and $count are 400. PricingPriceBookEntryList gains page_info { next_cursor, prev_cursor, limit }.
- **The order.** (sku_id, charge_kind, model, id), all ascending and all non-null. It replaces the in-memory order (sku_id, charge_kind, period or "", id). period is null for usage and one-time entries, and the toolkit cursor codec cannot carry a null. So month and year entries of one recurring SKU and one model now follow their id. Before, all month entries came before all year entries. That is a served order change. GET /price-books/{id}/export keeps the old order, because it reads the whole book on one answer.
- **The cursor.** Its hash is the first 8 bytes of the SHA-256 of the extractor's $filter hash and the day the page is judged on, as hex. So a cursor replayed under another $filter, or on another as_of, is 400 FILTER_MISMATCH. as_of of today and no as_of are one narrowing. A cursor minted without as_of before midnight is 400 FILTER_MISMATCH after it: the client reads the list again, because a read never mixes two days.
- **The money and the day hold per page.** Every page carries the usage split, current_price and next_price on the read's one day (D-473). The money's second judgement (D-434, D-440) is made once per request on the book, so every page shows the money or none does.
- **Refusals, in order.** 403 without price_book_entry read; 503 when the policy cannot judge the money; 400 QUERY_INVALID for a plain key other than as_of, limit and cursor, or one given twice; 400 DATE_INVALID; the pager's 400s ($orderby, $select, $count, a filter it does not take, limit 0, a cursor that does not read, $orderby beside a cursor); 400 FILTER_MISMATCH; 404 for a book the tenant does not hold; then 403 PRICE_BOOK_READ_REQUIRED (D-473), judged before any entry, price or usage is read.
- **No q.** SKU names live in Products. The screen searches with GET /bss-products/v1/skus?q= and narrows this list with $filter=sku_id in (...). The served text says so.
- **The statements.** A page makes D-472's seven statements whatever its size and place: the book, the book under the money's grant, the page, the three usage reads and the default chain. The usage and the prices are read for the page's entries only. A refused dated read reads the book alone.
- **The tests.** tests/entry_paging.rs (SQLite) and tests/postgres_entry_paging.rs: the pages at every size from 1 to past the book join into the one order, across a usage SKU's model boundary and a recurring group's id-only boundary, and prev_cursor reads the first page again; each filter field alone and together, paged by one, and the refused filters; the 500 default and the clamp on 501 entries; the cursor under another $filter or as_of; the pager's refusals before the book; the as_of 403 with the book its only read; the day on every page; seven statements for the first page of 501, its last page and a filtered page; and the served text, parameters and vocabulary. tests/contract/price_book_entry_usage.json gains page_info and nothing else.

Breaking for a book of more than 500 entries: a caller that does not follow next_cursor sees the first 500, as D-442 said of the book list. Breaking for a caller that reads the order: within one SKU and charge kind, the entries follow model, then id, where month entries came before year entries. The deploy notes name both, with the deployed database's largest book (entries per book) measured before the deploy.

**Source:** Owner, 2026-10-01 (the pricing-mfe asks v4, 51); phase 9 plan rev 4 (run 9.8; review H3, A4). Amends D-434 and D-473.

#### D-484 [M] A plan stores time-stable list facts; selling and change are derived from the day

**Status:** DECIDED 2026-10-02.

A plan's list state depends on the day: a scheduled revision reads published from its date (D-447). A stored state column would go stale at midnight. The summary therefore stores only facts a write can recompute, and the day-dependent axes are derived from them and the request's day. The day is the gear clock's UTC date. There is no background job and no `CURRENT_DATE`.

- **The columns**, on `pricing_plan`, migration `m20261002_000020_plan_summary`. `work_revision_id` and `work_state` (`draft` or `pending`) are the at-most-one open revision (D-451). `scheduled_revision_id` and `scheduled_from` are the scheduled revision waiting for its date. `published_revision_id` is the stored published revision. `current_book_id` and `current_currency` are the book and currency of `work ?? scheduled ?? published`, D-460's current revision whether or not the scheduled one is due. `last_activity_at` is the latest `updated_at` of the plan and its revisions, superseded ones included. An id is stored if and only if its state or date. Postgres pairs them with CHECKs. SQLite cannot add a CHECK that names two columns, and rebuilding the plan would rebuild the revision table 000017 just widened, so SQLite enforces the same pairs with triggers.
- **The axes.** `selling` is true when a published revision is stored or `scheduled_from` is on or before the request's day. A draft-only plan and a plan with no revisions are false. `change` is the work state, else `scheduled` while `scheduled_from` is still ahead, else `none`. The counts' true and false add up to the total.
- **The backfill** runs in Rust inside the migration: it reads the plans and the revisions separately and updates by id. It does not join on a uuid, which on SQLite compares a blob with text and updates nothing. It is tested on both backends over a plan with no revisions, a draft only, a draft beside a published revision, a future scheduled revision, a due scheduled revision not yet switched, and a superseded history.
- **One maintenance point.** `plan_summary::refresh` recomputes the summary from the plan's revisions (one select with each book's currency, one update of the summary columns only). It runs inside the writing functions of `plan_revision_repo` (`insert`, `update_draft`, `try_lock`, `unlock`, `publish`, `supersede`, `schedule`, `switch_due`, `unschedule`, `delete_draft`) and `plan_repo` (`insert`, `rename`, `set_published`), on the caller's runner, only after a write that matched a row. It never changes the plan's `version` or `updated_at`. `try_lock` refreshes only when it locked. `switch_due` refreshes once, only when it switched, after `advance_published`; `advance_published` does not refresh again. A no-op `catch_up` adds no statement. `delete_unpublished` is on the allow-list: the row is gone. A source scan fails when a new write in those two repos neither refreshes nor is on that list.

**Source:** Owner, 2026-10-01 (#31, form B); phase 9 plan rev 4 (run 9.8b; review N1, N2). Amends D-453.

#### D-485 [M] The plans list pages on the stored summary and counts the derived axes

**Status:** DECIDED 2026-10-02.

GET /plans answered every plan of the tenant. A tenant of more than 500 plans needs a page, and the screen filters by whether a plan sells and by the change in hand.

- **The page.** The toolkit pager over `pricing_plan`. `limit` (alias `$top`) defaults to 500 and is clamped at 500. `cursor` (alias `$skiptoken`). `$filter` over `code`, `name`, `book_id` (the current book), `currency` (the current currency) and `last_activity_at`. `id` is not a filter field. `$orderby` over `code` (the default), `name` and `last_activity_at`, with `id` breaking the tie in that direction.

**Amended 2026-10-02 (phase 9 review F1b).** The served `$filter` contract is `PlanFilterField`, those five fields. A `$filter` on `id` is invalid. `PricingPlanList` gains `page_info`. Each plan serves `last_activity_at`, so the instant a page is ordered by is in the JSON. The plan's own `updated_at` stays the If-Match clock and is not a list field.
- **The plain keys**, each in the cursor hash (400 `FILTER_MISMATCH`): `q`, a case-insensitive literal substring of the code or the name; `selling=true|false`; `change=none,draft,pending,scheduled`, a comma list; the existing `sku_id` (D-434). Its stored-state EXISTS goes into the page query and the counts. It is not the effective state, and it is never an in-memory filter over a page.
- **The hydration.** The page's plans then read their revision headers, the items of the current and in-effect revisions, the units and the current revisions' books, in four grouped statements. `current` gains `book: { code, name, currency }`. GET /plans is five statements per non-empty page, whatever the page size. GET /plans/{id} keeps its own reads and adds the book.

D-515 amends this entry: that book also carries `id`, `valid_from` and `valid_until`. The same grouped read already loads the row. No new statement.
- **The served axes.** Each plan gains `selling: bool` and `change` (`none`, `draft`, `pending`, `scheduled`). They are computed in Rust from the hydrated revisions with the D-447 code, and a test asserts they equal the SQL axes and that `current` and `in_effect` agree with them. The clock passing a scheduled date, with no write, flips them in the list and in the counts.
- **The counts.** GET /plans/counts answers `{ by_selling: { true, false }, by_change: { none, draft, pending, scheduled }, total }`. It is one grouped statement under the list's whole narrowing minus the paging and the order. It is registered before GET /plans/{id}. `change` is in the response enum census.
- **Breaking** for a tenant of more than 500 plans: a caller that does not follow `next_cursor` sees the first 500, in code order. The deploy notes name it, with the deployed database's plan count measured before the deploy.

**Source:** Owner, 2026-10-01 (#31, form B); phase 9 plan rev 4 (run 9.8b; review N1, N7). Amends D-434, D-460 and D-453. Amended by D-515.

#### D-486 [M] A SKU's entries narrow, order and page in memory

**Status:** DECIDED 2026-10-01.

One SKU's entries are bounded by the tenant's books and its charge kinds, not by traffic. The book's name and currency live on the book, and today's status is computed from prices, so neither is stored on the entry. GET /bss-pricing/v1/price-book-entries?sku_id= therefore still reads what it read, and narrows, orders and pages in memory (owner 2026-10-01, #25 form A). No schema change.

- **The plain keys.** sku_id is required. book_id is 1 to 50 distinct price book ids, comma-separated; fifty-one copies of one id are that one id, and more than 50 distinct ids is 400 QUERY_INVALID. currency is three uppercase letters, the shape of a book currency (D-438). q is a case-insensitive literal substring of the book's code or name; an empty q does not narrow, and `%` is not a wildcard. status is priced, scheduled or unpriced, one or several, comma-separated. changing is true or false. A repeated key, a malformed value, as_of, and any other key are 400 QUERY_INVALID. $filter, $select and $count are 400 QUERY_INVALID, judged before any read: there is no in-memory evaluator for them.
- **The order.** $orderby names book_name (the default) or status, asc or desc, parsed with parse_orderby and applied in memory. The entry id breaks a tie in that same direction. Any other field, id included, and more than one key, is 400 INVALID_ORDERBY_FIELD. The status order is priced, then scheduled, then unpriced.
- **The page.** limit defaults to 500 and is clamped at 500; a limit of 0 reads one row, as the house pager does. cursor is the house CursorV1 codec. It carries the order and the last row's sort key and id. Its hash is the first 8 bytes of the SHA-256 of the plain keys (sku_id, book_id, currency, q, status, changing), not an extractor hash of $filter, so a changed narrowing is 400 FILTER_MISMATCH. The book list's spelling does not change the hash: book_id=a,b and book_id=b,a are one narrowing. $orderby beside a cursor is 400 ORDER_WITH_CURSOR, judged before the cursor is decoded. A cursor that does not read is 400.
- **The status.** It comes from the entry's PricingEntryPriceCounts on today. priced when an approved price is in force today on any chain, default or value (active > 0); else scheduled when an approved price starts later (scheduled > 0); else unpriced. changing is true when draft + pending > 0. An entry can be priced and changing together. A value-only priced entry stays priced while current_price stays null. Both fields are served. They are not money: today's usage split is served without price_book read (D-473).
- **Money.** current_price keeps D-434's per-book rule. The route stays dated on today and still answers as_of with 400 QUERY_INVALID. The book's entries list keeps 9.5d-1's as_of 403.
- **The statements.** Unchanged: seven, for a SKU in 5 books and in 50, and for a narrowing that keeps every entry and one that keeps none. The narrowing does not add a statement.
- **The tests.** tests/sku_reads.rs: every key and every refusal, both orders and the id tie-break, a page boundary, cursor misuse, the 500 default and the clamp, the status table (a value-only priced entry, a scheduled-only entry, a draft on a priced entry), the money rule, and the pin. tests/response_enums.rs: status is in CLOSED. tests/served_contract.rs: the route's text names the keys and the refusals.

Breaking for a SKU in more than 500 entries: a caller that does not follow next_cursor sees the first 500. The default order is book_name, then id, which replaces D-434's order by book code, charge kind, period, model and id. The deploy notes name both.

**Source:** Owner, 2026-10-01 (#25, form A); phase 9 plan rev 4 (run 9.8c; review N5). Amends D-434. Amended by D-517.

D-517 amends this entry: `$filter=id in (…)` of at most 200 ids replaces `sku_id`. The SKU read still refuses every other `$filter`.

#### D-490 [M] Pricing's approval units answer the approvals inbox through pricing's own doors

**Status:** DECIDED 2026-10-01.

The approvals inbox (`bss-approvals`, AP-D-1 to AP-D-4) serves ONE paged list, ONE count, ONE card and ONE vote door over the approval units of every BSS gear. The units stay in their gears. Each gear implements the inbox's source port, `bss_approvals_sdk::ApprovalSourceV1`, over its own door functions, and the inbox asks it AS THE CALLER. This entry is pricing's side; products P-D-250 is its twin.

- **The source.** `api::rest::authoring::inbox_source::PricingApprovalSource`. `module.rs` registers it at init in the ClientHub as `dyn ApprovalSourceV1`, scoped `ClientScope::new("pricing")`, over the gear's own `AuthoringState` and `PolicyEnforcer`. It copies no rule of a door: it calls the doors.
- **The page.** It is the list door's own read, `approvals::list_units` over `approval_repo::page_units`, in the list's serializable transaction, under the list's grant (`approval_unit` read) and its narrowing (`unit_narrowing`). So the page has the list's refusals, items, decisions, impact (D-470's `impact=false` included) and statements.
  - **The keyset (plan review H1, L1).** The inbox asks for up to `limit` units strictly after its key `(submitted_at, id)` for this source, or from the start. The source builds the pager's own `CursorV1` for that key in the list's one order, `approval_repo::submission_order`: `s` is that order's signed tokens, `+submitted_at,+id` or `-submitted_at,-id`, each of its keys takes its value from the inbox's key, encoded by the pager's codec (`encode_cursor_value`) under the list mapping's cursor kind, `f` is empty and `d` is `fwd`. `page_units` then reads the order from the cursor and compares the columns as it does for its own cursors (`build_cursor_predicate`). A first page has no cursor: the source puts `submission_order` on the query, as the list door does, and `page_units` reads the order from the query alone. There is no second SQL predicate and no compare of text. `has_more` is whether the pager minted a next cursor.
  - The order is D-470's, which this entry makes the inbox's merge key: `submitted_at` as an instant, then the id in the same direction. The ids of the two gears compare alike: a Postgres `uuid`, the bytes of a Rust `Uuid` and lower-case hex are one order. Both gears store whole microseconds. `SQLite` keeps `submitted_at` as text, which does not order as time inside one second (D-470), so the exact order of a walk is proved on Postgres.
  - The source never parses `$orderby` and never reads the gear's cursor token. Since the phase 9 review's themes I and K (9.5d-2) the order has one source, `submission_order` (R36), and `list_units` takes the caller's `SecurityContext` (R7); the source follows both and reads the page as the door does.
- **The counts (plan review M5).** `approvals::count_units` on the plain connection, as the counts door reads it (R32): never in the list's serializable transaction.
- **The card.** The card door's handler. Its 404 is the source's `None`, a unit this tenant does not hold. The facade's owner resolution asks every source (AP-D-3). With `impact=false` the source drops the card's impact.
- **`subject_live` is null (plan review M1).** The unit card loads no predecessor. The chain walk of `GET /price-books/{id}/publish-changes` does not enter an inbox page. A batched read is later work.
- **A vote (plan review H3).** The source sends the vote to the vote door itself, through the authoring router as `module.rs` serves it: the gear's enforcer, the correlation layer (D-431) and the platform's error layer. The door therefore judges its own grant (`approval_unit` approve, or submit for a withdraw, under `access_scope`, not `Command::store`'s tenant scope), separation of duties, quorum, generation and staleness. It keys the idempotency row under its own endpoint, `/bss-pricing/v1/approval-units/{id}/<action>`. The request body is the caller's exact bytes, with the caller's `Idempotency-Key`. The answer is the door's status, headers and body, unchanged: the receipt `{ have, need, outcome, unit }`, or the refusal with its `instance` (the door's path) and, on `GENERATION_MISMATCH` and `UNIT_STALE`, its `generation`. So a vote through the inbox and one through the door with the same key and body are ONE vote: the second replays the first. The same key with another body is 409 `IDEMPOTENCY_CONFLICT` (D-429).
- **A kind pricing does not record (plan review H4).** It is an empty page and zero counts, decided in the source before the grant and before any door. The door itself still refuses it with 400 `QUERY_INVALID` on `kind` (D-470's closed set). A state or an id the door refuses is that 400 for the inbox's whole read. `book_id` stays the alias of `ref_id`: the prices units of that book and no `plan_revision` unit, whose `ref_id` is the revision (AP-D-2).
- **The tests.** `tests/approvals_inbox_source.rs` holds the census: the same request through the door (the router `module.rs` serves) and through the source answers equal status, code and body bytes. It covers the list in both orders after any key, under six narrowings, with and without the impact; the list's and the counts' refusals (an unknown state, a `ref_id` and a `book_id` that differ, the grant), rendered at the door's path through the same error layer; the counts under six narrowings; the card, its miss and its grant; every vote refusal (an unreadable body, a missing key, `GENERATION_MISMATCH`, `NOTE_REQUIRED`, `NOTE_TOO_LONG`, `BODY_UNEXPECTED`, `SOD_VIOLATION`, `NOT_SUBMITTER`, the grant, 404, `DUPLICATE_VOTE`, `IDEMPOTENCY_CONFLICT`, `UNIT_ALREADY_DECIDED`); and `UNIT_STALE` with its `generation`. A source vote and a door vote with one key replay once, in both orders. The QueryRecorder pins the page at three statements on pricing's tables for 10 and for 100 units, every one in the list's transaction, and the counts at one statement outside any transaction. The authz census (`tests/rest_authz.rs`) counts the source's one read judgement: `require_authenticated(` and `authz::access_scope(` each appear once more than before, in the helper the page and the counts share; the card and the votes add none, since they call the doors. The facade over both real gears in one process is products' `approval_units/inbox_e2e_tests.rs`, and its walk on Postgres is products' `tests/postgres_approvals_inbox.rs` (P-D-250).

**Source:** Owner, 2026-10-01 (asked how to merge the two approval-unit methods into one, then "yes, A, agreed" for the read-and-route facade, then "write the plan"; Run 2 started before 9.5d-2 on the owner's word). Approvals inbox plan rev 2 (Run 2; design 1 and 3; plan review H1, H3, H4, M1, M5, L1). Amends D-470: its order is the inbox's merge key.

**Amended by D-496 (2026-10-02).** A kind pricing does not record is an empty page only after `state` has been accepted. An unknown state is the list door's 400 `UNIT_STATE_INVALID`.

#### D-491 [M] An entry op stores its policy reference as a named object

**Status:** DECIDED 2026-10-02.

An entry operation that keeps a policy without the declaration stores `usage_policy_reference` as `{ "policy_id", "version", "digest" }`. A row written earlier as the positional array `[policy_id, version, digest]` still reads. New rows write the named object.

**Source:** Phase 9 review F1b.

#### D-496 [M] The inbox source judges `state` before a foreign empty page

**Status:** DECIDED 2026-10-02.

A kind pricing does not record stays an empty page and zero counts (D-490). The source judges `state` with `approvals::state_filter` first. An unknown state is 400 `UNIT_STATE_INVALID` for the page and the counts, including when the kind is one pricing does not record. A known state, or no state, then takes the foreign empty set.

**Source:** Phase 9 review (products lens a; the pricing source). Amends D-490. Twin of products P-D-252.

#### D-497 [M] A unit says whether its reader may reject or withdraw it, and approve includes the grant

**Status:** DECIDED 2026-10-02.

`PricingApprovalUnitDto` carries `caller_can_reject` and `caller_can_withdraw` beside `caller_can_approve`. The three flags are the same rule as products P-D-255: approve is the engine's rule and the `approval_unit:approve` grant; reject is that grant, the unit pending, and no vote by the caller in this generation; withdraw is the submitter, the unit pending, and the submit grant. Each request compiles those two grants once and tests every unit against them.

**Source:** Owner, 2026-10-02 (ask 63, "все ок"). Amends D-471. Twin of products P-D-255.

#### D-501 [H] Authorized SDK reads share the frozen preview snapshot and canonical JSON digests

**Status:** DECIDED 2026-09-30.

Pricing provides `PricingReadV1::{resolve, price, current_revision}` through ClientHub. Queries name
an explicit catalog tenant, authorized against PDP-derived constraints under plan:read or price:read;
a subject name grants nothing. The existing REST matrix and approved-price shape remain frozen.
A shared snapshot owns local loading, resolution and dated Products evidence outside the transaction.
The dated Products read is pricing's system actor on both paths (D-424, amended 2026-10-02).
The SDK requires complete commercial inputs and reports `IncompleteCommercialInputs` when a priced
cell cannot supply them, while REST preserves nullable historical previews. The refusal is a failed
precondition: violation type `INCOMPLETE_COMMERCIAL_INPUTS`, subject the missing field, description
`incomplete commercial inputs: <field>` (amended 2026-10-02, phase 9 review F1). Missing legacy entry policy
remains `None`. Minimal invoice/policy projection types support the exact read signature, without
publishing later acceptance, hold or meter-provider methods.

Current-revision lookup persists due scheduled switches through the existing audited outbox transaction
before reading the published pointer; a future revision remains waiting. All three methods are SafeRead
and require no command idempotency key; switch persistence remains idempotent.

Money SHA-256 covers canonical JSON `{domain:"pricing.money.v1",payload:{currency,model,minimum_fee}}`,
including every model operand and excluding identity and mutable closing metadata. Selected-binding
SHA-256 uses `pricing.bindings.v1` and every binding field, dated unit, entry identity, requested dimension,
policy reference/content and invoice input. The exact template uses `pricing.template.v1`; policy content
uses `pricing.policy.v1`. Numeric values are normalized strings, optional fields explicit null, keys
ordered by UTF-16, and tier order retained. Rust and Node verify one frozen fixture. Slice 07 defines
the projection, read failures and compatibility boundary in full. This is the first Pricing-owned
producer surface, not delivery of consumer integration or sale acceptance.

**Source:** Pricing Seam Contracts plan, Task 1 (G1), revisions 2 and 3. Extends D-419–D-422 without
changing their REST behavior; uses phase-8 promotion from D-450–D-451. Later tasks own policy storage,
new-sale gates and receipts. Decision numbers D-470–D-499 remain reserved for concurrent phase-9 work.
Amended 2026-10-02 (phase 9 review F1): the incomplete-input violation type is `INCOMPLETE_COMMERCIAL_INPUTS`.

#### D-502 [H] Immutable usage policies belong to entries and their semantic key

**Status:** DECIDED 2026-10-01.

D-502 binds an immutable UsageRatingPolicy to each new usage entry. The create requires
`usage_rating_policy` for usage (`MISSING_RATING_POLICY` otherwise) and refuses it for recurring
or one-time entries (`UNEXPECTED_RATING_POLICY`). The closed input contains rating_window
(BillingCycle or CalendarHour with UTC), aggregation_scope (subscription_line or resource),
reset (rating_window_start), quantity_semantics (meter usage_type_id/version, unit, SUM fold,
accrual_policy_version), and partial_window (actual_quantity_full_thresholds). Empty or whitespace-only
meter identifiers, versions, units or accrual versions are `METER_POLICY_MISMATCH`. The server assigns
policy_id, version 1 and the lowercase SHA-256 canonical content digest; author input refuses these
identity fields. The entry PATCH cannot change or clear policy. Item and price requests refuse policy
fields. Changed content requires a new entry, then a revision explicitly selecting it.

Policy rows are append-only on both databases and deduplicate by (tenant_id, digest), checking stored
content on every reuse. Migration 18 adds the nullable entry reference (id, version, digest), an
all-null-or-all-present check, and a tenant-qualified composite foreign key including digest. The entry
key is (book_id, sku_id, charge_kind, coalesce(period, ''), model, coalesce(usage_policy_digest, ''));
only absent policy uses the empty index token. Hourly and billing-cycle variants coexist; equal content
cannot evade uniqueness through a new UUID. Entry reads, export, write answers and durable create
receipts materialize policy content with its identity; legacy/non-usage entries return null.

Tx A persists typed content and operation input schema_version 1 before the remote reserve. Tx B
inserts or reuses the policy and writes the entry atomically. A crash cannot change content; replay
returns the confirmed receipt. Unversioned persisted creates decode as legacy and may recover with
null policy; new versioned usage creates cannot take that path. Re-reserve and delete preserve the
original entry reference. Migration assigns no policy to old entries, including published plans;
they continue to read and resolve. Authoritative meter verification, publication gates and resolve
policy projection belong to Task 3 and are not delivered by D-502.

D-502: a plan item remains a SKU and its selected entry (D-467), with no policy override,
treatment, included quantity or minimum quantity. Copy/clone within a book preserves entry IDs.
Changing a draft's book matches the full (SKU, charge kind, normalized period, model, policy digest)
key and an equal dimension key. With no equivalent target, the item retains the old entry and
ITEM_BOOK_FOREIGN blocks publication. An hourly entry never silently becomes monthly, and an absent
legacy policy never becomes a new policy. Explicit item selection chooses the replacement entry.

**Amended by D-513.** On author input, `quantity_semantics.fold`, `reset` and `partial_window` may be absent or null. The parse fills `SUM`, `rating_window_start` and `actual_quantity_full_thresholds` before validation, the content digest, storage and the meter check. An explicit value is accepted and an unknown value is refused. Stored and served policies still carry all three. There is no migration.

**Amended by D-514.** The stored and served policy is the five rating rules. `quantity_semantics` is not stored. A deploy-3 body may still send it; the server verifies it and drops it. The entry stores `usage_sku_version`, the SKU head's `published_version` at create. Migration `m20261002_000021_policy_references_sku` rewrites stored policies and leaves that column null on rows written before it.

**Source:** Pricing Seam Contracts plan revision 3, Task 2. Amends D-386, D-401, D-427. Atlas C10 ownership is refined from item to entry; the atlas source remains externally owned. Amended by D-513 and D-514.

#### D-503 [H] Exact meter evidence gates usage publication and stays out of historical reads

**Status:** DECIDED 2026-10-01.

D-503 adds exact-version semantic validation to D-502. Pricing consumes
`pricing-sdk::meter_semantics::UsageMeterSemanticsV1::resolve(ctx, MeterRef)` as the authorized
caller, before opening a Pricing transaction. `MeterSemantics` carries the exact meter identity
and version, canonical unit, SUM fold, accrual-policy version, source-integrated flag and provider
evidence digest. All quantity fields and the SKU's unit and usage-type identity must agree;
otherwise `METER_POLICY_MISMATCH` refuses the write. There is no substitution of a latest version.

New entry-create work uses schema version 2 and persists the captured declaration before reservation.
Recovery validates that captured evidence against the reservation's SKU without another meter lookup.
Unversioned and version-1 work keep their original recovery rules; they acquire no invented evidence.
The existing D-401 cancellation of unreserved abandoned creates remains unchanged. A later fresh
request must resolve its own evidence. Confirmation recovery preserves the original entry and policy.

D-503 validates a usage entry's policy at price and plan-revision submit and final apply.
Products and meter reads happen outside Pricing transactions, as the acting caller. The subjects
consume captured results, recheck the entry identity/version in their existing transaction and keep
provider evidence digests in approval snapshots. Dependency failures remain typed observations until
the engine reaches a semantic gate, preserving non-final votes, rejects and withdrawals. Authorized
successful command replay precedes dependency observations.

Detached publication observations are checked against the complete local selection (including
added, removed or re-pointed items/prices) and entry identities before any captured refusal is
consumed. Local drift rolls back and repeats the authorized replay lookup, detached capture and
transaction within `toolkit_db::DEFAULT_TX_RETRY_ATTEMPTS`; driver contention shares that same
budget. Exhaustion is `UNIT_CONTENDED`. The second provider evidence read runs immediately before
the transaction, still outside it. A different answer for the same captured selection remains
`METER_EVIDENCE_CHANGED` and is never retried. This applies to price submit, plan submit,
publish-changes and both subjects' voting/apply paths. Entry-create recovery continues to use
its persisted evidence, and historical reads make no provider calls.


The revision fingerprint now includes each selected entry ID and its policy ID/version/digest,
read from entry rows in the same transaction. Policy content remains entry-owned; no plan-item
column or override is added. Changed selection refreshes the approval generation (`UNIT_STALE`)
and an old approval cannot publish it. A scheduled revision is checked at approval; D-450's later
switch does not revalidate dependencies. New usage approvals require a policy-bearing entry;
legacy approved prices and published revisions remain readable.

D-503 refuses CalendarHour with any `min_fee` at price create, submit and apply
(`UNSUPPORTED_TERMS`), and when publishing a revision selecting such approved money. A successor,
temporary pair and return keep their entry and therefore the same policy, window, scope and reset.
Policy changes require a different entry and an explicitly selected revision. The existing dated
SKU chain guard uses immutable Products history captured before the transaction.

D-503 projects the entry's optional typed `usage_rating_policy` on each REST resolve item
and each SDK binding. The materialized identity/content is loaded from local policy storage alongside
the selected entry; historical reads never call the meter provider. SDK bindings retain the same
`price_book_entry_id` as their price. Entry reads and exports retain D-502's optional projection.
A BillingCycle VM entry beside a CalendarHour cloudlet entry keeps two independent policies;
there is no plan-wide window or aggregation across subscription lines. Missing legacy policy is null.

**External production dependency E1 (not delivered by Pricing).** Types Registry owns immutable
meter declaration storage/lifecycle; Usage Collector owns the semantic read adapter; source/IRM
owners supply accrual-definition provenance. Their delivery is separate from this Pricing work.
The consumer port, validation and contract-test provider do not establish authoritative production
meter semantics. ClientHub must supply a real `UsageMeterSemanticsV1`; there is no successful
production fallback. Its absence is typed `UnconfiguredMeterSemantics` with canonical
`UNCONFIGURED_DEPENDENCY`; a configured outage is 503, and denial is 403. None becomes
`MISSING_RATING_POLICY` or an empty semantic result.

E1 blocks real usage-entry creation, new price/plan publication and usage sales at their semantic
gates until the authoritative provider is wired. Delivery must identify the implementing gear/adapter
and its tracked work item, and demonstrate exact-version resolution, canonical unit matching,
declared SUM/additivity, source integration provenance, historical immutability, caller authorization,
outage behavior and VM/cloudlet contract vectors against the real provider. These responsibilities
are required ownership for handoff, not evidence that another team has accepted or implemented the
work. Pricing's contract tests certify its consumer behavior only; production readiness remains
blocked until that external evidence exists.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

**Owner amendment 2026-10-01: E1 has a raw and a derived kind.** A derived (composite) usage meter
computes one quantity from other usage; a cloudlet is 128 MB of RAM and 400 MHz of CPU. Products
declares it as a derived usage type with an immutable version: its inputs at exact versions, the
formula as data, the granularity it applies at and its output unit. Rating evaluates it per
subscription line and rating window. The usage collector reports raw meters only. These are
products P-D-229 and rating T-D-39, decisions made on branch `bss/pricebook-meters` (`d8f78cf9b`)
and carried onto this branch by the derived usage types plan. E1 therefore has two parts:

- **E1a, raw meters:** Types Registry declarations answer through the Usage Collector's semantic
  adapter, with source/IRM accrual provenance, as above.
- **E1b, derived meters:** Products' derived usage type at its exact version answers: its
  canonical output unit and the digest of its stored declaration, which names the inputs at their
  exact versions and the formula (products P-D-233).

A policy's `MeterRef` names either kind. `UsageMeterSemanticsV1`, `validate_meter_policy` and the
publication and acceptance gates do not change: one provider behind the port answers both kinds,
and each kind owes the delivery evidence above against its own source. Pricing computes no derived
quantity.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

**Amended 2026-10-01 by products P-D-233: E1b is provided; E1a is still external.** Products registers
the one `UsageMeterSemanticsV1` in the ClientHub. For a derived meter, named
`MeterRef { usage_type_id: "products.derived/<code>@<n>", version: "<n>" }`, it answers from its own
store, in the caller's tenant and under products `sku:read`: `canonical_unit` the version's output
unit, `fold` SUM, `accrual_policy_version` `derived-v1:<stored digest hex>`, `source_integrated` true,
and `digest` the stored SHA-256 of the declaration's canonical bytes. A `version` that is not canonical
or disagrees with `@<n>` is 400 `METER_POLICY_MISMATCH`; an unknown code, version or tenant is one 400
`METER_VERSION_UNKNOWN`; a store outage is 503 and a denial 403. Every other meter answers exactly as an
absent provider does (`UNCONFIGURED_DEPENDENCY`): the raw-meter provider (E1a) is not built, so raw usage
stays blocked at its semantic gates. A derived meter is sellable: products' `tests/derived_meter_e2e.rs`
sells a cloudlet through Pricing's entry, price, plan and sellability gates with no test provider.
Pricing's checks do not change.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

**Amended by D-514.** The meter asked of the provider is the SKU head's `usage_type_ref`, not a copy stored on the policy. A derived id `products.derived/<code>@<n>` is asked at version `<n>`. The provider's canonical unit, fold and `source_integrated` must match the SKU and the policy fold. A deploy-3 `quantity_semantics` object, when present, must equal that answer and is then dropped.

**Source:** Pricing Seam Contracts plan revision 3, Task 3 (G2). Extends D-393, D-408 and D-502; preserves D-449–D-453 scheduling. The externally owned atlas C01/C10 is not modified by this task. Amended 2026-10-01 by the owner: E1a/E1b (products P-D-229, rating T-D-39 on branch `bss/pricebook-meters`). Amended 2026-10-01 by products P-D-233: E1b is provided by Products; E1a is still external. Amended by D-514.


#### D-504 [H] Pure new-sale terms validate a bounded commercial profile and snapshot integrity

**Status:** DECIDED 2026-10-01

D-504 defines the pure new-sale profile, narrower than the readable catalog:

| Selected binding | Supported profile | Stable refusal |
| --- | --- | --- |
| Recurring | Flat or PerUnit; month/year equal to BillingTerms | UnsupportedModel / BillingCycleMismatch |
| One-time | Flat or PerUnit; no recurring period or usage policy | UnsupportedModel / UnsupportedTerms |
| Usage BillingCycle | PerUnit, Volume or Graduated; period null; immutable explicit policy | MissingRatingPolicy / MeterPolicyMismatch |
| Usage CalendarHour | Same usage models; UTC, SUM, subscription_line or resource scope; no minimum fee, including zero | UnsupportedTerms |
| BillingCycle minimum fee | SubscriptionLine only; any Resource-scoped floor is refused | UnsupportedTerms |

**Amended 2026-10-02 (phase 9 review F1b).** `refuses_minimum_fee` is that predicate at price validation, at the plan-revision check, and at sale validation. A minimum fee with CalendarHour or a resource-scoped policy is unsupported at all three.

**Amended by D-514.** The usage policy in this table is the five rating rules. The binding's unit is the dated SKU's unit. It is not copied into the policy.
| FX / cross-currency sale | Currency must equal the selected price currency | CURRENCY_MISMATCH |
| Package, promotions, phases, allowances, quarter | Not part of new-sale terms; historical catalog reads remain intact | UnsupportedModel / UnsupportedTerms |

Invoice terms must be schema version 1, month/year and UTC, with explicit order or positive,
non-nil seller-policy provenance. Orders resolves this Subscriptions-owned snapshot before Pricing
is called. Calendar anchors are the first day at midnight UTC (January 1 for a year). Hourly usage
requires an hour-aligned SubscriptionStart anchor; BillingCycle-only usage allows a 10:30 anniversary.
A 10:30 activation with a valid calendar invoice anchor is supported. Pricing never chooses, shifts
or rounds an anchor. Different entries in one plan may retain different rating windows.

Quantity and fixed period count must be positive. Selected bindings must cover exactly one cell per
item, with no duplicate/foreign selection, and agree on requested dimensions and binding/price entry
identity. Every price currency must equal the market currency; a region dimension must equal the
market region, including when the price came via the default chain. Invoice inputs require dated SKU
identity/version/code/name, the unit for PerUnit/usage, nonempty template/GL/tax, the existing book
currency scale and HalfEven. The existing book currency spelling/minor-digit rules apply; this slice
adds neither a currency registry nor FX. Money is nonnegative and tier validation delegates to
`domain::money::validate_tiers`. BillingTerms, policy, money and template digests are recomputed;
policy unit must equal the dated binding unit. No second tier interpreter is introduced.

The deterministic VM fixture uses entry 2, EUR, no dimension/region, 2026-10-01T00:00Z, monthly
calendar invoice terms, Rolling, quantity 1, VM BillingCycle policy, SKU v3, VM-2CPU-4GB,
VM 2 vCPU / 4 GB, VM·hour, PerUnit 0.047, no floor, VM_REVENUE, cloud-services, VM usage,
scale 2 and HalfEven. Supported test variants recertify altered content digests; integrity tests
intentionally retain a stale digest. Exact threshold 10 exercises the existing half-open arithmetic.

Pure SaleObservation is the specified five booleans, derived from verified live reads. Non-current or
unavailable revisions and inactive/unsellable SKUs return NotSellable; missing coverage returns
ResolutionChanged. That shape intentionally does not distinguish retired from deprecated or off-sale.
Provider failures are not observations of commercial ineligibility: missing E1 remains 400
UNCONFIGURED_DEPENDENCY naming UsageMeterSemanticsV1, configured outage remains 503 and denial 403.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

Commercial RuleError carries a typed reason alongside its stable uppercase code. Canonical invalid
arguments retain the concrete reason in field-violation metadata (400); resolution/payload/expiry/
eligibility changes use aborted reason metadata (409); denial is permission denied (403), and an
authorized missing receipt is not found (404). The existing Toolkit RFC 9457 conversion is reused.
Wire scalars retain unsupported values in typed errors; strict BillingTerms decoding rejects missing
snapshots, unknown/duplicate fields, numeric versions, unsupported schema, cycles and timezone.
The SDK stays free of serde/storage types. No acceptance method or NewSale HTTP route is exposed here.

The SDK adds bss.billing-terms.v1, pricing.request.v1 and pricing.terms.v1 canonical JSON projections.
BillingTerms excludes its own digest; requests include all commercial intent and exclude command
metadata; accepted terms include query plus sorted complete bindings, including entry/policy identity,
without receipt identity/server timestamps. Exact decimals and all integers are strings; instants
normalize to UTC with nine fractional digits. Four new frozen vectors are verified in Rust and Node.

Task 5 must supply the complete live item universe, re-resolve selections and compare the caller's
selected-binding digest independently, validate authoritative meter observations outside transactions,
and preserve the accepted snapshot. The pure terms function cannot detect an item omitted from both
its query and its binding arguments. No persistence, migration 19, acceptance/hold method, receipt
reader, provider implementation or downstream Rating/Billing scheduler is delivered by this task.

**Source:** Pricing Seam Contracts plan revision 3, Task 4; atlas C01/C10 and F22/F23/F24/F31 are read-only design specifications, not downstream integration evidence.

#### D-505 [H] Durable commercial receipt storage

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts plan, Task 5a.

Task 5a adds migration `m20260930_000019_commercial_receipts` after the committed policy migration.
Acceptance, first hold and successful command mappings are separate append-only, tenant-scoped
records. This is storage delivery; provider authorization/registration and acceptance orchestration
remain Tasks 5b/5c, and live hold eligibility remains Task 6. The SDK declares the planned receipt
values and both commercial trait signatures without registering a partial provider.

- `pricing_acceptance`: composite primary key `(tenant_id, id)`; unique
  `(tenant_id, order_id, order_version, line_id)`; request and terms digests, receipt JSON,
  accepted instant, original deadline and creator. The business index also serves order lookup;
  the primary key serves acceptance-ID lookup within the catalog tenant.
- `pricing_hold`: composite primary key `(tenant_id, id)`; unique `(tenant_id, acceptance_id)`;
  tenant-qualified acceptance FK; frozen activation, terms digest, held snapshot, creator and
  creation instant. One hold cannot change activation or extend the acceptance deadline.
- `pricing_commercial_command`: unique `(tenant_id, caller_tenant_id, caller_id, operation,
  idempotency_key)` and composite row primary key; request digest and receipt kind/id. Nullable
  acceptance/hold target columns implement real tenant-qualified FKs. A CHECK requires exactly the
  target named by receipt kind/id and the matching `check`/`hold` operation. Each target has a
  tenant-prefixed lookup index. Caller identity must come from SecurityContext in the later provider.

All repositories take an explicit AccessScope and a DBRunner, supporting the caller's transaction.
They offer insert and scoped lookup, without update/delete/cleanup. Insert-or-get uses a targeted
ON CONFLICT DO NOTHING followed by a scoped winner read and digest comparison; it never catches a
unique violation inside an aborted PostgreSQL transaction. Acceptance races compare request digest;
hold races compare terms digest and activation; command races compare request digest and target.
The surrounding application owns bounded transaction retry for driver contention and atomic receipt,
command and audit commit. Those application behaviors are not claimed by this storage chunk.

Receipt JSON is TEXT on both databases so persistence returns the exact stored bytes. Runtime
`infra/commercial_terms/wire.rs` dispatches explicit receipt schema 1 to frozen typed DTOs, preserving
BillingTerms' own schema version 1 inside the acceptance snapshot. The hold contains its own schema
and frozen bindings; its tenant-qualified acceptance reference identifies the retained BillingTerms
snapshot, which the SDK HeldBindings type does not duplicate. Unknown versions/fields, duplicate
fields, omitted nullable fields and lossy scalars fail decoding. Readers never rehash issued digests
or rerun today's sale validator. Future additive schemas require new explicit version readers while
retaining the v1 reader; they must never rewrite historical digests or default historical terms.

Decimals, integers, UUIDs and digests use explicit exact string adapters. Order versions cover all
positive u64 values, stored as canonical decimal TEXT with equivalent backend bounds checks. Instants
use UTC RFC3339 with nine fractional digits in receipt JSON and relational TEXT columns; PostgreSQL
`timestamptz` would truncate nanoseconds. Lexical deadline ordering is valid for this fixed-width UTC
profile. Schema-1 timestamps require years 0000–9999. No floating-point path is introduced.

No receipt or command expires from storage after 24 hours. `hold_until` limits eligibility only;
retention follows the order/financial audit lifecycle, with no automatic cleanup in this slice.
Mutable catalog rows are not FK parents of immutable receipts, so catalog lifecycle changes cannot
cascade into issued snapshots. Tenant axes, selected entry/policy/price, descriptors, invoice template
text/digest/provenance and BillingTerms remain inside the original typed snapshot.

Evidence: `tests/acceptance_receipts.rs`, `tests/postgres_commercial_receipts.rs`, the schema-1 JSON
golden, both schema goldens and migration/guard tests. The duplicate-business test goes red when its
unique index is removed and green after restoration from the pre-probe copy.

#### D-506 [H] Authorized commercial provider boundary

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts plan, Task 5b.

Task 5b registers separate `dyn SellabilityV1` and `dyn PricingAcceptanceV1` providers in ClientHub.
They share `CommercialTermsService::new(state, enforcer, clock, policy)`. PricingReadProvider still
implements only resolve, price and current_revision. Explicit Contract IR classifies check/hold as
IdempotentWrite and acceptance/check_fulfilment as SafeRead; command metadata contains only a key.
Caller identity always comes from SecurityContext. No commercial REST command is introduced.

All four commercial methods first authenticate and ask the PDP for
`gts.cf.bss.pricing.acceptance.v1~`, with existing owner_tenant_id/resource_id constraints. Check uses
create on the catalog collection; acceptance and check_fulfilment use read on the receipt; hold uses
hold on the receipt. The shared gate verifies the requested catalog belongs to the compiled tenant
scope. Receipt lookup then retains both PDP constraints and an explicit catalog-tenant filter, even
when the principal has grants for several catalogs. Unknown or foreign-catalog ids are not found
within an authorized catalog; an unauthorized catalog is denied before storage.

Acceptance reading decodes the stored v1 snapshot through the 5a repository without consulting the
current catalog, recomputing digests, checking expiry or selecting seller defaults. Restart reads
retain the exact stored bytes when re-encoded. The SDK commercial reason mapping now names the
acceptance resource and preserves concrete invalid-argument/conflict/denial/not-found metadata.
PDP or storage outages remain 503. A missing required PDP is a named UNCONFIGURED_DEPENDENCY;
a supplied canonical PDP outage detail is retained, while raw database diagnostics stay in logs.

Configuration key `seller_hold_policy` contains positive `version: u64` and
`duration_seconds: u32`; absent policy defaults to version 1 / 86400. An explicitly supplied policy
must provide both fields and contain no unknown fields. Startup validates before registering any
provider. Clock reuses `reference_work::Clock`; `infra::clock::SystemClock` re-exports its existing
WallClock. The inherited default jitter hook remains for reference recovery and is unused here.
The test-only FixedClock stores an instant and advances explicitly, without sleeping. Pending
operations sample it for diagnostics only; authoritative commit-time sampling belongs to 5c/6.

Check remains typed NotYetAvailable / canonical unimplemented (501) until 5c; hold and
check_fulfilment remain the same until Task 6. Each refuses only after authorization and writes
nothing. This intermediate boundary commit is not a public commercial release: G3 must complete
before consumers can rely on successful commercial commands. Production meter semantics (E1),
consumer delivery and deployment PDP grants remain external obligations.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

#### D-507 [H] Atomic acceptance and durable authenticated command replay

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts plan, Task 5c.

Task 5c implements SellabilityV1::check on the D-506 shared service and D-505 receipt schema 1.
Acceptance create and plan read authorization precede digest computation and every replay. New
resolution additionally authorizes each selected price. Catalog identity is the explicit seller
axis; authenticated caller tenant/id come exclusively from SecurityContext.

The command scope is catalog/caller tenant/caller/operation/key. Equal request digests return the
stored immutable receipt; different content returns IdempotencyConflict. The business identity is
catalog/order/version/line. A new key with equal content attaches transactionally to that receipt;
different content returns AcceptanceMismatch. Neither replay path queries Products, meter evidence,
current revision, price windows or seller TTL. Command records have no expiry or in-flight state.

After due-revision promotion, one serializable local snapshot captures the plan and its revisions,
items, entries and policies, all candidate price rows, book, dimensions and invoice defaults. Dated
SKU descriptors, live caller-authorized SKU reads and exact meter declarations are detached from
that transaction. The full live item universe, current revision, coverage, market, Task 4 validators
and caller selected-binding digest must agree. No catalog or caller binding is trusted by digest
alone. Temporary promotional prices remain unsupported for new acceptance.

One serializable write transaction rechecks command/business identity and the captured local rows
before sampling Clock. It refuses elapsed price windows and wrong hold-policy versions; a newly due
revision forces fresh capture. Local drift uses the G2 retry_unit_capture/SelectionMoved budget and
ends in ResolutionChanged if exhausted. Unique-key conflicts reread the winner and compare digests.
The acceptance, successful command mapping and local audit row commit together; the audit retains
the observed SKU revision and exact meter evidence. Receipt IDs, accepted_at and hold_until are
issued only in this transaction. UTC instants and exact decimals retain the D-505 storage format.

A pre-commit failure leaves no receipt, command or acceptance audit. A committed result survives a
new database pool/service and replays after expiry, policy changes, provider outage or supersession.
Hold and check_fulfilment remain authorized NotYetAvailable until Task 6. Acceptance is not a grant
to activate an order; release still requires Task 6 and the G3 controller gate. E1 remains external.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

Evidence: acceptance_receipts (real AcceptanceFixture, atomic audit failure/restart, replay and
provider failures), deterministic detached-provider generation/clock/scheduled-switch races, and
mutation probes removing generation checks, moving replay ahead of authorization and refreshing
hold_until on replay. No migration or reinterpretation of previously issued receipts is needed.

#### D-508 [H] Frozen first holds and fresh original-binding eligibility

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts plan, Task 6.

Task 6 replaces the D-506/D-507 pending hold and fulfilment answers on CommercialTermsService.
Authorize the receipt action and look up the exact acceptance under the complete PDP scope before
child-row access or exact replay. Hold/command IDs differ from the acceptance ID: use the PDP scope's
tenant_only projection for these related rows only after the scoped parent lookup succeeds, retaining
explicit catalog/caller/key predicates and checking the replayed hold's parent. The canonical hold request
hash includes all tenant axes, acceptance ID and terms digest, current market and UTC activation
instant; caller and command key remain authenticated command-scope fields. Exact successful hold
commands return their stored receipt even after expiry, retirement, closing or provider outage.
They never attest current eligibility.

Every check_fulfilment, and every hold without exact command replay, loads the authorized original
acceptance, compares the complete tenant axes, terms digest and market, validates frozen BillingTerms
compatibility, and reads the live SKU for retirement. Deprecation and off-sale are allowed. Original
price IDs supply current closing metadata; no successor walk, current-revision check, meter refresh
or descriptor replacement occurs. Server time and activation must precede the persisted hold_until
and any original temporary_until or explicit effective_to at midnight UTC. Successor-induced
effective_to alone is never an accepted-binding end. A future explicit/temporary end bounds
valid_before. A backdated activation cannot revive an expired acceptance or ended price.

A first hold permits start_at <= activation_at < hold_until, including ordinary workflow delay.
The commit transaction rereads all captured original price rows, samples the server Clock, checks
half-open boundaries, and atomically inserts one hold and its authenticated command mapping.
Local drift or contention retries the complete detached observation with the existing bounded
budget; exhaustion is ResolutionChanged. An identical activation under another key returns the
original hold after fresh checks, without extending TTL. Another activation conflicts. The first
activation, entry identity, policy content/digest, money, SKU descriptors and invoice inputs stay
frozen; a successor revision selecting a different-window entry cannot change them.

An eligibility observation is never a reusable admission token. Subscriptions must check fresh
eligibility immediately before its first activation intent and fence the committed order version
and fulfilment attempt itself; it also owns actual served intervals. Pricing makes no cross-gear
atomicity claim. Receipt schema 1 and permanent historical price/acceptance reads are unchanged.

Evidence: fulfilment_holds covers F07 EUR 10/SKU v3 versus EUR 12/SKU v4, a 10:00 submit and 10:03
hold, expiry/closing boundaries, off-sale/retirement, denial/outage, concurrency, atomic rollback,
restart, bounded recapture and immutable entry/policy/invoice pins. Mutation probes reject using
successor effective_to as expiry, checking eligibility before replay, and renewing TTL on a new
key. The G3 controller gate follows this task; consumer implementation and E1 remain external.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).

#### D-509 [H] Executable Pricing seam fixtures and transport boundary

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts plan, Task 7.

The five schema-1 JSON fixtures in pricing/tests/seam_fixtures are decoded by test-only typed serde
DTOs with a closed scenario enum. Their combined commercial view separates entry-owned immutable
policy, price-owned money and sale-owned BillingTerms. Provider tests author policy-bearing entries,
approve prices and publish plans before comparing complete entry, policy, money, SKU and invoice
pins through ClientHub resolve, price, current_revision, check, acceptance, check_fulfilment and hold.
Mixed windows retain independent entry policies. Two plans may reuse an entry and policy while their
accepted line/item identities remain distinct; Pricing introduces no cross-subscription aggregation key.
Entry-policy mismatch is refused, and book remapping selects only an exact policy match.

F02/F23/F24 arithmetic invokes existing domain::money::decode and amount_for as catalog representation
checks. Source integration, UTC hourly scheduling, counter reset, manifests, monthly composition and
invoice rounding are external obligations, not executed consumer integrations. F07 preserves every
accepted pin after successor publication and delayed activation; F22/F31 exercise typed refusals and
strict wire boundaries. Missing BillingTerms, quarterly, and unknown fx/phases/promotions keys are
wire-adapter vectors with no production transport. These five cases only run the decoder or parse
step; they issue no provider command, so receipt-count assertions for them would be vacuous and
are omitted. The other six refusal cases execute provider/REST commands and retain persistence
assertions. A typed cross-currency sale reaches the provider and returns CURRENCY_MISMATCH.
Unsupported wire values cannot be represented by the SDK's closed enums.
The illustrative vm-hours/cloudlet-hours names remain declarations of the existing test provider only.

The commercial command surface is SDK-only: check and hold are IdempotentWrite; resolve, price,
current_revision, acceptance and check_fulfilment are SafeRead. Every provider call requires its
SecurityContext and PDP scope, including replay and system-named callers. A future remote transport
must bind these exact ports and services. This slice adds no public HTTP command API, hourly event,
scheduler or provisioning operation. Existing REST reads retain their contracts, including the optional
entry-policy projection on resolve. Existing phase-9 contracts are not reinterpreted.

Atlas 1.1.0 source SHA-256: 95ca93b9814b7ff131711990d4d9fea246776704820370407c97a10a36f8c4ed.
Its older C01 method placement and C10 plan-item ownership remain a publishing-owner reconciliation;
the delivered SDK ports and entry ownership are authoritative for these executable provider fixtures.
No external atlas file is edited, no consumer integration status is promoted, and E1 production meter
semantics remains externally blocked.
E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233).
Task 8 and the shared G4 controller gate follow this slice.


#### D-510 [H] Database parity and provider handoff

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts plan, Task 8.

Retain D-501–D-509's final typed ports, refusal reasons, PDP authorization and indefinite receipt
retention. Task 8 adds a common SQLite/PostgreSQL conformance suite and actual persisted-database
restart boundaries; it does not change production signatures, schema or commercial behavior.
The complete method/query/output matrix and source links are in
[DESIGN](DESIGN.md#executable-seam-fixture-boundary-d-509). New usage entries own immutable policies;
semantic entry identity includes canonical policy content digest. Book remapping also matches the
registered dimension and cannot silently replace a policy. Price/plan submit and final apply verify
exact immutable meter evidence, with no dependency call inside the Pricing transaction. D-504's
supported-model matrix remains binding: recurring/one-time Flat or PerUnit, usage PerUnit/Volume/
Graduated, explicit month/year BillingTerms, no hourly minimum fee or included quantity.

The pre-seam migration proof preserves phase-9 entries, published bindings and unversioned operation
payloads; tenant-qualified receipt/policy keys and the extended entry index are inspected. Identical
and different-policy concurrent entry creates and the one-winner acceptance race use real services.
Acceptance/hold replay is durable beyond 24 hours and uses the original frozen policy/money/terms;
new fulfilment still requires live eligibility. The report records actual scoped checks at the final
commit. The orchestrator owns the single G4 controller gate.

Atlas baseline 1.1.0 has SHA-256
`95ca93b9814b7ff131711990d4d9fea246776704820370407c97a10a36f8c4ed`.
Its owner must reconcile C00/C01/C10 and regenerate the embedded download. The SDD atlas-handoff.md
lists exact changes and final-SHA implementation/test links; it is not a published baseline or release
certificate. F07/F22/F31 provider evidence and F23/F24 Pricing input/math evidence remain distinct
from unexecuted downstream scheduler, collection and invoicing specifications.

**External production obligations remain open.** E1 = E1a (raw meters, the usage collector / types registry; external) + E1b (derived meters, provided by Products since P-D-233). E1a (raw meters): Types Registry owns immutable
declarations, Usage Collector the authorized exact-version semantic adapter, and source/IRM owners
the accrual provenance. E1b (derived meters; products P-D-229 and rating T-D-39) is delivered by
Products (products P-D-233): the one provider behind `UsageMeterSemanticsV1` answers a derived usage
type at its exact version with its canonical output unit and the digest of its stored declaration,
which names the inputs at their exact versions and the formula, and answers every raw meter as
unconfigured until E1a is delivered behind it. Delivery must identify the implementation and tracked
work and prove, for each kind, canonical units, SUM/additivity, source integration, historical
immutability, authorization, outage behavior and real VM/cloudlet vectors; for E1b that evidence is
products' meter-semantics tests and its `tests/derived_meter_e2e.rs`.
E2: Orders resolves Subscriptions-owned versioned BillingTerms and authenticates
payer/market; Subscriptions checks committed order/version and attempt fencing immediately before
activation. E3: deployment grants scoped actions to Orders, Subscriptions and Rating; names confer no
privilege. E4: Collector retains immutable source history, Subscriptions schedules incompatible policy
changes at the next UTC hour boundary, and Rating consumes the original history. Rating owns hourly
scheduling/reset/catch-up and exact amounts; Billing sums exact contributions before HALF_EVEN invoice
rounding. Pricing tests do not certify those downstream behaviors.


#### D-511 [H] Commercial commands enforce scoped prices and nonempty activation windows

**Status:** DECIDED 2026-10-01

**Source:** Pricing Seam Contracts review fix run; security/concurrency M1–M2 and L1–L5,
data/docs L1–L4. Refines D-503, D-507 and D-508.

New acceptance reads every selected price under its returned PDP price-read scope. A scoped-out
price is PermissionDenied (403), including fresh hold and check_fulfilment; a foreign receipt remains
ReceiptNotFound (404). At the commit clock, start_at must be strictly before hold_until. Otherwise
ActivationOutsideAcceptedWindow refuses the command before receipt, command or audit insertion.
A start inside the half-open window can be held normally.

SDK check/hold keys share REST's invalid-argument validation: nonempty, at most 255 printable ASCII
bytes. Products definite refusals propagate; contention, rate limits and outages map to registry
unavailability (503). UnconfiguredDependency places its code in type, dependency in subject and
message in description.

Policy-less plan selections omit usage_policy from approval content, preserving the pre-seam
fingerprint. A real policy reference still fingerprints its id, version and digest. The regression
freezes the hash produced by edb532550 on the same rows. SQLite migration 18's pre/post sqlite_master
DDL comparison preserves every old clause, including unnamed CHECKs and the price entry/version
UNIQUE; only identifier quoting, formatting, clause order and the explicitly added policy clauses
are normalized. A removed-CHECK mutation must fail this test.

#### D-512 [H] A plan item may wait for its entry in a draft

**Status:** DECIDED 2026-10-02.

The owner asked why a draft could not take a SKU with no entry, and then add the entry later. It can. Submit and publish still need an entry, and an approved price, for every item.

- **The create.** `POST /plan-revisions/{id}/items` takes `price_book_entry_id` as optional. Absent or null adds an entry-less item, reserves the SKU in Products, and answers `price_book_entry_id: null`. A given entry is judged as before: `ITEM_BOOK_FOREIGN`, `ITEM_ENTRY_SKU_MISMATCH`, `ENTRY_NOT_FOUND`. The SKU rules are unchanged (D-465): deprecated, bundle, fenced, retiring, draft, `ITEM_SKU_TAKEN`.
- **The PATCH.** `PATCH /plan-items/{id}` may set the entry on an entry-less item. A null entry stays 400 `ITEM_ENTRY_MISSING`. A PATCH never clears an entry.
- **Draft only.** An entry-less item exists only in a draft. The checks show it `ITEM_ENTRY_MISSING`, label "Every item points at a price". Submit, the scheduled apply and publish refuse the revision with the code any red check already answers: 400 `REVISION_CHECKS_RED` at submit, 409 `APPLY_REFUSED` at apply. No new code. The item meters nothing and resolves with no chains, the legacy behaviour.
- **Copy, clone, remap.** Each keeps an entry-less item entry-less. The remap still maps every item that has an entry. It never chooses an entry: a SKU may have several entries in a book.
- **The fingerprint (D-490).** A revision whose items all have entries fingerprints as before, so no pending unit is refreshed on this deploy. An entry-less item, if it reached a unit, would be one object in `items`, sorted by `sku_id`, `{"sku_id", "price_book_entry_id": null}` and no `usage_policy`. Submit refuses first, so it does not.
- **Storage.** No migration. `plan::stored_treatment` stores `included` when the entry is null, the row the column CHECK admits, and `paid` once a PATCH sets the entry.
- **The tests.** `tests/plan_item_doors.rs`: the door adds an entry-less item, the checks name it, submit refuses, a PATCH with a price turns the checks green, and copy, clone and the remap keep it entry-less while resolve of a published one has no chains. A foreign entry and a mismatched SKU stay refused. The insert and the remap statements are unchanged, so no new Postgres test.

**Source:** Owner, 2026-10-02 (why a draft cannot add a SKU with no entry and add the entry later; then yes, do that). Amends D-407, D-413, D-420, D-467 and D-469.

#### D-513 [M] A usage policy's single-valued fields default on input

**Status:** DECIDED 2026-10-02.

The owner asked whether `quantity_semantics.fold` should default when it has one choice, and then said to default the single-valued fields.

`POST /price-books/{id}/entries` is the only author input that carries a policy. On that body:

- `quantity_semantics.fold` absent or null becomes `SUM`;
- `reset` absent or null becomes `rating_window_start`;
- `partial_window` absent or null becomes `actual_quantity_full_thresholds`.

An explicit value is accepted as before. An unknown value is refused as before. `null` is the same as absent: the author named no choice, and each field has one legal value. The parse fills the three fields before validation, the content digest, deduplication by `(tenant_id, digest)`, storage and the meter check. A request that omits them and a request that spells them out are the same policy.

The stored row does not change, and there is no migration. Entry reads, export, write answers, durable create receipts, resolve and approval snapshots still return all five parts. The request schema lists the three fields as not required, each with its default. The response schemas keep them required.

The SDK `UsageRatingPolicyInput` stays a total type. No SDK consumer builds an author policy. The REST request shape is the parse that fills the defaults.

**Amended by D-514.** `fold` defaults on the request itself. `quantity_semantics.fold` is used only when a deploy-3 body sends `quantity_semantics` and the top-level `fold` is absent. A disagreement between the two folds is 400 `METER_POLICY_MISMATCH`.

**Source:** Owner, 2026-10-02 (whether `quantity_semantics.fold` should default when it has one choice; then yes, default those fields). Amends D-502. Amended by D-514.

#### D-514 [H] A usage rating policy is its rating rules and the entry stores the SKU revision

**Status:** DECIDED 2026-10-02.

The owner did not want the policy to copy the SKU's meter, unit and accrual version. The entry stores the SKU revision it was checked against. The policy stores the rating rules.

- **The policy.** `rating_window`, `aggregation_scope`, `reset`, `partial_window` and `fold`. The canonical digest covers those five fields. Equal rules share one row per tenant. `quantity_semantics` is not stored and is not in the digest.
- **The entry.** `usage_sku_version` is the SKU head's `published_version` at create, for a usage entry created after this decision. It is null for every other entry and for usage entries created before the migration. A non-null version requires a policy and is at least 1.
- **Deploy 3.** A body may still send `quantity_semantics`. The server checks the meter, the unit and the accrual against the SKU and the provider, then drops the object. A conflict between its `fold` and the top-level `fold` is 400 `METER_POLICY_MISMATCH`. The request schema marks the object deprecated.
- **The meter.** Later checks read the SKU head through `skus_for_write`. A derived meter's id is pinned for the SKU's life, so the head names the meter of every revision. The dated-version walk remains only to supply the unit to the price chain guard.
- **Migration** `m20261002_000021_policy_references_sku`. It rewrites each stored policy into the rules-only content and digest, re-points entries, and keeps the old policy rows. `usage_sku_version` stays null. It refuses to run when `pricing_acceptance` holds a receipt whose frozen binding still embeds `quantity_semantics`. The digest is the Rust digest of `bss-pricing-sdk`, on SQLite and on Postgres. SQLite enforces the new check with triggers, the same precedent as 000020.
- **Readers.** Resolve still serves `meter` and `sku_version`. `AcceptedBinding.meter` is the dated SKU's usage type, absent for a non-usage binding, and it is not part of the digest. The entry DTO serves `usage_sku_version`. A plan revision's fingerprint keeps the policy id, version and digest, and the SKU revision when one was recorded. The approval snapshot shows the rules and that revision. A unit pending before the digest moved answers 400 `UNIT_STALE` on its first vote and records no vote; the next generation applies.

**Source:** Owner, 2026-10-02 ("не хочу дубликатов", "можем хранить ревизию SKU и не дублировать?", "да ок пишем"). Amends D-502, D-503, D-504 and D-513.

#### D-515 [M] A plan row's book carries its id and validity

**Status:** DECIDED 2026-10-02.

`GET /plans` items carry `current.book` as `PricingPlanBook`. It named the book's code, name and currency (D-485) and not its id, so a row could not link to the book or draw its validity without a second index.

- **The fields.** `id`, `code`, `name`, `currency`, `valid_from` and `valid_until`. The two dates are `YYYY-MM-DD` or null, the same pair a book read serves. `id` is the book's id.
- **Where.** Every answer that builds `PricingPlanCurrent`: the list, `GET /plans/{id}`, and the create, clone and rename answers. The book row is the one the list already reads for `current`. The page stays five statements.
- **The test.** `tests/plan_overview.rs`: a book with both dates, and a book open on both sides, on the create and on the list.

**Source:** Owner, 2026-10-02 (ask 67). Amends D-485.

#### D-516 [M] A named book carries its identity beside its id

**Status:** DECIDED 2026-10-02.

A screen that shows a revision, or a price unit, had only the book's id. It could not name the book without a second index.

- **Revision headers.** `PricingPlanRevisionHeader` keeps `book_id` and gains `book: { id, code, name, currency }`. Every header on a plan answer carries it, not only `current`. `GET /plans` still reads the books in the one grouped statement it already makes for `current`; the id list is every revision's book. A non-empty page stays five statements.
- **Approval snapshots.** The snapshots the review reads — a pricing unit on the list, the card, a submit receipt, and the same units through the approvals inbox — gain `book: { id, code, name, currency }` beside each `book_id` whose value is an id. That is the prices snapshot's `book_id`, and `book_id` inside a plan revision's `before` and `after`. A `book_id` that is a diff object is left as it is. The stored snapshot and the fingerprint are unchanged, so a pending unit is not refreshed. The books are one grouped read for the page, and no read when no snapshot names a book. A book the tenant no longer holds leaves `book` absent, and the unit still reads.
- **The book list.** `GET /price-books?$filter=id in (…)` already answers 200. It is not rebuilt. `tests/book_reads.rs` `the_book_list_filters_by_id` and `tests/postgres_book_reads.rs` pin `eq`, `in`, a malformed uuid and the cursor hash, on both backends.
- **The tests.** `tests/book_identity.rs` for the header, including a header that is not current, and for both snapshot kinds on the receipt, the card and the list.

**Source:** Owner, 2026-10-02 (ask 54). Amends D-460 and D-408.

#### D-517 [M] Entries can be read by id

**Status:** DECIDED 2026-10-02.

`GET /price-book-entries` required `sku_id` and refused `$filter`. A price unit's review could not name the unit's entries without reading each book whole.

- **The alternative.** `$filter=id in (…)` lists those entries. At most 200 distinct ids. It replaces `sku_id`: any other key beside it, including `sku_id`, is 400 `QUERY_INVALID`. Without `sku_id` and without this filter, `sku_id` is still required.
- **The shape.** The filter is one `id in` list, or `id eq` for one id. Another field, `or`, `ne`, and more than 200 ids are 400 `QUERY_INVALID`. An id the tenant does not hold, or the caller's entry scope does not admit, is left out. The answer is the SKU export's item, in id order, one page, no cursor.
- **What stays.** Prices are shown only to a caller who also holds `price_book` read (D-434, D-440). Without that grant the entries are listed and `current_price` and `next_price` are null. The read is tenant scoped. The SKU read's keys, order, page and seven statements are unchanged.
- **The test.** `tests/sku_reads.rs`: the named entries, an omitted id, another tenant, the money grant, and the refused shapes.

**Amended 2026-10-03 (branch review, finding 1): the filter's length and its declaration.** The read takes the raw `$filter` itself, so the toolkit OData extractor's length budget never ran, and the 200-id limit was judged only after the whole expression was parsed. The route also published the OData field table, `id: eq|ne|in`, although the read refuses `ne`.

- **The length.** A `$filter` longer than the toolkit's `MAX_FILTER_LEN` (8192 bytes) is 400 `QUERY_INVALID` before it is parsed. A filter of exactly that length is still read.
- **The declaration.** `$filter` is declared as a plain query parameter. Its description names the two accepted shapes, `id eq <id>` and `id in (<id>, ...)`, the 200 ids and the 8192 bytes. The route publishes no `x-odata-filter`. The architecture lint DE0802 asks for `with_odata_filter`, which publishes the toolkit's operator table for a uuid field (`eq|ne|in`), so this one route's registration is exempt from it, as products' browse door is.
- **The tests.** `tests/sku_reads.rs`: a well-formed filter one byte over the limit is refused, one at the limit is read, and the served contract declares the filter as above.

**Source:** Owner, 2026-10-02 (ask 68). Amends D-486. The 2026-10-03 amendment: the branch review of the backlog asks.

#### D-518 [M] The plan list, the plan counts, the book list and the settings answer 304

**Status:** DECIDED 2026-10-03.

- **The three list reads.** `GET /plans`, `GET /plans/counts` and `GET /price-books` answer a weak `ETag` of the JSON body they serve: `W/"` plus 22 base64url characters of its SHA-256. They also send `Cache-Control: private, no-cache`.
- **The tag is the caller's own body.** It is not a row version. A body that differs per caller has a different tag, so a `304` never gives one caller the view of another caller. The book list carries no price, so its body does not change with the money grant.
- **The settings.** `GET /settings` is one document. Its strong `ETag` stays the row version (`"0"`, `"1"`, …) that a `PUT` sends back as `If-Match`; it gets no weak tag. An `If-None-Match` that matches that same tag is `304`. Both its answers send `Cache-Control: private, no-cache`.
- **The comparison.** `If-None-Match` matches by weak comparison (RFC 9110), including `*` and a comma-separated list. A match is `304` with an empty body, the same `ETag` and the same `Cache-Control`. Only a `200` is turned into a `304`; an error passes through unchanged.
- **What stays.** The strong `ETag` that the other single-resource reads serve for `If-Match` is unchanged. The statement counts of these reads are unchanged.
- **The tests.** `tests/conditional_reads.rs`: the first read, the `304` on a repeated read, and a new tag after a write, for each of the four reads.

**Amended 2026-10-04 (branch review): `no-cache`, not `no-store`.** A `304` needs the copy the browser stored: the browser sends that copy's tag, and a match tells it to use the copy. `no-store` forbids the browser to keep any copy, so under it every read would be a full `200` and the tag would save nothing. These reads therefore keep `private, no-cache`, and do not follow the rule that asks `no-store` of an API answer with per-user data (RUST-SEC-002). The trade-off is accepted for these four reads only:

- **What may stay in the browser.** A per-user body may sit in the browser's private cache, on disk, after the session ends: a plan page with its authors' names (D-519), the counts, a book page, and the settings with `updated_by_name`.
- **What still holds.** `private` keeps every answer out of shared caches: a proxy or a CDN stores none of them. `no-cache` makes the browser ask the server before each use of its copy, so the caller is authenticated and authorized again on every read. A list's tag is the caller's own body, so a `304` confirms only a copy that this caller would be served now, never another caller's view. The settings' tag is the row version (above): a `304` confirms the stored settings, and the copy's `updated_by_name` is the one read when the copy was stored (D-519).
- **What stays.** Every other read keeps the headers it had.

**Source:** Owner, 2026-10-03 (asks 56 and 57). Extends D-469. The 2026-10-04 amendment: the owner's answer to the branch review's question ("ok" to the recommendation).

#### D-519 [M] Every actor id a read shows carries its current name

**Status:** DECIDED 2026-10-03.

A screen showed `created_by`, `updated_by`, `actor` and `submitted_by` as ids. It had no way to name the person without
an Account Management read per id of its own.

- **The fields.** Beside each actor id the answer gains a sibling `<field>_name`, a string or null. The ids stay, so
  nothing breaks. The nine fields are `created_by` on `PricingPriceDto`, `PricingPlanItemDto`,
  `PricingPlanRevisionHeader`, `PricingPlanCurrent`, `PricingPlanDto` and `PricingPlanRevisionDto`; `updated_by` on
  `PricingSettingsDto`; `actor` on `PricingDecisionDto`; and `submitted_by` on `PricingApprovalUnitDto`.
- **The source.** `bss_rest::actor_names` reads AM's public user read, `list_users` with an id-set filter in the
  caller's own tenant, with the caller's own context. AM decides which profiles the caller may see; pricing adds no
  permission. The label is the display name, then first and last name, then the username. AM is a soft dependency:
  the client is found in the client hub at each lookup, and pricing declares no gear dependency on it.
- **One lookup per answer.** Each read builds its answer in its transaction, then collects every actor id of the whole
  page or document and resolves them once, after the transaction. The ids are deduplicated and read in chunks of 200,
  at most four chunks at once, inside one 2 s budget for the answer. No statement is added: the statement pins of
  `tests/book_reads.rs` and the plan list are unchanged. No transaction waits on AM.
- **Null.** A name is null when it is not available now: AM refused the profile to this caller, found no such user,
  failed, did not answer within the budget, or is not deployed. The read never fails because of AM; it is still 200.
- **System.** Pricing's system actor (`PRICING_SYSTEM_ACTOR`) and the nil id of the platform's system context read
  `"System"`, and AM is not asked.
- **The reads.** `GET /settings`, `GET /plans`, `GET /plans/{id}`, `GET /plan-revisions/{id}`, `GET /plan-items/{id}`,
  `GET /approval-units`, `GET /approval-units/{id}`, `GET /price-book-entries` (by SKU or by id),
  `GET /price-book-entries/{id}`, `GET /price-book-entries/{id}/prices`, `GET /price-books/{id}/entries`,
  `GET /price-books/{id}/export` and `GET /price-books/{id}/publish-changes`.
- **Writes name nobody.** A POST answer is stored as its Idempotency-Key's receipt and replayed, and a name must not be
  stored. So no write answer names anyone: its `*_name` fields are null, and a read of the resource names the actors.
- **The settings PUT.** A settings read is a PUT body without `version`, `updated_at` and `updated_by` (D-438), and the
  PUT refuses an unknown field. So that this stays true, the PUT accepts `updated_by_name` and ignores it: nothing
  breaks for a client that sends a read back.
- **No storage, no cache.** Pricing stores no name and caches none. A renamed user reads the new name on the next read.
- **Caching.** The names are part of the body. The weak `ETag` of `GET /plans` covers them (D-518), so a rename changes
  the tag. `GET /settings` keeps its strong version tag (D-518): a rename does not change it, and a revalidation keeps
  the copy the browser holds until the settings are written again. D-365 of the earlier register (commit `7da21fb75`)
  named approval participants and answered `Cache-Control: private, no-store`. Under D-518 that becomes
  `private, no-cache`: the reads D-518 covers revalidate, and the others keep the headers they had.
- **The tests.** `tests/actor_names.rs`: every read above names its actors with one directory call, a system actor reads
  `"System"` without a call, a failing directory leaves every name null on a 200, a rename shows on the next read, a
  page of 50 plans by three authors makes one call for the three, a write answer's names are null, a settings read
  stays a PUT body whose name is never written, and a hub without AM reads null names. `bss_rest`'s own tests pin the
  chunks, the concurrency, the budget, the error mapping and the label order.

**Source:** Owner, 2026-10-02 (ask 32: "there was already code that resolves the names through AM; do it that way on
the server"). Ported from the approval participant names of commit `7da21fb75`. Extends D-438, D-460 and D-461.
#### D-520 [H] A scheduled price is cancelled through the prices unit

**Status:** DECIDED 2026-10-03.

A cancel is a new item kind of the existing prices unit, not a new engine. The operator cancels an approved price that has not started yet (ask 19), and the price before it in the chain is open again up to the next price that stays.

- **The row.** `POST /prices/{id}/cancel` (no body, price author, Idempotency-Key) creates a draft price row with `change_kind = cancel` and `target_price_id` naming the price, and answers 201 with it. The row brings no new money. It copies the named price's money, chain and start without change, and it is never a price of the chain: no chain, normalisation, overlap rule, count, price in force, next price, resolve, pin or binding reads it. A draft change is deleted, not edited: PATCH answers 409 `PRICE_NOT_DRAFT`, and DELETE removes it.
- **The unit.** The author submits the row as any draft price: `POST /prices/{id}/submit` on the row, or the book's publish-changes, alone or with prices. Separation of duties, quorum and generations are the engine's, unchanged (D-393). Withdraw and reject leave the named price untouched. On apply the named price becomes `cancelled` and records `cancelled_by_unit_id`, and the row itself becomes `approved`: it is the record of the change. One unit may mix prices and changes, and it applies all of them or none.
- **The state.** `pricing_price.state` gains `cancelled`, which is terminal. A cancelled price leaves every chain: normalisation, `WINDOW_OVERLAP` and every read of the price in force skip it. Normalisation then recomputes the end of the price before it onto the next start that remains, or leaves it open (D-390), and apply writes that end as it writes every re-closed predecessor.
- **Guards,** at the door, at submit and again at apply. The named price is approved and starts after today, no other pending change names it, and no consumer's binding names it (amended below; it read "it is not `keep_for_bound`"). Refusals: 409 `PRICE_NOT_SCHEDULED` (not approved, or started, at the door or at submit), 409 `PRICE_ALREADY_STARTED` (started between submit and apply), 409 `PRICE_CHANGE_PENDING` (another pending change names it, or the same unit names it twice), 409 `PRICE_BOUND`. A refusal at apply rolls the whole unit back: the race keeps its own code, `PRICE_ALREADY_STARTED`, and any other guard met again there is 409 `APPLY_REFUSED` naming its code, as every apply refusal is.
- **Storage.** Migration `m20261003_000022_price_cancel_and_end` adds `change_kind` (`set`, `cancel` or `end`, default `set`), `target_price_id` and `cancelled_by_unit_id`, and widens the state check with `cancelled`. SQLite rebuilds `pricing_price` with its indexes. An applied change keeps its price's start, so `pricing_price_approved_start` covers `change_kind = 'set'` only: one price per start, and a change never takes it.
- **Readers.** `PricingPriceDto` serves `change_kind`, `target_price_id` and `cancelled_by_unit_id`; `PricingPriceState` and `PricingPriceStatus` gain `cancelled`. A change row shows its state as its status (draft, pending, rejected) and `superseded` once applied, so no status narrowing lists it as a price in force. The entry's price list and the export keep the cancelled price and every change row. GET /prices/{id} still answers a cancelled price by id, as stored (D-422); it answers 404 for a change row. The publish-changes listing shows a draft change with the price it names as its `before`, and the unit snapshot does the same. `PricesPublished` lists every price whose window or state the apply changed (amended below).

**Amended 2026-10-03 (run Asks-B2b): the event lists what changed.** Before, `PricesPublished` listed the unit's prices only, so a unit of changes alone published an empty list, and a consumer learned of a cancel, an end or a re-closed predecessor only by reading the entry again. Now the event lists every price whose window or state the apply changed, each as the apply left it, in ascending price id:

- each price of the unit, with the window its chain was approved with, as before;
- each approved price before them whose end the chain moved: re-closed onto a new start, or re-opened onto the next start that remains, or to open-ended;
- the price a `cancel` cancelled, with state `cancelled` and its stored window;
- the price an `end` ended, with its new end.

A price whose window did not move is not listed, and a new `keep_for_bound` mark alone is not news. A `cancel` or `end` row is a record of the change, not a price, so the event never lists it. Each listed price gains `state`, its stored state after the apply (`approved` or `cancelled`). The field is additive and optional: an event written before it has none, and none is not written. `effective_to` already carries an end, so no other field is added.

**Amended 2026-10-03 (run Asks-B2b): a cancelled price says so where it is read.** Before, GET /prices/{id} and `PricingReadV1::price` served a cancelled price by id with no state, so the reader could not tell that it was cancelled.

- **REST.** `PricingPinnedPriceDto` gains `status`, from its own closed set `PricingPinnedPriceStatus` (amended below; it was the entry list's set, `PricingPriceStatus`). A cancelled price carries `cancelled`, the token the list shows. The field is optional and absent on an approved price: its display status (scheduled, active, superseded) depends on the day, and D-422 keeps this read free of anything computed from today. So the consumer goldens of approved prices do not change.
- **SDK.** `ImmutablePrice` gains `state: PriceState` (`Approved` or `Cancelled`), the read contract's own closed set. `price()` serves `Cancelled` for a cancelled price, with its money as approved. Resolve, pins and bindings never return a cancelled price, so a binding's price is always `Approved`. `state` is in neither digest, so no money digest or binding digest moves. The frozen v1 receipt wire carries no state and reads back `Approved`: a binding names an approved price, and a price that a binding names is never cancelled (the guard below).
- A cancelled price is never the price in force (the chain exclusion above), and it stays readable by id.

**Amended 2026-10-03 (run Asks-B2b): only a real binding blocks a cancel.** The guard read "the named price is not `keep_for_bound`", and that flag refused ask 19's common case: a scheduled price followed by a `new` price was always refused `PRICE_BOUND`.

- **Why the flag is not the test.** The apply sets `keep_for_bound` on the price in force before every `new` price of a touched chain (D-397): it marks the price a pinned renewal stays on, because a pinned renewal does not take a `new` price. It is a property of the chain's shape and says nothing about whether anyone holds the price. A price that has not started yet cannot have been bound by its start date, so on a scheduled price the flag never stands for a holder. Cancelling the price does not lose the mark either: the apply marks the price before the `new` price again, which is now the re-opened predecessor.
- **The test.** A cancel is refused with 409 `PRICE_BOUND` only when a consumer holds a binding on the price: an acceptance in `pricing_acceptance` (the catalog tenant's) whose receipt's bindings name it. The bindings are the resolved selections of the receipt's order line, so "an acceptance that names the price" and "an acceptance whose selection's binding names it" are the same rows. The guard runs at the door, at submit and again at apply, as before; at apply it is 409 `APPLY_REFUSED` naming `PRICE_BOUND`.
- **What was measured.** The bindings pricing stores are the acceptance receipts (`pricing_acceptance.receipt_json`, schema 1, `bindings[].price.price_id`). A hold (`pricing_hold`) freezes its acceptance's bindings and needs that acceptance, so it names no price that its acceptance does not. `pricing_commercial_command` names receipts only. Pins are the consumer's: resolve takes them on each call, and pricing stores none. The receipt is text, so the database narrows the tenant's acceptances to the receipts that mention the id, and each one is decoded: the id elsewhere in a receipt (an order id, say) is not a binding.
- **In the shipped flows the guard refuses nothing yet.** A commercial check binds only a price in force on the check day and on the start day (`PRICE_CLOSED` otherwise), so no acceptance it writes names a price that has not started. The guard keeps the rule for any binding that names a price before its start.

**Amended 2026-10-03 (branch review): the columns are paired.** `m20261003_000022` only checked that `change_kind` is a known value, so a `cancel` row with no `target_price_id`, a `set` row with one, or a `cancelled` price with no `cancelled_by_unit_id` inserted without error and surfaced later as a corrupt row. The migration (not yet shipped, so changed in place) now adds two CHECKs on both engines: `(change_kind = 'set') = (target_price_id IS NULL)` and `(state = 'cancelled') = (cancelled_by_unit_id IS NOT NULL)`. The SQLite rebuild carries them, and so does `m20261003_000023`'s copy of the price table, which now copies a change's `target_price_id` with its row. Every writer already keeps them. The migration tests on both engines refuse each broken pairing. In code, a change is a cancel or an end (`infra::prices::Change`), and a stored end without its new end is a corrupt row.

**Amended 2026-10-04 (branch review): the pinned price's status is one value.** `PricingPinnedPriceDto.status` was typed with the entry list's seven-value `PricingPriceStatus`, so the served schema offered `draft`, `pending`, `rejected`, `scheduled`, `active` and `superseded`, which this read never serves: it carries `cancelled` or nothing. The field is new in this branch, so it now has its own one-value closed set, `PricingPinnedPriceStatus { Cancelled }`, and the served schema lists only `cancelled`. The wire value is the same, and the field is still optional and absent on an approved price. `tests/response_enums.rs` pins the one value in the spec, and `closed_sets_tests::a_pinned_price_status_is_cancelled_alone` pins its token against the list's `cancelled`.

**Deferred, noted 2026-10-04 (branch review): the binding guard's scan.** `acceptance_repo::binds_price` narrows the catalog tenant's acceptances with an unanchored `LIKE` on the receipt text (the price id anywhere in `receipt_json`), which no index serves, and decodes each receipt it finds. It runs once per cancel at each guard point (the door, the submit and the apply), and each run reads every acceptance of the tenant. That is acceptable while acceptances are few: they come only from Orders, through the commercial check, and Orders has no live traffic yet. Before Orders carries real volume, the guard must become an indexed lookup from a price to the bindings that name it, for example rows keyed by tenant and price id, written with each acceptance.

**Source:** Owner, 2026-10-02 and 2026-10-03 ("все ок сейчас будем писать план", "ок погнали"); asks 19 and 58a. Amends D-390, D-393 and D-422. The 2026-10-03 amendments: the controller's decisions on run Asks-B2's questions 1, 2 and 5, and the branch review of the backlog asks. The 2026-10-04 amendment and note: the owner's answers to the branch review's questions ("ok" to the recommendations).

#### D-521 [H] A live price is ended through the prices unit

**Status:** DECIDED 2026-10-03.

An end is the other new item kind of the prices unit (ask 58a). The operator ends an approved price that is live or scheduled on a date, and the chain then has no price in force from that date until its next start.

- **The row.** `POST /prices/{id}/end` with `{ "effective_to": "YYYY-MM-DD" }` (price author, Idempotency-Key) creates a draft row with `change_kind = end`, `target_price_id` and the new end, and answers 201 with it. It is the same kind of row as a cancel (D-520): it brings no new money, it is never a price of the chain, and it is submitted, withdrawn, rejected and approved like any draft price.
- **Apply.** The named price gets the new end and `closed_explicitly = true`, and the row becomes `approved`. D-390 still holds: an explicit end survives every later normalisation, and a successor that starts inside it still closes it at that start. A later cancel of the next price leaves the end where it is.
- **Guards,** at the door, at submit and again at apply. The named price is approved and has not ended by today. The new end is after today, after the price's start, and no later than its current end: the next approved start, or its own explicit end, whichever is sooner. Refusals: 400 `END_DATE_INVALID` (a new end outside that range, or a date that does not parse), 409 `PRICE_CHANGE_PENDING` (another pending change names the price, or the same unit names it twice), 409 `PRICE_ALREADY_ENDED` (the price is not approved, or it has ended). An end that is no longer after today when the unit applies is 409 `APPLY_REFUSED` naming `END_DATE_INVALID`, and the unit applies nothing.
- **Readers.** The ended price reads `closed_explicitly` with its new end; from that date the entry's price in force (`current_price`) and resolve find no price of that chain until its next start. The change row reads as D-520 describes.

**Amended 2026-10-03 (run Asks-B2b): the event.** `PricesPublished` lists the ended price with its new end and state `approved`, as D-520's amendment lists every price whose window or state an apply changed. A unit of ends alone no longer publishes an empty list. A price after the ended one whose window did not move is not listed.

**Source:** Owner, 2026-10-02 and 2026-10-03 ("все ок сейчас будем писать план", "ок погнали"); ask 58a. Amends D-390 and D-393.

#### D-522 [H] A finished book can be archived, and archiving it releases its entries' SKU references (twin of products P-D-263)

**Status:** DECIDED 2026-10-03.

A finished price book stayed on the Price Books screen for ever, and its entries kept their SKU references live in Products. So a SKU that only a finished book named stayed `SKU_REFERENCED` and could not be retired (ask 58).

- **A mark, not a state.** `pricing_price_book` gains `archived_at` and `archived_by`, both null until the book is archived (`m20261003_000023_book_archive`). Nothing that reads a revision's state, a price's state or the stats changes.
- **The doors.** `POST /price-books/{id}/archive` and `POST /price-books/{id}/unarchive`, under the book write grant (price_book author) and If-Match against the book's version, each with an audit row (`price_book.archive`, `price_book.unarchive`) and a new version. A book already in the asked state is answered as it is, and nothing is written.
- **Which book is archived.** The refusals come in this order: 403 without the grant; 400 for a missing or malformed If-Match; 404; 409 `STALE_REVISION`; 409 `BOOK_IN_PLAN` while a plan has a draft, pending, scheduled or published revision on the book (`plan_revision_repo::plans_on_books`, the read the stats and the delete use); 409 `BOOK_HAS_PENDING` while a `prices` unit of the book is in review (a pending price, cancel or end; the read the stats' `pending_units` count); 409 `ENTRY_CONFIRMATION_PENDING` while an entry's reference is being confirmed, as the entry delete refuses it. Superseded revisions do not refuse an archive: they are history.
- **The release (ask 58).** In the archive's transaction every entry whose reference is `confirmed` or `lost` becomes `released` (one statement), and each gets a reference op of the new kind `release`, in state `releasing`, for its reservation, with the reason `book_archived` in its work record. The op is the `delete` op's release path: `observe_release` calls Products' `release`. The door drives the ops after the commit; an op it cannot finish is durable, and the ticker finishes it after the in-flight grace. Once Products has released them, its `SKU_REFERENCED` no longer counts the book, so the SKU can be retired and then archived (products P-D-263). `GET /reference-ops` shows each op's `reason`: `book_archived` for a `release`, null for every other op (a closed set, amended below).
- **`released`.** The entry's `reference_state` gains `released` (on SQLite the entry, price and plan item tables are rebuilt; the reference op table too, for the kind). The reconciliation scans `confirmed` and `lost` entries only, so it never re-reserves a released one. A re-reservation that a SKU refuses leaves a released entry `released`: it is not lost, and no `PriceBookEntryReferenceLost` is announced.
- **Read-only.** An archived book's entries and prices stay, and are read-only: an entry create or PATCH, a price create or PATCH, a cancel, an end, a submit or an apply of a price, and a plan item create or PATCH naming one of its entries are 409 `BOOK_ARCHIVED` (a submit and an apply answer it as the prices unit answers `ENTRY_REFERENCE_LOST`). A re-reservation or a create whose write meets an archived book is refused `BOOK_ARCHIVED` and cancelled, which releases its new reservation. A delete still runs: it adds no money and no reference. The plan checks count an item on a `released` entry as they count one on a `lost` entry.
- **Unarchive.** The mark is cleared, and each `released` entry gets a `rereserve` op; the door drives them after the commit. While a release or a re-reservation of an entry is still open, the unarchive is refused 409 `ENTRY_RELEASE_PENDING` (amended below; it skipped such an entry). An entry whose SKU refuses the new reservation (retired, say) stays `released`, and the book is unarchived anyway. The answer, `PricingPriceBookUnarchiveDto`, is the book and `released_entries`: the entries still released when it is built (null when that read fails, amended below). Such an entry stays read-only, 409 `ENTRY_REFERENCE_RELEASED`, until an archive and an unarchive re-reserve it.
- **Lists hide archived books.** `GET /price-books`, its search and its `sku_id` picker leave an archived book out. `$filter` names `archived`, a boolean: `archived eq true` keeps only the archived books, `archived eq false` is the default made explicit, and any other use of it is 400 (`bss_rest::archived`, which products' lists share). The list keeps its five statements (`tests/book_reads.rs`). A read by id, the export, publish-changes and the consumer reads ignore the mark. The book answers carry `archived_at`, `archived_by` and `archived_by_name` (D-519).
- **No automatic archive.** Archiving is always an operator's act.
- **The tests.** `tests/book_archive.rs` (ask 58, the refusals, the read-only doors, the unarchive, the release after a failed drive, a re-reservation that meets an archived book), `tests/postgres_book_archive.rs` (the migration up and down, the doors on Postgres), the migration's `_tests.rs` on SQLite, and products' `tests/book_archive_e2e.rs`, where both gears run: the archive releases the reference in Products, which then retires the SKU and archives it.

**Amended 2026-10-03 (branch review, finding 2): the submit answers 409.** The prices unit refused a submit or a publish-changes of a released entry's price with `BOOK_ARCHIVED` or `ENTRY_REFERENCE_RELEASED`, and the door mapped both through its catch-all to 400. The entry above and the archive door said 409. Both codes are now 409, as `ENTRY_REFERENCE_LOST` is. The submit and publish-changes doors name them among their refusals. `tests/book_archive.rs` asserts the 409 and that the draft stays a draft, on an archived book and after an unarchive that left the entry released.

**Amended 2026-10-03 (branch review, finding 6): the door's drive is bounded.** The archive and the unarchive drove their reference ops one after another in the request, with no deadline, and nothing caps a book's entries. Now each door drives at most 8 ops at once, each on a task of its own, under one 3 s deadline for the whole door. At the deadline the door answers; the drives still running stop, and their ops stay durable for the ticker, as a failed drive's do. The door logs how many it left. `tests/book_archive.rs`: with Products stalling every release far past the deadline, the archive answers within seconds, its releases open, and the ticker finishes them.

**Amended 2026-10-03 (branch review, finding 29): the unarchive answers what committed.** The unarchive reads the entries still released after its commit and its drive. When that read failed, the door answered an error for an unarchive that had committed, and a retry under the same If-Match met 409 `STALE_REVISION`. The list cannot be built inside the unarchive's transaction: it depends on the drive that follows the commit. So a failed read is logged, and the door answers the unarchived book with `released_entries` null: not known now, never an invented list. The book's entry list says which entries are still released. `released_entries` is therefore nullable. `tests/book_archive.rs` makes the read fail after the commit and asserts the 200, the null and the committed version.

**Amended 2026-10-03 (branch review, finding 13): the op's reason is a closed set read from its work.** `GET /reference-ops` pulled `reason` out of the op's work record as untyped JSON and served it as a free string; a record that did not decode read as `reason: null`, while the drive (`Work::read`) and every other field of the op refuse it. Now the reason is read through `Work::read`, and it is the closed set `PricingReferenceOpReason` (D-439), whose one value is `book_archived`; it stays null for every other op. A work record that does not decode, or a reason outside the set, is a corrupt row: 500, the token not echoed. `tests/book_archive.rs` poisons a release op's work both ways and restores it.

**Amended 2026-10-03 (branch review): the archive mark is a pair.** `archived_at` and `archived_by` were two independent nullable columns. `m20261003_000023` (not yet shipped, so changed in place) now adds `CHECK ((archived_at IS NULL) = (archived_by IS NULL))`: on Postgres as `pricing_price_book_archive_mark_check`, on SQLite on `archived_by`'s column, which `down` therefore drops first. The migration tests on both engines refuse half a mark.

**Amended 2026-10-04 (branch review): the unarchive waits for open reference work.** The unarchive made no `rereserve` op for an entry whose `release` (or `rereserve`) op was still open, and answered 200. Once the ticker finished that release, the entry stayed `released`, and so read-only, in a book that was no longer archived, until another archive and unarchive. Now the unarchive refuses while any entry of the book has an open `release` or `rereserve` op: 409 `ENTRY_RELEASE_PENDING`, after 404 and `STALE_REVISION` and after a book that is not archived is answered as it is. It is the mirror of the archive's `ENTRY_CONFIRMATION_PENDING`. Nothing is written: the book stays archived at the version the caller read, so the same If-Match unarchives it once the work is done (the door's drive, or the ticker after the in-flight grace). Every `released` entry of an unarchive that passes gets its `rereserve` op. The tests: `tests/book_archive.rs`, `an_unarchive_is_refused_while_a_release_or_a_rereserve_is_open` (an open release, and an open re-reservation in a book archived again) and `an_unarchive_after_the_ticker_finished_the_release_rereserves_the_entry`.

**Deferred, noted 2026-10-04 (branch review): the plan revisions' references.** An archive releases the SKU references of the book's entries only. A superseded plan revision on the book does not refuse the archive (it is history), but its items keep the SKU references they hold in Products, and nothing releases them. So a SKU that such a revision names stays `SKU_REFERENCED` after the archive and cannot be retired. Releasing the references of superseded revisions belongs with plan retirement, which D-410 defers, and comes back with it.

**Source:** Owner, 2026-10-03 ("archived"; ask 58b). Twin of products P-D-263. Amends D-408 and D-444; extends D-407 and D-442. The 2026-10-03 amendments: the branch review of the backlog asks. The 2026-10-04 amendment and note: the owner's answers to the branch review's questions ("ok" to the recommendations).
