//! `IdpPluginClient` impl — kept separate from service.rs so domain state (Service) is reviewable independently of the SDK contract glue.

use async_trait::async_trait;
use toolkit_odata::filter::{FilterNode, FilterOp, ODataValue};
use toolkit_odata::{CursorV1, ODataOrderBy, OrderKey, Page, SortDir};
use toolkit_security::SecurityContext;

use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpDeprovisionUserRequest,
    IdpListUsersRequest, IdpPluginClient, IdpProvisionFailure, IdpProvisionResult,
    IdpProvisionTenantRequest, IdpProvisionUserRequest, IdpUpdateUserRequest, IdpUser,
    IdpUserDuplicateField, IdpUserFilterField, IdpUserOperationFailure,
};

use super::service::{Service, UpdateUserOutcome};

fn matches_filter(user: &IdpUser, filter: &FilterNode<IdpUserFilterField>) -> bool {
    match filter {
        FilterNode::Binary { field, op, value } => eval_binary(user, *field, *op, value),
        FilterNode::Composite {
            op: FilterOp::And,
            children,
        } => children.iter().all(|c| matches_filter(user, c)),
        FilterNode::Composite {
            op: FilterOp::Or,
            children,
        } => children.iter().any(|c| matches_filter(user, c)),
        FilterNode::Composite { .. } => unreachable!(
            "the OData parser only emits And/Or as composite ops; everything else \
             surfaces as Binary/InList/Not - reaching this arm signals a bug \
             upstream of the plugin SPI"
        ),
        FilterNode::Not(inner) => !matches_filter(user, inner),
        FilterNode::InList { field, values } => values
            .iter()
            .any(|v| eval_binary(user, *field, FilterOp::Eq, v)),
    }
}

fn eval_binary(
    user: &IdpUser,
    field: IdpUserFilterField,
    op: FilterOp,
    value: &ODataValue,
) -> bool {
    // Project the comparable string from the user row. `None` on an
    // optional field surfaces as `None`: `Eq` then never matches; `Ne`
    // always matches (an absent value is, by definition, "not equal" to
    // any concrete probe).
    let lhs: Option<String> = match field {
        IdpUserFilterField::Id => Some(user.id.to_string()),
        IdpUserFilterField::Username => Some(user.username.clone()),
        IdpUserFilterField::Email => user.email.clone(),
        IdpUserFilterField::DisplayName => user.display_name.clone(),
        IdpUserFilterField::FirstName => user.first_name.clone(),
        IdpUserFilterField::LastName => user.last_name.clone(),
    };
    let rhs: String = match value {
        ODataValue::String(s) => s.clone(),
        ODataValue::Uuid(u) => u.to_string(),
        other => unreachable!(
            "IdpUserFilterField declares only String and Uuid kinds - the REST parser \
             rejects every other ODataValue at the boundary; got {other:?}"
        ),
    };
    let Some(lhs) = lhs else {
        return matches!(op, FilterOp::Ne);
    };
    let lo = |s: &str| s.to_lowercase();
    match op {
        FilterOp::Eq => lhs == rhs,
        FilterOp::Ne => lhs != rhs,
        FilterOp::Contains => lo(&lhs).contains(&lo(&rhs)),
        FilterOp::StartsWith => lo(&lhs).starts_with(&lo(&rhs)),
        FilterOp::EndsWith => lo(&lhs).ends_with(&lo(&rhs)),
        other => unreachable!(
            "Gt/Ge/Lt/Le/In/And/Or are not legal on the String/Uuid IdpUserFilterField \
             surface - REST parser rejects upstream; got {other:?}"
        ),
    }
}

fn compare_by_order(a: &IdpUser, b: &IdpUser, order: &ODataOrderBy) -> std::cmp::Ordering {
    for key in &order.0 {
        let lhs = project_field(a, &key.field);
        let rhs = project_field(b, &key.field);
        let ord = lhs.cmp(&rhs);
        let ord = match key.dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        };
        if !ord.is_eq() {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

/// Project a user field by its `OData` name to a comparable `String`.
/// Absent `Option<String>` sorts as empty string (so absent < any
/// non-empty in `Asc`, > any non-empty in `Desc`). Unknown field names
/// cannot reach here — the REST `IdpUserFilterField` whitelist rejects
/// them at the parser boundary.
fn project_field(u: &IdpUser, field: &str) -> String {
    match field {
        "id" => u.id.to_string(),
        "username" => u.username.clone(),
        "email" => u.email.clone().unwrap_or_default(),
        "display_name" => u.display_name.clone().unwrap_or_default(),
        "first_name" => u.first_name.clone().unwrap_or_default(),
        "last_name" => u.last_name.clone().unwrap_or_default(),
        other => unreachable!(
            "unknown order field {other:?}; REST parser whitelists \
             IdpUserFilterField only - reaching this arm signals a bug \
             upstream of the plugin SPI"
        ),
    }
}

/// Project an item to its sort-key tuple per the effective order.
fn project_key_tuple(u: &IdpUser, order: &ODataOrderBy) -> Vec<String> {
    order.0.iter().map(|k| project_field(u, &k.field)).collect()
}

/// Compare a candidate item's projected key tuple against the
/// reference key tuple from the cursor, applying per-key direction
/// from the effective order. Used to skip items already returned on
/// the previous page.
fn compare_key_to_cursor(
    item_keys: &[String],
    cursor_keys: &[String],
    order: &ODataOrderBy,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    for (idx, key) in order.0.iter().enumerate() {
        let lhs = item_keys.get(idx).map_or("", String::as_str);
        let rhs = cursor_keys.get(idx).map_or("", String::as_str);
        let ord = lhs.cmp(rhs);
        let ord = match key.dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        };
        if !ord.is_eq() {
            return ord;
        }
    }
    Ordering::Equal
}

#[async_trait]
impl IdpPluginClient for Service {
    async fn provision_tenant(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionTenantRequest,
    ) -> Result<IdpProvisionResult, IdpProvisionFailure> {
        // Return non-empty deterministic metadata so AM's `Some` arm
        // in `activate_tenant` / `upsert_idp_metadata` is exercised by
        // every E2E flow that wires this plugin. A real provider would
        // place vendor-issued identifiers here; the echo plugin
        // surfaces a pure-function projection of the request inputs.
        Ok(IdpProvisionResult::new(Some(Self::echo_tenant_metadata(
            req,
        ))))
    }

    async fn deprovision_tenant(
        &self,
        _ctx: &SecurityContext,
        _req: &IdpDeprovisionTenantRequest,
    ) -> Result<(), IdpDeprovisionFailure> {
        Ok(())
    }

    async fn provision_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpProvisionUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        let tenant_id = req.tenant_context.tenant_id;
        let user = Self::echo_user(tenant_id, &req.payload);
        self.record_user(tenant_id, user.clone());
        Ok(user)
    }

    async fn deprovision_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpDeprovisionUserRequest,
    ) -> Result<(), IdpUserOperationFailure> {
        // Both `removed` and `already-absent` are success per the trait
        // doc on `deprovision_user`; AM does not distinguish them.
        let _ = self.forget_user(req.tenant_context.tenant_id, req.user_id);
        Ok(())
    }

    async fn update_user(
        &self,
        _ctx: &SecurityContext,
        req: &IdpUpdateUserRequest,
    ) -> Result<IdpUser, IdpUserOperationFailure> {
        match self.apply_user_patch(req.tenant_context.tenant_id, req.user_id, &req.patch) {
            UpdateUserOutcome::Updated(user) => Ok(user),
            UpdateUserOutcome::NotFound => Err(IdpUserOperationFailure::NotFound {
                detail: format!(
                    "user {} not found in tenant {}",
                    req.user_id, req.tenant_context.tenant_id
                ),
            }),
            UpdateUserOutcome::UsernameTaken => Err(IdpUserOperationFailure::DuplicateUser {
                field: IdpUserDuplicateField::Username,
                detail: "username already in use in this tenant scope".to_owned(),
            }),
        }
    }

    async fn list_users(
        &self,
        _ctx: &SecurityContext,
        req: &IdpListUsersRequest,
    ) -> Result<Page<IdpUser>, IdpUserOperationFailure> {
        // The per-tenant snapshot is a `HashMap` view; iteration order
        // is non-deterministic. Apply a typed OData ORDER BY walk so the
        // cursor (CursorV1 key-tuple boundary) is stable across calls — a
        // paginated client walking the snapshot must observe a
        // deterministic sequence, otherwise the same row could surface
        // on two pages or be skipped entirely. Default to `username ASC`
        // when the caller passes none (matches the AM service-layer's
        // default injection from Task 3.2; the plugin keeps its own copy
        // because nothing forces the AM service to be in front, e.g.
        // integration tests that call the plugin directly). Append
        // `id ASC` as a final tiebreaker via `ensure_tiebreaker`
        // (idempotent when `id` is already in the keys).
        let mut snapshot = self.snapshot_users(req.tenant_context.tenant_id);
        if let Some(filter) = req.filter.as_ref() {
            snapshot.retain(|u| matches_filter(u, filter));
        }
        let effective_order = req
            .order
            .clone()
            .unwrap_or_else(|| {
                ODataOrderBy(vec![OrderKey {
                    field: "username".into(),
                    dir: SortDir::Asc,
                }])
            })
            .ensure_tiebreaker("id", SortDir::Asc);
        snapshot.sort_by(|a, b| compare_by_order(a, b, &effective_order));

        // Decode any caller-supplied cursor. The cursor's `s` field encodes
        // the signed-token form of the order the cursor was issued under;
        // `validate_cursor_against` rejects when that disagrees with the
        // current request's effective order. ONLY order drift is detected
        // — filter drift is the caller's responsibility per the AM contract
        // (caller MUST NOT change `$filter` between continuation requests
        // with the same cursor). A malformed / drifted cursor surfaces as
        // `Rejected` so a hostile / buggy client cannot smuggle arbitrary
        // state.
        let cursor: Option<CursorV1> =
            match req.pagination.cursor() {
                None => None,
                Some(raw) => Some(CursorV1::decode(raw).map_err(|err| {
                    IdpUserOperationFailure::Rejected {
                        detail: format!("static-idp-plugin: invalid cursor: {err}"),
                    }
                })?),
            };
        if let Some(c) = cursor.as_ref()
            && let Err(err) = toolkit_odata::validate_cursor_against(c, &effective_order, None)
        {
            return Err(IdpUserOperationFailure::Rejected {
                detail: format!(
                    "static-idp-plugin: cursor was issued for a different \
                     $filter / $orderby than the current request: {err}"
                ),
            });
        }

        // Skip all items whose key tuple is <= the cursor's k (we already
        // returned them on the previous page).
        let skipped: Vec<IdpUser> = match cursor.as_ref() {
            Some(c) => snapshot
                .into_iter()
                .filter(|u| {
                    compare_key_to_cursor(
                        &project_key_tuple(u, &effective_order),
                        &c.k,
                        &effective_order,
                    )
                    .is_gt()
                })
                .collect(),
            None => snapshot,
        };

        let top = req.pagination.top() as usize;
        let mut page_items: Vec<IdpUser> = skipped.into_iter().take(top + 1).collect();
        // `has_more` is implied by `page_items.last()` being a row that
        // belongs to the *next* page (we fetched `top + 1` then popped it).
        let next_cursor = if page_items.len() > top {
            page_items.pop();
            let next = match page_items.last() {
                // `len() > top` then `pop()` leaves at least `top` items
                // (`top >= 1` is enforced upstream by `IdpUserPagination::new`).
                // No `expect` / `unwrap` here — fall through to `None` if the
                // invariant ever drifts, so the page still terminates cleanly.
                None => {
                    return Ok(Page::new(
                        page_items,
                        toolkit_odata::PageInfo {
                            next_cursor: None,
                            prev_cursor: None,
                            limit: u64::from(req.pagination.top()),
                        },
                    ));
                }
                Some(last) => CursorV1 {
                    k: project_key_tuple(last, &effective_order),
                    o: effective_order.0.first().map_or(SortDir::Asc, |k| k.dir),
                    s: effective_order.to_signed_tokens(),
                    f: None,
                    d: "fwd".to_owned(),
                },
            };
            Some(
                next.encode()
                    .map_err(|err| IdpUserOperationFailure::Rejected {
                        detail: format!("static-idp-plugin: failed to encode next cursor: {err}"),
                    })?,
            )
        } else {
            None
        };

        Ok(Page::new(
            page_items,
            toolkit_odata::PageInfo {
                next_cursor,
                prev_cursor: None, // backward pagination not supported by this plugin
                limit: u64::from(req.pagination.top()),
            },
        ))
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod pagination_tests;
