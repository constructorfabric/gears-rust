# Independent review of the 2026-09-23 remediation

Scope: HIGH (D-106…D-111), MEDIUM (D-112…D-139, Q-31) and LOW fixes. Read-only reviewer, three
lenses (A fix fidelity · B new contradictions · C build-ability). All findings quoted and
re-verified; `make design-check` passes and caught none of them.

**1 HIGH · 7 MEDIUM · 9 LOW**

## HIGH
- **R-H1 [B] `sales_path` is derived from what Orders cannot observe.** `01:1037` / `:1266` / *Create
  Transition* step 6 set `partner_placed` "when step 1 authorized the create through the delegated
  partner path", but after D-111 `08:170` "Orders never classifies a path as delegated" and `08:1323`
  "Orders cannot observe which PDP path was used" (also `01:1672`). D-106 (H-5) and D-111 (H-7)
  collide; D-130's recording-party bar and `recording_path` depend on the value. Fix: an observable
  source (PDP-returned path marker via `upreq-pdp-policy-integration`, or a stated proxy such as
  "a proof reference was supplied on the allowed create"), recorded in D-106.

## MEDIUM
- **R-M1 [B] D-114 follow-up read reopens the M-37 timing/outage oracle on writes** (`01:696` vs
  `08:622-628`, `:688-692`): existing-but-hidden = two PDP calls (and a 503 if the second fails);
  nonexistent = one. Fix: also make the follow-up call on the no-row arm, discarded.
- **R-M2 [C] Delegation-proof denial disclosure has no decidable test and leaks existence**
  (`08:624-627`, `:633-635`, `:675-676`, `:707-708`). Fix: disclose only when a non-delegated
  `order × read` would allow, or always `order-not-found` on targeted reads.
- **R-M3 [C] Boundary "malformed request" rejections have no registered reason** (`06:385`,
  `:826-827`; `04:397`, `:574-576`) — M-40's defect recreated; "like a missing expected version"
  invites 428. D-136 silent on `failure_reason` sent with a completed outcome. Fix: register one
  `request-invalid` (InvalidArgument/400) and cite it everywhere.
- **R-M4 [B] Audit `reason` column overloaded** (`01:1604` "Registered reason", non-null) vs caller
  cancel/hold free text (`07:407`, `:904`), D-136 enum (`06:492`, `:827`), D-82 vocabulary
  (`04:316`); optional hold reason (D-138) has nothing to write; `OrderHeld` still carries "the hold
  reason" (`01:2355`). Fix: nullable `caller_reason` column under the D-99 hash, or a per-trigger rule.
- **R-M5 [C] Workflow runs the re-check but no read exposes `overlap_scope_key` or the version market**
  (`03:687-689` vs `08:965-967`, `UPSTREAM:305-307`). Fix: add them to 08 §4.2's composed read or a
  Workflow-only read.
- **R-M6 [B] 03 §2.2 still says the within-basket half is evaluable without the port** (`03:258-259`),
  but after D-126 it needs `maxConcurrentActive` (`03:880-886`). Fix: both halves unevaluable until
  amended SUB-O5.
- **R-M7 [C] Draft PATCH trigger selection reads state before authorization** (`02:384`, `:409` vs
  `01:2971-2972`, `08:1133`). Fix: select the trigger by field class only and let the engine refuse,
  or authorize the pre-read as `order × read`.

## LOW
- R-L1 [A] 01 §3.6 steps 22/25 still append one audit entry (D-117 per-field rule missing; no-op edit
  undefined) (`01:745`, `:749`).
- R-L2 [B] D-137 flag not read by effective-policy selection; turning it off later leaves seller rows
  effective (`07:456`, `:609-610`).
- R-L3 [A] "presence read" survives in `DESIGN.md:256`, `:791`; `README.md:148`, `:152`; `ADR/0003:33`.
- R-L4 [A] "four paged collections" at `DECISIONS.md:165`; "Both per-order collections" at `08:225`.
- R-L5 [A] D-134 text still cites `07 §3.6` for `POST /cancel` (`DECISIONS.md:2423`).
- R-L6 [B] Pre-D-115 actor-class wording at `DESIGN.md:1116`, `:1149`; `07:462`.
- R-L7 [A] 07 *Cancel Order* still declares "non-terminal state" as a guard (`07:902`); "Hold columns"
  leftover (`07:380`).
- R-L8 [A] "identity/party port" survives M-13 (`03:684-686`; `06:741`, `:820`).
- R-L9 [A] Reflect Verdict step 1 "Declare three guards … pre-checks neither" (`06:385`).

## Verified consistent
D-110 × rows 26/27 × D-113 × D-112 × 06 new algorithms · D-114 × D-111 × D-68 (apart from R-M1/R-M2)
· every *Run Gate and Submit* step citation after D-108/D-123 · D-134 × 07 *Cancel Order* × rows
16/23/27 · policy channel D-121/D-133/D-137 · D-126/D-127/D-136/D-89 · D-116…D-120 × audit hash ·
counts (24 endpoints, 9 ports, 2.25 s / 2.5 s ceilings, 27 rows, 139 decisions, 31 Qs / 26 unanswered,
27 H / 38 M tally) · retired reasons absent · 26 LOW items sampled resolved (L-06.3, L-07.1 partial).
