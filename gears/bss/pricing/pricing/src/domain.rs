//! Domain layer — the publish engine's vocabulary and rules.
//!
//! Carries no infrastructure types (enforced by DE0301): the repositories,
//! entities and transports live in [`crate::infra`] and [`crate::api`], and the
//! domain is what stays true regardless of where the rows are stored.

pub mod allowance;
pub mod approval;
pub mod audit;
pub mod bulk;
pub mod bundle;
pub mod bundle_rules;
pub mod bundle_sellability;
pub mod concurrency;
pub mod contracts;
pub mod coverage;
pub mod currency_binding;
pub mod cutover;
pub mod error;
pub mod evaluation_policy;
pub mod events;
pub mod grandfather;
pub mod import;
pub mod instant;
pub mod lifecycle;
pub mod materiality;
pub mod membership_change;
pub mod migration;
pub mod migration_delta;
pub mod money;
pub mod overlay;
pub mod overlay_rules;
pub mod plan;
pub mod plan_rules;
pub mod plan_shape;
pub mod ports;
pub mod price_record;
pub mod price_row;
pub mod projection;
pub mod publish;
pub mod read_model;
pub mod repricing;
pub mod retirement;
pub mod rules;
pub mod scope_key;
pub mod sellability;
pub mod snapshot;
pub mod supersession;
pub mod synthesis;
pub mod tax_display;
pub mod taxonomy;
pub mod validation;
pub mod window;
