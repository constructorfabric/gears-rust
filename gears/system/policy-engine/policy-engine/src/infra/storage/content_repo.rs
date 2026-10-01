//! `SeaORM` implementations of the content repository ports
//! ([`crate::domain::repos`]) over the secure data layer.
//!
//! # Guardrails
//!
//! - Every query and write goes through `.secure().scope_with(scope)`,
//!   `secure_insert` or `secure_insert_many`; raw `Select::{all,one,count}`,
//!   `UpdateMany::exec` and `DeleteMany::exec` are banned by the workspace
//!   clippy configuration, and no SQL text is written here.
//! - Management methods use the caller's scope unchanged. A row outside it is
//!   reported as absent: scope refusals of the secure layer
//!   (`TenantNotInScope`, `Denied`) map to [`RepoError::NotFound`].
//! - Writes that attach a row to a parent (a version to its bundle, an
//!   assignment to its bundle) first read the parent **with the caller's
//!   scope**, so a caller cannot attach to a parent it cannot see even when
//!   it knows the parent's identity (the foreign key alone would accept it).
//!
//! # Service scope
//!
//! [`service_scope`] (`AccessScope::allow_all()`) is used by exactly the
//! decision path: [`OrmActiveContentLoader`] reads the `assignment` rows of a
//! tenant chain, their bundles' active `bundle_version` rows and those
//! versions' `document` rows.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use policy_engine_sdk::management::odata::BundleFilterField;
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder};
use time::{OffsetDateTime, UtcOffset};
use toolkit_db::odata::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureUpdateExt,
    secure_insert, secure_insert_many,
};
use toolkit_odata::{ODataQuery, Page, SortDir};
use uuid::Uuid;

use super::entity::{assignment, bundle, bundle_version, document};
use super::mapper;
use super::odata_mapper::BundleODataMapper;
use crate::domain::model::{
    Assignment, AssignmentId, Bundle, BundleId, BundleVersion, Document, VersionId, VersionState,
};
use crate::domain::ports::ActiveBinding;
use crate::domain::repos::{
    ActivationOutcome, ActiveContentLoader, AssignmentRepository, BundleRepository, RepoError,
    VersionRepository, conflict,
};

/// Largest `IN (...)` list one statement binds; well under every supported
/// backend's parameter limit.
const IN_CHUNK: usize = 500;

const DRAFT: i16 = VersionState::Draft.code();
const ACTIVE: i16 = VersionState::Active.code();
const SUPERSEDED: i16 = VersionState::Superseded.code();

/// The gear's own scope for the request-independent statements listed in
/// the module documentation, and for nothing else.
fn service_scope() -> AccessScope {
    AccessScope::allow_all()
}

fn utc(at: OffsetDateTime) -> OffsetDateTime {
    at.to_offset(UtcOffset::UTC)
}

/// Scope refusals read as absence; everything else is a storage failure.
#[allow(clippy::needless_pass_by_value)] // used as `map_err(scope_err)`
fn scope_err(e: ScopeError) -> RepoError {
    match e {
        ScopeError::TenantNotInScope { .. } | ScopeError::Denied(_) => RepoError::NotFound,
        other => RepoError::Database(other.to_string()),
    }
}

/// Like [`scope_err`], with a unique violation reported as `on_unique` and a
/// foreign-key violation (missing parent) as absence.
fn write_err(e: ScopeError, on_unique: &'static str) -> RepoError {
    if e.is_unique_violation() {
        RepoError::conflict(on_unique)
    } else if e.is_foreign_key_violation() {
        RepoError::NotFound
    } else {
        scope_err(e)
    }
}

fn odata_err(e: toolkit_odata::Error) -> RepoError {
    match e {
        toolkit_odata::Error::Db(message) => RepoError::Database(message),
        other => RepoError::Query(other),
    }
}

// ---------------------------------------------------------------------------
// Shared version helpers
// ---------------------------------------------------------------------------

async fn version_row<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    id: VersionId,
) -> Result<Option<bundle_version::Model>, RepoError> {
    bundle_version::Entity::find()
        .filter(bundle_version::Column::Id.eq(id.0))
        .secure()
        .scope_with(scope)
        .one(runner)
        .await
        .map_err(scope_err)
}

/// The version row of `id`, required to be a draft: absent (or outside the
/// scope) is `NotFound`, any other state `VERSION_NOT_DRAFT`.
async fn draft_row<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    id: VersionId,
) -> Result<bundle_version::Model, RepoError> {
    match version_row(runner, scope, id).await? {
        None => Err(RepoError::NotFound),
        Some(row) if row.state != DRAFT => Err(RepoError::conflict(conflict::VERSION_NOT_DRAFT)),
        Some(row) => Ok(row),
    }
}

/// Loads the documents of `versions` with `scope` and assembles them,
/// documents by name.
async fn with_content<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    versions: Vec<bundle_version::Model>,
) -> Result<Vec<BundleVersion>, RepoError> {
    let version_ids: Vec<Uuid> = versions.iter().map(|v| v.id).collect();
    let mut documents_by_version: HashMap<Uuid, Vec<Document>> = HashMap::new();
    for chunk in version_ids.chunks(IN_CHUNK) {
        let rows = document::Entity::find()
            .filter(document::Column::VersionId.is_in(chunk.iter().copied()))
            .order_by_asc(document::Column::Name)
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(scope_err)?;
        for row in rows {
            let version_id = row.version_id;
            documents_by_version
                .entry(version_id)
                .or_default()
                .push(mapper::document_from_model(&row)?);
        }
    }
    versions
        .into_iter()
        .map(|row| {
            let id = row.id;
            let mut version = mapper::version_header_from_model(&row)?;
            version.documents = documents_by_version.remove(&id).unwrap_or_default();
            Ok(version)
        })
        .collect()
}

/// Inserts `documents` under `version_id`, owned by `owner_tenant_id`.
async fn insert_content<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    version_id: VersionId,
    owner_tenant_id: Uuid,
    documents: &[Document],
) -> Result<(), RepoError> {
    if documents.is_empty() {
        return Ok(());
    }
    let rows = documents
        .iter()
        .map(|d| mapper::document_active_model(version_id, owner_tenant_id, d))
        .collect::<Vec<_>>();
    secure_insert_many::<document::Entity>(rows, scope, runner)
        .await
        .map_err(|e| write_err(e, conflict::DUPLICATE_DOCUMENT_NAME))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Bundles
// ---------------------------------------------------------------------------

/// [`BundleRepository`] over `policy_engine__bundle`.
#[derive(Clone, Copy)]
pub struct OrmBundleRepository {
    limit_cfg: LimitCfg,
}

impl OrmBundleRepository {
    /// Repository whose listings page with `limit_cfg`.
    #[must_use]
    pub const fn new(limit_cfg: LimitCfg) -> Self {
        Self { limit_cfg }
    }
}

#[async_trait]
impl BundleRepository for OrmBundleRepository {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle: &Bundle,
    ) -> Result<Bundle, RepoError> {
        let row =
            secure_insert::<bundle::Entity>(mapper::bundle_active_model(bundle), scope, runner)
                .await
                .map_err(|e| write_err(e, conflict::BUNDLE_NAME_TAKEN))?;
        Ok(mapper::bundle_from_model(row))
    }

    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: BundleId,
    ) -> Result<Option<Bundle>, RepoError> {
        let row = bundle::Entity::find()
            .filter(bundle::Column::Id.eq(id.0))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(scope_err)?;
        Ok(row.map(mapper::bundle_from_model))
    }

    async fn list_page<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        query: &ODataQuery,
    ) -> Result<Page<Bundle>, RepoError> {
        let base = bundle::Entity::find().secure().scope_with(scope);
        paginate_odata::<BundleFilterField, BundleODataMapper, _, _, _, _>(
            base,
            runner,
            query,
            ("id", SortDir::Desc),
            self.limit_cfg,
            mapper::bundle_from_model,
        )
        .await
        .map_err(odata_err)
    }

    async fn update<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: BundleId,
        name: Option<&str>,
        description: Option<&str>,
        at: OffsetDateTime,
    ) -> Result<Bundle, RepoError> {
        let mut update = bundle::Entity::update_many()
            .col_expr(bundle::Column::UpdatedAt, Expr::value(utc(at)))
            .filter(bundle::Column::Id.eq(id.0));
        if let Some(name) = name {
            update = update.col_expr(bundle::Column::Name, Expr::value(name.to_owned()));
        }
        if let Some(description) = description {
            let stored = (!description.is_empty()).then(|| description.to_owned());
            update = update.col_expr(bundle::Column::Description, Expr::value(stored));
        }
        let result = update
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(|e| write_err(e, conflict::BUNDLE_NAME_TAKEN))?;
        if result.rows_affected == 0 {
            return Err(RepoError::NotFound);
        }
        self.get(runner, scope, id)
            .await?
            .ok_or(RepoError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// [`VersionRepository`] over `policy_engine__bundle_version` and
/// `policy_engine__document`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrmVersionRepository;

#[async_trait]
impl VersionRepository for OrmVersionRepository {
    async fn next_ordinal<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle_id: BundleId,
    ) -> Result<i32, RepoError> {
        let highest = bundle_version::Entity::find()
            .filter(bundle_version::Column::BundleId.eq(bundle_id.0))
            .order_by_desc(bundle_version::Column::Ordinal)
            .secure()
            .scope_with(scope)
            .limit(1)
            .one(runner)
            .await
            .map_err(scope_err)?;
        match highest {
            None => Ok(1),
            Some(row) => row.ordinal.checked_add(1).ok_or_else(|| {
                RepoError::Database(format!("bundle {bundle_id}: version ordinal overflow"))
            }),
        }
    }

    async fn insert_draft<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        version: &BundleVersion,
    ) -> Result<BundleVersion, RepoError> {
        if version.state != VersionState::Draft {
            return Err(RepoError::conflict(conflict::VERSION_NOT_DRAFT));
        }
        // The parent must be visible to the caller; the foreign key alone
        // would accept any existing bundle.
        let parent = bundle::Entity::find()
            .filter(bundle::Column::Id.eq(version.bundle_id.0))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(scope_err)?
            .ok_or(RepoError::NotFound)?;
        if parent.owner_tenant_id != version.owner_tenant_id {
            return Err(RepoError::Database(format!(
                "version {} must be owned by its bundle's tenant {}",
                version.id, parent.owner_tenant_id
            )));
        }
        let row = secure_insert::<bundle_version::Entity>(
            mapper::version_active_model(version),
            scope,
            runner,
        )
        .await
        .map_err(|e| write_err(e, conflict::DRAFT_EXISTS))?;
        insert_content(
            runner,
            scope,
            version.id,
            version.owner_tenant_id,
            &version.documents,
        )
        .await?;
        let mut out = mapper::version_header_from_model(&row)?;
        out.documents.clone_from(&version.documents);
        Ok(out)
    }

    async fn get_with_content<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
    ) -> Result<Option<BundleVersion>, RepoError> {
        let Some(row) = version_row(runner, scope, id).await? else {
            return Ok(None);
        };
        Ok(with_content(runner, scope, vec![row]).await?.pop())
    }

    async fn list_for_bundle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle_id: BundleId,
    ) -> Result<Vec<BundleVersion>, RepoError> {
        bundle_version::Entity::find()
            .filter(bundle_version::Column::BundleId.eq(bundle_id.0))
            .order_by_desc(bundle_version::Column::Ordinal)
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(scope_err)?
            .into_iter()
            .map(|row| mapper::version_header_from_model(&row))
            .collect()
    }

    async fn replace_draft_content<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
        documents: &[Document],
    ) -> Result<BundleVersion, RepoError> {
        let row = draft_row(runner, scope, id).await?;
        document::Entity::delete_many()
            .filter(document::Column::VersionId.eq(id.0))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(scope_err)?;
        insert_content(runner, scope, id, row.owner_tenant_id, documents).await?;
        let mut out = mapper::version_header_from_model(&row)?;
        out.documents = documents.to_vec();
        Ok(out)
    }

    async fn delete_draft<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
    ) -> Result<(), RepoError> {
        draft_row(runner, scope, id).await?;
        // Documents cascade in the same statement.
        bundle_version::Entity::delete_many()
            .filter(
                Condition::all()
                    .add(bundle_version::Column::Id.eq(id.0))
                    .add(bundle_version::Column::State.eq(DRAFT)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(scope_err)?;
        Ok(())
    }

    async fn activate<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: VersionId,
        actor: Uuid,
        at: OffsetDateTime,
    ) -> Result<ActivationOutcome, RepoError> {
        let row = draft_row(runner, scope, id).await?;
        let previous = bundle_version::Entity::find()
            .filter(
                Condition::all()
                    .add(bundle_version::Column::BundleId.eq(row.bundle_id))
                    .add(bundle_version::Column::State.eq(ACTIVE)),
            )
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(scope_err)?;
        if let Some(previous) = &previous {
            bundle_version::Entity::update_many()
                .col_expr(bundle_version::Column::State, Expr::value(SUPERSEDED))
                .filter(
                    Condition::all()
                        .add(bundle_version::Column::Id.eq(previous.id))
                        .add(bundle_version::Column::State.eq(ACTIVE)),
                )
                .secure()
                .scope_with(scope)
                .exec(runner)
                .await
                .map_err(scope_err)?;
        }
        // Conditional on the draft state again: the read above is advisory,
        // this statement is the precondition. The partial unique index
        // refuses a second active version committed concurrently.
        let result = bundle_version::Entity::update_many()
            .col_expr(bundle_version::Column::State, Expr::value(ACTIVE))
            .col_expr(bundle_version::Column::ActivatedAt, Expr::value(utc(at)))
            .col_expr(bundle_version::Column::ActivatedBy, Expr::value(actor))
            .filter(
                Condition::all()
                    .add(bundle_version::Column::Id.eq(id.0))
                    .add(bundle_version::Column::State.eq(DRAFT)),
            )
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(|e| write_err(e, conflict::CONCURRENT_ACTIVATION))?;
        if result.rows_affected == 0 {
            return Err(RepoError::conflict(conflict::VERSION_NOT_DRAFT));
        }
        let activated = version_row(runner, scope, id)
            .await?
            .ok_or(RepoError::NotFound)
            .and_then(|row| mapper::version_header_from_model(&row))?;
        Ok(ActivationOutcome {
            activated,
            superseded: previous.map(|p| VersionId(p.id)),
        })
    }
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

/// [`AssignmentRepository`] over `policy_engine__assignment`.
#[derive(Debug, Clone, Copy, Default)]
pub struct OrmAssignmentRepository;

impl OrmAssignmentRepository {
    /// The bundle's assignments within `scope`, by tenant then identity.
    /// Test support only (asserting what a mutation wrote); no production
    /// surface lists assignments by bundle.
    ///
    /// # Errors
    ///
    /// `Database` on storage failure.
    #[cfg(test)]
    pub async fn list_for_bundle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle_id: BundleId,
    ) -> Result<Vec<Assignment>, RepoError> {
        Ok(assignment::Entity::find()
            .filter(assignment::Column::BundleId.eq(bundle_id.0))
            .order_by_asc(assignment::Column::TenantId)
            .order_by_asc(assignment::Column::Id)
            .secure()
            .scope_with(scope)
            .all(runner)
            .await
            .map_err(scope_err)?
            .into_iter()
            .map(|row| mapper::assignment_from_model(&row))
            .collect())
    }
}

#[async_trait]
impl AssignmentRepository for OrmAssignmentRepository {
    async fn insert<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        assignment: &Assignment,
    ) -> Result<Assignment, RepoError> {
        // The bundle must be visible to the caller; the foreign key alone
        // would accept any existing bundle.
        let visible = bundle::Entity::find()
            .filter(bundle::Column::Id.eq(assignment.bundle_id.0))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(scope_err)?;
        if visible.is_none() {
            return Err(RepoError::NotFound);
        }
        let row = secure_insert::<assignment::Entity>(
            mapper::assignment_active_model(assignment),
            scope,
            runner,
        )
        .await
        .map_err(|e| write_err(e, conflict::ASSIGNMENT_EXISTS))?;
        Ok(mapper::assignment_from_model(&row))
    }

    async fn get<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: AssignmentId,
    ) -> Result<Option<Assignment>, RepoError> {
        let row = assignment::Entity::find()
            .filter(assignment::Column::Id.eq(id.0))
            .secure()
            .scope_with(scope)
            .one(runner)
            .await
            .map_err(scope_err)?;
        Ok(row.map(|row| mapper::assignment_from_model(&row)))
    }

    async fn set_enforce<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: AssignmentId,
        enforce: bool,
        at: OffsetDateTime,
    ) -> Result<Assignment, RepoError> {
        let result = assignment::Entity::update_many()
            .col_expr(assignment::Column::Enforce, Expr::value(enforce))
            .col_expr(assignment::Column::UpdatedAt, Expr::value(utc(at)))
            .filter(assignment::Column::Id.eq(id.0))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(scope_err)?;
        if result.rows_affected == 0 {
            return Err(RepoError::NotFound);
        }
        self.get(runner, scope, id)
            .await?
            .ok_or(RepoError::NotFound)
    }

    async fn delete<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: AssignmentId,
    ) -> Result<(), RepoError> {
        let result = assignment::Entity::delete_many()
            .filter(assignment::Column::Id.eq(id.0))
            .secure()
            .scope_with(scope)
            .exec(runner)
            .await
            .map_err(scope_err)?;
        if result.rows_affected == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Active content (service scope)
// ---------------------------------------------------------------------------

/// [`ActiveContentLoader`] under the service scope, bounded to active content
/// (see the module documentation).
#[derive(Debug, Clone, Copy, Default)]
pub struct OrmActiveContentLoader;

#[async_trait]
impl ActiveContentLoader for OrmActiveContentLoader {
    async fn load_for_tenants<C: DBRunner>(
        &self,
        runner: &C,
        tenants: &[Uuid],
    ) -> Result<Vec<ActiveBinding>, RepoError> {
        let scope = service_scope();
        let mut assignments = Vec::new();
        for chunk in tenants.chunks(IN_CHUNK) {
            assignments.extend(
                assignment::Entity::find()
                    .filter(assignment::Column::TenantId.is_in(chunk.iter().copied()))
                    .secure()
                    .scope_with(&scope)
                    .all(runner)
                    .await
                    .map_err(scope_err)?,
            );
        }
        if assignments.is_empty() {
            return Ok(Vec::new());
        }
        let mut bundle_ids: Vec<Uuid> = assignments.iter().map(|a| a.bundle_id).collect();
        bundle_ids.sort_unstable();
        bundle_ids.dedup();
        let mut versions = Vec::new();
        for chunk in bundle_ids.chunks(IN_CHUNK) {
            versions.extend(
                bundle_version::Entity::find()
                    .filter(bundle_version::Column::State.eq(ACTIVE))
                    .filter(bundle_version::Column::BundleId.is_in(chunk.iter().copied()))
                    .secure()
                    .scope_with(&scope)
                    .all(runner)
                    .await
                    .map_err(scope_err)?,
            );
        }
        let by_bundle: HashMap<BundleId, Arc<BundleVersion>> =
            with_content(runner, &scope, versions)
                .await?
                .into_iter()
                .map(|v| (v.bundle_id, Arc::new(v)))
                .collect();
        Ok(assignments
            .into_iter()
            .filter_map(|row| {
                let version = by_bundle.get(&BundleId(row.bundle_id))?.clone();
                Some(ActiveBinding {
                    assignment: mapper::assignment_from_model(&row),
                    version,
                })
            })
            .collect())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "content_repo_tests.rs"]
mod content_repo_tests;
