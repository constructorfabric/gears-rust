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
