// Created: 2026-08-26 by Virtuozzo International GmbH
//! Setting declarations.

pub mod admin;
pub mod repo;
pub mod service;

pub use admin::{CreateDeclaration, Created, DeclarationAdmin, FieldClass};
pub use repo::{Declaration, DeclarationDraft, DeclarationMetadata, DeclarationRepository};
pub use service::DeclarationService;

/// Where a declaration came from: authored by an administrator, or
/// contributed by a gear. Every source, in the order the gauges report them.
pub const SOURCES: [&str; 2] = ["admin_authored", "module_contributed"];
