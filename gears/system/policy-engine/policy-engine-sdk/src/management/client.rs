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
/// [`VERSION_NOT_DRAFT`](super::reason::VERSION_NOT_DRAFT).
#[async_trait]
pub trait PolicyManagementClientV1: Send + Sync {
    // -- Bundles -------------------------------------------------------------

    /// Creates a bundle, with no version. Capability: author.
    ///
    /// # Errors
    ///
    /// [`BUNDLE_NAME_TAKEN`](super::reason::BUNDLE_NAME_TAKEN) when the name
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
    /// Not found; [`BUNDLE_NAME_TAKEN`](super::reason::BUNDLE_NAME_TAKEN).
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
    /// [`DRAFT_EXISTS`](super::reason::DRAFT_EXISTS) when the bundle already
    /// has a draft (so a retried call is a conflict, not a second draft);
    /// [`SEED_NOT_IN_BUNDLE`](super::reason::SEED_NOT_IN_BUNDLE).
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

    /// Lists every retained version of a bundle, newest ordinal first,
    /// without content. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found for the bundle.
    async fn list_versions(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
    ) -> Result<Vec<BundleVersion>, ManagementError>;

    /// Replaces the whole content of a draft in one transaction. Operational
    /// limits are checked on write; everything else is left to validation.
    /// Capability: author.
    ///
    /// # Errors
    ///
    /// [`VERSION_NOT_DRAFT`](super::reason::VERSION_NOT_DRAFT);
    /// [`CONTENT_LIMIT_EXCEEDED`](super::reason::CONTENT_LIMIT_EXCEEDED); not
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
    /// [`VERSION_NOT_DRAFT`](super::reason::VERSION_NOT_DRAFT); not found.
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
    /// [`VERSION_NOT_DRAFT`](super::reason::VERSION_NOT_DRAFT) for a
    /// superseded version;
    /// [`VALIDATION_FAILED`](super::reason::VALIDATION_FAILED); not found.
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
    /// [`TENANT_BOUNDARY`](super::reason::TENANT_BOUNDARY) for a tenant behind
    /// a self-managed barrier from the caller's context;
    /// [`ASSIGNMENT_EXISTS`](super::reason::ASSIGNMENT_EXISTS); not found for
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
    /// Not found; [`TENANT_BOUNDARY`](super::reason::TENANT_BOUNDARY).
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
    /// [`TENANT_BOUNDARY`](super::reason::TENANT_BOUNDARY); not found.
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
    /// [`TENANT_BOUNDARY`](super::reason::TENANT_BOUNDARY).
    async fn unassign(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<(), ManagementError>;
}
