//! In-process clients: the management contract and the license-resolver
//! backend plugin (DESIGN §3.2, Management API and Resolver Adapter).

use std::sync::Arc;

use async_trait::async_trait;
use license_enforcement_sdk::dto::{
    BindRequest, CheckExplainRequest, CheckExplanation, DelegateRequest, Delegation,
    DelegationVerbRequest, ImportEvidenceRequest, IssueGrantRequest, IssuePackRequest, IssueResult,
    ListQuery, RenewRequest, ReplaceRequest, VerbRequest,
};
use license_enforcement_sdk::models::{Grant, Pack};
use license_enforcement_sdk::{DelegationId, GrantId, LicenseEnforcementApi, PackId};
use license_resolver_sdk::{
    LicenseCheckRequest, LicenseDecision, LicenseResolverError, LicenseResolverPluginClient,
};
use time::OffsetDateTime;
use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use toolkit_odata::{CursorV1, ODataOrderBy, ODataQuery};
use toolkit_security::SecurityContext;

use super::check::CheckService;
use super::error::DomainError;
use super::grants::{BindingChange, GrantService, Target, Verb};
use super::repo::GrantRepo;

/// The `ListQuery` of the contract as the `ODataQuery` that the REST extractor
/// would build from the same parameters.
///
/// # Errors
///
/// `InvalidArgument` for a malformed filter, order, cursor, or limit.
pub fn odata_query(q: &ListQuery) -> Result<ODataQuery, CanonicalError> {
    let mut query = ODataQuery::new();
    if let Some(raw) = q.filter.as_deref().map(str::trim).filter(|f| !f.is_empty()) {
        // The limits of the REST extractor, so that the SDK path is no cheaper to abuse.
        if raw.len() > toolkit::api::odata::MAX_FILTER_LEN {
            return Err(
                DomainError::invalid("$filter", "Filter too long", "FILTER_TOO_LONG").into(),
            );
        }
        let parsed = toolkit_odata::parse_filter_string(raw).map_err(CanonicalError::from)?;
        if parsed.node_count() > toolkit::api::odata::MAX_NODES {
            return Err(DomainError::invalid(
                "$filter",
                "Filter too complex",
                "FILTER_TOO_COMPLEX",
            )
            .into());
        }
        let hash = toolkit_odata::pagination::short_filter_hash(Some(parsed.as_expr()));
        query = query.with_filter(parsed.into_expr());
        if let Some(hash) = hash {
            query = query.with_filter_hash(hash);
        }
    }
    if q.cursor.is_some() && q.orderby.is_some() {
        return Err(toolkit_odata::Error::OrderWithCursor.into());
    }
    if let Some(cursor) = &q.cursor {
        let cursor = CursorV1::decode(cursor).map_err(|_| toolkit_odata::Error::InvalidCursor)?;
        query = query.with_cursor(cursor).with_order(ODataOrderBy::empty());
    } else if let Some(order) = &q.orderby {
        query = query
            .with_order(toolkit::api::odata::parse_orderby(order).map_err(CanonicalError::from)?);
    }
    if let Some(limit) = q.limit {
        if limit == 0 {
            return Err(toolkit_odata::Error::InvalidLimit.into());
        }
        query = query.with_limit(limit);
    }
    Ok(query)
}

fn later(op: &'static str) -> CanonicalError {
    DomainError::NotImplemented(op).into()
}

/// The management contract in process.
#[domain_model]
pub struct LocalClient<R: GrantRepo> {
    grants: Arc<GrantService<R>>,
    check: Arc<CheckService<R>>,
}

impl<R: GrantRepo> LocalClient<R> {
    #[must_use]
    pub fn new(grants: Arc<GrantService<R>>, check: Arc<CheckService<R>>) -> Self {
        Self { grants, check }
    }
}

#[async_trait]
impl<R: GrantRepo> LicenseEnforcementApi for LocalClient<R> {
    async fn issue_pack(
        &self,
        ctx: SecurityContext,
        req: IssuePackRequest,
    ) -> Result<IssueResult, CanonicalError> {
        Ok(self
            .grants
            .issue(
                &ctx,
                req.holder_tenant_id,
                req.grants,
                true,
                req.idempotency_token,
            )
            .await?)
    }

    async fn issue_grant(
        &self,
        ctx: SecurityContext,
        req: IssueGrantRequest,
    ) -> Result<IssueResult, CanonicalError> {
        Ok(self
            .grants
            .issue(
                &ctx,
                req.holder_tenant_id,
                vec![req.grant],
                false,
                req.idempotency_token,
            )
            .await?)
    }

    async fn import_evidence(
        &self,
        _ctx: SecurityContext,
        _req: ImportEvidenceRequest,
    ) -> Result<IssueResult, CanonicalError> {
        if self.grants.plugin().intakes().import {
            Err(later("import_evidence"))
        } else {
            Err(DomainError::IntakeNotServed.into())
        }
    }

    async fn get_pack(&self, ctx: SecurityContext, id: PackId) -> Result<Pack, CanonicalError> {
        Ok(self.grants.get_pack(&ctx, id).await?)
    }

    async fn list_packs(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Pack>, CanonicalError> {
        let subtree = query.include_subtree;
        let query = odata_query(&query)?;
        Ok(self.grants.list_packs(&ctx, &query, subtree).await?)
    }

    async fn get_grant(&self, ctx: SecurityContext, id: GrantId) -> Result<Grant, CanonicalError> {
        Ok(self.grants.get_grant(&ctx, id).await?)
    }

    async fn list_grants(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Grant>, CanonicalError> {
        let subtree = query.include_subtree;
        let query = odata_query(&query)?;
        Ok(self.grants.list_grants(&ctx, &query, subtree).await?)
    }

    async fn get_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
    ) -> Result<Delegation, CanonicalError> {
        Ok(self.grants.get_delegation(&ctx, id).await?)
    }

    async fn accept_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Accept,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn reject_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Reject,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn suspend_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Suspend,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn reinstate_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Reinstate,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn revoke_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Revoke,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn renew_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: RenewRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .renew_grant(
                &ctx,
                id,
                req.window,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn bind_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: BindRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .change_binding(
                &ctx,
                id,
                BindingChange::Bind(req.subject.into()),
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn unbind_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .change_binding(
                &ctx,
                id,
                BindingChange::Unbind,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn return_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Return,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn recall_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Ok(self
            .grants
            .grant_verb(
                &ctx,
                id,
                Verb::Recall,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn replace_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError> {
        Ok(self.grants.replace(&ctx, Target::Grant(id), req).await?)
    }

    async fn accept_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Ok(self
            .grants
            .pack_verb(
                &ctx,
                id,
                Verb::Accept,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn reject_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Ok(self
            .grants
            .pack_verb(
                &ctx,
                id,
                Verb::Reject,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn suspend_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Ok(self
            .grants
            .pack_verb(
                &ctx,
                id,
                Verb::Suspend,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn reinstate_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Ok(self
            .grants
            .pack_verb(
                &ctx,
                id,
                Verb::Reinstate,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn revoke_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Ok(self
            .grants
            .pack_verb(
                &ctx,
                id,
                Verb::Revoke,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn renew_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: RenewRequest,
    ) -> Result<Pack, CanonicalError> {
        Ok(self
            .grants
            .renew_pack(
                &ctx,
                id,
                req.window,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn replace_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError> {
        Ok(self.grants.replace(&ctx, Target::Pack(id), req).await?)
    }

    async fn delegate(
        &self,
        ctx: SecurityContext,
        req: DelegateRequest,
    ) -> Result<Delegation, CanonicalError> {
        Ok(self.grants.delegate(&ctx, req).await?)
    }

    async fn accept_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Ok(self
            .grants
            .delegation_verb(
                &ctx,
                id,
                Verb::Accept,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn reject_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Ok(self
            .grants
            .delegation_verb(
                &ctx,
                id,
                Verb::Reject,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn return_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Ok(self
            .grants
            .delegation_verb(
                &ctx,
                id,
                Verb::Return,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn recall_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Ok(self
            .grants
            .delegation_verb(
                &ctx,
                id,
                Verb::Recall,
                req.idempotency_token,
                req.expected_version,
            )
            .await?)
    }

    async fn check_explain(
        &self,
        ctx: SecurityContext,
        req: CheckExplainRequest,
    ) -> Result<CheckExplanation, CanonicalError> {
        let request = self.grants.explain_request(&ctx, req).await?;
        let d = self
            .check
            .check(&request, OffsetDateTime::now_utc())
            .await?;
        match (d.grant_id, d.reason.clone()) {
            (Some(id), None) if d.granted => Ok(CheckExplanation::granted(id, d.grace)),
            (None, Some(reason)) if !d.granted => {
                Ok(CheckExplanation::denied(reason, d.termination_reason))
            }
            _ => Err(DomainError::Internal(format!("an inconsistent decision: {d:?}")).into()),
        }
    }
}

/// The backend plugin of license-resolver.
#[domain_model]
pub struct ResolverAdapter<R: GrantRepo> {
    check: Arc<CheckService<R>>,
}

impl<R: GrantRepo> ResolverAdapter<R> {
    #[must_use]
    pub fn new(check: Arc<CheckService<R>>) -> Self {
        Self { check }
    }
}

#[async_trait]
impl<R: GrantRepo> LicenseResolverPluginClient for ResolverAdapter<R> {
    async fn is_licensed(
        &self,
        request: LicenseCheckRequest,
    ) -> Result<LicenseDecision, LicenseResolverError> {
        match self.check.check(&request, OffsetDateTime::now_utc()).await {
            Ok(decision) => Ok(decision.into_license_decision()),
            Err(e @ DomainError::DependencyUnavailable(_)) => {
                tracing::warn!(error = %e, "check failed closed");
                Err(LicenseResolverError::ServiceUnavailable(e.to_string()))
            }
            Err(e) => {
                tracing::error!(error = %e, "check failed closed");
                Err(LicenseResolverError::Internal(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
