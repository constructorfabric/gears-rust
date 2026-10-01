//! Management service: the content lifecycle behind
//! [`PolicyManagementClientV1`](policy_engine_sdk::PolicyManagementClientV1).
//!
//! - [`authz`]: [`ManagementAuthorizer`], one authorisation per capability
//!   against the caller's own context.
//! - [`lifecycle`]: conversions between the SDK's content model and the
//!   domain's, and the write-time content checks.
//! - [`service`]: [`ManagementService`], bundles and versions: create, read,
//!   list, update, draft (empty or seeded), replace, validate, activate,
//!   delete draft.
//! - [`assignments`]: [`GovernanceService`], the assignment surface.
//! - [`error`]: the projection of every condition onto the SDK's error table
//!   ([`policy_engine_sdk::management`] module documentation).
//!
//! # Security
//!
//! Every method asks the policy enforcer for exactly the capability the SDK
//! documents, with the content owner as the `owner_tenant_id` resource
//! property; mutations run with the returned scope in the statement's
//! `WHERE`; a deny-all scope is a denial. Reads of a specific bundle or
//! version the caller may not read are `not_found`; a targeted mutation the
//! caller may not perform is `not_found` unless the caller can read the
//! content (then `CAPABILITY_DENIED`).

use toolkit_macros::domain_model;

use crate::domain::repos::{AssignmentRepository, BundleRepository, VersionRepository};

pub mod assignments;
pub mod authz;
pub mod error;
pub mod lifecycle;
pub mod local_client;
pub mod service;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_support;

pub use assignments::{GovernanceService, Reach, assignment_to_sdk};
pub use authz::{AccessTarget, Authorization, AuthzError, ManagementAuthorizer};
pub use error::{ManagementFailure, reason_of};
pub use local_client::PolicyManagementLocalClient;
pub use service::{ManagementService, ManagementServiceParts};

/// The repositories the management service composes into one transaction.
///
/// An associated type per port because the ports are generic over the
/// database runner and therefore not object safe. [`StoreSet`] is the plain
/// implementation the gear instantiates with the storage adapters.
///
/// The associated types are bounded by the repository ports, which are
/// generic over the sealed `toolkit_db` runner (`domain::repos` module
/// documentation); no `sea_orm` type is used.
#[allow(unknown_lints, de0301_no_infra_in_domain)]
pub trait ContentStore: Send + Sync + 'static {
    /// `policy_engine__bundle`.
    type Bundles: BundleRepository + 'static;
    /// `policy_engine__bundle_version` with documents.
    type Versions: VersionRepository + 'static;
    /// `policy_engine__assignment`.
    type Assignments: AssignmentRepository + 'static;

    /// The bundle repository.
    fn bundles(&self) -> &Self::Bundles;
    /// The version repository.
    fn versions(&self) -> &Self::Versions;
    /// The assignment repository.
    fn assignments(&self) -> &Self::Assignments;
}

/// A [`ContentStore`] made of one value per repository.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct StoreSet<B, V, A> {
    /// Bundles.
    pub bundles: B,
    /// Versions.
    pub versions: V,
    /// Assignments.
    pub assignments: A,
}

#[allow(unknown_lints, de0301_no_infra_in_domain)]
impl<B, V, A> ContentStore for StoreSet<B, V, A>
where
    B: BundleRepository + 'static,
    V: VersionRepository + 'static,
    A: AssignmentRepository + 'static,
{
    type Bundles = B;
    type Versions = V;
    type Assignments = A;

    fn bundles(&self) -> &B {
        &self.bundles
    }
    fn versions(&self) -> &V {
        &self.versions
    }
    fn assignments(&self) -> &A {
        &self.assignments
    }
}
