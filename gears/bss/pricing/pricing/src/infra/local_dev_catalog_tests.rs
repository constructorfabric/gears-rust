//! What the fabricated catalog must keep true.
//!
//! The three properties that make the mode auditable are the ones worth pinning:
//! the ids stay in the reserved namespace, the codes stay marked, and the ids
//! stay **stable**, because a plan binds to one and an id that moved on restart
//! would leave every bound plan pointing at nothing while still looking bound.

use super::{DEV_LOCAL_CODE_PREFIX, DEV_LOCAL_SKU_PREFIX, LocalDevStaticProductCatalog};

#[test]
fn every_id_is_in_the_reserved_namespace() {
    // The sweep `WHERE sku_id::text LIKE 'ddddddd%'` is the whole remedy for a
    // stand that ran this mode, so an id outside the prefix is a row nobody
    // will find later.
    for entry in LocalDevStaticProductCatalog::skus() {
        assert!(
            entry.sku_id.to_string().starts_with(DEV_LOCAL_SKU_PREFIX),
            "{} is outside the reserved namespace",
            entry.sku_id
        );
    }
}

#[test]
fn every_code_says_it_is_fabricated() {
    for entry in LocalDevStaticProductCatalog::skus() {
        assert!(
            entry.sku_code.starts_with(DEV_LOCAL_CODE_PREFIX),
            "{} does not admit what it is in the pick-list",
            entry.sku_code
        );
    }
}

/// The ids a bound plan keeps across a restart, pinned as a golden list.
///
/// Two calls compared with each other cannot say this: `skus()` is a `vec!` of
/// literals over a pure function of a constant, so the two lists are equal by
/// construction and stay equal if the derivation moves to a random or
/// clock-derived id. The literals are the independent reading — the same reason
/// `supersession_tests` pins a vector rather than comparing a value with itself.
#[test]
fn ids_are_stable_across_calls() {
    assert_eq!(
        LocalDevStaticProductCatalog::skus()
            .iter()
            .map(|sku| sku.sku_id.to_string())
            .collect::<Vec<_>>(),
        [
            "ddddddd1-0000-4000-8000-000000000001",
            "ddddddd2-0000-4000-8000-000000000002",
            "ddddddd3-0000-4000-8000-000000000003",
            "ddddddd4-0000-4000-8000-000000000004",
            "ddddddd5-0000-4000-8000-000000000005",
            "ddddddd6-0000-4000-8000-000000000006",
            "ddddddd7-0000-4000-8000-000000000007",
            "ddddddd8-0000-4000-8000-000000000008",
            "ddddddd9-0000-4000-8000-000000000009",
            "ddddddda-0000-4000-8000-00000000000a",
        ]
    );
}

#[test]
fn ids_are_distinct() {
    let ids: std::collections::BTreeSet<_> = LocalDevStaticProductCatalog::skus()
        .iter()
        .map(|s| s.sku_id)
        .collect();
    assert_eq!(ids.len(), LocalDevStaticProductCatalog::skus().len());
}

#[test]
fn the_set_carries_all_three_statuses_and_both_unit_cases() {
    // A pick-list that only ever showed publishable, metered entries would not
    // exercise the two distinctions the surface has to render: a SKU with no
    // declared unit is priced per period, and a draft or deprecated one is not
    // something to price against.
    let skus = LocalDevStaticProductCatalog::skus();
    for status in ["published", "draft", "deprecated"] {
        assert!(
            skus.iter().any(|s| s.status == status),
            "no {status} entry to exercise the surface with"
        );
    }
    assert!(skus.iter().any(|s| s.metering_unit.is_some()));
    assert!(skus.iter().any(|s| s.metering_unit.is_none()));
}
