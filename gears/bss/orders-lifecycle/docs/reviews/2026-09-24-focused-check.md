# Focused check of the RR fixes (D-146…D-149) — 2026-09-24

Read-only. **0 HIGH · 3 MEDIUM · 6 LOW.** RR-H1 closed for Lifecycle's own rules: no partner role
actor's action becomes customer consent in any of six walked cases. The other ten RR findings read
as resolved (RR-L2 for reads only).

## MEDIUM
- **FC-M1** Partner-placed order submitted by the buyer without proof now gets automatic acceptance
  (`05:472-476`), contradicting "On the partner-placed path, acceptance MUST be recorded as a separate
  first-class instant" (`05:518`), and `recording_path` stops being a copy of `sales_path`
  (`05:314`, `:350`). Fix: also require `sales_path = self_service`, or restate 05:518.
- **FC-M2** Step 4's `sales_path = partner_placed` condition only ever adds a bar on buyer-tenant role
  actors (partner-home actors are already caught by the tenant check): a buyer who submitted a
  partner-placed order is barred from renewed acceptance. Fix: drop the condition, or state whom it
  targets.
- **FC-M3** A partner colleague who is not a role actor is stopped only by PDP policy (`05:312`,
  `08:1040`), and UPSTREAM §2.9 lists no such obligation. Fix: step 4 refuses when the recording
  request carried a delegation proof or the caller's `subject_tenant_id ≠ resource_tenant_id`.

## LOW
- FC-L1 08 matrix (`08:1040`) not updated for D-146 (Partner bar and Direct Customer "initial submit
  records it").
- FC-L2 Proof-proxy qualifier uneven (`01:1064`, `05:350`, `08:181`); submit rule never switches to
  the PDP-accepted proof although D-146's rationale says item 4 closes it; Q-30 row not extended.
- FC-L3 D-120 "both commit" untrue for identical concurrent edits after D-149 (`04:697-698`).
- FC-L4 Boundary reason for a submit missing `expected_draft_revision` unnamed (`01:3059-3061`).
- FC-L5 04 (`04:316-317`) still describes `reason` pre-D-148.
- FC-L6 RR-L2 closed for reads only; targeted write proof denials keep no durable proof reason.
