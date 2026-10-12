//! Admission Control SDK.
//!
//! Public contract of the admission-control gear:
//!
//! - [`AdmissionClientV1`] - the admission client enforcing gears call.
//! - [`AdmissionEnginePluginClientV1`] - the admission engine plugin contract.
//! - [`models`] - request, verdict, refusal causes and the refusal event
//!   payload published on the audit topic.
//! - [`gts`] - GTS identifiers and schemas this gear owns.
//! - [`error`] - the gear's canonical error family.
//!
//! The admission request carries no subject: identity comes from the
//! `SecurityContext` only. Events carry property names, never values.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]

pub mod api;
pub mod error;
pub mod gts;
pub mod models;
pub mod plugin_api;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod test_support;

pub use api::AdmissionClientV1;
pub use error::{AdmissionError, AdmissionResourceError};
pub use gts::{
    ADMISSION_CONTROL_RESOURCE, AUDIT_TOPIC_ID, AdmissionEnginePluginSpecV1, REFUSAL_EVENT_TYPE,
};
pub use models::{
    Admission, AdmissionRequest, FailureCondition, IDENTIFIER_MAX_LEN, PROPERTY_MAX_DEPTH,
    PolicyReference, RESOURCE_TYPE_MAX_LEN, Refusal, RefusalCause, RefusalEvent, RefusalEventCause,
    SizeBound, Verdict, validate_identifier, validate_resource_type,
};
pub use plugin_api::{
    AdmissionEnginePluginClientV1, EngineFailure, EngineFailureCondition, EngineRequest,
    EngineResult,
};
pub use toolkit_canonical_errors::{self, CanonicalError, Problem};
