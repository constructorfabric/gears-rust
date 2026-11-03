//! [`ManagementService`]: the management service; bundles and versions here,
//! assignments in [`assignments`](super::assignments).
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
    self as sdk, BundlePatch, ManagementError, NewBundle, VersionContent, VersionDetail,
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
    bundle_to_sdk, check_bundle, check_write, detail_to_sdk, documents_from_content,
    seed_documents, version_to_sdk,
};
use crate::domain::model::{Bundle, BundleId, BundleVersion, DocumentId, VersionId, VersionState};
use crate::domain::ports::HierarchyPort;
use crate::domain::repos::{BundleRepository, RepoError, VersionRepository};
use crate::domain::validation::ContentValidator;

/// What [`ManagementService::plan_activation`] found.
#[domain_model]
#[derive(Debug)]
pub(super) enum Activation {
    /// The version is already active: activating it is a no-op.
    AlreadyActive(BundleVersion),
    /// A valid draft, to activate under `auth` if its content is still the
    /// `validated` documents.
    Draft {
        /// The publish authorisation.
        auth: Authorization,
        /// Its document ids as validated, ascending.
        validated: Vec<DocumentId>,
    },
}

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
    /// Reachability and ancestry, read under the caller's context (the
    /// uncached adapter: management reads must see the provider's answer).
    pub hierarchy: Arc<dyn HierarchyPort>,
}

/// Who acts, when, under which scope: what a transactional body needs.
#[domain_model]
pub(super) struct TxEnv<S> {
    pub(super) store: Arc<S>,
    pub(super) scope: AccessScope,
    pub(super) actor_id: Uuid,
    pub(super) now: OffsetDateTime,
}

/// The management service: bundles, versions and assignments.
#[domain_model]
pub struct ManagementService<S: ContentStore> {
    pub(super) db: DBProvider<ManagementFailure>,
    pub(super) store: Arc<S>,
    pub(super) authz: ManagementAuthorizer,
    pub(super) validator: Arc<ContentValidator>,
    pub(super) hierarchy: Arc<dyn HierarchyPort>,
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
            hierarchy: parts.hierarchy,
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
                Subject::Tenant(owner),
            )
            .await?;
        check_bundle(Some(&bundle.name), Some(&bundle.description))?;
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
                        } => ManagementError::BundleNameTaken {
                            name: new.name.clone(),
                        },
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
        bundle_id: BundleId,
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
        let scope = self
            .authz
            .read(ctx, AccessTarget::collection())
            .await
            .map_err(|e| error::authz(e, || ManagementError::CapabilityDenied))?
            .scope;
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
        bundle_id: BundleId,
        patch: BundlePatch,
    ) -> Result<sdk::Bundle, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(bundle_id),
                Subject::Bundle(bundle_id),
            )
            .await?;
        check_bundle(patch.name.as_deref(), patch.description.as_deref())?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let id = bundle_id;
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
                    .map_err(|e| match e {
                        RepoError::Conflict {
                            reason: crate::domain::repos::conflict::BUNDLE_NAME_TAKEN,
                        } => ManagementError::BundleNameTaken {
                            name: patch.name.clone().unwrap_or_default(),
                        },
                        other => error::repo(other, Subject::Bundle(bundle_id)),
                    })?;
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
        bundle_id: BundleId,
        seed_from: Option<VersionId>,
    ) -> Result<sdk::BundleVersion, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(bundle_id),
                Subject::Bundle(bundle_id),
            )
            .await?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let id = bundle_id;
                let versions = env.store.versions();
                let documents = match seed_from {
                    None => Vec::new(),
                    Some(seed) => {
                        let seed = versions
                            .get_with_content(tx, &env.scope, seed)
                            .await
                            .map_err(|e| error::repo(e, Subject::Version(seed)))?
                            .filter(|v| v.bundle_id == id)
                            .ok_or(ManagementError::SeedNotInBundle { seed })?;
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
        bundle_id: BundleId,
        version_id: VersionId,
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

    /// Lists the retained versions of a bundle, newest first, without
    /// content, one cursor page at a time. Capability: read.
    ///
    /// # Errors
    ///
    /// Not found for the bundle; invalid argument for a malformed query.
    pub async fn list_versions(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        query: &ODataQuery,
    ) -> Result<Page<sdk::BundleVersion>, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let scope = self
            .read_access(ctx, &bundle, bundle_id, Subject::Bundle(bundle_id))
            .await?;
        let conn = self.conn()?;
        self.visible_bundle(&conn, &scope, bundle_id).await?;
        let page = self
            .store
            .versions()
            .list_page(&conn, &scope, bundle_id, query)
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(bundle_id)))?;
        Ok(Page::new(
            page.items.iter().map(version_to_sdk).collect(),
            page.page_info,
        ))
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
        bundle_id: BundleId,
        version_id: VersionId,
        content: VersionContent,
    ) -> Result<VersionDetail, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(version_id),
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
                    .replace_draft_content(tx, &env.scope, version_id, &documents)
                    .await
                    .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
                Ok(detail_to_sdk(&replaced))
            })
        })
        .await
    }

    /// Validates a version's content, in any state, with the checks
    /// activation applies; never activates and changes nothing. Capability:
    /// author.
    ///
    /// # Errors
    ///
    /// `VALIDATION_FAILED` with every finding; not found;
    /// `CAPABILITY_DENIED`; `service_unavailable` when the registry cannot
    /// answer.
    pub async fn validate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<(), ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(version_id),
                Subject::Version(version_id),
            )
            .await?;
        let conn = self.conn()?;
        let version = self
            .load_version(&conn, &auth.scope, bundle_id, version_id)
            .await?;
        self.validate_content(&version).await
    }

    /// Activates a draft: validates it and supersedes the previously active
    /// version in one transaction. Activating the active version succeeds
    /// unchanged. Capability: publish.
    ///
    /// # Errors
    ///
    /// `VERSION_NOT_DRAFT` for a superseded version; `VALIDATION_FAILED` with
    /// every finding; not found; `CAPABILITY_DENIED`; `CONCURRENT_CHANGE`
    /// when the draft changed after it was validated.
    pub async fn activate_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<sdk::BundleVersion, ManagementError> {
        let (auth, validated) = match self.plan_activation(ctx, bundle_id, version_id).await? {
            Activation::AlreadyActive(version) => return Ok(version_to_sdk(&version)),
            Activation::Draft { auth, validated } => (auth, validated),
        };
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let versions = env.store.versions();
                let current = versions
                    .lock_draft(tx, &env.scope, version_id)
                    .await
                    .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
                if current != validated {
                    // A draft write committed after validation: the content
                    // about to become active is not what was validated.
                    return Err(ManagementError::ConcurrentChange.into());
                }
                let outcome = versions
                    .activate(tx, &env.scope, version_id, env.actor_id, env.now)
                    .await
                    .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
                Ok(version_to_sdk(&outcome.activated))
            })
        })
        .await
    }

    /// Every check [`Self::activate_version`] runs before its transaction,
    /// shared with its dry run; reads only.
    ///
    /// # Errors
    ///
    /// As [`Self::activate_version`], but for `CONCURRENT_CHANGE`.
    pub(super) async fn plan_activation(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<Activation, ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Publish,
                AccessTarget::owned_by(owner).resource(version_id),
                Subject::Version(version_id),
            )
            .await?;
        let conn = self.conn()?;
        let version = self
            .load_version(&conn, &auth.scope, bundle_id, version_id)
            .await?;
        match version.state {
            VersionState::Active => return Ok(Activation::AlreadyActive(version)),
            VersionState::Superseded => return Err(ManagementError::VersionNotDraft),
            VersionState::Draft => {}
        }
        self.validate_content(&version).await?;
        // Document ids are fresh on every replace: the same ids are the same
        // content.
        let mut validated: Vec<DocumentId> = version.documents.iter().map(|d| d.id).collect();
        validated.sort_unstable();
        Ok(Activation::Draft { auth, validated })
    }

    /// The content checks validation and activation share.
    ///
    /// # Errors
    ///
    /// `VALIDATION_FAILED` with every finding; `service_unavailable` when the
    /// registry cannot answer.
    async fn validate_content(&self, version: &BundleVersion) -> Result<(), ManagementError> {
        let findings = self
            .validator
            .validate(&version.documents)
            .await
            .map_err(error::unavailable)?;
        if findings.is_empty() {
            Ok(())
        } else {
            Err(error::validation_failed(&findings))
        }
    }

    /// Deletes a draft with its documents. Capability: author.
    ///
    /// # Errors
    ///
    /// `VERSION_NOT_DRAFT`; not found; `CAPABILITY_DENIED`.
    pub async fn delete_draft_version(
        &self,
        ctx: &SecurityContext,
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<(), ManagementError> {
        let bundle = self.prefetch_bundle(bundle_id).await?;
        let owner = bundle.owner_tenant_id;
        let auth = self
            .mutation_access(
                ctx,
                Capability::Author,
                AccessTarget::owned_by(owner).resource(version_id),
                Subject::Version(version_id),
            )
            .await?;
        self.commit(ctx, &auth, move |tx, env| {
            Box::pin(async move {
                let versions = env.store.versions();
                version_of(tx, versions, &env.scope, bundle_id, version_id).await?;
                versions
                    .delete_draft(tx, &env.scope, version_id)
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
    pub(super) async fn prefetch_bundle(&self, id: BundleId) -> Result<Bundle, ManagementError> {
        let conn = self.conn()?;
        self.store
            .bundles()
            .get(&conn, &AccessScope::allow_all(), id)
            .await
            .map_err(|e| error::repo(e, Subject::Bundle(id)))?
            .ok_or_else(|| error::not_found(Subject::Bundle(id)))
    }

    /// The bundle as the caller's `scope` sees it.
    pub(super) async fn visible_bundle<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        id: BundleId,
    ) -> Result<Bundle, ManagementError> {
        self.store
            .bundles()
            .get(runner, scope, id)
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
        bundle_id: BundleId,
        version_id: VersionId,
    ) -> Result<BundleVersion, ManagementError> {
        self.store
            .versions()
            .get_with_content(runner, scope, version_id)
            .await
            .map_err(|e| error::repo(e, Subject::Version(version_id)))?
            .filter(|v| v.bundle_id == bundle_id)
            .ok_or_else(|| error::not_found(Subject::Version(version_id)))
    }

    /// Content read on a prefetched bundle; a denial is an absence.
    pub(super) async fn read_access(
        &self,
        ctx: &SecurityContext,
        bundle: &Bundle,
        resource_id: impl Into<Uuid>,
        absent: Subject,
    ) -> Result<AccessScope, ManagementError> {
        let target = AccessTarget::owned_by(bundle.owner_tenant_id)
            .resource(resource_id)
            .prefetched();
        self.authz
            .read(ctx, target)
            .await
            .map(|auth| auth.scope)
            .map_err(|e| error::authz(e, || error::not_found(absent)))
    }

    /// A mutating capability on `target`'s content. A denial is
    /// `CAPABILITY_DENIED` when the caller may see the content (its own
    /// tenant, or content it may read) and an absence otherwise, so a
    /// refusal never confirms content the caller cannot see.
    pub(super) async fn mutation_access(
        &self,
        ctx: &SecurityContext,
        capability: Capability,
        target: AccessTarget,
        absent: Subject,
    ) -> Result<Authorization, ManagementError> {
        let err = match self.authz.authorize(ctx, capability, target).await {
            Ok(auth) => return Ok(auth),
            Err(err) => err,
        };
        let owner = match target.owner_tenant_id {
            Some(owner) if err == AuthzError::Denied && owner != ctx.subject_tenant_id() => owner,
            _ => return Err(error::authz(err, || ManagementError::CapabilityDenied)),
        };
        let probe = AccessTarget::owned_by(owner).prefetched();
        Err(match self.authz.read(ctx, probe).await {
            Ok(_) => ManagementError::CapabilityDenied,
            Err(err) => error::authz(err, || error::not_found(absent)),
        })
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
        .map_err(|e| error::repo(e, Subject::Bundle(id)))?;
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
    bundle_id: BundleId,
    version_id: VersionId,
) -> Result<(), ManagementFailure> {
    let headers = versions
        .list_for_bundle(runner, scope, bundle_id)
        .await
        .map_err(|e| error::repo(e, Subject::Version(version_id)))?;
    if headers.iter().any(|v| v.id == version_id) {
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
