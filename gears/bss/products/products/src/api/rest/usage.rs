//! Pricing's usage of the SKUs a read returns, through the port pricing fills (P-D-197).
//!
//! Information for the SKUs screen, and never a reason for the read to fail: an absent port, a
//! refusal (the caller holds no pricing `price_book_entry:read`), any error, and a call that does
//! not finish all leave `usage: null`. The port is called once per read, on a task of its own,
//! after the read's own work and outside any transaction of this gear, so a port that opens its
//! own connection or breaks cannot disturb the read; a call still running when the read stops
//! waiting for it (past [`PORT_BOUND`], or because the read itself was dropped) is aborted. The
//! usage never takes part in a fence, a retirement or a type change (P-D-188, P-D-194).
//!
//! The list's `priced` and `in_plan` filters are the exception (P-D-212): they ask the port's
//! `usage_sets` once per request, bounded the same way, and a filter the port cannot answer is
//! never an unfiltered page — a refusal is 403 `USAGE_FORBIDDEN`, an absent port, an error or a
//! call past the bound 503 `USAGE_UNAVAILABLE`. The picker keys (`priced_in`, `not_priced_in`,
//! `not_in_revision`; P-D-246) ask the port's `sku_ids_in` once per key under the same rules.
use super::{ApiState, dto::SkuUsageDto};
use crate::domain::error::DomainError;
use bss_products_sdk::sku_usage::{SkuUsage, SkuUsageSets, SkuUsageV1, UsageScope};
use std::{collections::BTreeMap, time::Duration};
use tokio_util::task::AbortOnDropHandle;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// How long a SKU read waits for the port: past it the call has not finished (P-D-197), and the
/// read answers `usage: null`.
const PORT_BOUND: Duration = Duration::from_secs(2);
/// Pricing's usage of `ids`, by SKU, from one call of the port resolved now (the two gears boot
/// in either order).
pub async fn of(
    state: &ApiState,
    ctx: &SecurityContext,
    ids: &[Uuid],
) -> BTreeMap<Uuid, SkuUsageDto> {
    if ids.is_empty() {
        return BTreeMap::new();
    }
    let Ok(port) = state.hub.get::<dyn SkuUsageV1>() else {
        return BTreeMap::new();
    };
    let (caller, tenant, asked) = (ctx.clone(), ctx.subject_tenant_id(), ids.to_vec());
    // Dropping the handle aborts the call, so a read its client abandons takes the call with it.
    let call = AbortOnDropHandle::new(tokio::spawn(async move {
        port.usage(&caller, tenant, &asked).await
    }));
    let Ok(answer) = tokio::time::timeout(PORT_BOUND, call).await else {
        tracing::warn!(
            bound = ?PORT_BOUND,
            "bss-products: the SKU usage port did not answer in time; usage is null"
        );
        return BTreeMap::new();
    };
    by_sku(ids, answer)
}
/// The answer by SKU: only the ids asked, the first answer of each; nothing when the port did not
/// answer.
fn by_sku(
    ids: &[Uuid],
    answer: Result<Result<Vec<SkuUsage>, CanonicalError>, tokio::task::JoinError>,
) -> BTreeMap<Uuid, SkuUsageDto> {
    let rows = match answer {
        Ok(Ok(rows)) => rows,
        Ok(Err(error)) => {
            unanswered(&error);
            return BTreeMap::new();
        }
        Err(error) => {
            tracing::warn!(%error, "bss-products: the SKU usage port did not finish; usage is null");
            return BTreeMap::new();
        }
    };
    let mut usage = BTreeMap::new();
    for row in rows {
        if ids.contains(&row.sku_id) {
            usage
                .entry(row.sku_id)
                .or_insert_with(|| SkuUsageDto::from(row));
        }
    }
    usage
}
/// A refusal is routine (a caller without pricing read); anything else is pricing failing.
fn unanswered(error: &CanonicalError) {
    if error.status_code() == 403 {
        tracing::debug!(%error, "bss-products: pricing refused the SKU usage; usage is null");
    } else {
        tracing::warn!(%error, "bss-products: pricing could not answer the SKU usage; usage is null");
    }
}
/// The tenant's priced and in-plan SKUs for a list filter (P-D-212), from one call of the port's
/// `usage_sets`, on a task of its own, bounded and aborted on drop like [`of`].
/// # Errors
/// 403 `USAGE_FORBIDDEN` when the port refuses the caller (no pricing `price_book_entry:read`);
/// 503 `USAGE_UNAVAILABLE` when no port is registered, when it fails or breaks, and when it does
/// not answer within [`PORT_BOUND`].
pub async fn sets(state: &ApiState, ctx: &SecurityContext) -> Result<SkuUsageSets, CanonicalError> {
    let Ok(port) = state.hub.get::<dyn SkuUsageV1>() else {
        return Err(unavailable(
            "no SKU usage port is registered; pricing is not mounted",
        ));
    };
    let (caller, tenant) = (ctx.clone(), ctx.subject_tenant_id());
    let call = AbortOnDropHandle::new(tokio::spawn(async move {
        port.usage_sets(&caller, tenant).await
    }));
    sets_of(tokio::time::timeout(PORT_BOUND, call).await)
}
/// The SKUs of one picker scope (P-D-246), from one call of the port's `sku_ids_in`, on a task of
/// its own, bounded and aborted on drop like [`sets`].
/// # Errors
/// As [`sets`]: 403 `USAGE_FORBIDDEN` when the port refuses the caller (a book without pricing
/// `price_book_entry:read`, a revision without it or without `plan:read`); 503
/// `USAGE_UNAVAILABLE` when no port is registered, when it fails or breaks, and when it does not
/// answer within [`PORT_BOUND`].
pub async fn scoped(
    state: &ApiState,
    ctx: &SecurityContext,
    scope: UsageScope,
) -> Result<Vec<Uuid>, CanonicalError> {
    let Ok(port) = state.hub.get::<dyn SkuUsageV1>() else {
        return Err(unavailable(
            "no SKU usage port is registered; pricing is not mounted",
        ));
    };
    let (caller, tenant) = (ctx.clone(), ctx.subject_tenant_id());
    let call = AbortOnDropHandle::new(tokio::spawn(async move {
        port.sku_ids_in(&caller, tenant, scope).await
    }));
    sets_of(tokio::time::timeout(PORT_BOUND, call).await)
}
/// A usage filter's 503.
fn unavailable(detail: &str) -> CanonicalError {
    DomainError::UsageUnavailable(detail.into()).into()
}
/// The sets (or one scope's set), or why a filter on them cannot be answered: never an empty or a
/// missing set.
fn sets_of<T>(
    answer: Result<
        Result<Result<T, CanonicalError>, tokio::task::JoinError>,
        tokio::time::error::Elapsed,
    >,
) -> Result<T, CanonicalError> {
    match answer {
        Ok(Ok(Ok(answered))) => Ok(answered),
        Ok(Ok(Err(error))) => Err(refused_or_failed(&error)),
        Ok(Err(error)) => Err(broken(&error)),
        Err(_) => Err(late()),
    }
}
/// A refusal is the caller's 403; anything else pricing answered is its 503.
fn refused_or_failed(error: &CanonicalError) -> CanonicalError {
    if error.status_code() == 403 {
        tracing::debug!(%error, "bss-products: pricing refused a SKU usage filter");
        DomainError::Forbidden {
            code: "USAGE_FORBIDDEN",
            detail: "pricing refused this caller the SKU usage".into(),
        }
        .into()
    } else {
        tracing::warn!(%error, "bss-products: pricing could not answer a SKU usage filter");
        unavailable("pricing could not answer the SKU usage")
    }
}
/// The call broke (a panic) or was cancelled.
fn broken(error: &tokio::task::JoinError) -> CanonicalError {
    tracing::warn!(%error, "bss-products: a SKU usage filter's call did not finish");
    unavailable("the SKU usage call did not finish")
}
/// The call outlived [`PORT_BOUND`]; dropping its handle aborts it.
fn late() -> CanonicalError {
    tracing::warn!(
        bound = ?PORT_BOUND,
        "bss-products: a SKU usage filter did not answer in time; the filter is refused"
    );
    unavailable("pricing did not answer the SKU usage in time")
}
