//! The two catalogs this crate ships without a collector.

use super::{LocalDevStaticUsageTypes, UnconfiguredUsageTypes};
use bss_products_sdk::usage_types::{UsageTypeAnswer, UsageTypeCatalog};

fn ctx() -> toolkit_security::SecurityContext {
    #[allow(clippy::expect_used)]
    toolkit_security::SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(0x7a_12))
        .subject_type("bss-products.test")
        .subject_tenant_id(uuid::Uuid::from_u128(0x7a_11))
        .build()
        .expect("both required builder fields are set above")
}

/// **An unconfigured catalog answers 501, and that is not an empty page.**
///
/// The whole point of the surface: a caller must be able to tell *"this
/// deployment has no usage types"* from *"nobody could be asked"*. Collapse
/// them and an authoring screen renders silence as a clean answer, which is
/// the failure this port was built to avoid.
#[tokio::test]
async fn an_unconfigured_catalog_refuses_the_list_rather_than_answering_empty() {
    let error = UnconfiguredUsageTypes
        .list(&ctx(), None, None, 100, None)
        .await
        .expect_err("an unconfigured catalog has no page to give");
    assert_eq!(
        error.title(),
        "Unimplemented",
        "the unconfigured answer is the 501 class, not a 503 and not a page: {error:?}"
    );
}

/// And its `resolve` stays fail-closed (P-D-184).
#[tokio::test]
async fn an_unconfigured_catalog_resolves_nothing_and_says_unavailable() {
    assert_eq!(
        UnconfiguredUsageTypes
            .resolve(
                &ctx(),
                "gts.cf.core.uc.usage_record.v1~cf.bss.usage_type.x.v1"
            )
            .await,
        UsageTypeAnswer::Unavailable,
        "not Unresolved: nobody was asked, so nobody said no"
    );
}

/// The fabricated set narrows the way a real catalog does, so a screen driven
/// against this mode behaves as it will against a supplier.
#[tokio::test]
async fn the_local_dev_catalog_lists_narrows_and_resolves_its_own_ids() {
    let all = LocalDevStaticUsageTypes
        .list(&ctx(), None, None, 100, None)
        .await
        .expect("the fabricated set is always available");
    assert!(all.items.len() >= 3, "{all:?}");
    assert!(
        all.items
            .iter()
            .all(|b| b.gts_id.starts_with(super::DEV_LOCAL_USAGE_TYPE_PREFIX)),
        "every fabricated id sits under the reserved prefix so it can be swept: {all:?}"
    );
    assert_eq!(all.next_cursor, None, "the whole set fits one page");

    let narrowed = LocalDevStaticUsageTypes
        .list(&ctx(), Some("storage"), None, 100, None)
        .await
        .expect("available");
    assert_eq!(narrowed.items.len(), 1, "{narrowed:?}");

    let one = narrowed.items[0].clone();
    assert_eq!(
        LocalDevStaticUsageTypes.resolve(&ctx(), &one.gts_id).await,
        UsageTypeAnswer::Resolved(one),
        "what the list offers, the gate accepts - the two halves of one port"
    );
    assert_eq!(
        LocalDevStaticUsageTypes
            .resolve(&ctx(), "gts.cf.core.uc.usage_record.v1~cf.x.y.v1")
            .await,
        UsageTypeAnswer::Unresolved,
        "and it says no to an id it does not carry, rather than Unavailable"
    );
}

/// P-D-207 (owner option b): the collector is asked as the caller, and its `PermissionDenied` is
/// `Forbidden` at resolve (the door's 403 `USAGE_TYPE_FORBIDDEN`), never `Unavailable` (a 503).
/// The picker's list refuses the same caller with a 403 as well.
#[tokio::test]
async fn a_collector_denial_is_forbidden_not_unavailable() {
    let catalog = super::CollectorUsageTypes::new(
        std::sync::Arc::new(crate::test_support::DenyingCollector),
        std::time::Duration::from_secs(2),
    );
    assert_eq!(
        catalog
            .resolve(
                &ctx(),
                "gts.cf.core.uc.usage_record.v1~cf.e2e.pricebook.storage.v1"
            )
            .await,
        UsageTypeAnswer::Forbidden
    );
    let error = catalog
        .list(&ctx(), None, None, 50, None)
        .await
        .expect_err("a refused caller has no page");
    assert_eq!(error.title(), "Permission Denied", "{error:?}");
    let error = catalog
        .list(&ctx(), Some("storage"), None, 50, None)
        .await
        .expect_err("a refused caller has no search either");
    assert_eq!(error.title(), "Permission Denied", "{error:?}");
}
