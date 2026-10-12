//! @cpt-dod:cpt-cf-bss-products-dod-derived-meter-semantics:p1
//! E1b: Products answers pricing's meter-semantics port, `UsageMeterSemanticsV1`, for its derived usage types
//! (P-D-229 decision 5, P-D-233).
//!
//! - **One dispatcher.** `gear.rs` registers [`ProductsMeterSemantics`] as the `ClientHub`'s one
//!   `dyn UsageMeterSemanticsV1`, as it registers `PricingReferenceRegistry` (decision 6). It is not wired through
//!   `#[toolkit::provides]`. A meter whose `usage_type_id` starts with the reserved `products.derived/` prefix is this
//!   gear's own data, and the dispatcher answers it from the store. Any other meter is a raw meter (E1a), and no
//!   raw-meter provider exists yet: the dispatcher answers exactly what pricing answers when no provider is registered,
//!   [`UnconfiguredMeterSemantics`]. It does not ask the PDP or the store for a raw meter.
//! - **The E1a extension point** (documented, not built). When a raw-meter provider exists (the usage collector's
//!   semantic adapter over the types registry's declarations), it registers in the `ClientHub` under a trait of
//!   `bss-products-sdk`, and this dispatcher calls that trait for every non-derived id, in place of the unconfigured
//!   answer. Pricing's port stays one registration: two providers registered as `dyn UsageMeterSemanticsV1` would
//!   replace each other.
//! - **The answer.** For `MeterRef { usage_type_id: "products.derived/<code>@<n>", version: "<n>" }`: the meter as
//!   asked; `canonical_unit` = the version's `output_unit`; `fold` = `Sum` (the granule outputs add up over a window);
//!   `accrual_policy_version` = `derived-v1:<stored digest hex>`; `source_integrated` = true; `digest` = the STORED
//!   digest (decision 3). Nothing here recomputes the digest from the declaration.
//! - **The order** (P-D-202): the caller's tenant, the PDP (`sku:read`, O-3), the meter's shape, the store.
//! - **The refusals:**
//!   - a nil tenant or subject: 403;
//!   - a denied `sku:read`: 403; an unreachable PDP: 503;
//!   - a `version` that is not canonical, or that disagrees with `@<n>`: 400 [`METER_POLICY_MISMATCH`];
//!   - an unknown code, an unknown version, another tenant's type, or a prefixed id that names no meter: ONE answer,
//!     400 [`METER_VERSION_UNKNOWN`], with the same detail, so the answer tells a caller nothing about other tenants;
//!   - a store failure: 503; a stored row that does not read (a corrupt row): a data-loss 500, detail
//!     `a stored derived meter row does not read`, cause logged, not on the wire. `Internal` hides a custom
//!     description, so data-loss is what keeps status 500 and puts that sentence on the wire.
//! - **The tenant pin.** The port carries no tenant: the provider reads in the CALLER's tenant,
//!   `ctx.subject_tenant_id()`, as the store's key, beside `tenant_only()` of the `sku:read` scope. A SKU
//!   `resource_id` does not filter a derived row. A grant whose scope spans several tenants (a parent reading its
//!   children) therefore never answers another tenant's meter. A constraint with no `owner_tenant_id` is deny-all.
//! - **No trusted subject.** Pricing resolves the semantics as its door's caller (D-503), never as its system actor,
//!   so every caller goes through the PDP; unlike the reference registry, no subject type is trusted here.
use crate::api::rest::{ApiState, derived_usage_types};
use crate::domain::derived::is_derived_ref;
use crate::infra::storage::RepoError;
use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bss_pricing_sdk::{
    Digest,
    meter_semantics::{MeterSemantics, UnconfiguredMeterSemantics, UsageMeterSemanticsV1},
    terms::{Fold, MeterRef},
};
use bss_products_sdk::derived::MeterId;
use std::sync::Arc;
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_security::SecurityContext;

/// A meter whose `version` is not canonical or disagrees with its id's `@<n>`: pricing's code (D-503).
pub const METER_POLICY_MISMATCH: &str = "METER_POLICY_MISMATCH";
/// A meter no version of the caller's tenant answers: one code for an unknown code, version or tenant.
pub const METER_VERSION_UNKNOWN: &str = "METER_VERSION_UNKNOWN";
/// The detail of every [`METER_VERSION_UNKNOWN`]: the same words whatever was not found.
const UNKNOWN_DETAIL: &str =
    "no derived usage type version of the caller's tenant answers this meter";

#[resource_error(gts_id!("cf.bss.products.derived_usage_type.v1~"))]
struct DerivedUsageTypeResource;

/// The `ClientHub`'s `dyn UsageMeterSemanticsV1`: derived meters from this gear's store (E1b), every other meter
/// unconfigured (E1a).
pub struct ProductsMeterSemantics {
    state: Arc<ApiState>,
    enforcer: Arc<PolicyEnforcer>,
}

impl ProductsMeterSemantics {
    /// The dispatcher over the gear's runtime: the same database and PDP the REST doors use.
    #[must_use]
    pub fn new(state: Arc<ApiState>, enforcer: Arc<PolicyEnforcer>) -> Self {
        Self { state, enforcer }
    }

    /// The derived meter's answer, in the caller's tenant (module doc: the order, the refusals, the pin).
    async fn derived(
        &self,
        ctx: &SecurityContext,
        meter: MeterRef,
    ) -> Result<MeterSemantics, CanonicalError> {
        let tenant = ctx.subject_tenant_id();
        if tenant.is_nil() || ctx.subject_id().is_nil() {
            return Err(DerivedUsageTypeResource::permission_denied()
                .with_reason("a meter is read for an identified caller of a tenant")
                .create());
        }
        let scope = derived_usage_types::read_scope(&self.enforcer, ctx)
            .await?
            .tenant_only();
        let Ok(id) = MeterId::parse(&meter.usage_type_id) else {
            return Err(unknown());
        };
        if MeterId::parse_version(&meter.version).ok() != Some(id.version()) {
            return Err(mismatch(&id));
        }
        let conn =
            self.state.db.conn().map_err(|e| {
                store_failure(&RepoError::Db(format!("derived meter connection: {e}")))
            })?;
        let (version, declaration) =
            derived_usage_types::stored_version(&conn, &scope, tenant, &id)
                .await
                .map_err(|e| store_failure(&e))?
                .ok_or_else(unknown)?;
        let digest = decode_digest(&version.digest).ok_or_else(|| {
            store_failure(&RepoError::CorruptRow(format!(
                "derived usage type {} version {} digest is not 64 lowercase hex digits",
                version.type_id, version.version
            )))
        })?;
        Ok(MeterSemantics {
            meter,
            canonical_unit: declaration.output_unit,
            fold: Fold::Sum,
            accrual_policy_version: version.accrual_policy_version(),
            source_integrated: true,
            digest,
        })
    }
}

#[async_trait]
impl UsageMeterSemanticsV1 for ProductsMeterSemantics {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        meter: MeterRef,
    ) -> Result<MeterSemantics, CanonicalError> {
        if !is_derived_ref(&meter.usage_type_id) {
            // E1a: the raw-meter provider is not built (module doc: the extension point).
            return Err(CanonicalError::from(UnconfiguredMeterSemantics));
        }
        self.derived(ctx, meter).await
    }
}

/// 400 [`METER_VERSION_UNKNOWN`] on `meter`, the same answer for every meter the tenant does not hold.
fn unknown() -> CanonicalError {
    DerivedUsageTypeResource::invalid_argument()
        .with_field_violation("meter", UNKNOWN_DETAIL, METER_VERSION_UNKNOWN)
        .create()
}

/// 400 [`METER_POLICY_MISMATCH`] on `meter.version`.
fn mismatch(id: &MeterId) -> CanonicalError {
    DerivedUsageTypeResource::invalid_argument()
        .with_field_violation(
            "meter.version",
            format!(
                "the version of {} is \"{}\", in canonical decimal",
                id.format(),
                id.version()
            ),
            METER_POLICY_MISMATCH,
        )
        .create()
}

/// A store failure is 503 (decision 5). A stored row that does not read stays 500, with a fixed detail:
/// a permanent fault, so pricing forwards it instead of answering a retryable 503.
fn store_failure(error: &RepoError) -> CanonicalError {
    if matches!(error, RepoError::CorruptRow(_)) {
        return derived_usage_types::corrupt_row(error);
    }
    tracing::error!(error = %error, "bss-products: the derived meter store failed");
    CanonicalError::service_unavailable().create()
}

/// The stored digest, 64 lowercase hex digits, as pricing's 32 bytes; `None` for any other text.
fn decode_digest(hex: &str) -> Option<Digest> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (out, pair) in digest.iter_mut().zip(pairs) {
        *out = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(digest)
}

#[cfg(test)]
#[path = "meter_semantics_tests.rs"]
mod meter_semantics_tests;
