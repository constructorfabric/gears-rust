//! Dimension values edit one at a time and show their use (D-436): `GET /dimension-keys` carries
//! `usage: { prices }` per value from one grouped count, `PATCH /dimension-keys` adds and removes
//! the values of one declared key under If-Match, a used value is not removed (409 naming it), and
//! the PUT judges removals from the same grouped count — each in a fixed number of statements.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::infra::storage::{
    entity::price_book_entry,
    repo::{price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::SkuType;
use plan_support::{Catalog, Fixture, book, entry_support, holding, scope, setup};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

async fn registry(f: &Fixture) -> (Value, String) {
    let (s, b, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    assert_eq!(s, 200, "{b}");
    (b, tag)
}
async fn put(f: &Fixture, items: Value) -> (Value, String) {
    let (_, tag) = registry(f).await;
    let (s, b, tag) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({ "items": items }),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    (b, tag)
}
async fn patch(f: &Fixture, body: Value, tag: Option<&str>) -> (u16, Value, String) {
    f.call("PATCH", "/dimension-keys", body, tag, None).await
}
/// An entry of `book` for a fresh SKU that names `key`, written through the repository.
async fn keyed_entry(f: &Fixture, catalog: &Catalog, book: Uuid, key: &str) -> Uuid {
    let now = time::OffsetDateTime::now_utc();
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: catalog.sku(SkuType::Usage),
            charge_kind: "usage".into(),
            period: None,
            model: "per_unit".into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: Some(key.to_owned()),
            invoice_line_override: None,
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}
/// A price of `entry` on value `dim` in `state`, written through the repository.
async fn valued_price(f: &Fixture, entry: Uuid, n: i32, dim: &str, state: &str) {
    let conn = f.db.conn().unwrap();
    let e = price_book_entry_repo::find(&conn, &scope(f), f.ctx.subject_tenant_id(), entry)
        .await
        .unwrap()
        .unwrap();
    let mut p = entry_support::price(&e);
    p.version_no = n;
    p.state = state.into();
    p.dim_value = Some(dim.to_owned());
    p.effective_from = time::Date::from_calendar_date(2031, time::Month::January, 1).unwrap()
        + time::Duration::days(i64::from(n));
    price_repo::insert(&conn, &scope(f), p).await.unwrap();
}
fn key(key: &str, values: &[(&str, u64)]) -> Value {
    json!({
        "key": key,
        "values": values
            .iter()
            .map(|(v, n)| json!({"value": v, "usage": {"prices": n}}))
            .collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn every_value_shows_the_prices_that_use_it() {
    let (f, catalog) = setup().await;
    // A tenant with no stored registry reads the seed, declared and not yet valued.
    assert_eq!(registry(&f).await.0, json!({"items":[key("region", &[])]}));
    put(
        &f,
        json!([
            {"key":"region","values":["eu","us","apac"]},
            {"key":"tier","values":["gold","silver"]},
        ]),
    )
    .await;
    let b = book(&f, "eur").await;
    let by_region = keyed_entry(&f, &catalog, b, "region").await;
    let also_region = keyed_entry(&f, &catalog, b, "region").await;
    let by_tier = keyed_entry(&f, &catalog, b, "tier").await;
    // Every state counts: a rejected or pending price still carries its value.
    valued_price(&f, by_region, 1, "eu", "approved").await;
    valued_price(&f, by_region, 2, "eu", "rejected").await;
    valued_price(&f, also_region, 1, "eu", "pending").await;
    valued_price(&f, also_region, 2, "us", "draft").await;
    valued_price(&f, by_tier, 1, "gold", "approved").await;
    let (body, _) = registry(&f).await;
    assert_eq!(
        body,
        json!({"items":[
            key("region", &[("eu", 3), ("us", 1), ("apac", 0)]),
            key("tier", &[("gold", 1), ("silver", 0)]),
        ]})
    );
    // The PUT answers the same shape.
    let (answered, _) = put(
        &f,
        json!([
            {"key":"region","values":["eu","us","apac","latam"]},
            {"key":"tier","values":["gold","silver"]},
        ]),
    )
    .await;
    assert_eq!(
        answered["items"][0],
        key("region", &[("eu", 3), ("us", 1), ("apac", 0), ("latam", 0)])
    );
    assert_eq!(answered, registry(&f).await.0);
}

#[tokio::test]
async fn a_patch_edits_the_values_of_one_declared_key() {
    let (f, catalog) = setup().await;
    // The seed key of a tenant with nothing stored is declared: the PATCH values it.
    let (_, seed_tag) = registry(&f).await;
    let (s, b, tag) = patch(
        &f,
        json!({"key":"region","add":["eu","us"]}),
        Some(&seed_tag),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b, json!({"items":[key("region", &[("eu", 0), ("us", 0)])]}));
    assert_eq!(registry(&f).await.1, tag, "the answer carries the new tag");
    put(
        &f,
        json!([
            {"key":"region","values":["eu","us"]},
            {"key":"tier","values":["gold","silver"]},
        ]),
    )
    .await;
    let (_, tag) = registry(&f).await;
    // Add and remove in one call; the other key is untouched; order: kept values, then added.
    let (s, b, tag2) = patch(
        &f,
        json!({"key":"region","add":["apac"," latam "],"remove":["us"]}),
        Some(&tag),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        b,
        json!({"items":[
            key("region", &[("eu", 0), ("apac", 0), ("latam", 0)]),
            key("tier", &[("gold", 0), ("silver", 0)]),
        ]})
    );
    // The tag it read is stale now.
    let (s, b, _) = patch(&f, json!({"key":"tier","add":["bronze"]}), Some(&tag)).await;
    assert_eq!(s, 409, "{b}");
    assert!(b.to_string().contains("STALE_REVISION"), "{b}");
    // Refusals, each changing nothing.
    for (body, status, code) in [
        (
            json!({"key":"size","add":["s","m"]}),
            400,
            "DIM_NOT_DECLARED",
        ),
        (
            json!({"key":"region","add":["eu"]}),
            400,
            "DIM_VALUE_DUPLICATE",
        ),
        (
            json!({"key":"region","add":["x","x"]}),
            400,
            "DIM_VALUE_DUPLICATE",
        ),
        (
            json!({"key":"region","add":["us"],"remove":["us"]}),
            400,
            "DIM_VALUE_DUPLICATE",
        ),
        (
            json!({"key":"region","remove":["mars"]}),
            400,
            "DIM_VALUE_UNKNOWN",
        ),
        (
            json!({"key":"region","add":["Bad Value"]}),
            400,
            "DIM_VALUE_INVALID",
        ),
        (
            json!({"key":"tier","remove":["silver"]}),
            400,
            "DIM_VALUES_FEW",
        ),
        // A PUT-shaped body: the PATCH names `add` and `remove`, never `values` (PT-14).
        (
            json!({"key":"region","values":["eu"]}),
            400,
            "unknown field `values`",
        ),
    ] {
        let (s, b, _) = patch(&f, body.clone(), Some(&tag2)).await;
        assert_eq!(s, status, "{body}: {b}");
        assert!(b.to_string().contains(code), "{body}: {b}");
    }
    assert_eq!(registry(&f).await.1, tag2, "no refusal wrote anything");
    // Removing every value leaves the key declared and unvalued.
    let (s, b, _) = patch(
        &f,
        json!({"key":"tier","remove":["gold","silver"]}),
        Some(&tag2),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"][1], key("tier", &[]));
    // Authorization first: config read alone, and a denied caller without If-Match, are 403.
    let (s, _, _) = f
        .call_as(
            &holding(&f, "config:read"),
            "PATCH",
            "/dimension-keys",
            json!({"key":"region","add":["x1"]}),
            Some(&tag2),
            None,
        )
        .await;
    assert_eq!(s, 403);
    let (s, _, _) = plan_support::request(
        &f.denied,
        &f.ctx,
        "PATCH",
        "/dimension-keys",
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(s, 403);
    let (s, _, _) = patch(&f, json!({"key":"region","add":["x1"]}), None).await;
    assert_eq!(s, 400, "If-Match is required");
    let _ = catalog;
}

// Probed in run 6.4: the PATCH removing a used value is refused, naming it.
#[tokio::test]
async fn a_used_value_is_not_removed_and_the_refusal_names_it() {
    let (f, catalog) = setup().await;
    put(
        &f,
        json!([{"key":"region","values":["eu","us","apac","latam"]}]),
    )
    .await;
    let b = book(&f, "eur").await;
    let e = keyed_entry(&f, &catalog, b, "region").await;
    valued_price(&f, e, 1, "us", "rejected").await;
    let (_, tag) = registry(&f).await;
    let (s, body, _) = patch(
        &f,
        json!({"key":"region","remove":["apac","us"]}),
        Some(&tag),
    )
    .await;
    assert_eq!(s, 409, "{body}");
    let text = body.to_string();
    assert!(text.contains("DIM_VALUE_IN_USE"), "{body}");
    assert!(
        text.contains("region=us"),
        "the refusal names the value: {body}"
    );
    assert!(
        !text.contains("apac"),
        "only the used value is named: {body}"
    );
    assert_eq!(registry(&f).await.1, tag, "nothing was removed");
    // The PUT's refusal names it too.
    let (s, body, _) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":"region","values":["eu","apac","latam"]}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 409, "{body}");
    assert!(body.to_string().contains("DIM_VALUE_IN_USE"), "{body}");
    assert!(body.to_string().contains("region=us"), "{body}");
    // An unused value goes.
    let (s, body, _) = patch(&f, json!({"key":"region","remove":["apac"]}), Some(&tag)).await;
    assert_eq!(s, 200, "{body}");
}

/// The statements on pricing's tables one call makes.
async fn statements(
    f: &Fixture,
    recorder: &toolkit_db::test_support::QueryRecorder,
    method: &str,
    body: Value,
) -> Vec<String> {
    let (_, tag) = registry(f).await;
    recorder.clear();
    let (s, b, _) = f
        .call(method, "/dimension-keys", body, Some(&tag), None)
        .await;
    assert!(s == 200, "{method}: {s} {b}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| q.sql)
        .collect()
}
/// `n` entries naming `region`, each with a price on every value.
async fn seed(f: &Fixture, catalog: &Catalog, book: Uuid, n: usize) {
    for _ in 0..n {
        let e = keyed_entry(f, catalog, book, "region").await;
        valued_price(f, e, 1, "eu", "approved").await;
        valued_price(f, e, 2, "us", "draft").await;
    }
}

#[tokio::test]
async fn the_reads_and_writes_count_the_values_in_the_same_statements_for_10_and_100_entries() {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    put(&f, json!([{"key":"region","values":["eu","us","apac"]}])).await;
    let b = book(&f, "eur").await;
    seed(&f, &catalog, b, 10).await;
    let get_10 = {
        recorder.clear();
        registry(&f).await;
        recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_"))
            })
            .map(|q| q.sql)
            .collect::<Vec<_>>()
    };
    let put_10 = statements(
        &f,
        &recorder,
        "PUT",
        json!({"items":[{"key":"region","values":["eu","us","apac","x1"]}]}),
    )
    .await;
    let patch_10 = statements(
        &f,
        &recorder,
        "PATCH",
        json!({"key":"region","add":["x2"],"remove":["x1"]}),
    )
    .await;
    seed(&f, &catalog, b, 90).await;
    let get_100 = {
        recorder.clear();
        let (body, _) = registry(&f).await;
        assert_eq!(body["items"][0]["values"][0]["usage"]["prices"], 100);
        recorder
            .events()
            .into_iter()
            .filter(|q| {
                q.table
                    .as_deref()
                    .is_some_and(|t| t.starts_with("pricing_"))
            })
            .map(|q| q.sql)
            .collect::<Vec<_>>()
    };
    let put_100 = statements(
        &f,
        &recorder,
        "PUT",
        json!({"items":[{"key":"region","values":["eu","us","apac","x1"]}]}),
    )
    .await;
    let patch_100 = statements(
        &f,
        &recorder,
        "PATCH",
        json!({"key":"region","add":["x3"],"remove":["x1"]}),
    )
    .await;
    for (what, ten, hundred) in [
        ("GET", get_10, get_100),
        ("PUT", put_10, put_100),
        ("PATCH", patch_10, patch_100),
    ] {
        for (i, sql) in hundred.iter().enumerate() {
            eprintln!("{what} statement {i}: {sql}");
        }
        assert_eq!(
            ten, hundred,
            "{what}: the same statements for 10 and 100 entries"
        );
    }
}

/// Second review of W1a, L1 (D-457): only writes of new text are judged. A key and values stored
/// before the caps, longer than 64 characters, that an entry and a price use, never lock the
/// registry: the PUT that keeps them passes, a PATCH names the key and removes an unused long
/// value, and only a key or value the stored registry does not hold is capped.
#[tokio::test]
async fn a_stored_text_over_its_cap_never_locks_the_dimension_registry() {
    use bss_pricing::infra::storage::{entity::dimension_key, repo::dimension_repo};
    let (f, catalog) = setup().await;
    let long = |c: char| c.to_string().repeat(65);
    let (key_k, used_v, unused_u) = (long('k'), long('v'), long('u'));
    dimension_repo::insert(
        &f.db.conn().unwrap(),
        &scope(&f),
        dimension_key::Model {
            tenant_id: f.ctx.subject_tenant_id(),
            key: key_k.clone(),
            values: json!([used_v, unused_u, "eu"]),
            version: 1,
        },
    )
    .await
    .unwrap();
    let b = book(&f, "eur").await;
    let entry = keyed_entry(&f, &catalog, b, &key_k).await;
    valued_price(&f, entry, 1, &used_v, "approved").await;
    // A PUT carries the stored key and its used value (leaving either out is 409), and adds a
    // short value: it passes.
    let (_, tag) = registry(&f).await;
    let (s, body, tag) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":key_k,"values":[used_v, unused_u, "eu", "us"]}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{body}");
    // A value the registry does not hold is still capped.
    let (s, body, _) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":key_k,"values":[used_v, "eu", long('w')]}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 400, "{body}");
    assert!(body.to_string().contains("FIELD_TOO_LONG"), "{body}");
    // A PATCH names the stored key and removes the unused long value.
    let (s, body, tag) = patch(&f, json!({"key":key_k,"remove":[unused_u]}), Some(&tag)).await;
    assert_eq!(s, 200, "{body}");
    // It adds a short value, but never a long new one.
    let (s, body, tag) = patch(&f, json!({"key":key_k,"add":["apac"]}), Some(&tag)).await;
    assert_eq!(s, 200, "{body}");
    let (s, body, _) = patch(&f, json!({"key":key_k,"add":[long('x')]}), Some(&tag)).await;
    assert_eq!(s, 400, "{body}");
    assert!(body.to_string().contains("FIELD_TOO_LONG"), "{body}");
    assert!(body.to_string().contains("\"add\""), "{body}");
}
