//! Policy Engine SDK.
//!
//! Public contract of the policy-engine gear:
//!
//! - [`management`] - [`PolicyManagementClientV1`], the administration surface
//!   (content lifecycle, assignments, validation), its models and reason codes.
//! - [`gts`] - GTS identifiers this gear owns: resource types, the admission
//!   engine plugin instance id, the management permission catalog and the
//!   policy entrypoint.
//! - [`error`] - reason codes of decisions.
//!
//! The engine itself is consumed only as an admission-control engine plugin.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![deny(missing_docs)]

pub mod error;
pub mod gts;
pub mod management;

pub use error::reason;
pub use gts::{
    ADMISSION_ENGINE_INSTANCE_ID, ASSIGNMENT_RESOURCE, BUNDLE_RESOURCE, BUNDLE_VERSION_RESOURCE,
    POLICY_ENTRYPOINT,
};
pub use management::{ManagementError, PolicyManagementClientV1};
pub use toolkit_canonical_errors::{self, CanonicalError, Problem};
