//! Content repository ports: bundles, versions (with documents),
//! assignments and the service-scope active-content load.

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::RepoError;
use crate::domain::model::{
    Assignment, AssignmentId, Bundle, BundleId, BundleVersion, Document, VersionId,
};
use crate::domain::ports::ActiveBinding;

// ---------------------------------------------------------------------------
// Values the ports exchange
// ---------------------------------------------------------------------------

/// Result of [`VersionRepository::activate`].
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct ActivationOutcome {
    /// The version now active (header only: `documents` is empty).
    pub activated: BundleVersion,
    /// The version of the same bundle that was active before and is now
    /// superseded, if any.
    pub superseded: Option<VersionId>,
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// `policy_engine__bundle`.
#[async_trait]
pub trait BundleRepository: Send + Sync {
    /// Inserts `bundle` as given (identity and timestamps supplied by the
    /// caller).
    ///
    /// # Errors
    ///
    /// `Conflict(BUNDLE_NAME_TAKEN)` when the owning tenant already has the
    /// name; `NotFound` when the owning tenant is outside `scope`.
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle: &Bundle,
    ) -> Result<Bundle, RepoError>;

    /// The bundle, if it exists within `scope`.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: BundleId,
    ) -> Result<Option<Bundle>, RepoError>;

    /// One `OData` page of the bundles within `scope`.
    ///
    /// # Errors
    ///
    /// `Query` for a refused order, cursor or limit.
    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Bundle>, RepoError>;

    /// Replaces the name and/or description (`None` leaves it) and sets
    /// `updated_at = at`.
    ///
    /// # Errors
    ///
    /// `NotFound`; `Conflict(BUNDLE_NAME_TAKEN)`.
    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: BundleId,
        name: Option<&str>,
        description: Option<&str>,
        at: OffsetDateTime,
    ) -> Result<Bundle, RepoError>;
}

/// `policy_engine__bundle_version` with its `policy_engine__document` rows.
#[async_trait]
pub trait VersionRepository: Send + Sync {
    /// The ordinal the next version of `bundle_id` takes: one more than the
    /// highest within `scope`, `1` for the first.
    async fn next_ordinal<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle_id: BundleId,
    ) -> Result<i32, RepoError>;

    /// Inserts a draft with its documents. Several statements: pass a
    /// transaction runner for atomicity.
    ///
    /// # Errors
    ///
    /// `Conflict(VERSION_NOT_DRAFT)` when `version.state` is not draft,
    /// `Conflict(DRAFT_EXISTS)` when the bundle already has a draft (or a
    /// concurrent creation took the ordinal),
    /// `Conflict(DUPLICATE_DOCUMENT_NAME)`, `NotFound` when the bundle is
    /// absent or outside `scope`.
    async fn insert_draft<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        version: &BundleVersion,
    ) -> Result<BundleVersion, RepoError>;

    /// The version with its documents, if within `scope`. Documents are
    /// ordered by name.
    async fn get_with_content<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
    ) -> Result<Option<BundleVersion>, RepoError>;

    /// Headers (`documents` empty) of the bundle's versions within `scope`,
    /// newest ordinal first.
    async fn list_for_bundle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle_id: BundleId,
    ) -> Result<Vec<BundleVersion>, RepoError>;

    /// Replaces a draft's documents. Several statements: pass a transaction
    /// runner.
    ///
    /// # Errors
    ///
    /// `NotFound`, `Conflict(VERSION_NOT_DRAFT)`,
    /// `Conflict(DUPLICATE_DOCUMENT_NAME)`.
    async fn replace_draft_content<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
        documents: &[Document],
    ) -> Result<BundleVersion, RepoError>;

    /// Removes a draft; its documents cascade.
    ///
    /// # Errors
    ///
    /// `NotFound`, `Conflict(VERSION_NOT_DRAFT)`.
    async fn delete_draft<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
    ) -> Result<(), RepoError>;

    /// Draft -> active, recording `actor` and `at`, and the bundle's
    /// previously active version -> superseded, on the same runner. Pass a
    /// transaction runner: on any error the caller rolls back, so the
    /// supersession never commits without the activation. The partial unique
    /// index (one active version per bundle) is the final guard against a
    /// concurrent activation of the same bundle.
    ///
    /// # Errors
    ///
    /// `NotFound`, `Conflict(VERSION_NOT_DRAFT)`,
    /// `Conflict(CONCURRENT_ACTIVATION)` when the index refused the change.
    async fn activate<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
        actor: Uuid,
        at: OffsetDateTime,
    ) -> Result<ActivationOutcome, RepoError>;
}

/// `policy_engine__assignment`.
#[async_trait]
pub trait AssignmentRepository: Send + Sync {
    /// Inserts `assignment` as given.
    ///
    /// # Errors
    ///
    /// `Conflict(ASSIGNMENT_EXISTS)` for a second assignment of the bundle to
    /// the same tenant; `NotFound` when the bundle is absent or the owning
    /// tenant is outside `scope`.
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        assignment: &Assignment,
    ) -> Result<Assignment, RepoError>;

    /// The assignment, if within `scope`.
    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: AssignmentId,
    ) -> Result<Option<Assignment>, RepoError>;

    /// Sets the `enforce` flag and `updated_at = at`.
    ///
    /// # Errors
    ///
    /// `NotFound`.
    async fn set_enforce<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: AssignmentId,
        enforce: bool,
        at: OffsetDateTime,
    ) -> Result<Assignment, RepoError>;

    /// Removes the assignment.
    ///
    /// # Errors
    ///
    /// `NotFound` when it does not exist within `scope`.
    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: AssignmentId,
    ) -> Result<(), RepoError>;
}

/// Service-scope read of the active content along a tenant chain.
#[async_trait]
pub trait ActiveContentLoader: Send + Sync {
    /// The assignments at any of `tenants` whose bundle has an active
    /// version, each with that version and its documents. Never drafts,
    /// superseded versions or bundles without an active version.
    async fn load_for_tenants<C: DBRunner>(
        &self,
        runner: &C,
        tenants: &[Uuid],
    ) -> Result<Vec<ActiveBinding>, RepoError>;
}
