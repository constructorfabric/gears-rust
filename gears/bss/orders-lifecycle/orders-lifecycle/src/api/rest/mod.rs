//! The REST boundary adapter: shared header/caller parsing and the delivered route groups.
//! S2-09 mounts the five draft authoring routes, the early S6-01/S6-04 package the three draft
//! read routes; S2-12 owns the pre-engine throttling ([`throttle`]) and the route census
//! ([`DELIVERED_OPERATIONS`], [`validate_delivered`]).
pub mod capture;
pub mod dto;
pub mod read;
pub mod throttle;
use crate::authz::{self, Action, Caller};
use axum::http::HeaderMap;
use bss_orders_lifecycle_sdk::{
    OrdersError,
    catalog::{OPERATIONS, Reason},
    models::{CallMeta, DelegationProofRef, DraftRevision, IdempotencyKey, OrderVersion},
};
use toolkit_canonical_errors::Problem;
use toolkit_security::SecurityContext;

/// The route census (S2-12): every S1-02 catalog operation is a registered contract, but only
/// these delivered, authorized implementations are mounted. Every other catalog path answers the
/// router's 404 and the local SDK's undelivered methods answer unavailable. The five workflow-only
/// operations (`reflect_approval`, `begin_fulfillment`, `report_spawn_signal`, `acknowledge`,
/// `workflow_cancel`) are service-only seams: they are never reachable from a human authoring
/// path, and when S5-08 mounts them they bind their own gateway zone.
pub const DELIVERED_OPERATIONS: [&str; 8] = [
    "create",
    "patch_order",
    "add_line",
    "patch_line",
    "remove_line",
    "get",
    "list",
    "list_lines",
];

/// The delivered operations that enter the engine, which the gateway caller zone bounds.
pub const DELIVERED_WRITES: [&str; 5] = [
    "create",
    "patch_order",
    "add_line",
    "patch_line",
    "remove_line",
];

/// Startup census of the delivered surface: each delivered identifier is a catalog operation,
/// each delivered write is a caller-facing operation whose actions are never a workflow seam,
/// and no delivered operation needs a service-only action.
///
/// # Errors
/// A delivered identifier missing from the catalog, or a delivered route bound to a
/// service-only action: initialization fails rather than mounting it.
pub fn validate_delivered() -> anyhow::Result<()> {
    for id in DELIVERED_OPERATIONS {
        let op = OPERATIONS.iter().find(|op| op.id == id).ok_or_else(|| {
            anyhow::anyhow!(
                "bss-orders-lifecycle: delivered operation {id} is not a catalog operation"
            )
        })?;
        let classes: &[Option<authz::PatchClass>] = if op.action == "field_class:write|edit" {
            &[
                Some(authz::PatchClass::Commercial),
                Some(authz::PatchClass::Administrative),
            ]
        } else {
            &[None]
        };
        for class in classes {
            let action = authz::operation_action(op, *class)
                .map_err(|e| anyhow::anyhow!("bss-orders-lifecycle: {id}: {e}"))?;
            anyhow::ensure!(
                !action.is_workflow_seam() && action != Action::OrderForceFailUnreconciled,
                "bss-orders-lifecycle: delivered operation {id} would mount a service-only or break-glass action"
            );
        }
    }
    for id in DELIVERED_WRITES {
        anyhow::ensure!(
            DELIVERED_OPERATIONS.contains(&id),
            "bss-orders-lifecycle: write {id} is not a delivered operation"
        );
    }
    Ok(())
}

/// Optional opaque delegation proof reference header, the REST carrier of
/// `CallMeta.delegation_proof_ref` (08 §4.4, D-202). Never logged, echoed or forwarded except as
/// PEP request context.
pub const DELEGATION_PROOF_HEADER: &str = "x-delegation-proof-ref";

/// `OpenAPI` declaration every Orders operation attaches through `OperationBuilder::param`.
#[must_use]
pub fn delegation_proof_param() -> toolkit::api::operation_builder::ParamSpec {
    toolkit::api::operation_builder::ParamSpec::header(DELEGATION_PROOF_HEADER).description(
        "Opaque delegation proof reference (1-512 printable ASCII). Forwarded unvalidated to \
         platform policy; recorded as supplied, never verified by Orders.",
    )
}

/// Extract the supplied proof reference once, before authorization, idempotency or any read.
///
/// # Errors
/// `request-invalid` when the header is repeated, not ASCII, empty, oversized, contains
/// characters outside printable ASCII, or embeds credential material (D-202).
pub fn delegation_proof(headers: &HeaderMap) -> Result<Option<DelegationProofRef>, OrdersError> {
    let invalid = || OrdersError::Refused(Reason::RequestInvalid);
    let mut values = headers.get_all(DELEGATION_PROOF_HEADER).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid());
    }
    let text = value.to_str().map_err(|_| invalid())?;
    let proof = DelegationProofRef::try_from(text.to_owned()).map_err(|_| invalid())?;
    crate::domain::audit::validate_proof_reference(&proof).map_err(|_| invalid())?;
    Ok(Some(proof))
}

/// The shared REST caller adapter: the authenticated context from the `AuthN` middleware plus the
/// supplied proof header, reaching the PEP exactly as the SDK path does.
///
/// # Errors
/// `request-invalid` for a malformed proof header.
pub fn caller(ctx: SecurityContext, headers: &HeaderMap) -> Result<Caller, OrdersError> {
    Ok(Caller::new(ctx, delegation_proof(headers)?))
}

/// The SDK path: the same caller from the typed call metadata.
#[must_use]
pub fn sdk_caller(ctx: &SecurityContext, meta: &CallMeta) -> Caller {
    Caller::new(ctx.clone(), meta.delegation_proof_ref.clone())
}

/// Exact strong commercial-version `ETag`; no weak tags, lists or wildcard.
///
/// # Errors
/// Returns the pre-authorization `expected-version-required` refusal.
pub fn expected_version(value: Option<&str>) -> Result<OrderVersion, OrdersError> {
    let refused = || OrdersError::Refused(Reason::ExpectedVersionRequired);
    let inner = value
        .and_then(|s| s.strip_prefix('"'))
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(refused)?;
    if inner.is_empty() || !inner.bytes().all(|b| b.is_ascii_digit()) {
        return Err(refused());
    }
    let n = inner.parse::<i64>().map_err(|_| refused())?;
    OrderVersion::try_from(n).map_err(|_| refused())
}

/// Parse the required key without normalization.
///
/// # Errors
/// Returns `request-invalid` on malformed/missing input.
pub fn idempotency_key(value: Option<&str>) -> Result<IdempotencyKey, OrdersError> {
    value
        .ok_or(OrdersError::Refused(Reason::RequestInvalid))?
        .to_owned()
        .try_into()
        .map_err(|_| OrdersError::Refused(Reason::RequestInvalid))
}

/// Submit carries only its independent draft revision; actor/version/key are not body fields.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitBody {
    pub expected_draft_revision: DraftRevision,
}

/// Apply the Orders HTTP status exception without reclassifying the canonical error.
#[must_use]
pub fn problem(error: OrdersError) -> Problem {
    let mut result = error.into_problem();
    if result.error_domain.as_deref() == Some("orders-lifecycle.v1")
        && result.error_code.as_deref() == Some("EXPECTED_VERSION_REQUIRED")
    {
        result.status = Some(428);
    }
    result
}
