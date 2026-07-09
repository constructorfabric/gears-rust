//! FX & multi-currency rate-service layer (Slice 5): resolve the locked rate for
//! a cross-currency entry over the local `ledger_fx_rate` store, freeze it in a
//! `ledger_fx_rate_snapshot`, and stamp the functional translation onto the
//! posting lines.
//!
//! - [`rate_source`] — resolution logic ([`RateSource`](rate_source::RateSource)):
//!   provider-precedence ordering + staleness screening over the candidate rows.
//! - [`rate_locker`] — translate + snapshot + stamp
//!   ([`RateLocker`](rate_locker::RateLocker)): the orchestration that resolves a
//!   rate, persists the per-lock snapshot, and writes the functional columns.
//!
//! These are standalone components in this slice — NOT yet hooked into the live
//! S1/S2 posting paths (`invoice_post` / `settle`). The functional-currency
//! source (the AMS legal-entity feed, design S5-F3) is not wired in this slice,
//! so the caller cannot yet drive them; they exist and are unit-tested ahead of
//! that wiring.

pub mod rate_locker;
pub mod rate_source;
pub mod revaluation_run;
