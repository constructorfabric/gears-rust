//! The management client trait.

use async_trait::async_trait;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use super::ManagementError;
use super::models::{
    Assignment, AssignmentId, AssignmentSpec, Bundle, BundleId, BundlePatch, BundleVersion,
    NewBundle, PublishOperation, VersionContent, VersionDetail, VersionId,
};

/// Policy management client, registered in `ClientHub` without scope and
/// consumed in-process by tooling and by the REST layer.
///
/// Every method takes the caller's `SecurityContext` first - the only source
/// of the acting subject - and is authorised for the capability its
/// documentation names ([`Capability`](crate::gts::permissions::Capability)):
/// one per method, except the assignment reads, which take either of two.
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

    /// Validates a version's content, in any state, with the checks
    /// activation applies to it. Never activates, never changes the version.
    /// Capability: author.
    ///
    /// # Errors
    ///
    /// [`ValidationFailed`](super::ManagementError::ValidationFailed) with
    /// every finding, each against the document that caused it - the error
    /// activating the content would return; not found; infrastructure
    /// failures that prevent validation (an unreachable registry).
    async fn validate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<(), ManagementError>;

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

    // -- Dry runs ------------------------------------------------------------

    /// Dry-runs a publish operation: runs every check the operation runs, in
    /// its order and against the same state, and writes nothing. Capability:
    /// publish, as the operation itself.
    ///
    /// `Ok` when the operation would succeed now (including as a no-op, such
    /// as activating the active version or withdrawing an absent
    /// assignment). The answer is advisory: it reserves nothing, and the
    /// operation checks again when it runs, so a concurrent writer can still
    /// make it fail ([`ConcurrentChange`](super::ManagementError::ConcurrentChange),
    /// [`AssignmentExists`](super::ManagementError::AssignmentExists)).
    ///
    /// # Errors
    ///
    /// Exactly the error the operation would return, masking included:
    /// content the caller may not see is not found, never refused.
    async fn check_publish(
        &self,
        ctx: &SecurityContext,
        operation: PublishOperation,
    ) -> Result<(), ManagementError>;

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
    /// a bundle the caller may not read;
    /// [`CapabilityDenied`](super::ManagementError::CapabilityDenied) when the
    /// caller may read the bundle but its publish grant does not cover the
    /// bundle or the new assignment.
    async fn assign(
        &self,
        ctx: &SecurityContext,
        spec: AssignmentSpec,
    ) -> Result<Assignment, ManagementError>;

    /// Reads an assignment the caller would see in
    /// [`list_assignments`](Self::list_assignments). Capability: read, or
    /// publish for a dormant assignment at the caller's tenants.
    ///
    /// # Errors
    ///
    /// Not found when absent or not visible to the caller.
    async fn get_assignment(
        &self,
        ctx: &SecurityContext,
        assignment_id: AssignmentId,
    ) -> Result<Assignment, ManagementError>;

    /// Lists, with the platform's cursor pagination, the assignments
    /// visible to the caller:
    ///
    /// - those of a bundle it may read, at a target in its subtree (barriers
    ///   ignored), made by a tenant on its side of every barrier - its own,
    ///   a descendant it reaches, or an ancestor that reaches it; one at a
    ///   target behind a barrier is shown dormant;
    /// - the dormant assignments at tenants it reaches on which it holds
    ///   publish, whatever their bundle.
    ///
    /// Filterable by `bundle_id` and `tenant_id` (the target); a filter
    /// naming content the caller cannot see yields no rows. Capability:
    /// read, or publish for the dormant ones.
    ///
    /// # Errors
    ///
    /// [`CapabilityDenied`](super::ManagementError::CapabilityDenied) when
    /// neither capability is granted; invalid argument for a malformed
    /// filter, limit or cursor;
    /// [`SubtreeTooLarge`](super::ManagementError::SubtreeTooLarge) when the
    /// caller's subtree is too large to evaluate the listing.
    async fn list_assignments(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<Assignment>, ManagementError>;

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
