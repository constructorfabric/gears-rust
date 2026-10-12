//! Shared REST plumbing; phase 1c adds the SKU registry routes.
use crate::domain::error::DomainError;
use crate::domain::validation::ValidationReport;
use crate::infra::storage::RepoError;

use axum::extract::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value as JsonValue;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::SecurityContext;

pub mod approval_policy;
pub mod approval_units;
pub mod browse;
pub mod categories;
pub mod closed_sets;
pub mod derived_usage_types;
pub mod dto;
pub(crate) mod governance;
mod names;
pub mod preconditions;
pub mod references;
mod replay;
pub mod sku_governance;
pub mod sku_history;
pub mod sku_list;
pub mod skus;
mod usage;
pub mod usage_types;

/// The service prefix every operation's path carries; `register_rest` mounts the operations, each
/// under its full path, and no router nests under it (RS-65).
pub const PREFIX: &str = "/bss-products/v1";
/// Optional replay key header.
pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";
/// Maximum replay key size in bytes.
pub const IDEMPOTENCY_KEY_MAX_BYTES: usize = 255;

/// Dependencies shared by the registry routes.
pub struct ApiState {
    pub db: toolkit_db::DBProvider<toolkit_db::DbError>,
    pub sink: crate::infra::broker::EventSink,
    pub usage_type_catalog: std::sync::Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog>,
    pub usage_type_catalog_source: &'static str,
    pub idempotency_retention_hours: u32,
    /// Public with the rest of the state so a test outside the crate (the approvals inbox's
    /// Postgres walk, P-D-250) can build one; `gear.rs` is the only production writer.
    pub fence_ttl_minutes: u32,
    /// As [`Self::fence_ttl_minutes`].
    pub reference_principals: std::collections::BTreeMap<uuid::Uuid, String>,
    /// Where pricing registers its `SkuUsageV1` port (P-D-197), resolved at each SKU read: the
    /// two gears boot in either order.
    pub hub: std::sync::Arc<toolkit::ClientHub>,
    /// The names of the actors a read shows (P-D-262), through Account Management when the hub
    /// holds it: [`ApiState::names_from`] in production, a fake directory in tests.
    pub actor_names: bss_rest::actor_names::ActorNames,
}

/// The actors that are not people (P-D-262): the nil id of the system's own acts (the orphan-fence
/// expiry, P-D-189) and pricing's system actor. A read names them "System" and never asks Account
/// Management.
pub const SYSTEM_ACTORS: [uuid::Uuid; 2] = [
    crate::infra::storage::repo::SYSTEM_ACTOR,
    bss_products_sdk::PRICING_SYSTEM_ACTOR,
];

impl ApiState {
    /// The actor names of a state over `hub`: Account Management, looked up at each read.
    #[must_use]
    pub fn names_from(
        hub: &std::sync::Arc<toolkit::ClientHub>,
    ) -> bss_rest::actor_names::ActorNames {
        bss_rest::actor_names::ActorNames::from_hub(std::sync::Arc::clone(hub), &SYSTEM_ACTORS)
    }
}

/// The caller of a REST door: 401 `AUTHENTICATION_REQUIRED` without a subject, a tenant or a
/// subject type. Pricing's system actor, in either half (P-D-222), is 403 `SYSTEM_ACTOR_RESERVED`:
/// the reference registry trusts it in-process, and only in-process code acts as it, so no REST
/// caller may, whatever its token asserts. Every door calls this first.
pub fn require_authenticated(
    extension_ctx: Option<Extension<SecurityContext>>,
) -> Result<SecurityContext, CanonicalError> {
    let Some(Extension(ctx)) = extension_ctx else {
        return Err(unauthenticated());
    };
    if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
        return Err(unauthenticated());
    }
    if ctx.subject_type().is_none() {
        return Err(unauthenticated());
    }
    if bss_products_sdk::is_pricing_system_actor(&ctx) {
        tracing::warn!(
            target: "bss_products.authz.deny",
            subject_id = %ctx.subject_id(),
            subject_tenant_id = %ctx.subject_tenant_id(),
            subject_type = ctx.subject_type().unwrap_or_default(),
            reason = SYSTEM_ACTOR_RESERVED,
            "bss-products: a REST caller asserted pricing's system actor"
        );
        return Err(DomainError::Forbidden {
            code: SYSTEM_ACTOR_RESERVED,
            detail: "pricing's system actor acts in-process only".into(),
        }
        .into());
    }
    Ok(ctx)
}

/// The way an archive door moves a SKU's or a category's archive mark (P-D-263). The call site
/// names it, not a `bool` (RS-54).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveMove {
    /// Set the mark: `archived_at` and `archived_by`.
    Archive,
    /// Clear the mark.
    Unarchive,
}

impl ArchiveMove {
    /// The `archived_by` the repository writes: `actor` to set the mark, `None` to clear it.
    pub(crate) const fn archived_by(self, actor: uuid::Uuid) -> Option<uuid::Uuid> {
        match self {
            Self::Archive => Some(actor),
            Self::Unarchive => None,
        }
    }

    /// Whether a row whose mark is `archived` is already where this move leaves it.
    pub(crate) const fn already(self, archived: bool) -> bool {
        match self {
            Self::Archive => archived,
            Self::Unarchive => !archived,
        }
    }
}

/// The refusal of a REST caller that asserts pricing's system actor (P-D-222).
pub const SYSTEM_ACTOR_RESERVED: &str = "SYSTEM_ACTOR_RESERVED";

/// Shared REST foundation helper.
#[must_use]
pub fn unauthenticated() -> CanonicalError {
    CanonicalError::unauthenticated()
        .with_reason("AUTHENTICATION_REQUIRED")
        .create()
}

/// Shared REST foundation helper.
pub fn repo_error_to_canonical(err: &RepoError) -> CanonicalError {
    tracing::error!(error = %err, "bss-products: repository failure");
    CanonicalError::internal(format!("bss-products: {err}")).create()
}

/// Shared REST foundation helper.
pub fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, DomainError> {
    let Some(raw) = headers.get(IDEMPOTENCY_KEY_HEADER) else {
        return Ok(None);
    };
    let value = raw
        .to_str()
        .map_err(|_| refuse_idempotency_key("the header value is not valid UTF-8"))?
        .trim();
    if value.is_empty() {
        return Err(refuse_idempotency_key(
            "the header is present but blank; send a stable, caller-chosen key or omit the \
             header entirely",
        ));
    }
    if value.len() > IDEMPOTENCY_KEY_MAX_BYTES {
        return Err(refuse_idempotency_key(&format!(
            "the header value is {} bytes long and a key is at most {IDEMPOTENCY_KEY_MAX_BYTES}",
            value.len()
        )));
    }
    Ok(Some(value.to_owned()))
}

/// Shared REST foundation helper.
fn refuse_idempotency_key(detail: &str) -> DomainError {
    let mut report = ValidationReport::new();
    report.violate("VALIDATION", IDEMPOTENCY_KEY_HEADER, detail);
    DomainError::Validation(report)
}

/// Shared REST foundation helper.
pub fn replay_response(status: i32, body: JsonValue) -> Response {
    let recorded = u16::try_from(status)
        .ok()
        .and_then(|code| StatusCode::from_u16(code).ok());
    if let Some(code) = recorded {
        return (code, axum::Json(body)).into_response();
    }
    tracing::error!(
        status,
        "bss-products: stored idempotency response_status is not a status code"
    );
    CanonicalError::internal(format!(
        "bss-products: stored idempotency response_status {status} is not a status code"
    ))
    .create()
    .into_response()
}

/// Transaction refusals preserve typed database errors for retry classification.
pub(crate) enum TxError {
    Refused(DomainError),
    Repo(RepoError),
    ApprovalDb(sea_orm::DbErr),
    GenerationMismatch {
        seen: i32,
        current: i32,
    },
    FencedReferences {
        code: &'static str,
        rows: serde_json::Value,
    },
    /// A list query the toolkit's pager refused (a value, an order field, a cursor): a 400.
    OData(toolkit_odata::Error),
    /// A derived usage type disappeared between the door's check and its write.
    DerivedTypeMissing,
}
// The `Sea` arm keeps the driver error. `Db` is the string form, and `DbError` is not `Clone`.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit_db::DbError> for TxError {
    fn from(e: toolkit_db::DbError) -> Self {
        match e {
            toolkit_db::DbError::Sea(source) => Self::Repo(RepoError::Driver {
                context: "transaction".into(),
                source,
            }),
            other => Self::Repo(RepoError::Db(other.to_string())),
        }
    }
}
impl From<bss_approval::ApprovalError> for TxError {
    fn from(e: bss_approval::ApprovalError) -> Self {
        match e {
            bss_approval::ApprovalError::Db(db) => Self::ApprovalDb(db),
            bss_approval::ApprovalError::GenerationMismatch { seen, current } => {
                Self::GenerationMismatch { seen, current }
            }
            other => Self::Refused(other.into()),
        }
    }
}
/// Driver errors reach the toolkit retry classifier unchanged.
pub(crate) fn contention_db_err(e: &TxError) -> Option<&sea_orm::DbErr> {
    match e {
        TxError::Repo(RepoError::Driver { source, .. }) | TxError::ApprovalDb(source) => {
            Some(source)
        }
        TxError::Repo(_)
        | TxError::Refused(_)
        | TxError::GenerationMismatch { .. }
        | TxError::FencedReferences { .. }
        | TxError::OData(_)
        | TxError::DerivedTypeMissing => None,
    }
}
/// Convert only after the retry loop has finished. Contention the retries could not clear is
/// a lost race the client may retry: 409 `CONTENDED`, never a 500.
pub(crate) fn tx_to_canonical(e: TxError) -> CanonicalError {
    tx_to_canonical_coded(e, false)
}
/// [`tx_to_canonical`] for an approval-unit door (submit, change, retire, approve, reject,
/// withdraw), where exhausted contention is `UNIT_CONTENDED` like a lost version race.
pub(crate) fn unit_tx_to_canonical(e: TxError) -> CanonicalError {
    tx_to_canonical_coded(e, true)
}
/// Whether a driver error is contention the toolkit's retry classifier would retry. This
/// gear runs on `PostgreSQL` or `SQLite` only, whose signatures do not overlap, and the
/// conversion sites hold no backend; the classifier is asked for both.
fn exhausted_contention(source: &sea_orm::DbErr) -> bool {
    [sea_orm::DbBackend::Postgres, sea_orm::DbBackend::Sqlite]
        .into_iter()
        .any(|backend| toolkit_db::contention::is_retryable_contention(backend, source))
}
fn tx_to_canonical_coded(e: TxError, unit: bool) -> CanonicalError {
    if contention_db_err(&e).is_some_and(exhausted_contention) {
        tracing::warn!(
            unit,
            "bss-products: transaction contention outlasted its retries"
        );
        return if unit {
            DomainError::from(bss_approval::ApprovalError::Contended).into()
        } else {
            DomainError::Conflict {
                code: "CONTENDED",
                detail: "a concurrent writer held this data through every retry; retry".into(),
            }
            .into()
        };
    }
    match e {
        TxError::Refused(d) => d.into(),
        TxError::FencedReferences { code, rows } => DomainError::Conflict {
            code,
            detail: rows.to_string(),
        }
        .into(),
        TxError::GenerationMismatch { seen, current } => {
            DomainError::from(bss_approval::ApprovalError::GenerationMismatch { seen, current })
                .into()
        }
        TxError::Repo(r) => repo_error_to_canonical(&r),
        TxError::OData(e) => e.into(),
        TxError::ApprovalDb(source) => repo_error_to_canonical(&RepoError::Driver {
            context: "approval".into(),
            source,
        }),
        TxError::DerivedTypeMissing => CanonicalError::internal(
            "bss-products: a derived usage type disappeared and its door did not map the miss",
        )
        .create(),
    }
}
/// Category assignment and retirement must not write-skew on `PostgreSQL`.
pub(crate) fn category_tx_config(state: &ApiState) -> toolkit_db::secure::TxConfig {
    if state.db.db().backend() == sea_orm::DbBackend::Postgres {
        toolkit_db::secure::TxConfig::serializable()
    } else {
        toolkit_db::secure::TxConfig::default()
    }
}

/// Map the PEP denial while keeping PDP outages fail-closed.
pub(crate) fn authz_error_to_canonical(
    err: crate::authz::AuthzError,
    denied: impl FnOnce(String) -> CanonicalError,
) -> CanonicalError {
    match err {
        crate::authz::AuthzError::Denied(reason) => denied(reason),
        crate::authz::AuthzError::Unavailable(detail) => {
            tracing::error!(detail, "bss-products: authorization service unavailable");
            CanonicalError::service_unavailable().create()
        }
    }
}

/// JSON shape errors use the same 400 validation envelope as field rules.
pub(crate) fn json_body<T>(
    body: Result<axum::Json<T>, axum::extract::rejection::JsonRejection>,
) -> Result<T, CanonicalError> {
    body.map(|axum::Json(body)| body).map_err(|error| {
        let mut report = ValidationReport::new();
        report.violate("VALIDATION", "body", error.body_text());
        DomainError::Validation(report).into()
    })
}

#[cfg(test)]
#[path = "rest_tests.rs"]
mod tests;

impl From<crate::infra::events::EventsError> for TxError {
    fn from(error: crate::infra::events::EventsError) -> Self {
        bss_approval::ApprovalError::from(error).into()
    }
}
