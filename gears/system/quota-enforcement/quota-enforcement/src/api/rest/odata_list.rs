//! `OData` on the REST lists: the `GET /quotas` `$filter` grammar, each list's one
//! order, and the forward-only cursor that wraps the storage plugin's opaque one.

use quota_enforcement_sdk::{QuotaFilterField, QuotaId, QuotaStatus, TenantId};
use std::fmt::Write as _;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_odata::ast::{Expr, Value as ODataValue};
use toolkit_odata::errors::OdataError;
use toolkit_odata::filter::{FilterNode, FilterOp, convert_expr_to_filter_node};
use toolkit_odata::{CursorV1, ODataOrderBy, SortDir};

use toolkit_security::crypto::hash::Sha256;

use crate::domain::error::DomainError;
use crate::domain::quotas::ListQuotasRequest;
use crate::domain::tokens;

/// The one direction every list pages in.
const FORWARD: &str = "fwd";

/// What `$filter` asked of the Quota list, before the domain validates it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct QuotaFilterParts {
    pub tenant_id: Option<TenantId>,
    pub projection_type: Option<gts::GtsTypeId>,
    pub subject_id: Option<String>,
    pub metric: Option<String>,
    pub status: Option<QuotaStatus>,
    pub ids: Option<Vec<QuotaId>>,
}

impl QuotaFilterParts {
    /// The domain list request this filter and page select.
    pub fn into_request(self, limit: u32, cursor: Option<String>) -> ListQuotasRequest {
        ListQuotasRequest {
            tenant_id: self.tenant_id,
            projection_type: self.projection_type,
            subject_id: self.subject_id,
            metric: self.metric,
            status: self.status,
            ids: self.ids.unwrap_or_default(),
            limit: Some(limit),
            cursor,
        }
    }

    /// Fingerprint of the effective filter, defaulted tenant included, ids in any order.
    pub fn binding(&self, effective_tenant: TenantId) -> String {
        let mut ids: Vec<String> = self.ids.iter().flatten().map(ToString::to_string).collect();
        ids.sort_unstable();
        let status = self.status.map(|s| match s {
            QuotaStatus::Active => "active",
            QuotaStatus::Deactivated => "deactivated",
        });
        fingerprint(&serde_json::json!([
            "quotas",
            effective_tenant.to_string(),
            self.projection_type.as_ref().map(AsRef::<str>::as_ref),
            self.subject_id,
            self.metric,
            status,
            ids,
        ]))
    }
}

/// Fingerprint a policy history continues: the policy it pages.
pub fn policy_binding(policy_id: &str) -> String {
    fingerprint(&serde_json::json!(["policy_versions", policy_id]))
}

/// 64 bits of SHA-256 over the canonical JSON, as hex.
fn fingerprint(value: &serde_json::Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.to_string().as_bytes());
    let digest = hasher.finalize();
    digest
        .as_ref()
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut out, byte| {
            _ = write!(out, "{byte:02x}");
            out
        })
}

/// Lower `$filter` onto the Quota list's slots; anything else is a 400, never dropped.
///
/// # Errors
/// A field, operator or connective outside the grammar, a field constrained
/// twice, or a value the list does not know.
pub fn lower_quota_filter(filter: Option<&Expr>) -> Result<QuotaFilterParts, Box<Problem>> {
    let mut parts = QuotaFilterParts::default();
    let Some(expr) = filter else {
        return Ok(parts);
    };
    let node = convert_expr_to_filter_node::<QuotaFilterField>(expr)
        .map_err(|e| filter_error(&e.to_string()))?;
    walk(&node, &mut parts)?;
    Ok(parts)
}

fn walk(
    node: &FilterNode<QuotaFilterField>,
    parts: &mut QuotaFilterParts,
) -> Result<(), Box<Problem>> {
    match node {
        FilterNode::Composite {
            op: FilterOp::And,
            children,
        } => children.iter().try_for_each(|child| walk(child, parts)),
        FilterNode::Binary {
            field,
            op: FilterOp::Eq,
            value,
        } => assign(*field, std::slice::from_ref(value), parts),
        FilterNode::InList { field, values } => assign(*field, values, parts),
        _ => Err(filter_error(
            "only `eq` (and `in` on `id`) joined by `and` are supported",
        )),
    }
}

fn assign(
    field: QuotaFilterField,
    values: &[ODataValue],
    parts: &mut QuotaFilterParts,
) -> Result<(), Box<Problem>> {
    use toolkit_odata::filter::FilterField as _;
    let twice = || filter_error(&format!("`{}` is constrained more than once", field.name()));
    match (field, values) {
        (QuotaFilterField::Id, values) => {
            if parts.ids.is_some() {
                return Err(twice());
            }
            let ids = values
                .iter()
                .map(|value| match value {
                    ODataValue::Uuid(id) => Ok(QuotaId::new(*id)),
                    _ => Err(filter_error("`id` takes UUIDs")),
                })
                .collect::<Result<_, _>>()?;
            parts.ids = Some(ids);
        }
        (QuotaFilterField::TenantId, [ODataValue::Uuid(id)]) => {
            set(&mut parts.tenant_id, TenantId::new(*id)).map_err(|()| twice())?;
        }
        (QuotaFilterField::ProjectionType, [ODataValue::String(raw)]) => {
            let projection = gts::GtsTypeId::try_new(raw).map_err(|_| {
                Box::new(Problem::from(DomainError::InvalidArgument {
                    field: "projection_type",
                    reason: tokens::PROJECTION_INVALID,
                }))
            })?;
            set(&mut parts.projection_type, projection).map_err(|()| twice())?;
        }
        (QuotaFilterField::SubjectId, [ODataValue::String(raw)]) => {
            set(&mut parts.subject_id, raw.clone()).map_err(|()| twice())?;
        }
        (QuotaFilterField::Metric, [ODataValue::String(raw)]) => {
            set(&mut parts.metric, raw.clone()).map_err(|()| twice())?;
        }
        (QuotaFilterField::Status, [ODataValue::String(raw)]) => {
            let status = match raw.as_str() {
                "active" => QuotaStatus::Active,
                "deactivated" => QuotaStatus::Deactivated,
                _ => {
                    return Err(Box::new(Problem::from(DomainError::InvalidArgument {
                        field: "status",
                        reason: tokens::STATUS_INVALID,
                    })));
                }
            };
            set(&mut parts.status, status).map_err(|()| twice())?;
        }
        _ => {
            return Err(filter_error(&format!(
                "`{}` takes one value with `eq`",
                field.name()
            )));
        }
    }
    Ok(())
}

fn set<T>(slot: &mut Option<T>, value: T) -> Result<(), ()> {
    if slot.is_some() {
        return Err(());
    }
    *slot = Some(value);
    Ok(())
}

/// Refuse a query key the list does not read, so a former parameter cannot widen the page.
///
/// # Errors
/// The first unsupported key.
pub fn reject_unknown_params(keys: &[(String, String)]) -> Result<(), Box<Problem>> {
    const READ: [&str; 6] = [
        "$filter",
        "$orderby",
        "$top",
        "limit",
        "cursor",
        "$skiptoken",
    ];
    match keys.iter().find(|(key, _)| !READ.contains(&key.as_str())) {
        Some((key, _)) => Err(unsupported_param(
            key,
            &format!("`{key}` is not a parameter of this list"),
        )),
        None => Ok(()),
    }
}

/// Refuse `$select`: the lists return whole items.
///
/// # Errors
/// `$select` was sent.
pub fn reject_select(select: Option<&Vec<String>>) -> Result<(), Box<Problem>> {
    match select {
        Some(_) => Err(unsupported_param("$select", "the list returns whole items")),
        None => Ok(()),
    }
}

/// Refuse `$filter` on a list that takes none.
///
/// # Errors
/// `$filter` was sent.
pub fn reject_filter(filter: Option<&Expr>) -> Result<(), Box<Problem>> {
    match filter {
        Some(_) => Err(filter_error("this list takes no `$filter`")),
        None => Ok(()),
    }
}

/// Accept only the list's keyset order, ascending.
///
/// # Errors
/// Any other field, direction, or a second key.
pub fn require_order(order: &ODataOrderBy, field: &str) -> Result<(), Box<Problem>> {
    match order.0.as_slice() {
        [] => Ok(()),
        [key] if key.field == field && key.dir == SortDir::Asc => Ok(()),
        _ => Err(odata(toolkit_odata::Error::InvalidOrderByField(format!(
            "only `{field} asc` is supported"
        )))),
    }
}

/// The page size: the platform default within `max` when absent.
///
/// # Errors
/// A size above `max`.
pub fn page_size(requested: Option<u64>, default: u32, max: u32) -> Result<u32, Box<Problem>> {
    match requested {
        None => Ok(default.min(max)),
        Some(size) => u32::try_from(size)
            .ok()
            .filter(|size| *size <= max)
            .ok_or_else(|| odata(toolkit_odata::Error::InvalidLimit)),
    }
}

/// Unwrap a continuation into the storage cursor it carries.
///
/// # Errors
/// A backward cursor, another order, or another filter than `binding`.
pub fn continuation(
    cursor: Option<&CursorV1>,
    sort: &str,
    binding: &str,
) -> Result<Option<String>, Box<Problem>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    if cursor.d != FORWARD || cursor.o != SortDir::Asc {
        return Err(odata(toolkit_odata::Error::InvalidCursor));
    }
    if cursor.s != sort {
        return Err(odata(toolkit_odata::Error::OrderMismatch));
    }
    if cursor.f.as_deref() != Some(binding) {
        return Err(odata(toolkit_odata::Error::FilterMismatch));
    }
    match cursor.k.as_slice() {
        [storage] => Ok(Some(storage.clone())),
        _ => Err(odata(toolkit_odata::Error::InvalidCursor)),
    }
}

/// Wrap the storage cursor of the next page, bound to `binding`.
///
/// # Errors
/// The cursor cannot be encoded.
pub fn next_cursor(
    storage: Option<String>,
    sort: &str,
    binding: &str,
) -> Result<Option<String>, Box<Problem>> {
    storage
        .map(|key| {
            CursorV1 {
                k: vec![key],
                o: SortDir::Asc,
                s: sort.to_owned(),
                f: Some(binding.to_owned()),
                d: FORWARD.to_owned(),
            }
            .encode()
            .map_err(|e| Box::new(Problem::from(DomainError::Internal(format!("cursor: {e}")))))
        })
        .transpose()
}

fn filter_error(message: &str) -> Box<Problem> {
    odata(toolkit_odata::Error::InvalidFilter(message.to_owned()))
}

fn odata(err: toolkit_odata::Error) -> Box<Problem> {
    Box::new(Problem::from(CanonicalError::from(err)))
}

fn unsupported_param(field: &str, message: &str) -> Box<Problem> {
    Box::new(Problem::from(
        OdataError::invalid_argument()
            .with_field_violation(field, message, "UNSUPPORTED_QUERY_PARAM")
            .create(),
    ))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "odata_list_tests.rs"]
mod odata_list_tests;
