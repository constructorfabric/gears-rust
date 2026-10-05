//! The approvals inbox's products source (P-D-250): this gear's own approval-unit doors, read and
//! voted on as the caller, through the door functions themselves, never a copy of their rules.
//!
//! - **The page** is the list door's own read ([`super::page_of`]) under the list's grant and
//!   narrowing. Its keyset is a `CursorV1` this source builds from the inbox's
//!   `(submitted_at, id)` key with the pager's own codec, so the predicate is the pager's column
//!   compare (P-D-227), never a second one.
//! - **The counts** are the counts door's handler: one grouped statement on the plain
//!   connection, never in the list's serializable transaction (the phase 9 review's R32).
//! - **The card** is the card door's read before its names; a miss inside the tenant is `None`.
//!   Its `impact_live`, the live SKU head, is the inbox's `subject_live`. The inbox names the
//!   card's actors itself, so one inbox card read makes one lookup (AP-D-11, P-D-262).
//! - **The system actors** are the ones this gear's reads name "System" (P-D-262): the nil id
//!   and pricing's system actor. The inbox names them so too.
//! - **The impact** of a `sku_change` or `sku_retire` unit is pricing's usage of its SKU: ONE
//!   `SkuUsageV1::usage` call per page, through the SKU read's own helper, so a refusal (no
//!   pricing entry read), an outage, a late answer or a missing port is `impact: null` and never a
//!   failed read (P-D-197). A `sku_publish` unit has none. `usage_sets` is never called.
//! - **A vote** is the vote door, sent through this gear's approval-unit router under its
//!   enforcer and the platform's error layer: the door's grant, its idempotency row under its own
//!   endpoint, its answer's bytes.
//! - A kind products does not record, and any `book_id` (products holds no book), are an empty
//!   page and zero counts, decided here before any door (AP-D-2).

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Extension, Query};
use axum::http::{Request, header};
use axum::response::Response;
use bss_approvals_sdk::{
    ApprovalSourceV1, InboxKind, InboxUnit, Order, SortKey, SourceCounts, SourceNarrowing,
    SourcePage, SourcePageQuery, VoteAction, VoteRequest, VoteResponse,
};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_db::odata::{ODataFieldMapping, encode_cursor_value};
use toolkit_odata::filter::FilterField;
use toolkit_odata::{CursorV1, ODataQuery, SortDir};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

use super::{ApiState, UnitCountsQuery, g, require_authenticated};
use crate::authz::{actions, resource_types};
use crate::domain::approvals::ApprovalKind;
use crate::infra::storage::repo::{UnitListField, UnitListMapping, submission_order};

/// The name this source is registered under in `ClientHub` (`ClientScope`).
pub const SOURCE: &str = "products";

/// The products gear's approval units for the approvals inbox.
pub struct ProductsApprovalSource {
    state: Arc<ApiState>,
    enforcer: authz_resolver_sdk::PolicyEnforcer,
    /// The approval-unit router under this gear's enforcer and the platform's error layer, which
    /// the gateway puts around every door. The vote doors are answered here.
    doors: axum::Router,
}

impl ProductsApprovalSource {
    /// The source over the gear's own state and enforcer.
    #[must_use]
    pub fn new(state: Arc<ApiState>, enforcer: authz_resolver_sdk::PolicyEnforcer) -> Self {
        let doors = super::router(state.clone(), &toolkit::api::OpenApiRegistryImpl::new())
            .layer(Extension(enforcer.clone()))
            .layer(axum::middleware::from_fn(
                toolkit::api::canonical_error_middleware,
            ));
        Self {
            state,
            enforcer,
            doors,
        }
    }

    /// Pricing's usage of the SKUs of the `sku_change` and `sku_retire` units, from one call, into
    /// their `impact`; null where the port did not answer.
    async fn fill_impact(&self, ctx: &SecurityContext, units: &mut [InboxUnit]) {
        let skus: Vec<Uuid> = units
            .iter()
            .filter(|unit| used(unit.kind))
            .map(|unit| unit.ref_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let usage = crate::api::rest::usage::of(&self.state, ctx, &skus).await;
        for unit in units.iter_mut().filter(|unit| used(unit.kind)) {
            unit.impact = usage
                .get(&unit.ref_id)
                .and_then(|row| serde_json::to_value(row).ok());
        }
    }
}

/// The kinds whose impact is pricing's usage of the SKU: a change and a retire. A publish has
/// none.
const fn used(kind: InboxKind) -> bool {
    matches!(kind, InboxKind::SkuChange | InboxKind::SkuRetire)
}

/// A narrowing products holds nothing under: a kind it does not record, or a book.
fn foreign(narrowing: &SourceNarrowing) -> bool {
    narrowing.book_id.is_some()
        || narrowing
            .kind
            .as_deref()
            .is_some_and(|kind| ApprovalKind::parse(kind).is_none())
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
                    "bss-products: the list's order names an unknown field: {}",
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
                CanonicalError::internal(format!(
                    "bss-products: the inbox key does not encode: {e}"
                ))
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
            CanonicalError::internal(format!("bss-products: a door's body did not read: {e}"))
                .create()
        })?;
    serde_json::from_slice(&bytes).map_err(|e| not_inbox(&e))
}

fn not_inbox(error: &serde_json::Error) -> CanonicalError {
    CanonicalError::internal(format!(
        "bss-products: a door's answer does not read as the inbox's: {error}"
    ))
    .create()
}

#[async_trait]
impl ApprovalSourceV1 for ProductsApprovalSource {
    async fn page(
        &self,
        ctx: &SecurityContext,
        q: &SourcePageQuery,
    ) -> Result<SourcePage, CanonicalError> {
        super::narrowing(q.narrowing.state.as_deref(), None, None)?;
        if foreign(&q.narrowing) {
            return Ok(SourcePage {
                units: Vec::new(),
                has_more: false,
            });
        }
        let ctx = require_authenticated(Some(Extension(ctx.clone())))?;
        let scope = g::scope(
            &self.enforcer,
            &ctx,
            &resource_types::APPROVAL_UNIT,
            actions::READ,
        )
        .await?;
        let filter = super::narrowing(
            q.narrowing.state.as_deref(),
            q.narrowing.kind.as_deref(),
            q.narrowing.ref_id,
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
        let approve_scope = g::grant_scope(&self.enforcer, &ctx, actions::APPROVE).await?;
        let submit_scope = g::grant_scope(&self.enforcer, &ctx, actions::SUBMIT).await?;
        let list = super::page_of(
            &self.state,
            scope,
            &ctx,
            filter,
            page,
            approve_scope,
            submit_scope,
        )
        .await?;
        let has_more = list.page_info.next_cursor.is_some();
        let mut units = list
            .items
            .into_iter()
            .map(|item| {
                serde_json::to_value(item).and_then(|item| InboxUnit::from_door(SOURCE, item))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| not_inbox(&e))?;
        if q.impact {
            self.fill_impact(&ctx, &mut units).await;
        }
        Ok(SourcePage { units, has_more })
    }

    async fn counts(
        &self,
        ctx: &SecurityContext,
        n: &SourceNarrowing,
    ) -> Result<SourceCounts, CanonicalError> {
        super::narrowing(n.state.as_deref(), None, None)?;
        if foreign(n) {
            return Ok(SourceCounts::default());
        }
        let response = super::counts(
            Extension(self.state.clone()),
            Extension(self.enforcer.clone()),
            Some(Extension(ctx.clone())),
            Ok(Query(UnitCountsQuery {
                state: n.state.clone(),
                kind: n.kind.clone(),
                ref_id: n.ref_id,
            })),
        )
        .await?;
        serde_json::from_value(door_json(response).await?).map_err(|e| not_inbox(&e))
    }

    fn system_actors(&self) -> &[Uuid] {
        &crate::api::rest::SYSTEM_ACTORS
    }

    async fn get(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        impact: bool,
    ) -> Result<Option<InboxUnit>, CanonicalError> {
        let ctx = require_authenticated(Some(Extension(ctx.clone())))?;
        match super::card(&self.state, &self.enforcer, &ctx, id).await {
            Ok(card) => {
                let door = serde_json::to_value(card).map_err(|e| not_inbox(&e))?;
                let mut unit = InboxUnit::from_door(SOURCE, door).map_err(|e| not_inbox(&e))?;
                if impact {
                    self.fill_impact(&ctx, std::slice::from_mut(&mut unit))
                        .await;
                }
                Ok(Some(unit))
            }
            // The card's miss is a unit this tenant does not hold (P-D-250, AP-D-3).
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
            "/bss-products/v1/approval-units/{id}/{}",
            action.as_str()
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ctx.clone());
        if let Some(key) = &request.idempotency_key {
            builder = builder.header("Idempotency-Key", key);
        }
        let door_request = builder.body(Body::from(request.body)).map_err(|e| {
            CanonicalError::internal(format!("bss-products: the vote does not build: {e}")).create()
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
                "bss-products: the vote answered a header that is not text: {e}"
            ))
            .create()
        })?;
        headers.push((name.as_str().to_owned(), value.to_owned()));
    }
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .map_err(|e| {
            CanonicalError::internal(format!("bss-products: the vote's body did not read: {e}"))
                .create()
        })?;
    Ok(VoteResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

#[cfg(test)]
#[path = "inbox_source_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "inbox_e2e_tests.rs"]
mod e2e_tests;
