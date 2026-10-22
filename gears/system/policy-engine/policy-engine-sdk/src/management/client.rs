//! The management client trait.

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use super::ManagementError;
use super::models::{
    Assignment, AssignmentId, AssignmentSpec, Bundle, BundleId, BundlePatch, BundleVersion,
    NewBundle, ValidationReport, VersionContent, VersionDetail, VersionId,
};

/// Policy management client, registered in `ClientHub` without scope and
/// consumed in-process by tooling and by the REST layer.
///
/// Every method takes the caller's `SecurityContext` first - the only source
/// of the acting subject - and is authorised for the one capability its
/// documentation names ([`Capability`](crate::gts::permissions::Capability)).
/// Content the caller may not see is reported as not found. Only a draft can
/// be modified; anything else is refused with
/// [`VersionNotDraft`](super::ManagementError::VersionNotDraft).
#[async_trait]
pub trait PolicyManagementClientV1: Send + Sync {
    // -- Bundles -------------------------------------------------------------

    /// Creates a bundle, with no version. Capability: author.
    ///
    /// # Errors
    ///
    /// [`BundleNameTaken`](super::ManagementError::BundleNameTaken) when the name
    /// is used in the owning tenant; not found for an owning tenant the
    /// caller may not manage.
    async fn create_bundle(
        &self,
        ctx: &SecurityContext,
        bundle: NewBundle,
    ) -> Result<Bundle, ManagementError>;

    /// Reads a bundle. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found when absent or not visible to the caller.
    async fn get_bundle(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
    ) -> Result<Bundle, ManagementError>;

    /// Lists the bundles of the tenants the caller may manage with the
    /// platform's cursor pagination. Capability: read.
    ///
    /// # Errors
    ///
    /// Invalid argument for a malformed limit or cursor.
    async fn list_bundles(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Bundle>, ManagementError>;

    /// Changes a bundle's name or description. Capability: author.
    ///
    /// # Errors
    ///
    /// Not found; [`BundleNameTaken`](super::ManagementError::BundleNameTaken).
    async fn update_bundle(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        patch: BundlePatch,
    ) -> Result<Bundle, ManagementError>;

    // -- Versions ------------------------------------------------------------

    /// Creates the bundle's draft with the next ordinal: empty, or seeded
    /// with a copy of the content of `seed_from`, any retained version of the
    /// same bundle. A bundle has at most one open draft. Capability: author.
    ///
    /// # Errors
    ///
    /// Not found for the bundle;
    /// [`DraftExists`](super::ManagementError::DraftExists) when the bundle already
    /// has a draft (so a retried call is a conflict, not a second draft);
    /// [`SeedNotInBundle`](super::ManagementError::SeedNotInBundle).
    async fn create_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        seed_from: Option<VersionId>,
    ) -> Result<BundleVersion, ManagementError>;

    /// Reads a version with its documents. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found when the version is absent, not of this bundle, or not
    /// visible.
    async fn get_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<VersionDetail, ManagementError>;

    /// Lists the retained versions of a bundle, newest ordinal first,
    /// without content, with the platform's cursor pagination. Capability:
    /// read.
    ///
    /// # Errors
    ///
    /// Not found for the bundle; invalid argument for a malformed limit or
    /// cursor.
    async fn list_versions(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        query: &ODataQuery,
    ) -> Result<Page<BundleVersion>, ManagementError>;

    /// Replaces the whole content of a draft in one transaction. Operational
    /// limits are checked on write; everything else is left to validation.
    /// Capability: author.
    ///
    /// # Errors
    ///
    /// [`VersionNotDraft`](super::ManagementError::VersionNotDraft);
    /// [`ContentLimitExceeded`](super::ManagementError::ContentLimitExceeded); not
    /// found.
    async fn replace_draft_content(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
        content: VersionContent,
    ) -> Result<VersionDetail, ManagementError>;

    /// Deletes a draft with its documents. Capability: author.
    ///
    /// # Errors
    ///
    /// [`VersionNotDraft`](super::ManagementError::VersionNotDraft); not found.
    async fn delete_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<(), ManagementError>;

    /// Validates a version and reports every finding against the document
    /// that caused it. Never activates, never changes the version.
    /// Capability: author.
    ///
    /// # Errors
    ///
    /// Not found; infrastructure failures that prevent validation (an
    /// unreachable registry). Invalid content is a report, not an error.
    async fn validate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<ValidationReport, ManagementError>;

    /// Activates a draft: validates and compiles it, and supersedes the
    /// bundle's previously active version in the same transaction. Activating
    /// the already active version succeeds without changing anything.
    /// Capability: publish.
    ///
    /// # Errors
    ///
    /// [`VersionNotDraft`](super::ManagementError::VersionNotDraft) for a
    /// superseded version;
    /// [`ValidationFailed`](super::ManagementError::ValidationFailed); not found.
    async fn activate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<BundleVersion, ManagementError>;

    // -- Assignments ---------------------------------------------------------

    /// Assigns a bundle to a tenant. It applies to the tenant and its
    /// descendants through whichever version of the bundle is active.
    /// Capability: publish.
    ///
    /// # Errors
    ///
    /// [`TenantBoundary`](super::ManagementError::TenantBoundary) for a tenant behind
    /// a self-managed barrier from the caller's context;
    /// [`AssignmentExists`](super::ManagementError::AssignmentExists); not found for
    /// the bundle.
    async fn assign(
        &self,
        ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<Assignment, ManagementError>;

    /// Reads an assignment. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found; [`TenantBoundary`](super::ManagementError::TenantBoundary).
    async fn get_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<Assignment, ManagementError>;

    /// Sets whether an assignment enforces its bundle's denials or only
    /// reports them. Capability: publish.
    ///
    /// # Errors
    ///
    /// [`TenantBoundary`](super::ManagementError::TenantBoundary); not found.
    async fn update_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
        enforce: bool,
    ) -> Result<Assignment, ManagementError>;

    /// Withdraws an assignment; withdrawing one that does not exist succeeds.
    /// Capability: publish.
    ///
    /// # Errors
    ///
    /// [`TenantBoundary`](super::ManagementError::TenantBoundary).
    async fn unassign(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<(), ManagementError>;
}
