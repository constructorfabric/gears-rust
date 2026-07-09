//! Background jobs run by the gear's `RunnableCapability` `serve` loop.
//!
//! These are **system-context, cross-tenant** jobs: they iterate all tenants
//! via the secure layer under the sanctioned all-tenants system scope
//! ([`toolkit_db::secure::AccessScope::allow_all`], the AM reaper/lease
//! pattern) and aggregate in memory (the gear has no raw-SQL / DB-side-aggregate
//! access), then narrow to one tenant via `AccessScope::for_tenant`.
//!
//! - [`tieout`] — daily tie-out (self-reconciliation) of `account_balance`
//!   vs the journal lines, plus the entry-balance backstop, no-negative
//!   re-check, and PENDING-mapping check. Raises invariant alarms.
//! - [`period_open`] — fiscal-period-open automation (ensures the current
//!   and next `fiscal_period` exist for every legal entity with a calendar).
//! - [`queue_applier`] — periodic sweep draining due queued allocations
//!   (allocate-before-settlement, §4.7) across all tenants; the backstop to the
//!   drain-on-settle hook.
//! - [`aged_alarms`] — periodic `Warn` alarms for queued work / parked
//!   unallocated cash that has aged past a threshold (§6).
//! - [`recognition_run`] — periodic ASC 606 S6 release: triggers a recognition
//!   run for every `(tenant, period)` with due `PENDING` segments (Slice 4 §4.3),
//!   the automatic backstop to the on-demand `POST /recognition-runs` endpoint.
//! - [`verifier`] — daily chain Verifier: re-walks every tenant's
//!   tamper-evidence hash chain and freezes + alarms a tenant whose chain no
//!   longer verifies (tamper alarm).
//! - [`attribution_sweep`] — the `payer-attribution-drift` detective seam
//!   (§4.7): a documented no-op until the AuthZ/Tenant resolver is wired into
//!   the gear; the resolver + per-entry comparison + alarm emission are the
//!   drop-in future.

pub mod aged_alarms;
pub mod attribution_sweep;
pub mod period_open;
pub mod queue_applier;
pub mod rate_sync;
pub mod recognition_run;
pub mod revaluation_run;
pub mod tieout;
pub mod verifier;
