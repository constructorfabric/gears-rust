//! M2-2: the D-180 interim addend's join on real PostgreSQL through the restricted runtime role
//! under the private count scope (F03 §4.2 predicate 7; `repo::inflight::count_other_orders`).
use super::claims::create_for;
use super::*;
use crate::domain::overlap::ClaimParties;
use crate::infra::storage::scoped::PrivateScope;
use sea_orm::{EntityTrait, IntoActiveModel};

const PLATFORM_SNAPSHOT: &str = r#"{"service_activation_required":false,"acceptance_due_required":false,"scope":"platform_default","resource_tenant_id":null,"policy_id":"00000000-0000-0000-0000-000000000121","revision":1}"#;

/// One current-version (2) line of `order` carrying `key`.
async fn line_on(pg: &Pg, order: u128, line: u128, key: &str) -> anyhow::Result<()> {
    line_at(pg, order, 2, line, key).await
}

/// One line of `order` at `version` carrying `key`.
async fn line_at(pg: &Pg, order: u128, version: i32, line: u128, key: &str) -> anyhow::Result<()> {
    line_at_items(pg, order, version, line, key, None).await
}

async fn line_at_items(
    pg: &Pg,
    order: u128,
    version: i32,
    line: u128,
    key: &str,
    items: Option<&serde_json::Value>,
) -> anyhow::Result<()> {
    let scope = if items.is_some() {
        "NULL".to_owned()
    } else {
        format!("'{key}'")
    };
    let collection = items.map_or_else(|| "NULL".to_owned(), |v| format!("'{v}'::jsonb"));
    let attempt = pinned_fixture_attempt(pg, order, version).await?;
    pg.sql(&format!(
        "INSERT INTO bss_orders__order_line_identity VALUES('{o}','{l}',now()); \
         INSERT INTO bss_orders__order_line(order_id,version,line_id,plan_id,plan_revision_id,selected_items,currency,contract_effective_date,service_activation_date,acceptance_due_date,term_kind,authored_term,billing_cycle,date_policy_switch_state,overlap_scope_key,overlap_item_keys,commercial_attempt_id,pricing_acceptance_id,pricing_request_digest,pricing_terms_digest,order_pin) \
         VALUES('{o}',{version},'{l}','{p}','{r}','[]','USD',CURRENT_DATE,CURRENT_DATE,CURRENT_DATE,'rolling','{{\"kind\":\"rolling\"}}','month','{s}',{scope},{collection},'{attempt}','{l}','{digest}','{digest}','{{}}')",
        o = u(order),
        l = u(line),
        p = u(200),
        r = u(201),
        s = PLATFORM_SNAPSHOT,
        digest = "00".repeat(32),
    ))
    .await
}

/// A live claim of `order` (version 2) on `(payer, resource, key)`.
async fn claim(pg: &Pg, order: u128, payer: u128, resource: u128, key: &str) -> anyhow::Result<()> {
    claim_at(pg, order, payer, resource, key, 2, false).await
}

/// A claim of `order` at `version` on `(payer, resource, key)`, released when `released`.
async fn claim_at(
    pg: &Pg,
    order: u128,
    payer: u128,
    resource: u128,
    key: &str,
    version: i32,
    released: bool,
) -> anyhow::Result<()> {
    let released_at = if released {
        "clock_timestamp()"
    } else {
        "NULL"
    };
    pg.sql(&format!(
        "INSERT INTO bss_orders__inflight_overlap_claim VALUES (gen_random_uuid(),'{}','{}','{key}','{}',{version},clock_timestamp(),{released_at})",
        u(payer),
        u(resource),
        u(order)
    ))
    .await
}

fn parties(payer: u128, resource: u128) -> ClaimParties {
    ClaimParties {
        payer_tenant_id: u(payer),
        resource_tenant_id: u(resource),
    }
}

async fn count(
    db: &Db,
    resource: u128,
    keys: &[&str],
    exclude: Option<u128>,
) -> anyhow::Result<std::collections::BTreeMap<String, u32>> {
    let keys: Vec<String> = keys.iter().map(|k| (*k).to_owned()).collect();
    let scope = PrivateScope::for_payer_count(u(30));
    Ok(repo::inflight::count_other_orders(
        &db.conn()?,
        &scope,
        parties(30, resource),
        &keys,
        exclude.map(u),
    )
    .await?)
}

/// The count unit is lines on each other holder's current version under another resource
/// tenant; a same-resource holder (predicate 9) and the current order are never counted.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_addend_counts_other_resource_tenants_lines_only() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    // Order 1 is the current order (payer 30, resource 10); 2 and 3 are other orders of the
    // payer under resource tenants 11 and 12; 4 is a same-resource sibling.
    for (order, resource) in [(1, 10), (2, 11), (3, 12), (4, 10)] {
        create_for(&db, order, 30, resource).await?;
        entity::order_version::Entity::insert(version(order, 2, Some(1)).into_active_model())
            .exec(&pg.raw)
            .await?;
        pg.sql(&format!("UPDATE bss_orders__order SET current_version=2,version_allocation_high_water=2 WHERE order_id='{}'", u(order))).await?;
    }
    // Two other orders under other resource tenants, one line each on `k`: count 2.
    line_on(&pg, 2, 2_001, "k").await?;
    claim(&pg, 2, 30, 11, "k").await?;
    line_on(&pg, 3, 3_001, "k").await?;
    claim(&pg, 3, 30, 12, "k").await?;
    // One other order with two lines on `k2`: the unit is lines, so count 2.
    line_on(&pg, 2, 2_002, "k2").await?;
    line_on(&pg, 2, 2_003, "k2").await?;
    claim(&pg, 2, 30, 11, "k2").await?;
    // A same-resource order on `same`: the engine's predicate 9, never counted here.
    line_on(&pg, 4, 4_001, "same").await?;
    claim(&pg, 4, 30, 10, "same").await?;
    let n = count(&db, 10, &["k", "k2", "same", "none"], Some(1)).await?;
    assert_eq!(
        n.iter().map(|(k, v)| (k.as_str(), *v)).collect::<Vec<_>>(),
        [("k", 2), ("k2", 2), ("none", 0), ("same", 0)]
    );
    // From resource 12, order 2's two `k2` lines count; with order 2 as the current order they
    // are excluded: 0.
    let n = count(&db, 12, &["k2"], Some(3)).await?;
    assert_eq!(n.get("k2"), Some(&2));
    let n = count(&db, 12, &["k2"], Some(2)).await?;
    assert_eq!(n.get("k2"), Some(&0), "the current order is excluded");
    // Order 5 (resource 13) holds only a released claim on `released`; order 6 (resource 14)
    // a live claim on `stale` at version 3 while its current version stays 2, and a line at
    // version 3 carrying the key (so the join would find it). Neither counts.
    for (order, resource) in [(5, 13), (6, 14)] {
        create_for(&db, order, 30, resource).await?;
        entity::order_version::Entity::insert(version(order, 2, Some(1)).into_active_model())
            .exec(&pg.raw)
            .await?;
        pg.sql(&format!("UPDATE bss_orders__order SET current_version=2,version_allocation_high_water=2 WHERE order_id='{}'", u(order))).await?;
    }
    line_on(&pg, 5, 5_001, "released").await?;
    claim_at(&pg, 5, 30, 13, "released", 2, true).await?;
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let locked = repo::LockedOrder::lock_current(
                tx,
                &toolkit_security::AccessScope::for_resources(vec![u(6)]),
                u(6),
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("order 6 not visible"))?;
            locked.insert_order_version(version(6, 3, Some(2))).await?;
            anyhow::Ok(())
        })
    })
    .await?;
    line_at(&pg, 6, 3, 6_001, "stale").await?;
    claim_at(&pg, 6, 30, 14, "stale", 3, false).await?;
    let n = count(&db, 10, &["released", "stale"], Some(1)).await?;
    assert_eq!(n.get("released"), Some(&0), "a released claim never counts");
    assert_eq!(
        n.get("stale"),
        Some(&0),
        // Guards both the `claim.version <= order.current_version` filter and the join on the
        // holder's current version in `repo/inflight.rs`: a claim above the current version is
        // never counted, and the version-3 line is never joined.
        "a claim above the holder's current version never counts"
    );
    Ok(())
}

/// R10 (D-180, F03 §4.2 predicate 7): an amendment that keeps a tuple retains its claim row at
/// the earlier version (`overlap::plan` never rewrites it). That holder's *current-version*
/// lines still count, as `held_same_tuple` reads retained claims.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_retained_claim_counts_the_holders_current_version_lines() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    // Order 2 (payer 30, resource 11) claimed `kept` at version 2 with one line.
    create_for(&db, 2, 30, 11).await?;
    entity::order_version::Entity::insert(version(2, 2, Some(1)).into_active_model())
        .exec(&pg.raw)
        .await?;
    pg.sql(&format!("UPDATE bss_orders__order SET current_version=2,version_allocation_high_water=2 WHERE order_id='{}'", u(2))).await?;
    line_at(&pg, 2, 2, 2_001, "kept").await?;
    claim_at(&pg, 2, 30, 11, "kept", 2, false).await?;
    // An amendment appended version 3 with two lines on `kept` and retained the version-2 claim.
    db.transaction_ref_mapped(|tx| {
        Box::pin(async move {
            let locked = repo::LockedOrder::lock_current(
                tx,
                &toolkit_security::AccessScope::for_resources(vec![u(2)]),
                u(2),
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("order 2 not visible"))?;
            locked.insert_order_version(version(2, 3, Some(2))).await?;
            anyhow::Ok(())
        })
    })
    .await?;
    pg.sql(&format!("UPDATE bss_orders__order SET current_version=3,version_allocation_high_water=3 WHERE order_id='{}'", u(2))).await?;
    line_at(&pg, 2, 3, 2_101, "kept").await?;
    line_at(&pg, 2, 3, 2_102, "kept").await?;
    // From resource 10: the two current-version lines count, never the version-2 line.
    let n = count(&db, 10, &["kept"], None).await?;
    assert_eq!(n.get("kept"), Some(&2), "{n:?}");
    // The same tuple is held for predicate 9 too: both reads agree on retained claims.
    let held = repo::inflight::held_same_tuple(
        &db.conn()?,
        &PrivateScope::for_payer_count(u(30)),
        parties(30, 11),
        &["kept".to_owned()],
        None,
    )
    .await?;
    assert_eq!(held, ["kept".to_owned()].into_iter().collect());
    Ok(())
}

/// The advisory predicate-9 query excludes the requester and scopes by full tuple, without
/// requiring the claim version to be current (retained claims remain authoritative).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn same_tuple_lookup_excludes_own_order_other_payers_resources_and_released_claims()
-> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    for (id, payer, resource, key) in [
        (1, 30, 10, "own"),
        (2, 30, 10, "held"),
        (3, 30, 11, "other-resource"),
        (4, 31, 10, "other-payer"),
        (5, 30, 10, "released"),
    ] {
        create_for(&db, id, payer, resource).await?;
        claim_at(&pg, id, payer, resource, key, 9, id == 5).await?;
    }
    let keys: Vec<String> = [
        "own",
        "held",
        "other-resource",
        "other-payer",
        "released",
        "missing",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let scope = PrivateScope::for_payer_count(u(30));
    let held =
        repo::inflight::held_same_tuple(&db.conn()?, &scope, parties(30, 10), &keys, Some(u(1)))
            .await?;
    assert_eq!(held, ["held".to_owned()].into_iter().collect());
    let held =
        repo::inflight::held_same_tuple(&db.conn()?, &scope, parties(30, 10), &keys, None).await?;
    assert_eq!(
        held,
        ["held".to_owned(), "own".to_owned()].into_iter().collect()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn collection_membership_counts_each_line_once_per_scope() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    create_for(&db, 2, 30, 11).await?;
    entity::order_version::Entity::insert(version(2, 2, Some(1)).into_active_model())
        .exec(&pg.raw)
        .await?;
    pg.sql(&format!("UPDATE bss_orders__order SET current_version=2,version_allocation_high_water=2 WHERE order_id='{}'",u(2))).await?;
    for line in [2001, 2002] {
        let items:Vec<_>=["a","a","b"].into_iter().enumerate().map(|(index,scope)| {
            serde_json::json!({"profile":"item_keys_v1","selection":{
                "line_id":u(line),"plan_revision_id":u(201),"paid_recurring_sku":u(300),
                "item_id":u(400+index as u128),"dimension_value":null,"sku_version":1
            },"overlap_scope_key":scope,"catalog_subscription_product_key":format!("product-{index}"),
                "classification":{"source":"postgres-test-provider","reference":"collection-fixture","version":"1"}})
        }).collect();
        let items = serde_json::Value::Array(items);
        line_at_items(&pg, 2, 2, line, "", Some(&items)).await?;
    }
    line_on(&pg, 2, 2003, "a").await?;
    claim(&pg, 2, 30, 11, "a").await?;
    claim(&pg, 2, 30, 11, "b").await?;
    assert_eq!(
        count(&db, 10, &["a", "b"], None).await?,
        [("a".into(), 3), ("b".into(), 2)].into_iter().collect()
    );
    assert_eq!(
        count(&db, 11, &["a", "b"], None).await?,
        [("a".into(), 0), ("b".into(), 0)].into_iter().collect()
    );
    assert_eq!(
        count(&db, 10, &["a", "b"], Some(2)).await?,
        [("a".into(), 0), ("b".into(), 0)].into_iter().collect()
    );
    Ok(())
}

/// The named tuple is read as payer-then-resource: a claim whose resource tenant equals the
/// asked payer (and whose payer equals the asked resource) is a different claim and never
/// counts. Distinct payer and resource make a swap visible (a swapped read would see order 2).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn counts_only_the_named_payer_never_the_resource_as_payer() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let (db, _) = pg.role("runtime").await?;
    // Order 1: payer 30 / resource 10 holds `k`. Order 2: payer 10 / resource 30 (the axes
    // reversed) holds `k`. Order 3: payer 30 / resource 11 holds `k`.
    for (order, payer, resource) in [(1, 30, 10), (2, 10, 30), (3, 30, 11)] {
        create_for(&db, order, payer, resource).await?;
        entity::order_version::Entity::insert(version(order, 2, Some(1)).into_active_model())
            .exec(&pg.raw)
            .await?;
        pg.sql(&format!("UPDATE bss_orders__order SET current_version=2,version_allocation_high_water=2 WHERE order_id='{}'", u(order))).await?;
        line_on(&pg, order, order * 1_000 + 1, "k").await?;
        claim(&pg, order, payer, resource, "k").await?;
    }
    let keys = vec!["k".to_owned()];
    // Payer 30, resource 12: orders 1 and 3 count (payer 30, other resources); order 2 is
    // payer 10, so it must not.
    let scope = PrivateScope::for_payer_count(u(30));
    let counted =
        repo::inflight::count_other_orders(&db.conn()?, &scope, parties(30, 12), &keys, None)
            .await?;
    assert_eq!(counted.get("k"), Some(&2));
    // Same-tuple under (payer 30, resource 10) holds order 1's key only.
    let held =
        repo::inflight::held_same_tuple(&db.conn()?, &scope, parties(30, 10), &keys, None).await?;
    assert_eq!(held, ["k".to_owned()].into_iter().collect());
    // Under payer 10's own scope, tuple (10, 30) finds order 2's claim and (30, 10) does not.
    let scope10 = PrivateScope::for_payer_count(u(10));
    let held = repo::inflight::held_same_tuple(&db.conn()?, &scope10, parties(10, 30), &keys, None)
        .await?;
    assert_eq!(held, ["k".to_owned()].into_iter().collect());
    let held = repo::inflight::held_same_tuple(&db.conn()?, &scope10, parties(30, 10), &keys, None)
        .await?;
    assert!(
        held.is_empty(),
        "payer and resource are not interchangeable"
    );
    Ok(())
}
