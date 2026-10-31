//! Setting declarations.

pub mod admin;
pub mod kinds;
pub mod repo;
pub mod service;

pub use admin::{
    CreateDeclaration, Created, DeclarationAdmin, FieldClass, HeldSecret, NOTHING_TO_REVIVE,
    PurgedDeclaration, ReleaseSummary, RevivalPlan, RevivalReadGate, RevivalReport, RevivalRow,
    Walk, strictest_classification,
};
pub use kinds::{DataClassification, DeclarationStatus, UnknownKind};
pub use repo::{
    BrowseCandidate, BrowseNarrowing, CategoryTally, Declaration, DeclarationDraft,
    DeclarationMetadata, DeclarationRepository,
};
pub use service::DeclarationService;

/// Where a declaration came from: authored by an administrator, or
/// contributed by a gear. Every source, in the order the gauges report them.
pub const SOURCES: [&str; 2] = ["admin_authored", "module_contributed"];
