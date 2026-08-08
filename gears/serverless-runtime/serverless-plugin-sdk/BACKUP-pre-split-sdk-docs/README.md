<!--
Created: 2026-07-30 by Constructor Tech
Updated: 2026-07-30 by Constructor Tech
-->

# BACKUP — superseded SDK documents, kept for reference only

**Nothing in this directory is current design, and nothing here is being maintained.** It is a
backup of the Serverless Runtime SDK documents as they stood before the SDK was split, retained
so the prior work is not lost. It is deliberately excluded from Constructor Studio validation
(`.cf-studio/config/artifacts.toml`), so `cfs validate` does not see it and the usual document
guarantees — structure, ID integrity, working links — do not hold here.

## What this is

Until 2026-07-30 there was one SDK for the Serverless Runtime, and one set of documents covering
both audiences: gears that *invoke* callables, and plugins that *execute* them. The SDK was then
split by audience, and those documents were superseded:

| Audience | Where it lives now | Status |
|---|---|---|
| Consuming gears | [`../../serverless-sdk/docs/`](../../serverless-sdk/docs/) | rewritten; reviewed and validating |
| Runtime plugins | not yet written | the plugin-facing SDK is not designed |

What remains here is the plugin-facing half, which had no rewritten counterpart to be replaced
by. It was written before the split, was never reviewed against it, and describes a crate that
does not exist. It is kept because it contains real prior work — the four ADRs in particular —
not because it is correct.

**When the plugin-facing SDK is designed**, write its documents fresh in that crate's own `docs/`
directory and use this only as source material. Do not promote these files as they stand: the
conflicts below are unresolved, and their requirement IDs were renamed mechanically rather than
re-derived.

## Known conflicts, unresolved

Anyone picking this up needs to settle these first. They are the reason this material is
parked rather than promoted.

1. **Two rival error taxonomies.** `{UserError, InvalidInput, Timeout, NotSupported, Internal}`
   → `{NonRetryable, Timeout, Retryable, ResourceLimit, Canceled}` in one place, versus
   `{…, Transient, Permanent, Unsupported{op}}` → `{User, Transient, Permanent, Internal}` in
   another. Both appear in these files. Neither has been reconciled with the host's
   `gts.cf.core.sless.err.v1~` category set (`retryable`, `non_retryable`, `resource_limit`,
   `timeout`, `canceled`).
2. **Compensation mechanism disagreement.** The host schema treats `on_failure` as a GTS
   *function* ID invoked as a fresh invocation with its own `invocation_id`; these docs make
   `compensate` a method on the workflow handler trait. A layered reconciliation was proposed
   (a plugin may register the compensation function ID itself and route that invocation to
   `compensate`) but never confirmed.
3. **`Schedule` / `Trigger` ownership.** The 2026-05-19 work moved these value types into SDK
   scope; the host DESIGN tables and features F-04/F-07 still treat them as host-only.
4. **ID namespace collision.** Everything here uses `cpt-cf-serverless-runtime-sdk-*`, which
   the consumer SDK now also uses. The plugin side needs its own prefix — likely
   `cpt-cf-serverless-runtime-plugin-sdk-*` — and that rename must be applied consistently
   across the DECOMPOSITION, features, and ADRs before any of it is cited elsewhere.
5. **May a workflow be a compensation handler?** The host's `x-gts-ref` currently permits only
   `function.v1~*`. This matters when a rollback is itself long-running and multi-step.

## Also stale

- `DESIGN.md` §1.3 contains an ASCII traits box still showing the pre-split
  `ServerlessRuntimeClient` carrying the plugin event port. Per the split, the event port is
  plugin-side and the consumer trait does not carry it.
- The plugin trait is still called `RuntimeAdapter` throughout. The host documents settled on
  "plugin" rather than "adapter" as the noun, and the ToolKit naming convention for a
  plugin-implemented contract is `<Gear>PluginClientV1`. The final name is this crate's decision
  to make.

## Current, for contrast

- Host gear docs: [`../../docs/`](../../docs/) — `PRD.md`, `DESIGN.md`,
  `DESIGN_RUST_TYPES.md`, `DESIGN_GTS_SCHEMAS.md`, `ADR/`, and `NEXT_ADR_SCOPE.md` §3 for
  open host-side gaps.
- Consumer SDK PRD: [`../../serverless-sdk/docs/PRD.md`](../../serverless-sdk/docs/PRD.md).
- The crate split is recorded as a dated amendment in
  [`ADR-0005`](../../docs/ADR/0005-cpt-cf-serverless-runtime-adr-thin-host.md).
