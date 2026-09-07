// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-operations:p1
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-concurrency:p1
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-eviction:p1
// @cpt-dod:cpt-cf-settings-service-dod-tenant-access-standalone:p1
//! Set, clear, read and list restrictions.

use std::sync::Arc;

use serde_json::json;
use settings_service_sdk::SettingKey;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::{
    AccessRepository, EffectiveAccess, Restriction, RestrictionDraft, TenantAccess,
    evict_access_change, may_restrict, restriction_tag, strictest,
};
use crate::api::precondition::{self, ETag};
use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;
use crate::domain::resolution::{EffectiveCache, TenantHierarchy};

/// Who is acting.
#[derive(Debug, Clone)]
pub struct AccessActor {
    /// The authenticated caller.
    pub ctx: SecurityContext,
    /// The request the change belongs to.
    pub request_id: String,
}

/// What a read of one pair returns.
#[derive(Debug, Clone)]
pub struct AccessReadout {
    /// The declaration.
    pub declaration: Declaration,
    /// The tenant asked about.
    pub tenant_id: Uuid,
    /// The pair's own row, if any.
    pub stored: Option<Restriction>,
    /// The effective access over the tenant's chain.
    pub effective: EffectiveAccess,
    /// The tag a mutation of the pair must present.
    pub etag: ETag,
}

/// The service.
pub struct AccessService<D, A, S> {
    declarations: D,
    access: A,
    sink: S,
    hierarchy: Arc<dyn TenantHierarchy>,
    platform: Arc<dyn PlatformScope>,
    cache: Arc<EffectiveCache>,
}

fn denied() -> DomainError {
    DomainError::Unauthorized {
        resource: settings_service_sdk::gts::VALUE_SCHEMA,
    }
}

fn absent() -> DomainError {
    DomainError::NotFound {
        resource: "declaration",
    }
}

fn snapshot(row: &Restriction) -> serde_json::Value {
    json!({
        "tenant_id": row.tenant_id,
        "access": row.access.as_str(),
        "set_by": row.set_by,
    })
}

impl<D, A, S> AccessService<D, A, S>
where
    D: DeclarationRepository,
    A: AccessRepository,
    S: AuditSink,
{
    /// Build the service over its repositories and ports.
    pub fn new(
        declarations: D,
        access: A,
        sink: S,
        hierarchy: Arc<dyn TenantHierarchy>,
        platform: Arc<dyn PlatformScope>,
        cache: Arc<EffectiveCache>,
    ) -> Self {
        Self {
            declarations,
            access,
            sink,
            hierarchy,
            platform,
            cache,
        }
    }

    /// The declaration at `key`, or absent when there is none or the caller's
    /// own effective access hides it: a caller cannot restrict, or learn of,
    /// what it cannot see.
    async fn visible_declaration<C: DBRunner>(
        &self,
        conn: &C,
        caller: Uuid,
        key: &SettingKey,
    ) -> Result<Declaration, DomainError> {
        let declaration = self
            .declarations
            .find_by_key(conn, &AccessScope::allow_all(), key.as_str())
            .await?
            .ok_or_else(absent)?;
        if self
            .effective_for(conn, declaration.id, caller)
            .await?
            .is_hidden()
        {
            return Err(absent());
        }
        Ok(declaration)
    }

    /// Effective access of one tenant for one declaration.
    async fn effective_for<C: DBRunner>(
        &self,
        conn: &C,
        declaration_id: Uuid,
        tenant: Uuid,
    ) -> Result<EffectiveAccess, DomainError> {
        let root = self.platform.root_tenant().await?;
        let chain = if tenant == root {
            vec![root]
        } else {
            self.hierarchy.chain(tenant).await?
        };
        let rows = self
            .access
            .find_in_tenants(conn, &AccessScope::allow_all(), declaration_id, &chain)
            .await?;
        Ok(strictest(&rows))
    }

    /// The target of a read: the caller's own tenant or a descendant that is
    /// not standalone.
    async fn readable_target(&self, caller: Uuid, target: Uuid) -> Result<(), DomainError> {
        if target != caller
            && (!self.hierarchy.is_within_subtree(caller, target).await?
                || self.hierarchy.is_standalone(target).await?)
        {
            return Err(denied());
        }
        Ok(())
    }

    async fn readout<C: DBRunner>(
        &self,
        conn: &C,
        declaration: Declaration,
        tenant_id: Uuid,
    ) -> Result<AccessReadout, DomainError> {
        let stored = self
            .access
            .find_one(conn, &AccessScope::allow_all(), declaration.id, tenant_id)
            .await?;
        let effective = self.effective_for(conn, declaration.id, tenant_id).await?;
        let etag = restriction_tag(stored.as_ref());
        Ok(AccessReadout {
            declaration,
            tenant_id,
            stored,
            effective,
            etag,
        })
    }

    /// Read one tenant's access for a setting.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] for a target outside the caller's
    /// subtree; [`DomainError::NotFound`] for a declaration absent or hidden.
    pub async fn read<C: DBRunner>(
        &self,
        conn: &C,
        actor: &AccessActor,
        key: &SettingKey,
        target: Uuid,
    ) -> Result<AccessReadout, DomainError> {
        let caller = actor.ctx.subject_tenant_id();
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-3
        self.readable_target(caller, target).await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-3
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-4
        let declaration = self.visible_declaration(conn, caller, key).await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-4
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-5
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-6
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-7
        self.readout(conn, declaration, target).await
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-7
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-6
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-read:p1:inst-ta-read-5
    }

    /// Store `read_only` or `hidden` for a strict descendant, inside the
    /// caller's transaction.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] unless the target is a reachable strict
    /// descendant; [`DomainError::Validation`] for `overridable`;
    /// [`DomainError::NotFound`] for a declaration absent or hidden;
    /// [`DomainError::PreconditionRequired`] and
    /// [`DomainError::PreconditionFailed`] on the tag.
    pub async fn set<C: DBRunner>(
        &self,
        conn: &C,
        actor: &AccessActor,
        key: &SettingKey,
        target: Uuid,
        access: TenantAccess,
        if_match: Option<&str>,
    ) -> Result<AccessReadout, DomainError> {
        let caller = actor.ctx.subject_tenant_id();
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-4
        if !may_restrict(self.hierarchy.as_ref(), caller, target).await? {
            return Err(denied());
        }
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-4
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-5
        if access == TenantAccess::Overridable {
            return Err(DomainError::Validation {
                field: "access".to_owned(),
                code: crate::field::VALIDATION,
                message: "`overridable` is the absence of a row; express it with DELETE".to_owned(),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-5
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-6
        let declaration = self.visible_declaration(conn, caller, key).await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-6
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-7
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-8
        let current = self
            .access
            .find_one(conn, &AccessScope::allow_all(), declaration.id, target)
            .await?;
        precondition::evaluate(if_match, &restriction_tag(current.as_ref()))?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-8
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-7
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-9
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-10
        // Stored even when a stricter ancestor already dominates it: it takes
        // effect when that restriction is lifted.
        let stored = self
            .access
            .upsert(
                conn,
                &AccessScope::allow_all(),
                RestrictionDraft {
                    declaration_id: declaration.id,
                    tenant_id: target,
                    access,
                    set_by: actor.ctx.subject_id().to_string(),
                },
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-10
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-9
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-11
        let operation = if current.is_some() {
            AuditOperation::Change
        } else {
            AuditOperation::Create
        };
        let mut record = AuditRecord::new(
            key.as_str(),
            target,
            actor.ctx.subject_id().to_string(),
            operation,
            actor.request_id.clone(),
        )
        .with_post_image(AuditValue::record(snapshot(&stored), false));
        if let Some(previous) = &current {
            record = record.with_pre_image(AuditValue::record(snapshot(previous), false));
        }
        self.sink
            .append(conn, &AccessScope::allow_all(), record)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-11
        self.readout(conn, declaration, target).await
    }

    /// Delete a strict descendant's row, inside the caller's transaction.
    /// Clearing an already absent row is a no-op that still requires the
    /// absent-state tag.
    ///
    /// # Errors
    /// As [`Self::set`], without the access validation.
    pub async fn clear<C: DBRunner>(
        &self,
        conn: &C,
        actor: &AccessActor,
        key: &SettingKey,
        target: Uuid,
        if_match: Option<&str>,
    ) -> Result<AccessReadout, DomainError> {
        let caller = actor.ctx.subject_tenant_id();
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-3
        if !may_restrict(self.hierarchy.as_ref(), caller, target).await? {
            return Err(denied());
        }
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-3
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-4
        let declaration = self.visible_declaration(conn, caller, key).await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-4
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-5
        let current = self
            .access
            .find_one(conn, &AccessScope::allow_all(), declaration.id, target)
            .await?;
        precondition::evaluate(if_match, &restriction_tag(current.as_ref()))?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-5
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-6
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-7
        if let Some(previous) = &current {
            self.access
                .delete(conn, &AccessScope::allow_all(), declaration.id, target)
                .await?;
            let record = AuditRecord::new(
                key.as_str(),
                target,
                actor.ctx.subject_id().to_string(),
                AuditOperation::Remove,
                actor.request_id.clone(),
            )
            .with_pre_image(AuditValue::record(snapshot(previous), false));
            self.sink
                .append(conn, &AccessScope::allow_all(), record)
                .await?;
        }
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-7
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-6
        self.readout(conn, declaration, target).await
    }

    /// Evict the target and its descendants after an access change; called
    /// after the transaction committed.
    ///
    /// # Errors
    /// [`DomainError`] when the tenant resolver cannot list the descendants.
    pub async fn evict(&self, key: &SettingKey, target: Uuid) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-12
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-8
        evict_access_change(&self.cache, self.hierarchy.as_ref(), key.as_str(), target).await
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-clear:p1:inst-ta-clear-8
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-set:p1:inst-ta-set-12
    }

    /// Every stored restriction for the setting inside the caller's subtree:
    /// the caller's own row, if an ancestor recorded one, and its reachable
    /// descendants' rows, standalone subtrees left out.
    ///
    /// # Errors
    /// [`DomainError::NotFound`] for a declaration absent or hidden; a read
    /// failure.
    pub async fn list<C: DBRunner>(
        &self,
        conn: &C,
        actor: &AccessActor,
        key: &SettingKey,
    ) -> Result<Vec<Restriction>, DomainError> {
        let caller = actor.ctx.subject_tenant_id();
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-3
        let declaration = self.visible_declaration(conn, caller, key).await?;
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-3
        // @cpt-begin:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-4
        let mut tenants = self.hierarchy.descendants(caller).await?;
        tenants.push(caller);
        let mut rows = self
            .access
            .find_in_tenants(conn, &AccessScope::allow_all(), declaration.id, &tenants)
            .await?;
        rows.sort_by_key(|row| row.tenant_id);
        // @cpt-end:cpt-cf-settings-service-flow-tenant-access-list:p1:inst-ta-list-4
        Ok(rows)
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod service_tests;
