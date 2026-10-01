//! [`ManagementService`]: bundles and versions.
//!
//! Every method backs one method of
//! [`PolicyManagementClientV1`](policy_engine_sdk::PolicyManagementClientV1)
//! with the same parameters and result, so the local client is a delegation.
//!
//! # Flow of a mutation
//!
//! 1. The target bundle is prefetched under the gear's own scope for its
//!    owning tenant only (the platform's prefetch pattern), and the one
//!    capability the operation needs is authorised for that owner.
//! 2. Content checks that need no transaction run (limits, validation for
//!    activation).
//! 3. One transaction: the entity change under the authorised scope.
//!
//! Retries are safe: a draft is unique per bundle (a retried creation is a
//! conflict), activating the active version is a no-op, and bundles are never
//! deleted.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use policy_engine_sdk::gts::permissions::Capability;
use policy_engine_sdk::management::{
    self as sdk, BundlePatch, ManagementError, NewBundle, ValidationReport, VersionContent,
    VersionDetail,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, DbConn, DbTx};
use toolkit_db::{DBProvider, Db};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::ContentStore;
use super::authz::{AccessTarget, Authorization, AuthzError, ManagementAuthorizer};
use super::error::{self, ManagementFailure, Subject};
use super::lifecycle::{
    bundle_to_sdk, check_write, detail_to_sdk, documents_from_content, seed_documents,
    version_to_sdk,
};
use crate::domain::model::{Bundle, BundleId, BundleVersion, VersionId, VersionState};
use crate::domain::repos::{BundleRepository, RepoError, VersionRepository};
use crate::domain::validation::{ContentValidator, Finding};

/// The future a mutation's transactional body returns.
pub(super) type TxFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ManagementFailure>> + Send + 'a>>;

/// Everything [`ManagementService::new`] is built from.
#[domain_model]
pub struct ManagementServiceParts<S> {
    /// The gear's database.
    pub db: Db,
    /// The repositories.
    pub store: Arc<S>,
    /// The platform policy enforcer.
    pub enforcer: PolicyEnforcer,
    /// The content validator.
    pub validator: Arc<ContentValidator>,
}

/// Who acts, when, under which scope: what a transactional body needs.
#[domain_model]
pub(super) struct TxEnv<S> {
    pub(super) store: Arc<S>,
    pub(super) scope: AccessScope,
    pub(super) actor_id: Uuid,
    pub(super) now: OffsetDateTime,
}

/// The management service: bundles and versions.
#[domain_model]
pub struct ManagementService<S: ContentStore> {
    pub(super) db: DBProvider<ManagementFailure>,
    pub(super) store: Arc<S>,
    pub(super) authz: ManagementAuthorizer,
    pub(super) validator: Arc<ContentValidator>,
}

impl<S: ContentStore> std::fmt::Debug for ManagementService<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagementService").finish_non_exhaustive()
    }
}

/// The current time at the precision every supported store keeps
/// (microseconds), so a value read back equals the value written.
pub(super) fn now() -> OffsetDateTime {
    let at = OffsetDateTime::now_utc();
    at.replace_microsecond(at.microsecond()).unwrap_or(at)
}

impl<S: ContentStore> ManagementService<S> {
    /// Builds the service and its authorizer from `parts`.
    #[must_use]
    pub fn new(parts: ManagementServiceParts<S>) -> Self {
        let db = DBProvider::<ManagementFailure>::new(parts.db);
        let authz = ManagementAuthorizer::new(parts.enforcer);
        Self {
            db,
            store: parts.store,
            authz,
            validator: parts.validator,
        }
    }

    // -- Bundles -------------------------------------------------------------

    /// Creates a bundle. Capability: author on the owning tenant.
    ///
    /// # Errors
    ///
    /// `BUNDLE_NAME_TAKEN`; not found for an owning tenant the caller may
    /// not manage; `CAPABILITY_DENIED`.
    pub async fn create_bundle(
        &self,
        ctx: &SecurityContext,
        bundle: NewBundle,
    ) -> Result<sdk::Bundle, ManagementError> {
        let owner = bundle
            .owner_tenant_id
            .unwrap_or_else(|| ctx.subject_tenant_id());
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner),
                owner,
                Subject::Tenant(owner),
            )
            .await?;
        let at = now();
        let new = Bundle {
            id: BundleId(Uuid::new_v4()),
            owner_tenant_id: owner,
            name: bundle.name,
            description: bundle.description,
            created_at: at,
            created_by: ctx.subject_id(),
            updated_at: at,
        };
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let inserted = env
                    .store
                    .bundles()
                    .insert(tx, &env.scope, &new)
                    .await
                    .map_err(|e| match e {
                        RepoError::Conflict {
                            reason: crate::domain::repos::conflict::BUNDLE_NAME_TAKEN,
                        } => error::bundle_name_taken(&new.name),
                        other => error::repo(other, Subject::Tenant(owner)),
                    })?;
                Ok(bundle_to_sdk(&inserted, None))
            })
        })
        .await
    }

    /// Reads a bundle. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found when absent or not visible.
    pub async fn get_bundle(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
    ) -> Result<sdk::Bundle, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let scope = self
            .read_access(ctx, &bundle, bundle_id, Subject::Bundle(bundle_id))
            .await?;
        let conn = self.conn()?;
        let bundle = self.visible_bundle(&conn, &scope, bundle_id).await?;
        let active = self.active_version_of(&conn, &scope, bundle.id).await?;
        Ok(bundle_to_sdk(&bundle, active))
    }

    /// Lists the bundles within the caller's read scope. Capability: read.
    ///
    /// # Errors
    ///
    /// `CAPABILITY_DENIED`; invalid argument for a malformed query.
    pub async fn list_bundles(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<sdk::Bundle>, ManagementError> {
        let scope = match self.authz.read(ctx, AccessTarget::collection()).await {
            Ok(auth) => auth.scope,
            Err(AuthzError::Denied) => return Err(error::capability_denied()),
            Err(AuthzError::Unavailable(detail)) => return Err(error::unavailable(detail)),
        };
        let conn = self.conn()?;
        let page = self
            .store
            .bundles()
            .list_page(&conn, &scope, query)
            .await
            .map_err(|e| error::repo(e, Subject::Tenant(ctx.subject_tenant_id())))?;
        let mut items = Vec::with_capacity(page.items.len());
        for bundle in &page.items {
            let active = self.active_version_of(&conn, &scope, bundle.id).await?;
            items.push(bundle_to_sdk(bundle, active));
        }
        Ok(Page::new(items, page.page_info))
    }

    /// Changes a bundle's name or description. Capability: author.
    ///
    /// # Errors
    ///
    /// Not found; `BUNDLE_NAME_TAKEN`; `CAPABILITY_DENIED`.
    pub async fn update_bundle(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        patch: BundlePatch,
    ) -> Result<sdk::Bundle, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(bundle_id),
                owner,
                Subject::Bundle(bundle_id),
            )
            .await?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let id = BundleId(bundle_id);
                let updated = env
                    .store
                    .bundles()
                    .update(
                        tx,
                        &env.scope,
                        id,
                        patch.name.as_deref(),
                        patch.description.as_deref(),
                        env.now,
                    )
                    .await
                    .map_err(|e| error::repo(e, Subject::Bundle(bundle_id)))?;
                let active = active_version_in(tx, env.store.versions(), &env.scope, id).await?;
                Ok(bundle_to_sdk(&updated, active))
            })
        })
        .await
    }

    // -- Versions ------------------------------------------------------------

    /// Creates the bundle's draft with the next ordinal, empty or seeded from
    /// a retained version of the same bundle. Capability: author.
    ///
    /// # Errors
    ///
    /// Not found for the bundle; `DRAFT_EXISTS`; `SEED_NOT_IN_BUNDLE`;
    /// `CAPABILITY_DENIED`.
    pub async fn create_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        seed_from: Option<Uuid>,
    ) -> Result<sdk::BundleVersion, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(bundle_id),
                owner,
                Subject::Bundle(bundle_id),
            )
            .await?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let id = BundleId(bundle_id);
                let versions = env.store.versions();
                let documents = match seed_from {
                    None => Vec::new(),
                    Some(seed) => {
                        let seed = versions
                            .get_with_content(tx, &env.scope, VersionId(seed))
                            .await
                            .map_err(|e| error::repo(e, Subject::Version(seed)))?
                            .filter(|v| v.bundle_id == id)
                            .ok_or_else(error::seed_not_in_bundle)?;
                        seed_documents(&seed)
                    }
                };
                let ordinal = versions
                    .next_ordinal(tx, &env.scope, id)
                    .await
                    .map_err(|e| error::repo(e, Subject::Bundle(bundle_id)))?;
                let draft = BundleVersion {
                    id: VersionId(Uuid::new_v4()),
                    bundle_id: id,
                    owner_tenant_id: owner,
                    ordinal,
                    state: VersionState::Draft,
                    created_at: env.now,
                    activated_at: None,
                    activated_by: None,
                    documents,
                };
                let inserted = versions
                    .insert_draft(tx, &env.scope, &draft)
                    .await
                    .map_err(|e| error::repo(e, Subject::Bundle(bundle_id)))?;
                Ok(version_to_sdk(&inserted))
            })
        })
        .await
    }

    /// Reads a version with its documents. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found when the version is absent, of another bundle, or not
    /// visible.
    pub async fn get_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        version_id: Uuid,
    ) -> Result<VersionDetail, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let scope = self
            .read_access(ctx, &bundle, version_id, Subject::Version(version_id))
            .await?;
        let conn = self.conn()?;
        let version = self
            .load_version(&conn, &scope, bundle_id, version_id)
            .await?;
        Ok(detail_to_sdk(&version))
    }

    /// Lists every retained version of a bundle, newest first, without
    /// content. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found for the bundle.
    pub async fn list_versions(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
    ) -> Result<Vec<sdk::BundleVersion>, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let scope = self
            .read_access(ctx, &bundle, bundle_id, Subject::Bundle(bundle_id))
            .await?;
        let conn = self.conn()?;
        self.visible_bundle(&conn, &scope, bundle_id).await?;
        let versions = self
            .store
            .versions()
            .list_for_bundle(&conn, &scope, BundleId(bundle_id))
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(bundle_id)))?;
        Ok(versions.iter().map(version_to_sdk).collect())
    }

    /// Replaces the whole content of a draft. Limits are checked on write;
    /// the rest is left to validation. Capability: author.
    ///
    /// # Errors
    ///
    /// `VERSION_NOT_DRAFT`; `CONTENT_LIMIT_EXCEEDED`;
    /// `DUPLICATE_DOCUMENT_NAME`; not found.
    pub async fn replace_draft_content(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        version_id: Uuid,
        content: VersionContent,
    ) -> Result<VersionDetail, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(version_id),
                owner,
                Subject::Version(version_id),
            )
            .await?;
        let documents = documents_from_content(&content);
        check_write(self.validator.limits(), &documents)?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let versions = env.store.versions();
                version_of(tx, versions, &env.scope, bundle_id, version_id).await?;
                let replaced = versions
                    .replace_draft_content(tx, &env.scope, VersionId(version_id), &documents)
                    .await
                    .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
                Ok(detail_to_sdk(&replaced))
            })
        })
        .await
    }

    /// Validates a version; never activates and changes nothing. Capability:
    /// author.
    ///
    /// # Errors
    ///
    /// Not found; `CAPABILITY_DENIED`; `service_unavailable` when the
    /// registry cannot answer. Invalid content is a report, not an error.
    pub async fn validate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        version_id: Uuid,
    ) -> Result<ValidationReport, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(version_id),
                owner,
                Subject::Version(version_id),
            )
            .await?;
        let conn = self.conn()?;
        let version = self
            .load_version(&conn, &auth.scope, bundle_id, version_id)
            .await?;
        let findings = self
            .validator
            .validate(&version.documents)
            .await
            .map_err(error::unavailable)?;
        Ok(ValidationReport {
            version_id,
            findings: findings.iter().map(Finding::to_sdk).collect(),
            validated_at: now(),
        })
    }

    /// Activates a draft: validates it and supersedes the previously active
    /// version in one transaction. Activating the active version succeeds
    /// unchanged. Capability: publish.
    ///
    /// # Errors
    ///
    /// `VERSION_NOT_DRAFT` for a superseded version; `VALIDATION_FAILED` with
    /// every finding; not found; `CAPABILITY_DENIED`.
    pub async fn activate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        version_id: Uuid,
    ) -> Result<sdk::BundleVersion, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Publish,
                AccessTarget::owned_by(owner).resource(version_id),
                owner,
                Subject::Version(version_id),
            )
            .await?;
        let conn = self.conn()?;
        let version = self
            .load_version(&conn, &auth.scope, bundle_id, version_id)
            .await?;
        match version.state {
            VersionState::Active => return Ok(version_to_sdk(&version)),
            VersionState::Superseded => return Err(error::version_not_draft()),
            VersionState::Draft => {}
        }
        let findings = self
            .validator
            .validate(&version.documents)
            .await
            .map_err(error::unavailable)?;
        if !findings.is_empty() {
            return Err(error::validation_failed(&findings));
        }
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let outcome = env
                    .store
                    .versions()
                    .activate(tx, &env.scope, VersionId(version_id), env.actor_id, env.now)
                    .await
                    .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
                Ok(version_to_sdk(&outcome.activated))
            })
        })
        .await
    }

    /// Deletes a draft with its documents. Capability: author.
    ///
    /// # Errors
    ///
    /// `VERSION_NOT_DRAFT`; not found; `CAPABILITY_DENIED`.
    pub async fn delete_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: Uuid,
        version_id: Uuid,
    ) -> Result<(), ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(version_id),
                owner,
                Subject::Version(version_id),
            )
            .await?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let versions = env.store.versions();
                version_of(tx, versions, &env.scope, bundle_id, version_id).await?;
                versions
                    .delete_draft(tx, &env.scope, VersionId(version_id))
                    .await
                    .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
                Ok(())
            })
        })
        .await
    }

    // -- Shared steps --------------------------------------------------------

    pub(super) fn conn(&self) -> Result<DbConn<'_>, ManagementError> {
        self.db.conn().map_err(ManagementError::from)
    }

    /// The bundle under the gear's own scope, only to learn its owning
    /// tenant for authorisation (the platform's prefetch pattern); nothing
    /// read here is returned before the caller is authorised.
    pub(super) async fn prefetch_bundle(&self, id: Uuid) -> Result<Bundle, ManagementError> {
        let conn = self.conn()?;
        self.store
            .bundles()
            .get(&conn, &AccessScope::allow_all(), BundleId(id))
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(id)))?
            .ok_or_else(|| error::not_found(Subject::Bundle(id)))
    }

    /// The bundle as the caller's `scope` sees it.
    async fn visible_bundle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Bundle, ManagementError> {
        self.store
            .bundles()
            .get(runner, scope, BundleId(id))
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(id)))?
            .ok_or_else(|| error::not_found(Subject::Bundle(id)))
    }

    async fn active_version_of<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: BundleId,
    ) -> Result<Option<VersionId>, ManagementError> {
        active_version_in(runner, self.store.versions(), scope, id)
            .await
            .map_err(ManagementError::from)
    }

    /// A version of `bundle_id` with its content.
    pub(super) async fn load_version<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        bundle_id: Uuid,
        version_id: Uuid,
    ) -> Result<BundleVersion, ManagementError> {
        self.store
            .versions()
            .get_with_content(runner, scope, VersionId(version_id))
            .await
            .map_err(|e| error::repo(e, Subject::Version(version_id)))?
            .filter(|v| v.bundle_id.0 == bundle_id)
            .ok_or_else(|| error::not_found(Subject::Version(version_id)))
    }

    /// Content read on a prefetched bundle; a denial is an absence.
    pub(super) async fn read_access(
        &self,
        ctx: &SecurityContext,
        bundle: &Bundle,
        resource_id: Uuid,
        absent: Subject,
    ) -> Result<AccessScope, ManagementError> {
        let target = AccessTarget::owned_by(bundle.owner_tenant_id)
            .resource(resource_id)
            .prefetched();
        match self.authz.read(ctx, target).await {
            Ok(auth) => Ok(auth.scope),
            Err(AuthzError::Denied) => Err(error::not_found(absent)),
            Err(AuthzError::Unavailable(detail)) => Err(error::unavailable(detail)),
        }
    }

    /// A mutating capability on content owned by `owner`. A denial is
    /// `CAPABILITY_DENIED` when the caller may see the content (its own
    /// tenant, or content it may read) and an absence otherwise, so a
    /// refusal never confirms content the caller cannot see.
    pub(super) async fn mutation_access(
        &self,
        ctx: &SecurityContext,
        capability: Capability,
        target: AccessTarget,
        owner: Uuid,
        absent: Subject,
    ) -> Result<Authorization, ManagementError> {
        match self.authz.authorize(ctx, capability, target).await {
            Ok(auth) => Ok(auth),
            Err(AuthzError::Unavailable(detail)) => Err(error::unavailable(detail)),
            Err(AuthzError::Denied) => {
                if owner == ctx.subject_tenant_id() {
                    return Err(error::capability_denied());
                }
                let probe = AccessTarget::owned_by(owner).prefetched();
                match self.authz.read(ctx, probe).await {
                    Ok(_) => Err(error::capability_denied()),
                    Err(AuthzError::Denied) => Err(error::not_found(absent)),
                    Err(AuthzError::Unavailable(detail)) => Err(error::unavailable(detail)),
                }
            }
        }
    }

    /// Runs `apply` in one transaction.
    pub(super) async fn commit<T, F>(
        &self,
        ctx: &SecurityContext,
        auth: &Authorization,
        apply: F,
    ) -> Result<T, ManagementError>
    where
        T: Send + 'static,
        F: for<'a> FnOnce(&'a DbTx<'a>, TxEnv<S>) -> TxFuture<'a, T> + Send + 'static,
    {
        let env = TxEnv {
            store: Arc::clone(&self.store),
            scope: auth.scope.clone(),
            actor_id: ctx.subject_id(),
            now: now(),
        };
        self.db
            .transaction(move |tx| Box::pin(async move { apply(tx, env).await }))
            .await
            .map_err(ManagementError::from)
    }
}

/// The bundle's active version, if any, within `scope`.
async fn active_version_in<C: DBRunner, V: VersionRepository>(
    runner: &C,
    versions: &V,
    scope: &AccessScope,
    id: BundleId,
) -> Result<Option<VersionId>, ManagementFailure> {
    let headers = versions
        .list_for_bundle(runner, scope, id)
        .await
        .map_err(|e| error::repo(e, Subject::Bundle(id.0)))?;
    Ok(headers
        .into_iter()
        .find(|v| v.state == VersionState::Active)
        .map(|v| v.id))
}

/// Refuses a version that is not one of `bundle_id`'s within `scope`.
async fn version_of<C: DBRunner, V: VersionRepository>(
    runner: &C,
    versions: &V,
    scope: &AccessScope,
    bundle_id: Uuid,
    version_id: Uuid,
) -> Result<(), ManagementFailure> {
    let headers = versions
        .list_for_bundle(runner, scope, BundleId(bundle_id))
        .await
        .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
    if headers.iter().any(|v| v.id.0 == version_id) {
        Ok(())
    } else {
        Err(ManagementFailure(error::not_found(Subject::Version(
            version_id,
        ))))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
