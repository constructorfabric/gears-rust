//! [`PolicyManagementLocalClient`]: the in-process
//! [`PolicyManagementClientV1`], registered in `ClientHub` without scope and
//! consumed by tooling and by the REST layer.
//!
//! Pure delegation to the [`ManagementService`]. Every method passes the caller's
//! `SecurityContext` through unchanged; no authorisation, projection or
//! error mapping happens here, so the local client and the REST projection
//! answer identically.

use std::sync::Arc;

use async_trait::async_trait;
use policy_engine_sdk::management::{
    Assignment, AssignmentId, AssignmentSpec, Bundle, BundleId, BundlePatch, BundleVersion,
    ManagementError, NewBundle, PolicyManagementClientV1, PublishOperation, VersionContent,
    VersionDetail, VersionId,
};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use super::ContentStore;
use super::service::ManagementService;

/// The management client over the in-process services.
#[domain_model]
pub struct PolicyManagementLocalClient<S: ContentStore> {
    service: Arc<ManagementService<S>>,
}

impl<S: ContentStore> std::fmt::Debug for PolicyManagementLocalClient<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyManagementLocalClient")
            .field("service", &self.service)
            .finish()
    }
}

impl<S: ContentStore> PolicyManagementLocalClient<S> {
    /// Delegates to `service`.
    #[must_use]
    pub const fn new(service: Arc<ManagementService<S>>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<S: ContentStore> PolicyManagementClientV1 for PolicyManagementLocalClient<S> {
    async fn create_bundle(
        &self,
        ctx: &SecurityContext,
        bundle: NewBundle,
    ) -> Result<Bundle, ManagementError> {
        self.service.create_bundle(ctx, bundle).await
    }

    async fn get_bundle(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
    ) -> Result<Bundle, ManagementError> {
        self.service.get_bundle(ctx, bundle_id).await
    }

    async fn list_bundles(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Bundle>, ManagementError> {
        self.service.list_bundles(ctx, query).await
    }

    async fn update_bundle(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        patch: BundlePatch,
    ) -> Result<Bundle, ManagementError> {
        self.service.update_bundle(ctx, bundle_id, patch).await
    }

    async fn create_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        seed_from: Option<VersionId>,
    ) -> Result<BundleVersion, ManagementError> {
        self.service
            .create_draft_version(ctx, bundle_id, seed_from)
            .await
    }

    async fn get_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<VersionDetail, ManagementError> {
        self.service.get_version(ctx, bundle_id, version_id).await
    }

    async fn list_versions(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        query: &ODataQuery,
    ) -> Result<Page<BundleVersion>, ManagementError> {
        self.service.list_versions(ctx, bundle_id, query).await
    }

    async fn replace_draft_content(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
        content: VersionContent,
    ) -> Result<VersionDetail, ManagementError> {
        self.service
            .replace_draft_content(ctx, bundle_id, version_id, content)
            .await
    }

    async fn delete_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<(), ManagementError> {
        self.service
            .delete_draft_version(ctx, bundle_id, version_id)
            .await
    }

    async fn validate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<(), ManagementError> {
        self.service
            .validate_version(ctx, bundle_id, version_id)
            .await
    }

    async fn activate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<BundleVersion, ManagementError> {
        self.service
            .activate_version(ctx, bundle_id, version_id)
            .await
    }

    async fn check_publish(
        &self,
        ctx: &SecurityContext,
        operation: PublishOperation,
    ) -> Result<(), ManagementError> {
        self.service.check_publish(ctx, operation).await
    }

    async fn assign(
        &self,
        ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<Assignment, ManagementError> {
        self.service.assign(ctx, spec).await
    }

    async fn get_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<Assignment, ManagementError> {
        self.service.get_assignment(ctx, assignment_id).await
    }

    async fn list_assignments(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Assignment>, ManagementError> {
        self.service.list_assignments(ctx, query).await
    }

    async fn update_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
        enforce: bool,
    ) -> Result<Assignment, ManagementError> {
        self.service
            .update_assignment(ctx, assignment_id, enforce)
            .await
    }

    async fn unassign(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<(), ManagementError> {
        self.service.unassign(ctx, assignment_id).await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
