//! `SeaORM` entities of the tables Rating owns (DECOMPOSITION §1 table ownership rule). Every
//! table is `bss_rating__<name>` (T-D-60, database ADR-0001) and every entity is `Scopable` on its
//! `tenant_id`, except the platform-scoped source checkpoints, capture rejects and outbox views.

pub mod child_window;
pub mod derived_declaration;
pub mod exception;
pub mod fact;
pub mod fact_head;
pub mod inbox;
pub mod operation;
pub mod operator_audit;
pub mod outbox;
pub mod pricing_binding;
pub mod pricing_resolve;
pub mod source_checkpoint;
pub mod source_loss;
pub mod usage_capture_reject;
pub mod usage_policy_projection;
pub mod usage_record;
pub mod window_schedule;
