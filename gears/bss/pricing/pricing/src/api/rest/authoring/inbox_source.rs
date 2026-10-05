//! The approvals inbox's pricing source (D-490): pricing's own approval-unit doors, read and voted
//! on as the caller, through the door functions themselves, never a copy of their rules.
//!
//! - **The page** is the list door's own read, `approvals::list_units` over `page_units`, under
//!   the list's grant and narrowing. Its keyset is a `CursorV1` this source builds from the
//!   inbox's `(submitted_at, id)` key with the pager's own codec, so the predicate is the pager's
//!   column compare (D-470), never a second one.
//! - **The counts** are the counts door's own grouped statement, on the plain connection, never in
//!   the list's serializable transaction (the phase 9 review's R32).
//! - **The card** is the card door's handler; a miss inside the tenant is `None`.
//! - **A vote** is the vote door, sent through the authoring router this gear serves, under the
//!   gear's enforcer and its error layer: the door's grant (`approval_unit` approve, or submit for
//!   a withdraw), its idempotency row under its own endpoint, its answer's bytes.
//! - A kind pricing does not record is an empty page and zero counts, decided here before any
//!   door (AP-D-2). `subject_live` is null: the unit card loads no predecessor (D-490).

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Path};
use axum::http::{Request, header};
use axum::response::Response;
use bss_approvals_sdk::{
    ApprovalSourceV1, InboxUnit, Order, SortKey, SourceCounts, SourceNarrowing, SourcePage,
    SourcePageQuery, VoteAction, VoteRequest, VoteResponse,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::odata::{ODataFieldMapping, encode_cursor_value};
use toolkit_odata::filter::FilterField;
use toolkit_odata::{CursorV1, ODataQuery, SortDir};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use super::support::{self, authz_failure, require_authenticated, transaction};
use super::{AuthoringState, approvals};
use crate::authz::{self, actions, resource_types};
use crate::infra::approval_kinds::Kind;
use crate::infra::storage::repo::approval_repo::{
    UnitListField, UnitListMapping, submission_order,
};

/// The name this source is registered under in `ClientHub` (`ClientScope`).
pub const SOURCE: &str = "pricing";

/// Pricing's approval units for the approvals inbox.
pub struct PricingApprovalSource {
    state: Arc<AuthoringState>,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
    /// The authoring router as this gear serves it (`module.rs`): its enforcer, then its error
    /// layer. The vote doors are answered here.
    doors: axum::Router,
}

impl PricingApprovalSource {
    /// The source over the gear's own state and enforcer.
    #[must_use]
    pub fn new(state: Arc<AuthoringState>, enforcer: authz_resolver_sdk::PolicyEnforcer) -> Self {
        let doors = super::with_caller_layers(
            super::router(state.clone(), &toolkit::api::OpenApiRegistryImpl::new()),
            enforcer.clone(),
        );
        Self {
            state,
            enforcer,
            doors,
        }
    }

    /// The caller and the list's grant: what the list and the counts doors check first.
    async fn read_scope(
        &self,
        ctx: &SecurityContext,
    ) -> Result<(SecurityContext, toolkit_db::secure::AccessScope), CanonicalError> {
        let ctx = require_authenticated(Some(Extension(ctx.clone())))?;
        let scope = authz::access_scope(
            &self.enforcer,
            &ctx,
            &resource_types::APPROVAL_UNIT,
            actions::READ,
            None,
            None,
        )
        .await
        .map_err(authz_failure)?;
        Ok((ctx, scope))
    }
}

/// A kind the narrowing names that pricing does not record: its page is empty, its counts zero.
fn foreign_kind(narrowing: &SourceNarrowing) -> bool {
    narrowing
        .kind
        .as_deref()
        .is_some_and(|kind| Kind::parse(kind).is_none())
}

/// The pager's own cursor for the key after which a page starts, in the list's one order
/// (`submission_order(dir)`, the order the door puts on its query): each of the order's keys takes
/// its value from the inbox's key, encoded by the pager's codec under the list mapping's cursor
/// kind; no narrowing hash, forward. `page_units` then reads the order from the cursor and
/// compares the columns as it does for its own cursors.
fn keyset_cursor(after: SortKey, dir: SortDir) -> Result<CursorV1, CanonicalError> {
    let order = submission_order(dir);
    let k = order
        .0
        .iter()
        .map(|key| {
            let field = UnitListField::from_name(&key.field).ok_or_else(|| {
                CanonicalError::internal(format!(
                    "bss-pricing: the list's order names an unknown field: {}",
                    key.field
                ))
                .create()
            })?;
            let value = match field {
                UnitListField::SubmittedAt => {
                    sea_orm::Value::TimeDateTimeWithTimeZone(Some(after.submitted_at))
                }
                UnitListField::Id => sea_orm::Value::Uuid(Some(after.id)),
            };
            encode_cursor_value(
                &value,
                <UnitListMapping as ODataFieldMapping<UnitListField>>::cursor_kind(field),
            )
            .map_err(|e| {
                CanonicalError::internal(format!("bss-pricing: the inbox key does not encode: {e}"))
                    .create()
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CursorV1 {
        k,
        o: dir,
        s: order.to_signed_tokens(),
        f: None,
        d: "fwd".to_owned(),
    })
}

/// The JSON a door answered.
async fn door_json(response: Response) -> Result<serde_json::Value, CanonicalError> {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|e| {
            CanonicalError::internal(format!("bss-pricing: a door's body did not read: {e}"))
                .create()
        })?;
    serde_json::from_slice(&bytes).map_err(|e| not_inbox(&e))
}

fn not_inbox(error: &serde_json::Error) -> CanonicalError {
    CanonicalError::internal(format!(
        "bss-pricing: a door's answer does not read as the inbox's: {error}"
    ))
    .create()
}

#[async_trait]
impl ApprovalSourceV1 for PricingApprovalSource {
    // AP-D-11: pricing's own system actor and the nil actor are not people the inbox asks AM for.
    fn system_actors(&self) -> &[Uuid] {
        &super::SYSTEM_ACTORS
    }

    async fn page(
        &self,
        ctx: &SecurityContext,
        q: &SourcePageQuery,
    ) -> Result<SourcePage, CanonicalError> {
        approvals::state_filter(q.narrowing.state.as_deref())?;
        if foreign_kind(&q.narrowing) {
            return Ok(SourcePage {
                units: Vec::new(),
                has_more: false,
            });
        }
        let (ctx, scope) = self.read_scope(ctx).await?;
        let filter = super::unit_narrowing(
            q.narrowing.state.as_deref(),
            q.narrowing.kind.as_deref(),
            q.narrowing.ref_id,
            q.narrowing.book_id,
        )?;
        let direction = match q.order {
            Order::Asc => SortDir::Asc,
            Order::Desc => SortDir::Desc,
        };
        // The page carries the order, as the list door's query does: a continuation's cursor carries
        // its own, a first page names `submission_order` (the phase 9 review's R36).
        let page = ODataQuery::new().with_limit(u64::from(q.limit));
        let page = match q.after {
            Some(after) => page.with_cursor(keyset_cursor(after, direction)?),
            None => page.with_order(submission_order(direction)),
        };
        let (approve_scope, submit_scope) = (
            crate::authz::grant_scope(&self.enforcer, &ctx, actions::APPROVE)
                .await
                .map_err(authz_failure)?,
            crate::authz::grant_scope(&self.enforcer, &ctx, actions::SUBMIT)
                .await
                .map_err(authz_failure)?,
        );
        let request = approvals::UnitListRequest {
            filter,
            page,
            impact: q.impact,
            approve_scope,
            submit_scope,
        };
        let listed = transaction(&self.state.db.db(), move |tx| {
            let (scope, ctx, request) = (scope.clone(), ctx.clone(), request.clone());
            Box::pin(async move { approvals::read_unit_page(tx, &scope, &ctx, &request).await })
        })
        .await?;
        let has_more = listed.page_info.next_cursor.is_some();
        let units = listed
            .items
            .into_iter()
            .map(|item| {
                serde_json::to_value(item)
                    .map_err(|e| not_inbox(&e))
                    .and_then(|item| InboxUnit::from_door(SOURCE, item).map_err(|e| not_inbox(&e)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SourcePage { units, has_more })
    }

    async fn counts(
        &self,
        ctx: &SecurityContext,
        n: &SourceNarrowing,
    ) -> Result<SourceCounts, CanonicalError> {
        approvals::state_filter(n.state.as_deref())?;
        if foreign_kind(n) {
            return Ok(SourceCounts::default());
        }
        let (ctx, scope) = self.read_scope(ctx).await?;
        let filter =
            super::unit_narrowing(n.state.as_deref(), n.kind.as_deref(), n.ref_id, n.book_id)?;
        // The counts door's connection: one grouped statement is its own snapshot, never read in
        // the list's serializable transaction.
        let conn = self.state.db.conn().map_err(support::DoorError::from)?;
        let response =
            approvals::count_units(&conn, &scope, ctx.subject_tenant_id(), &filter).await?;
        serde_json::from_value(door_json(response).await?).map_err(|e| not_inbox(&e))
    }

    async fn get(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        impact: bool,
    ) -> Result<Option<InboxUnit>, CanonicalError> {
        let answer = super::get_approval_unit(
            Extension(self.state.clone()),
            Extension(self.enforcer.clone()),
            Some(Extension(ctx.clone())),
            Path(id),
        )
        .await;
        match answer {
            Ok(response) => {
                let mut unit = InboxUnit::from_door(SOURCE, door_json(response).await?)
                    .map_err(|e| not_inbox(&e))?;
                if !impact {
                    unit.impact = None;
                }
                Ok(Some(unit))
            }
            // The card's miss is a unit this tenant does not hold (D-490, AP-D-3).
            Err(error) if error.status_code() == 404 => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn vote(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        action: VoteAction,
        request: VoteRequest,
    ) -> Result<VoteResponse, CanonicalError> {
        let mut builder = Request::post(format!(
            "/bss-pricing/v1/approval-units/{id}/{}",
            action.as_str()
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ctx.clone());
        if let Some(key) = &request.idempotency_key {
            builder = builder.header("Idempotency-Key", key);
        }
        let door_request = builder.body(Body::from(request.body)).map_err(|e| {
            CanonicalError::internal(format!("bss-pricing: the vote does not build: {e}")).create()
        })?;
        let Ok(response) = self.doors.clone().oneshot(door_request).await;
        answer_of(response).await
    }
}

/// The door's answer as the inbox passes it on: status, headers in order, body bytes.
async fn answer_of(response: Response) -> Result<VoteResponse, CanonicalError> {
    let status = response.status().as_u16();
    let mut headers = Vec::with_capacity(response.headers().len());
    for (name, value) in response.headers() {
        let value = value.to_str().map_err(|e| {
            CanonicalError::internal(format!(
                "bss-pricing: the vote answered a header that is not text: {e}"
            ))
            .create()
        })?;
        headers.push((name.as_str().to_owned(), value.to_owned()));
    }
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|e| {
            CanonicalError::internal(format!("bss-pricing: the vote's body did not read: {e}"))
                .create()
        })?;
    Ok(VoteResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}
