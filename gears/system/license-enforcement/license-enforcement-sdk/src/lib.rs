//! License Enforcement SDK
//!
//! The contract of the `license_enforcement` gear. It has no logic.
//!
//! - [`LicenseEnforcementApi`] — management of grants, packs, and delegations,
//!   with its REST projection [`LicenseEnforcementApiRest`].
//! - [`models`] — the grant model; [`DenialReason`] — why a check denies.
//! - [`GrantAdmissionPluginV1`] — the admission plugin, the only source of root
//!   grants. [`GrantEvaluatorV1`] — decides which grants satisfy a check.
//!   The binary links exactly one of each; see [`register_admission_plugin!`]
//!   and [`register_evaluator!`].
//! - [`error`] — discriminators of the `CanonicalError`s of the contract.
//! - [`RecipientProjectionV1`] — the GTS type that recipient rules address.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

pub mod admission;
pub mod contract;
pub mod dto;
pub mod error;
pub mod evaluator;
pub mod gts;
pub mod ids;
mod link;
pub mod models;
pub mod reason;
pub mod rest;

pub use admission::{
    Admission, AdmissionError, AdmissionInput, AdmissionPluginRegistration, AdmittedRoot,
    CallerIdentity, Expect, GrantAdmissionPluginV1, Intake, Intakes, MaterialError, MaterialWrite,
    NewEvidence, PluginDenial, PluginFault, PluginInfo, PluginMaterial, RejectionReason,
    RevocationState, RootFields, StoredEvidence, Verification, VerifyOutcome,
    linked_admission_plugin,
};
pub use contract::{LicenseEnforcementApi, license_enforcement_api_ir};
pub use evaluator::{
    BindingFilter, Evaluation, EvaluationError, EvaluatorDenial, EvaluatorRegistration,
    GrantEvaluatorV1, linked_evaluator,
};
pub use gts::{RecipientProjectionV1, RecipientStatus};
pub use ids::{DelegationId, EvidenceId, GrantId, PackId};
pub use link::LinkError;
pub use reason::DenialReason;
pub use rest::{LicenseEnforcementApiRest, license_enforcement_api_rest_http_binding};

#[cfg(feature = "rest-client")]
pub use rest::{LicenseEnforcementApiRestClient, LicenseEnforcementApiRestResolvingClient};

#[doc(hidden)]
pub mod __private {
    pub use inventory;
}
