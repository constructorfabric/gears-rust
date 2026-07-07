//! Invoice-post domain (architecture §5). Pure, backend-agnostic logic for the
//! first business posting: the balanced direct-split line builder (Variant A —
//! no Contract-liability line), account-mapping with route-to-suspense, the
//! line-negation reversal + `MAPPING_CORRECTION` flow, and AR-aging buckets.
//!
//! Every module here is pure (no infra / DB imports — dylint DE0301): it
//! computes over SDK value types and produces SDK `PostEntry`/`PostLine`. The
//! orchestrator that resolves chart account ids and drives the foundation engine
//! lives in `crate::infra::invoice_post` (an infra service, like
//! `period_close`), because it needs repo + posting access.

pub mod aging;
pub mod builder;
pub mod mapping;
pub mod policy;
pub mod reversal;
