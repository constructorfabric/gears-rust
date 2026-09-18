// Created: 2026-08-26 by Constructor Tech
//! Setting declarations.

pub mod admin;
pub mod repo;
pub mod service;

pub use admin::{CreateDeclaration, Created, DeclarationAdmin, FieldClass};
pub use repo::{Declaration, DeclarationDraft, DeclarationMetadata, DeclarationRepository};
pub use service::DeclarationService;
