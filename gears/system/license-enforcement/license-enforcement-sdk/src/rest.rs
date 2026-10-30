//! REST projection of [`LicenseEnforcementApi`].
//!
//! With `rest-client` the macro emits `LicenseEnforcementApiRestClient` and its
//! directory-resolving wrapper; with `rest-server` it emits the route
//! registration that the gear hosts. It registers every route; the gear
//! rejects a create or a verb that needs an intake the linked plugin does not
//! serve.
//!
//! `list_packs` and `list_grants` are `#[server_manual]`: the gear registers
//! `GET /packs` and `GET /grants` with `OperationBuilder`, the `OData`
//! extractor, and `with_odata_filter`, as other gears do. The client sends
//! [`ListQuery`], which that extractor binds.
//!
//! Every method is retryable: reads are safe, and every write carries an
//! idempotency token, so a repeated write returns the first result.

use toolkit::Page;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::contract::LicenseEnforcementApi;
use crate::dto::{
    BindRequest, CheckExplainRequest, CheckExplanation, DelegateRequest, Delegation,
    DelegationVerbRequest, ImportEvidenceRequest, IssueGrantRequest, IssuePackRequest, IssueResult,
    ListQuery, RenewRequest, ReplaceRequest, VerbRequest,
};
use crate::ids::{DelegationId, GrantId, PackId};
use crate::models::{Grant, Pack};

/// HTTP projection of [`LicenseEnforcementApi`].
#[toolkit::rest_contract(
    base_path = "/license-enforcement/v1",
    visibility = "exposed",
    require_full_coverage
)]
pub trait LicenseEnforcementApiRest: LicenseEnforcementApi {
    /// `POST /packs`
    #[post("/packs")]
    #[retryable]
    async fn issue_pack(
        &self,
        ctx: SecurityContext,
        req: IssuePackRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// `POST /grants`
    #[post("/grants")]
    #[retryable]
    async fn issue_grant(
        &self,
        ctx: SecurityContext,
        req: IssueGrantRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// `POST /evidence`
    #[post("/evidence")]
    #[retryable]
    async fn import_evidence(
        &self,
        ctx: SecurityContext,
        req: ImportEvidenceRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// `GET /packs/{id}`
    #[get("/packs/{id}")]
    #[retryable]
    async fn get_pack(&self, ctx: SecurityContext, id: PackId) -> Result<Pack, CanonicalError>;

    /// `GET /packs`
    #[get("/packs")]
    #[retryable]
    #[server_manual]
    async fn list_packs(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Pack>, CanonicalError>;

    /// `GET /grants/{id}`
    #[get("/grants/{id}")]
    #[retryable]
    async fn get_grant(&self, ctx: SecurityContext, id: GrantId) -> Result<Grant, CanonicalError>;

    /// `GET /grants`
    #[get("/grants")]
    #[retryable]
    #[server_manual]
    async fn list_grants(
        &self,
        ctx: SecurityContext,
        query: ListQuery,
    ) -> Result<Page<Grant>, CanonicalError>;

    /// `GET /delegations/{id}`
    #[get("/delegations/{id}")]
    #[retryable]
    async fn get_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
    ) -> Result<Delegation, CanonicalError>;

    /// `POST /grants/{id}/accept`
    #[post("/grants/{id}/accept")]
    #[retryable]
    async fn accept_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/reject`
    #[post("/grants/{id}/reject")]
    #[retryable]
    async fn reject_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/suspend`
    #[post("/grants/{id}/suspend")]
    #[retryable]
    async fn suspend_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/reinstate`
    #[post("/grants/{id}/reinstate")]
    #[retryable]
    async fn reinstate_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/revoke`
    #[post("/grants/{id}/revoke")]
    #[retryable]
    async fn revoke_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/renew`
    #[post("/grants/{id}/renew")]
    #[retryable]
    async fn renew_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: RenewRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/bind`
    #[post("/grants/{id}/bind")]
    #[retryable]
    async fn bind_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: BindRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/unbind`
    #[post("/grants/{id}/unbind")]
    #[retryable]
    async fn unbind_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/return`
    #[post("/grants/{id}/return")]
    #[retryable]
    async fn return_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/recall`
    #[post("/grants/{id}/recall")]
    #[retryable]
    async fn recall_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: VerbRequest,
    ) -> Result<Grant, CanonicalError>;

    /// `POST /grants/{id}/replace`
    #[post("/grants/{id}/replace")]
    #[retryable]
    async fn replace_grant(
        &self,
        ctx: SecurityContext,
        id: GrantId,
        req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// `POST /packs/{id}/suspend`
    #[post("/packs/{id}/suspend")]
    #[retryable]
    async fn suspend_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError>;

    /// `POST /packs/{id}/reinstate`
    #[post("/packs/{id}/reinstate")]
    #[retryable]
    async fn reinstate_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError>;

    /// `POST /packs/{id}/revoke`
    #[post("/packs/{id}/revoke")]
    #[retryable]
    async fn revoke_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: VerbRequest,
    ) -> Result<Pack, CanonicalError>;

    /// `POST /packs/{id}/renew`
    #[post("/packs/{id}/renew")]
    #[retryable]
    async fn renew_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: RenewRequest,
    ) -> Result<Pack, CanonicalError>;

    /// `POST /packs/{id}/replace`
    #[post("/packs/{id}/replace")]
    #[retryable]
    async fn replace_pack(
        &self,
        ctx: SecurityContext,
        id: PackId,
        req: ReplaceRequest,
    ) -> Result<IssueResult, CanonicalError>;

    /// `POST /delegations`
    #[post("/delegations")]
    #[retryable]
    async fn delegate(
        &self,
        ctx: SecurityContext,
        req: DelegateRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// `POST /delegations/{id}/accept`
    #[post("/delegations/{id}/accept")]
    #[retryable]
    async fn accept_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// `POST /delegations/{id}/reject`
    #[post("/delegations/{id}/reject")]
    #[retryable]
    async fn reject_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// `POST /delegations/{id}/return`
    #[post("/delegations/{id}/return")]
    #[retryable]
    async fn return_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// `POST /delegations/{id}/recall`
    #[post("/delegations/{id}/recall")]
    #[retryable]
    async fn recall_delegation(
        &self,
        ctx: SecurityContext,
        id: DelegationId,
        req: DelegationVerbRequest,
    ) -> Result<Delegation, CanonicalError>;

    /// `POST /check-explain`
    #[post("/check-explain")]
    #[retryable]
    async fn check_explain(
        &self,
        ctx: SecurityContext,
        req: CheckExplainRequest,
    ) -> Result<CheckExplanation, CanonicalError>;
}
