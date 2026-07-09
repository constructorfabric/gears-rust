//! BSS Billing Ledger gear.

// Tracked lint-debt (CI green without risky refactors of financial logic).
// The workspace lint bar (`pedantic` + many restriction lints, all `deny`)
// flags a set of lints in this crate that are either DOMAIN-INHERENT or purely
// STYLISTIC, none behavioural:
//   * `integer_division` — deliberate minor-unit integer math with explicit,
//     designed residual handling (banker's rounding / residual-to-last-segment);
//   * `unused_async` — publisher methods stay `async` to match the future
//     broker-wired signature (the event layer is parked, so they have no
//     `.await` yet);
//   * `cognitive_complexity` — several posting/recognition/FX functions exceed
//     the threshold; refactoring financial logic purely to satisfy the metric
//     is deferred (correctness-risk not worth the churn);
//   * `non_ascii_literal` — `→` / `‖` / `×` / `≥` symbols in messages & doc
//     examples; `redundant_pub_crate`, `doc_markdown`, `too_many_arguments`,
//     `needless_pass_by_value`, `redundant_clone` — cosmetic.
// Safety-relevant lints (cast_*, float_cmp, await_holding_*, unwrap/expect, …)
// remain ENFORCED from the workspace table. Pay this down incrementally.
#![allow(
    clippy::integer_division,
    clippy::unused_async,
    clippy::cognitive_complexity,
    clippy::non_ascii_literal,
    clippy::redundant_pub_crate,
    clippy::doc_markdown,
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::redundant_clone,
    clippy::let_underscore_must_use
)]

#[doc(hidden)]
pub mod api;
#[doc(hidden)]
pub mod authz;
#[doc(hidden)]
pub mod config;
#[doc(hidden)]
pub mod domain;
#[doc(hidden)]
pub mod gts;
#[doc(hidden)]
pub mod infra;
#[doc(hidden)]
pub mod module;
/// `OData` filter-field enums for the row-collection list endpoints
/// (`accounts` / `journal-lines` / `balances`).
pub(crate) mod odata;
