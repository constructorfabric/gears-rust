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
use super::grants::{BindingChange, GrantService};
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
}

impl<R: GrantRepo> LocalClient<R> {
    #[must_use]
    pub fn new(grants: Arc<GrantService<R>>) -> Self {
        Self { grants }
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
        _ctx: SecurityContext,
        _query: ListQuery,
    ) -> Result<Page<Pack>, CanonicalError> {
        Err(later("list_packs"))
    }

    async fn get_grant(&self, ctx: SecurityContext, id: GrantId) -> Result<Grant, CanonicalError> {
        Ok(self.grants.get_grant(&ctx, id).await?)
    }

    async fn list_grants(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Grant>, CanonicalError> {
        let query = odata_query(&query)?;
        Ok(self.grants.list_grants(&ctx, &query).await?)
    }

    async fn get_delegation(
        &self,
        _ctx: SecurityContext,
        _id: DelegationId,
    ) -> Result<Delegation, CanonicalError> {
        Err(later("get_delegation"))
    }

    async fn accept_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("accept_grant"))
    }

    async fn reject_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("reject_grant"))
    }

    async fn suspend_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("suspend_grant"))
    }

    async fn reinstate_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("reinstate_grant"))
    }

    async fn revoke_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("revoke_grant"))
    }

    async fn renew_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: RenewRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("renew_grant"))
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
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("return_grant"))
    }

    async fn recall_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: VerbRequest,
    ) -> Result<Grant, CanonicalError> {
        Err(later("recall_grant"))
    }

    async fn replace_grant(
        &self,
        _ctx: SecurityContext,
        _id: GrantId,
        _req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError> {
        Err(later("replace_grant"))
    }

    async fn suspend_pack(
        &self,
        _ctx: SecurityContext,
        _id: PackId,
        _req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Err(later("suspend_pack"))
    }

    async fn reinstate_pack(
        &self,
        _ctx: SecurityContext,
        _id: PackId,
        _req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Err(later("reinstate_pack"))
    }

    async fn revoke_pack(
        &self,
        _ctx: SecurityContext,
        _id: PackId,
        _req: VerbRequest,
    ) -> Result<Pack, CanonicalError> {
        Err(later("revoke_pack"))
    }

    async fn renew_pack(
        &self,
        _ctx: SecurityContext,
        _id: PackId,
        _req: RenewRequest,
    ) -> Result<Pack, CanonicalError> {
        Err(later("renew_pack"))
    }

    async fn replace_pack(
        &self,
        _ctx: SecurityContext,
        _id: PackId,
        _req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError> {
        Err(later("replace_pack"))
    }

    async fn delegate(
        &self,
        _ctx: SecurityContext,
        _req: DelegateRequest,
    ) -> Result<Delegation, CanonicalError> {
        Err(later("delegate"))
    }

    async fn accept_delegation(
        &self,
        _ctx: SecurityContext,
        _id: DelegationId,
        _req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Err(later("accept_delegation"))
    }

    async fn reject_delegation(
        &self,
        _ctx: SecurityContext,
        _id: DelegationId,
        _req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Err(later("reject_delegation"))
    }

    async fn return_delegation(
        &self,
        _ctx: SecurityContext,
        _id: DelegationId,
        _req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Err(later("return_delegation"))
    }

    async fn recall_delegation(
        &self,
        _ctx: SecurityContext,
        _id: DelegationId,
        _req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError> {
        Err(later("recall_delegation"))
    }

    async fn check_explain(
        &self,
        _ctx: SecurityContext,
        _req: CheckExplainRequest,
    ) -> Result<CheckExplanation, CanonicalError> {
        Err(later("check_explain"))
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
