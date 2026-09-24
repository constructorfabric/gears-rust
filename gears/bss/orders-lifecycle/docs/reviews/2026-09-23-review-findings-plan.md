# Plan — fixing the remediation-review findings (2026-09-23)

Source: `2026-09-23-remediation-review.md` (1 HIGH · 7 MEDIUM · 9 LOW). New decisions from **D-140**.

## Choices (recommendation first)

| Item | Question | Options |
|---|---|---|
| R-H1 | Observable source for `sales_path` | **A: proxy now — `partner_placed` iff the allowed create carried a delegation proof reference (same proxy 08 §4.4 uses for logging); upstream ask for a PDP path marker to replace it** · B: block on a PDP path marker |
| R-M2 | When a delegation-proof denial may be disclosed | **A: targeted requests always answer `order-not-found`; `delegation-proof-required/-invalid` only on untargeted requests (list, create, preview)** · B: disclose when a non-delegated `order × read` on the target allows |
| R-M4 | Where caller reasons live in the audit | **A: new nullable `caller_reason` column (free text / closed failure value), `reason` stays the registered reason; D-99 hash gains a version** · B: per-trigger rule for what `reason` holds |
| R-M5 | How Workflow reads re-check inputs | **A: extend 08 §4.2's composed read (Workflow principal) with per-line `overlap_scope_key` and the version market** · B: a separate Workflow-only read |
| R-M7 | Draft PATCH trigger selection | **A: select by field classes only; the engine refuses a state mismatch (`not-admissible` / `commercial-field-immutable`)** · B: authorize a pre-read as `order × read` |
| R-L1 | Admin edit where no field changes | **A: rejected at the boundary as `request-invalid` (R-M3)** · B: succeeds writing one entry with no changed field |

## Mechanical

- **R-M1** — 01 *Attempt Transition* step 1: on the no-row arm also make the D-114 follow-up `order × read` (empty properties, discarded), so both arms cost the same calls and share outage behaviour.
- **R-M3** — register `request-invalid` (InvalidArgument, 400) for boundary validation failures that have no specific reason; cite it at 06 `denial_reason` / `failure_reason`, 04 unknown delta key; state `failure_reason` on a completed outcome is `request-invalid`.
- **R-M6** — 03 §2.2: both overlap halves are unevaluable until amended SUB-O5 exists.
- **R-L2** — effective TTL policy selection ignores `scope = seller` rows while `ttl_seller_override_enabled` is off.
- **R-L3…R-L9** — wording: "presence" → "occupancy" (DESIGN, README, ADR-0003); "four" → "five" collections; D-134 `07 §4.6`; pre-D-115 actor-class wording; 07 *Cancel Order* drops "non-terminal state" guard and "Hold columns"; "identity/party port" → "identity port"; Reflect Verdict "pre-checks none of them".

## Batches (sequential, fresh subagent each, `make design-check` after each)

| # | Items | Main files |
|---|---|---|
| 1 | R-H1, R-M1, R-M2 | 01 §3.6/§3.7, 05, 08 §2.1/§3.6, UPSTREAM §2.9, D-106/D-111/D-114 |
| 2 | R-M3, R-M4, R-L1 | 01 registry + audit schema + steps 22/25, 04, 06, 07, D-99 |
| 3 | R-M5, R-M6, R-M7 | 08 §4.2, 03 §2.2/§3.6, 02 *Edit Order* / *Edit or Remove Line*, 06 |
| 4 | R-L2…R-L9 | 07, DESIGN, README, ADR-0003, DECISIONS, 03, 06, 08 |
| 5 | Re-review (read-only) of batches 1–4 | — |

## Owner choices (2026-09-23)

All six take the recommended option (A): R-H1 proof-reference proxy + upstream ask for a PDP path
marker · R-M2 targeted requests always `order-not-found`, proof reasons only on untargeted ·
R-M4 new nullable `caller_reason` column, D-99 hash versioned · R-M5 extend 08 §4.2 composed read ·
R-M7 trigger by field class only · R-L1 no-op admin edit → `request-invalid`.
