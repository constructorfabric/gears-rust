//! The SKU list on `OData` and its counts (P-D-210, P-D-211), through the door.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::api::rest::{ApiState, categories, skus};
use crate::domain::{category::NewCategory, sku::NewSku};
use crate::infra::storage::repo;
use crate::test_support::{body_json, get, problem_code, resolved_usage_types, rest_app_on_db};
use axum::{Router, http::StatusCode};
use bss_products_sdk::models::{Lifecycle, SkuType};
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use serde_json::Value;
use std::sync::Arc;
use time::OffsetDateTime;
use toolkit::api::OpenApiRegistry;
use toolkit_db::secure::{AccessScope, SecureEntityExt};
use uuid::Uuid;

fn doors(s: Arc<ApiState>, o: &dyn OpenApiRegistry) -> Router {
    categories::router(Arc::clone(&s), o).merge(skus::router(s, o))
}

/// Percent-encode a query component (everything but the unreserved characters).
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn uri(path: &str, params: &[(&str, &str)]) -> String {
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect();
    format!("/bss-products/v1/{path}?{}", query.join("&"))
}
fn list(params: &[(&str, &str)]) -> String {
    uri("skus", params)
}
fn counts(params: &[(&str, &str)]) -> String {
    uri("skus/counts", params)
}

/// A door over its own database, with the database's tenant and scope to seed through.
struct Door {
    app: Router,
    state: Arc<ApiState>,
    tenant: Uuid,
    scope: AccessScope,
    /// Holds the database's temporary directory for the door's life.
    _dsn: crate::test_support::TestDsn,
}
impl Door {
    async fn new() -> Self {
        let (db, scope, tenant, dsn) = crate::test_support::test_db().await;
        let (app, state) = rest_app_on_db(tenant, doors, resolved_usage_types(), "test", db).await;
        Self {
            app,
            state,
            tenant,
            scope,
            _dsn: dsn,
        }
    }
    async fn category(&self, code: &str) -> Uuid {
        repo::insert_category(
            &self.state.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            NewCategory {
                code: code.into(),
                name: code.into(),
                is_default: false,
                sort_order: 0,
            },
            OffsetDateTime::now_utc(),
        )
        .await
        .unwrap()
        .id
    }
    /// A SKU in `lifecycle`, written at `at` (its `updated_at`).
    async fn sku(&self, s: Seed) -> Uuid {
        let conn = self.state.db.conn().unwrap();
        let row = repo::insert_sku(
            &conn,
            &self.scope,
            self.tenant,
            NewSku {
                code: s.code.into(),
                name: s.name.into(),
                r#type: s.ty,
                category_id: s.category,
                description: String::new(),
                sellable: true,
                gl_code: s.gl_code.map(Into::into),
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: s.usage_type_ref.map(Into::into),
                unit: s.unit.map(Into::into),
            },
            self.tenant,
            s.at,
        )
        .await
        .unwrap();
        if s.lifecycle != Lifecycle::Draft {
            repo::set_lifecycle(
                &conn,
                &self.scope,
                self.tenant,
                row.id,
                &[Lifecycle::Draft],
                s.lifecycle,
                s.at,
            )
            .await
            .unwrap();
        }
        if s.locked {
            let revision = repo::find_sku(&conn, &self.scope, self.tenant, row.id)
                .await
                .unwrap()
                .unwrap()
                .revision;
            assert!(
                repo::try_lock_sku(
                    &conn,
                    &self.scope,
                    self.tenant,
                    row.id,
                    Uuid::new_v4(),
                    revision
                )
                .await
                .unwrap()
            );
        }
        row.id
    }
    async fn get(&self, uri: &str) -> (StatusCode, Value) {
        let r = get(&self.app, self.tenant, uri).await;
        let status = r.status();
        (status, body_json(r).await)
    }
    /// The codes of a list page that must answer 200.
    async fn codes(&self, uri: &str) -> Vec<String> {
        let (status, body) = self.get(uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        codes(&body)
    }
    async fn refused(&self, uri: &str) -> Value {
        let (status, body) = self.get(uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        body
    }
}
fn codes(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["code"].as_str().unwrap().to_owned())
        .collect()
}
#[derive(Clone, Copy)]
struct Seed {
    code: &'static str,
    name: &'static str,
    ty: SkuType,
    category: Option<Uuid>,
    lifecycle: Lifecycle,
    locked: bool,
    unit: Option<&'static str>,
    usage_type_ref: Option<&'static str>,
    gl_code: Option<&'static str>,
    at: OffsetDateTime,
}
fn seed(code: &'static str) -> Seed {
    Seed {
        code,
        name: code,
        ty: SkuType::Recurring,
        category: None,
        lifecycle: Lifecycle::Draft,
        locked: false,
        unit: None,
        usage_type_ref: None,
        gl_code: None,
        at: OffsetDateTime::now_utc(),
    }
}
fn sorted(mut v: Vec<&str>) -> Vec<String> {
    v.sort_unstable();
    v.into_iter().map(str::to_owned).collect()
}

/// Every filter field narrows the list: the two closed ones by value, the two nullable ones by
/// `eq null` / `ne null` too, and the three text and id fields.
#[tokio::test]
async fn every_filter_field_narrows_the_list() {
    let d = Door::new().await;
    let x = d.category("x").await;
    let y = d.category("y").await;
    let a = d
        .sku(Seed {
            name: "Alpha",
            category: Some(x),
            lifecycle: Lifecycle::Published,
            ..seed("A")
        })
        .await;
    d.sku(Seed {
        name: "Beta",
        ty: SkuType::Usage,
        category: Some(x),
        locked: true,
        ..seed("B")
    })
    .await;
    d.sku(Seed {
        name: "Gamma",
        ty: SkuType::OneTime,
        lifecycle: Lifecycle::Deprecated,
        ..seed("C")
    })
    .await;
    let dd = d
        .sku(Seed {
            name: "Delta",
            ty: SkuType::Bundle,
            category: Some(y),
            lifecycle: Lifecycle::Retired,
            ..seed("D")
        })
        .await;
    d.sku(Seed {
        name: "Epsilon",
        ty: SkuType::Usage,
        ..seed("E")
    })
    .await;
    let (x, a, dd) = (x.to_string(), a.to_string(), dd.to_string());
    for (filter, expected) in [
        ("lifecycle eq 'draft'".to_owned(), vec!["B", "E"]),
        ("lifecycle ne 'draft'".to_owned(), vec!["A", "C", "D"]),
        (
            "lifecycle in ('published', 'deprecated')".to_owned(),
            vec!["A", "C"],
        ),
        ("type eq 'usage'".to_owned(), vec!["B", "E"]),
        ("type in ('bundle', 'one_time')".to_owned(), vec!["C", "D"]),
        (format!("category_id eq {x}"), vec!["A", "B"]),
        ("category_id eq null".to_owned(), vec!["C", "E"]),
        ("category_id ne null".to_owned(), vec!["A", "B", "D"]),
        ("pending_unit_id ne null".to_owned(), vec!["B"]),
        (
            "pending_unit_id eq null".to_owned(),
            vec!["A", "C", "D", "E"],
        ),
        ("code eq 'C'".to_owned(), vec!["C"]),
        ("code in ('A', 'E')".to_owned(), vec!["A", "E"]),
        ("contains(name, 'lph')".to_owned(), vec!["A"]),
        ("startswith(name, 'Ep')".to_owned(), vec!["E"]),
        (format!("id eq {a}"), vec!["A"]),
        (format!("id in ({a}, {dd})"), vec!["A", "D"]),
        (
            "category_id eq null and lifecycle eq 'draft'".to_owned(),
            vec!["E"],
        ),
        (
            "pending_unit_id ne null or type eq 'bundle'".to_owned(),
            vec!["B", "D"],
        ),
        // Text functions on an open text field reach the pager. On `lifecycle` each one is the
        // `in` of the lifecycles it matches (P-D-264).
        ("startswith(type, 'us')".to_owned(), vec!["B", "E"]),
        ("contains(lifecycle, 'pub')".to_owned(), vec!["A"]),
    ] {
        assert_eq!(
            d.codes(&list(&[("$filter", &filter)])).await,
            sorted(expected),
            "{filter}"
        );
    }
    for filter in [
        "type eq 'bad'",
        "lifecycle eq 'bad'",
        "lifecycle in ('draft', 'bad')",
        "type ne 'onetime'",
        "code eq null",
        "lifecycle ne null",
        "category_id in (null)",
        "updated_at gt 2026-01-01T00:00:00Z",
        "sellable eq true",
        "category_id gt null",
        "not (lifecycle eq 'draft')",
        "not contains(lifecycle, 'pub')",
    ] {
        let body = d.refused(&list(&[("$filter", filter)])).await;
        assert_eq!(problem_code(&body), "INVALID_FILTER", "{filter}: {body}");
    }
}

/// Each published order walks the whole list page by page with its cursor, every SKU once, in
/// the order one page shows — ties on `updated_at` broken by `id` — and back again.
#[tokio::test]
async fn every_order_pages_with_a_stable_cursor_across_ties() {
    let d = Door::new().await;
    let t1 = OffsetDateTime::now_utc() - time::Duration::hours(2);
    let t2 = t1 + time::Duration::minutes(5);
    let mut rows: Vec<(String, String, OffsetDateTime, Uuid)> = Vec::new();
    for (code, name, at) in [
        ("K1", "zeta", t1),
        ("K7", "alpha", t2),
        ("K3", "mu", t1),
        ("K5", "beta", t1),
        ("K2", "omega", t2),
        ("K6", "kappa", t1),
        ("K4", "delta", t2),
    ] {
        let id = d
            .sku(Seed {
                name,
                at,
                ..seed(code)
            })
            .await;
        rows.push((code.to_owned(), name.to_owned(), at, id));
    }
    // The oracle: the primary key, then an optional ascending code (the one order with a
    // secondary key, RT-16), then the id.
    let expected_then = |key: &str, desc: bool, then_code: bool| -> Vec<String> {
        let mut r = rows.clone();
        r.sort_by(|a, b| {
            let primary = match key {
                "code" => a.0.cmp(&b.0),
                "name" => a.1.cmp(&b.1),
                _ => a.2.cmp(&b.2),
            };
            let primary = if desc { primary.reverse() } else { primary };
            let secondary = if then_code {
                a.0.cmp(&b.0)
            } else {
                std::cmp::Ordering::Equal
            };
            primary.then(secondary).then(a.3.cmp(&b.3))
        });
        r.into_iter().map(|r| r.0).collect()
    };
    let expected = |key: &str, desc: bool| expected_then(key, desc, false);
    for (orderby, key, desc) in [
        ("code", "code", false),
        ("code desc", "code", true),
        ("name asc", "name", false),
        ("name desc", "name", true),
        ("updated_at", "updated_at", false),
        ("updated_at desc", "updated_at", true),
        ("updated_at desc, code", "updated_at", true),
    ] {
        let one_page = d.codes(&list(&[("$orderby", orderby)])).await;
        assert_eq!(
            one_page,
            expected_then(key, desc, orderby == "updated_at desc, code"),
            "{orderby}"
        );
        let (status, mut page) = d.get(&list(&[("$orderby", orderby), ("limit", "2")])).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let mut walked = codes(&page);
        let mut pages = vec![page.clone()];
        while let Some(next) = page["page_info"]["next_cursor"].as_str().map(str::to_owned) {
            let (status, body) = d.get(&list(&[("cursor", &next), ("limit", "2")])).await;
            assert_eq!(status, StatusCode::OK, "{orderby}: {body}");
            walked.extend(codes(&body));
            pages.push(body.clone());
            page = body;
        }
        assert_eq!(
            walked, one_page,
            "{orderby}: the walk is the one page, once each"
        );
        assert_eq!(pages.len(), 4, "{orderby}");
        // Back from the last page: its previous page is the one before it.
        let prev = pages[3]["page_info"]["prev_cursor"].as_str().unwrap();
        let back = d.codes(&list(&[("cursor", prev), ("limit", "2")])).await;
        assert_eq!(back, codes(&pages[2]), "{orderby}: back one page");
    }
    // Every order ends with the id: the default order is the code.
    assert_eq!(d.codes(&list(&[])).await, expected("code", false));
}

/// What the list refuses: projections and totals, an order key that is not published, the old
/// parameters and any other key, and a cursor replayed with another narrowing.
#[tokio::test]
async fn the_list_refuses_what_it_does_not_publish() {
    let d = Door::new().await;
    for code in ["A", "B", "C"] {
        d.sku(seed(code)).await;
    }
    for (params, field) in [
        (vec![("$select", "code")], "$select"),
        (vec![("$count", "true")], "$count"),
        (vec![("$skip", "1")], "$skip"),
        (vec![("category", "x")], "category"),
        (vec![("after", "A")], "after"),
        (vec![("type", "usage")], "type"),
        (vec![("lifecycle", "draft")], "lifecycle"),
        (vec![("bogus", "1"), ("q", "a")], "bogus"),
        (vec![("q", "a"), ("q", "b")], "q"),
    ] {
        let body = d.refused(&list(&params)).await;
        assert!(body.to_string().contains(field), "{params:?}: {body}");
    }
    for orderby in [
        "lifecycle",
        "type",
        "category_id",
        "pending_unit_id desc",
        "sellable",
    ] {
        let body = d.refused(&list(&[("$orderby", orderby)])).await;
        assert_eq!(
            problem_code(&body),
            "INVALID_ORDERBY_FIELD",
            "{orderby}: {body}"
        );
    }
    let body = d.refused(&list(&[("limit", "0")])).await;
    assert_eq!(problem_code(&body), "INVALID_LIMIT", "{body}");
    // A cursor answers the narrowing it was issued for, and only that one.
    let (status, page) = d
        .get(&list(&[
            ("q", "a"),
            ("$filter", "code ne 'Z'"),
            ("limit", "1"),
        ]))
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let (status, page) = d.get(&list(&[("limit", "1")])).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let cursor = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        d.codes(&list(&[("cursor", &cursor), ("limit", "1")])).await,
        ["B"]
    );
    for params in [
        vec![("cursor", cursor.as_str()), ("q", "b")],
        vec![("cursor", cursor.as_str()), ("$filter", "code ne 'A'")],
    ] {
        let body = d.refused(&list(&params)).await;
        assert_eq!(problem_code(&body), "FILTER_MISMATCH", "{params:?}: {body}");
    }
    let body = d
        .refused(&list(&[("cursor", &cursor), ("$orderby", "name")]))
        .await;
    assert!(body.to_string().contains("cursor"), "{body}");
}

/// `q` is a case-insensitive substring of five columns, taken literally; `SQLite` folds ASCII
/// only (Postgres folds Unicode: `tests/postgres_sku_list.rs`).
#[tokio::test]
async fn q_is_a_case_insensitive_literal_substring_of_five_columns() {
    let d = Door::new().await;
    d.sku(Seed {
        name: "Storage",
        unit: Some("GiB"),
        usage_type_ref: Some("gts.cf.usage.disk~"),
        gl_code: Some("GL-4000"),
        ..seed("STOR-1")
    })
    .await;
    d.sku(Seed {
        name: "Compute 50%_off",
        ..seed("COMP")
    })
    .await;
    d.sku(Seed {
        name: r"Back\slash",
        ..seed("BACK")
    })
    .await;
    d.sku(Seed {
        name: "Compute 50xyoff",
        ..seed("DECOY")
    })
    .await;
    d.sku(Seed {
        name: "\u{411}\u{435}\u{442}\u{430}", // Beta, in Cyrillic
        ..seed("CYR")
    })
    .await;
    for (q, expected) in [
        ("stor", vec!["STOR-1"]),
        ("STORAGE", vec!["STOR-1"]),
        ("gib", vec!["STOR-1"]),
        ("USAGE.DISK", vec!["STOR-1"]),
        ("gl-4000", vec!["STOR-1"]),
        ("%", vec!["COMP"]),
        ("_", vec!["COMP"]),
        ("50%_", vec!["COMP"]),
        (r"\", vec!["BACK"]),
        ("compute 50", vec!["COMP", "DECOY"]),
        ("\u{411}\u{435}\u{442}\u{430}", vec!["CYR"]),
        ("nothing", vec![]),
    ] {
        assert_eq!(d.codes(&list(&[("q", q)])).await, sorted(expected), "q={q}");
    }
    // ASCII-only folding on SQLite, pinned: another case of a Cyrillic letter does not match.
    assert!(
        d.codes(&list(&[("q", "\u{431}\u{435}\u{442}\u{430}")]))
            .await
            .is_empty()
    );
    // An empty `q` is no search.
    assert_eq!(d.codes(&list(&[("q", "")])).await.len(), 5);
    // `q` and `$filter` intersect.
    assert_eq!(
        d.codes(&list(&[("q", "compute"), ("$filter", "code eq 'DECOY'")]))
            .await,
        ["DECOY"]
    );
}

/// `$top` (alias `limit`) defaults to 50 and is clamped at 200.
#[tokio::test]
async fn the_page_defaults_to_50_and_is_clamped_at_200() {
    let d = Door::new().await;
    for i in 0..205 {
        d.sku(seed(Box::leak(format!("S{i:03}").into_boxed_str())))
            .await;
    }
    for (params, size) in [
        (vec![], 50),
        (vec![("limit", "7")], 7),
        (vec![("$top", "7")], 7),
        (vec![("limit", "500")], 200),
        (vec![("$top", "201")], 200),
        (vec![("limit", "200")], 200),
    ] {
        let (status, page) = d.get(&list(&params)).await;
        assert_eq!(status, StatusCode::OK, "{params:?}: {page}");
        assert_eq!(page["items"].as_array().unwrap().len(), size, "{params:?}");
        assert_eq!(page["page_info"]["limit"], size, "{params:?}");
        assert!(page["page_info"]["next_cursor"].is_string(), "{params:?}");
    }
}

/// The counts: every SKU, each lifecycle and those in review, narrowed like the list; a
/// top-level `lifecycle` term is dropped, one under `or` or `not` refused; nothing that pages.
#[tokio::test]
async fn the_counts_follow_the_list_without_its_lifecycle_terms() {
    let d = Door::new().await;
    let x = d.category("x").await;
    for s in [
        Seed {
            category: Some(x),
            locked: true,
            ..seed("D1")
        },
        seed("D2"),
        Seed {
            category: Some(x),
            lifecycle: Lifecycle::Published,
            ..seed("P1")
        },
        Seed {
            name: "storage",
            lifecycle: Lifecycle::Published,
            locked: true,
            ..seed("P2")
        },
        Seed {
            lifecycle: Lifecycle::Deprecated,
            ..seed("X1")
        },
        Seed {
            category: Some(x),
            lifecycle: Lifecycle::Published,
            ..seed("R1")
        },
        Seed {
            name: "old storage",
            lifecycle: Lifecycle::Retired,
            ..seed("Z1")
        },
    ] {
        d.sku(s).await;
    }
    let count_of = |body: &Value| -> Vec<u64> {
        [
            "all",
            "draft",
            "published",
            "deprecated",
            "retired",
            "in_review",
        ]
        .iter()
        .map(|k| body[k].as_u64().unwrap_or_else(|| panic!("{k}: {body}")))
        .collect()
    };
    let x = x.to_string();
    for (params, expected) in [
        (vec![], vec![7, 2, 3, 1, 1, 2]),
        (
            vec![("$filter", "lifecycle eq 'draft'".to_owned())],
            vec![7, 2, 3, 1, 1, 2],
        ),
        (
            vec![(
                "$filter",
                format!("lifecycle in ('draft', 'published') and category_id eq {x}"),
            )],
            vec![3, 1, 2, 0, 0, 1],
        ),
        (
            vec![(
                "$filter",
                format!("category_id eq {x} and lifecycle eq 'retired'"),
            )],
            vec![3, 1, 2, 0, 0, 1],
        ),
        (
            vec![("$filter", "category_id eq null".to_owned())],
            vec![4, 1, 1, 1, 1, 1],
        ),
        // P-D-264: a text function on `lifecycle` is a lifecycle term, dropped like the others,
        // whether it matches a lifecycle or none.
        (
            vec![("$filter", "contains(lifecycle, 'draft')".to_owned())],
            vec![7, 2, 3, 1, 1, 2],
        ),
        (
            vec![(
                "$filter",
                format!("startswith(lifecycle, 'd') and category_id eq {x}"),
            )],
            vec![3, 1, 2, 0, 0, 1],
        ),
        (
            vec![(
                "$filter",
                format!("category_id eq {x} and endswith(lifecycle, 'zzz')"),
            )],
            vec![3, 1, 2, 0, 0, 1],
        ),
        (vec![("q", "storage".to_owned())], vec![2, 0, 1, 0, 1, 1]),
        (
            vec![
                ("q", "storage".to_owned()),
                ("$filter", "pending_unit_id ne null".to_owned()),
            ],
            vec![1, 0, 1, 0, 0, 1],
        ),
    ] {
        let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (status, body) = d.get(&counts(&params)).await;
        assert_eq!(status, StatusCode::OK, "{params:?}: {body}");
        assert_eq!(count_of(&body), expected, "{params:?}: {body}");
    }
    // Each lifecycle count is the length of the list narrowed to it.
    let (_, all) = d.get(&counts(&[("q", "storage")])).await;
    for lifecycle in ["draft", "published", "deprecated", "retired"] {
        let listed = d
            .codes(&list(&[
                ("q", "storage"),
                ("$filter", &format!("lifecycle eq '{lifecycle}'")),
            ]))
            .await
            .len();
        assert_eq!(
            all[lifecycle].as_u64().unwrap(),
            listed as u64,
            "{lifecycle}"
        );
    }
    for filter in [
        "lifecycle eq 'draft' or category_id eq null",
        "not (lifecycle eq 'draft')",
        "category_id eq null and (lifecycle eq 'draft' or code eq 'A')",
        "lifecycle eq 'bad'",
        "updated_at gt 2026-01-01T00:00:00Z",
        "contains(lifecycle, 'draft') or category_id eq null",
        "not contains(lifecycle, 'draft')",
    ] {
        let body = d.refused(&counts(&[("$filter", filter)])).await;
        assert_eq!(problem_code(&body), "INVALID_FILTER", "{filter}: {body}");
    }
    for (key, value) in [
        ("$orderby", "code"),
        ("$top", "5"),
        ("limit", "5"),
        ("cursor", "abc"),
        ("$skiptoken", "abc"),
        ("$select", "code"),
        ("bogus", "1"),
    ] {
        let body = d.refused(&counts(&[(key, value)])).await;
        assert_eq!(
            problem_code(&body),
            "UNSUPPORTED_QUERY_PARAM",
            "{key}: {body}"
        );
    }
}

/// The counts recover orphan fences in their transaction as the list does. An expired retire
/// fence returns to its lifecycle with the flag clear; a live one stays that lifecycle with
/// `retire_pending` (P-D-248).
#[tokio::test]
async fn the_counts_and_the_list_expire_orphan_fences_alike() {
    let d = Door::new().await;
    let mut fenced = Vec::new();
    for (code, age) in [
        ("OLD", time::Duration::hours(2)),
        ("NEW", time::Duration::ZERO),
    ] {
        let id = d
            .sku(Seed {
                lifecycle: Lifecycle::Published,
                ..seed(code)
            })
            .await;
        repo::fence_sku(
            &d.state.db.conn().unwrap(),
            &d.scope,
            d.tenant,
            id,
            repo::Fence::Retire,
            Uuid::new_v4(),
            OffsetDateTime::now_utc() - age,
        )
        .await
        .unwrap();
        fenced.push(id);
    }
    let (status, body) = d.get(&counts(&[])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["published"].as_u64(), Some(2), "{body}");
    assert!(body.get("retiring").is_none(), "{body}");
    assert_eq!(
        d.codes(&list(&[("$filter", "retire_pending eq true")]))
            .await,
        ["NEW"]
    );
    let mut published = d
        .codes(&list(&[("$filter", "lifecycle eq 'published'")]))
        .await;
    published.sort();
    assert_eq!(published, ["NEW", "OLD"]);
    let refused = d
        .refused(&list(&[("$filter", "lifecycle eq 'retiring'")]))
        .await;
    assert_eq!(problem_code(&refused), "INVALID_FILTER", "{refused}");
}

// ------------------------------------------------------------------ fixed statements

/// A door over a database whose statements are recorded.
async fn recorded_door(n: usize) -> (Door, toolkit_db::test_support::QueryRecorder) {
    use sea_orm_migration::MigratorTrait;
    let dsn = crate::test_support::TestDsn::new("products-recorded-");
    let (db, recorder) = toolkit_db::test_support::connect_with_recorder(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..toolkit_db::ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        toolkit_db::outbox::outbox_migrations_with_prefix(
            crate::infra::events::OUTBOX_TABLE_PREFIX,
        )
        .unwrap(),
    )
    .await
    .unwrap();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let (app, state) = rest_app_on_db(
        tenant,
        doors,
        resolved_usage_types(),
        "test",
        toolkit_db::DBProvider::new(db),
    )
    .await;
    let d = Door {
        app,
        state,
        tenant,
        scope,
        _dsn: dsn,
    };
    for i in 0..n {
        d.sku(Seed {
            lifecycle: if i % 2 == 0 {
                Lifecycle::Published
            } else {
                Lifecycle::Draft
            },
            ..seed(Box::leak(format!("R{i:03}").into_boxed_str()))
        })
        .await;
    }
    recorder.clear();
    (d, recorder)
}
/// The statements on the gear's tables one read makes, with their bind counts.
async fn statements(
    d: &Door,
    recorder: &toolkit_db::test_support::QueryRecorder,
    uri: &str,
) -> Vec<(String, usize)> {
    recorder.clear();
    let (status, body) = d.get(uri).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("products_"))
        })
        .map(|q| (q.sql, q.param_count))
        .collect()
}

/// The list and the counts make the same statements, with the same binds, for 10 and for 100
/// SKUs: one fence expiry and one read each.
#[tokio::test]
async fn the_list_and_the_counts_read_in_fixed_statements_for_10_and_100_skus() {
    let (ten, ten_rec) = recorded_door(10).await;
    let (hundred, hundred_rec) = recorded_door(100).await;
    for uri in [
        list(&[
            ("$filter", "lifecycle eq 'published'"),
            ("q", "r0"),
            ("limit", "200"),
        ]),
        list(&[("$orderby", "updated_at desc")]),
        counts(&[
            ("$filter", "lifecycle eq 'draft' and category_id eq null"),
            ("q", "r"),
        ]),
    ] {
        let a = statements(&ten, &ten_rec, &uri).await;
        let b = statements(&hundred, &hundred_rec, &uri).await;
        for (i, (sql, binds)) in b.iter().enumerate() {
            eprintln!("{uri} statement {i} ({binds} binds): {sql}");
        }
        assert_eq!(a.len(), 2, "{uri}: one fence expiry and one read: {a:#?}");
        assert_eq!(a, b, "{uri}: the same statements whatever the size");
    }
}

/// The archive mark adds no statement (P-D-263): the list hiding archived rows, the list of
/// archived rows and the counts with their `archived` number read in the same two statements, for
/// 10 and for 100 SKUs.
#[tokio::test]
async fn the_archived_list_and_counts_read_in_the_same_fixed_statements() {
    let (ten, ten_rec) = recorded_door(10).await;
    let (hundred, hundred_rec) = recorded_door(100).await;
    for uri in [
        list(&[("$filter", "archived eq true")]),
        list(&[("$filter", "archived eq false and lifecycle eq 'published'")]),
        counts(&[("$filter", "archived eq true")]),
    ] {
        let a = statements(&ten, &ten_rec, &uri).await;
        let b = statements(&hundred, &hundred_rec, &uri).await;
        assert_eq!(a.len(), 2, "{uri}: one fence expiry and one read: {a:#?}");
        assert_eq!(a, b, "{uri}: the same statements whatever the size");
    }
}

/// The orphan-fence expiry the list and the counts run first is set-based (P-D-189, P-D-211): a
/// read that finds 1 expired fence and a read that finds 5 make the same number of statements —
/// the tenant's expired fences, one UPDATE lifting them all, one INSERT of all their audit rows,
/// then the read — and every fence found is lifted, each with its `sku.fence_expired` row.
#[tokio::test]
async fn the_fence_expiry_reads_in_the_same_statements_for_1_and_5_expired_fences() {
    use crate::infra::storage::entity::audit_log;
    use sea_orm::{ColumnTrait, Condition, EntityTrait};
    use toolkit_db::secure::SecureEntityExt;
    for uri in [list(&[("limit", "200")]), counts(&[])] {
        let mut traces = Vec::new();
        for k in [1_usize, 5] {
            let (d, recorder) = recorded_door(0).await;
            let conn = d.state.db.conn().unwrap();
            let mut ids = Vec::new();
            for i in 0..6 {
                ids.push(
                    d.sku(Seed {
                        lifecycle: Lifecycle::Published,
                        ..seed(Box::leak(format!("F{i}").into_boxed_str()))
                    })
                    .await,
                );
            }
            for id in &ids[..k] {
                repo::fence_sku(
                    &conn,
                    &d.scope,
                    d.tenant,
                    *id,
                    repo::Fence::Retire,
                    Uuid::new_v4(),
                    OffsetDateTime::now_utc() - time::Duration::hours(2),
                )
                .await
                .unwrap();
            }
            let trace = statements(&d, &recorder, &uri).await;
            for (i, (sql, binds)) in trace.iter().enumerate() {
                eprintln!("{uri} k={k} statement {i} ({binds} binds): {sql}");
            }
            let expired = audit_log::Entity::find()
                .secure()
                .scope_with(&d.scope)
                .filter(Condition::all().add(audit_log::Column::Action.eq("sku.fence_expired")))
                .all(&conn)
                .await
                .unwrap();
            assert_eq!(expired.len(), k, "{uri}: one audit row per fence lifted");
            let (_, body) = d.get(&counts(&[])).await;
            assert_eq!(
                (body["published"].as_u64(), body.get("retiring")),
                (Some(6), None),
                "{uri} k={k}: every expired fence is lifted: {body}"
            );
            traces.push(trace);
        }
        assert_eq!(
            traces[0].len(),
            traces[1].len(),
            "{uri}: the same statements for 1 and 5 expired fences: {traces:#?}"
        );
        assert_eq!(
            traces[0].len(),
            5,
            "{uri}: the fold, the fences, one UPDATE, one audit INSERT, the read: {traces:#?}"
        );
    }
}

// ------------------------------------------------------------------ P-D-212: usage filters

use bss_products_sdk::sku_usage::{SkuUsage, SkuUsageSets, SkuUsageV1, UsageScope};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit_security::SecurityContext;

/// What the scripted port does when asked.
#[derive(Clone, Copy, Debug)]
enum Answer {
    Answers,
    /// 403: the caller holds no pricing `price_book_entry:read`.
    Refuses,
    /// 503: pricing cannot answer.
    Fails,
    Panics,
    Hangs,
}
/// Pricing's port as a scripted double: its sets, the usage derived from them, and every call by
/// method, in call order.
struct SetsPort {
    answer: Answer,
    sets: SkuUsageSets,
    /// The SKUs each scope holds (P-D-246); a scope not set holds none.
    scoped: Mutex<Vec<(UsageScope, Vec<Uuid>)>>,
    /// Every scope `sku_ids_in` was asked, in call order.
    asked: Mutex<Vec<UsageScope>>,
    calls: Mutex<Vec<&'static str>>,
    abandoned: AtomicUsize,
}
struct Abandoned<'a>(&'a AtomicUsize);
impl Drop for Abandoned<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl SetsPort {
    fn new(answer: Answer, priced: &[Uuid], in_plan: &[Uuid]) -> Arc<Self> {
        let sorted = |ids: &[Uuid]| {
            let mut v = ids.to_vec();
            v.sort_unstable();
            v
        };
        Arc::new(Self {
            answer,
            sets: SkuUsageSets {
                priced: sorted(priced),
                in_plan: sorted(in_plan),
            },
            scoped: Mutex::default(),
            asked: Mutex::default(),
            calls: Mutex::default(),
            abandoned: AtomicUsize::default(),
        })
    }
    fn calls(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
    /// Declare the SKUs `scope` holds.
    fn scope(&self, scope: UsageScope, ids: &[Uuid]) {
        self.scoped.lock().unwrap().push((scope, ids.to_vec()));
    }
    /// The scopes asked since the last call, in call order.
    fn asked(&self) -> Vec<UsageScope> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
    async fn answer<T>(&self, ok: T) -> Result<T, CanonicalError> {
        match self.answer {
            Answer::Answers => Ok(ok),
            Answer::Refuses => Err(bss_products_sdk::sku_usage::sku_usage_denied()),
            Answer::Fails => Err(bss_products_sdk::sku_usage::sku_usage_unavailable(
                "pricing is down",
            )),
            Answer::Panics => panic!("the SKU usage port broke"),
            Answer::Hangs => {
                let _abandoned = Abandoned(&self.abandoned);
                std::future::pending().await
            }
        }
    }
}
#[async_trait::async_trait]
impl SkuUsageV1 for SetsPort {
    async fn usage(
        &self,
        _ctx: &SecurityContext,
        _tenant: Uuid,
        sku_ids: &[Uuid],
    ) -> Result<Vec<SkuUsage>, CanonicalError> {
        self.calls.lock().unwrap().push("usage");
        let usage = sku_ids
            .iter()
            .map(|&sku_id| SkuUsage {
                sku_id,
                entries: u64::from(self.sets.priced.contains(&sku_id)),
                plans: u64::from(self.sets.in_plan.contains(&sku_id)),
                ..SkuUsage::default()
            })
            .collect();
        self.answer(usage).await
    }
    async fn usage_sets(
        &self,
        _ctx: &SecurityContext,
        _tenant: Uuid,
    ) -> Result<SkuUsageSets, CanonicalError> {
        self.calls.lock().unwrap().push("usage_sets");
        self.answer(self.sets.clone()).await
    }
    async fn sku_ids_in(
        &self,
        _ctx: &SecurityContext,
        _tenant: Uuid,
        scope: UsageScope,
    ) -> Result<Vec<Uuid>, CanonicalError> {
        self.calls.lock().unwrap().push("sku_ids_in");
        self.asked.lock().unwrap().push(scope);
        let ids = self
            .scoped
            .lock()
            .unwrap()
            .iter()
            .find(|(held, _)| *held == scope)
            .map(|(_, ids)| ids.clone())
            .unwrap_or_default();
        self.answer(ids).await
    }
}

/// The picker scopes (P-D-246) and the multi-id read (ask 46).
#[path = "sku_list_picker_tests.rs"]
mod pickers;

/// `priced` and `in_plan` keep or drop pricing's sets, alone, together and beside `q` and
/// `$filter`; the counts narrow alike. A read asks `usage_sets` once when it filters by usage and
/// never otherwise, and the list asks `usage` once for its page.
#[tokio::test]
async fn priced_and_in_plan_keep_or_drop_pricings_sets_with_one_call_of_each_method() {
    let d = Door::new().await;
    let mut id = std::collections::BTreeMap::new();
    for code in ["A", "B", "C", "D", "E"] {
        id.insert(code, d.sku(seed(code)).await);
    }
    // Pricing also names SKUs this tenant does not hold: they change nothing.
    let port = SetsPort::new(
        Answer::Answers,
        &[id["A"], id["B"], id["C"], Uuid::new_v4()],
        &[id["B"], Uuid::new_v4()],
    );
    d.state.hub.register::<dyn SkuUsageV1>(port.clone());
    for (params, expected) in [
        (vec![("priced", "true")], vec!["A", "B", "C"]),
        (vec![("priced", "false")], vec!["D", "E"]),
        (vec![("in_plan", "true")], vec!["B"]),
        (vec![("in_plan", "false")], vec!["A", "C", "D", "E"]),
        (
            vec![("priced", "true"), ("in_plan", "false")],
            vec!["A", "C"],
        ),
        (vec![("priced", "false"), ("in_plan", "true")], vec![]),
        (
            vec![("priced", "true"), ("$filter", "code ne 'A'"), ("q", "c")],
            vec!["C"],
        ),
    ] {
        // An empty page asks no usage for it.
        let calls: &[&str] = if expected.is_empty() {
            &["usage_sets"]
        } else {
            &["usage_sets", "usage"]
        };
        assert_eq!(
            d.codes(&list(&params)).await,
            sorted(expected),
            "{params:?}"
        );
        assert_eq!(port.calls(), calls, "{params:?}: one call of each");
    }
    // No usage filter: only the page's usage.
    assert_eq!(d.codes(&list(&[])).await.len(), 5);
    assert_eq!(port.calls(), ["usage"]);
    // The page's usage agrees with the sets it was filtered by.
    let (_, page) = d.get(&list(&[("priced", "true")])).await;
    for item in page["items"].as_array().unwrap() {
        assert_eq!(item["usage"]["entries"], 1, "{item}");
    }
    port.calls();
    // The counts: one call of `usage_sets` with a usage filter, none without.
    let (status, body) = d.get(&counts(&[("priced", "true")])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["all"].as_u64(), body["draft"].as_u64()),
        (Some(3), Some(3))
    );
    assert_eq!(port.calls(), ["usage_sets"]);
    let (_, body) = d
        .get(&counts(&[("in_plan", "false"), ("priced", "true")]))
        .await;
    assert_eq!(body["all"], 2, "{body}");
    assert_eq!(port.calls(), ["usage_sets"]);
    let (_, body) = d.get(&counts(&[])).await;
    assert_eq!(body["all"], 5, "{body}");
    assert!(port.calls().is_empty());
    // A malformed flag is 400 before pricing is asked.
    for (key, value) in [("priced", "yes"), ("in_plan", "1"), ("priced", "")] {
        let body = d.refused(&list(&[(key, value)])).await;
        assert_eq!(
            problem_code(&body),
            "INVALID_QUERY_PARAMS",
            "{key}={value}: {body}"
        );
        let body = d.refused(&counts(&[(key, value)])).await;
        assert_eq!(
            problem_code(&body),
            "INVALID_QUERY_PARAMS",
            "{key}={value}: {body}"
        );
    }
    assert!(
        port.calls().is_empty(),
        "a refused query asks pricing nothing"
    );
    // The cursor carries the usage filters: replayed with another value, it is 400.
    let (_, page) = d.get(&list(&[("priced", "true"), ("limit", "1")])).await;
    let cursor = page["page_info"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        d.codes(&list(&[
            ("priced", "true"),
            ("limit", "1"),
            ("cursor", &cursor)
        ]))
        .await,
        ["B"]
    );
    for params in [
        vec![("priced", "false"), ("cursor", cursor.as_str())],
        vec![("cursor", cursor.as_str())],
        vec![
            ("priced", "true"),
            ("in_plan", "true"),
            ("cursor", cursor.as_str()),
        ],
    ] {
        let body = d.refused(&list(&params)).await;
        assert_eq!(problem_code(&body), "FILTER_MISMATCH", "{params:?}: {body}");
    }
}

/// A usage filter pricing cannot answer fails the read: 403 `USAGE_FORBIDDEN` when it refuses
/// the caller, 503 `USAGE_UNAVAILABLE` when no port is registered, when it fails and when it
/// breaks — never an unfiltered page. Without a usage filter the list still answers, `usage:
/// null`.
#[tokio::test]
async fn a_usage_filter_pricing_cannot_answer_fails_the_read_and_never_widens_it() {
    for answer in [
        None,
        Some(Answer::Refuses),
        Some(Answer::Fails),
        Some(Answer::Panics),
    ] {
        let d = Door::new().await;
        d.sku(seed("A")).await;
        let port = answer.map(|a| SetsPort::new(a, &[], &[]));
        if let Some(port) = &port {
            d.state.hub.register::<dyn SkuUsageV1>(port.clone());
        }
        let (status, code) = match answer {
            Some(Answer::Refuses) => (StatusCode::FORBIDDEN, "USAGE_FORBIDDEN"),
            _ => (StatusCode::SERVICE_UNAVAILABLE, "USAGE_UNAVAILABLE"),
        };
        for uri in [
            list(&[("priced", "false")]),
            list(&[("in_plan", "false"), ("q", "a")]),
            counts(&[("priced", "false")]),
        ] {
            let (got, body) = d.get(&uri).await;
            assert_eq!(got, status, "{answer:?} {uri}: {body}");
            assert!(body.get("items").is_none(), "never a page: {body}");
            assert!(body.to_string().contains(code), "{answer:?} {uri}: {body}");
        }
        let (got, body) = d.get(&list(&[])).await;
        assert_eq!(got, StatusCode::OK, "{answer:?}: {body}");
        assert_eq!(body["items"][0]["usage"], Value::Null, "{answer:?}: {body}");
        if let Some(port) = port {
            assert_eq!(
                port.calls(),
                ["usage_sets", "usage_sets", "usage_sets", "usage"],
                "{answer:?}: the filter was asked of pricing, once per read"
            );
        }
    }
}

/// A usage filter whose call never returns is 503 once the bound elapses, and the call is
/// aborted, not left running.
#[tokio::test]
async fn a_usage_filter_that_never_answers_is_503_after_its_bound_and_is_aborted() {
    let d = Door::new().await;
    d.sku(seed("A")).await;
    let port = SetsPort::new(Answer::Hangs, &[], &[]);
    d.state.hub.register::<dyn SkuUsageV1>(port.clone());
    let started = std::time::Instant::now();
    let (status, body) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        d.get(&list(&[("priced", "true")])),
    )
    .await
    .expect("the read answers although the port never does");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.to_string().contains("USAGE_UNAVAILABLE"), "{body}");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(2),
        "the 2 s bound"
    );
    for _ in 0..100 {
        if port.abandoned.load(Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("a call past its bound is aborted, not left running");
}

/// A usage filter's id set is ONE bind: the list and the counts make the same statements, with
/// the same binds, for a set of 10 ids and a set of 5000, and each keeps the right SKUs.
#[tokio::test]
async fn a_usage_set_of_any_size_filters_through_one_bind() {
    let mut traces = Vec::new();
    for extra in [10, 5000] {
        let (d, recorder) = recorded_door(20).await;
        let conn = d.state.db.conn().unwrap();
        let page = repo::page_skus(
            &conn,
            &d.scope,
            d.tenant,
            d.state.db.db().backend(),
            &repo::SkuListFilter::default(),
            &toolkit_odata::ODataQuery::default().with_limit(3),
        )
        .await
        .map_err(|_| "page")
        .unwrap();
        let mut priced: Vec<Uuid> = page.items.iter().map(|s| s.id).collect();
        priced.extend((0..extra).map(|_| Uuid::new_v4()));
        let port = SetsPort::new(Answer::Answers, &priced, &[]);
        d.state.hub.register::<dyn SkuUsageV1>(port.clone());
        let listed = statements(
            &d,
            &recorder,
            &list(&[("priced", "true"), ("limit", "200")]),
        )
        .await;
        let (_, body) = d.get(&list(&[("priced", "true")])).await;
        assert_eq!(codes(&body), ["R000", "R001", "R002"], "{extra}");
        let (_, body) = d.get(&list(&[("priced", "false")])).await;
        assert_eq!(body["items"].as_array().unwrap().len(), 17, "{extra}");
        let counted = statements(&d, &recorder, &counts(&[("priced", "false")])).await;
        let (_, body) = d.get(&counts(&[("priced", "false")])).await;
        assert_eq!(body["all"], 17, "{extra}: {body}");
        traces.push((listed, counted));
    }
    for (listed, counted) in &traces {
        assert_eq!(listed.len(), 2, "{listed:#?}");
        assert_eq!(counted.len(), 2, "{counted:#?}");
    }
    assert_eq!(
        traces[0], traces[1],
        "the same statements and binds for 10 and 5000 ids"
    );
}

/// An empty set keeps nothing, and its negation every SKU.
#[tokio::test]
async fn an_empty_usage_set_keeps_nothing_and_its_negation_everything() {
    let d = Door::new().await;
    for code in ["A", "B"] {
        d.sku(seed(code)).await;
    }
    d.state
        .hub
        .register::<dyn SkuUsageV1>(SetsPort::new(Answer::Answers, &[], &[]));
    assert!(d.codes(&list(&[("priced", "true")])).await.is_empty());
    assert_eq!(d.codes(&list(&[("priced", "false")])).await, ["A", "B"]);
    assert!(d.codes(&list(&[("in_plan", "true")])).await.is_empty());
    assert_eq!(d.codes(&list(&[("in_plan", "false")])).await, ["A", "B"]);
}

/// A due `lifecycle_next` is the lifecycle in force. The `CASE` serves a top-level `eq`, `ne` or
/// `in`, a text function as the `in` of the lifecycles it matches (P-D-264), and those joined by
/// `and`. `or` and `not` are 400 on the list and the counts, the counts' text, and the stored
/// column is not compared (P-D-249).
#[tokio::test]
async fn a_due_lifecycle_is_filtered_through_the_case_or_refused() {
    let d = Door::new().await;
    let due = d
        .sku(Seed {
            lifecycle: Lifecycle::Published,
            ..seed("DUE")
        })
        .await;
    d.sku(Seed {
        lifecycle: Lifecycle::Published,
        ..seed("STAY")
    })
    .await;
    let now = crate::infra::storage::stored_now();
    let written = repo::set_lifecycle_next(
        &d.state.db.conn().unwrap(),
        &d.scope,
        d.tenant,
        due,
        &[Lifecycle::Published],
        Lifecycle::Deprecated,
        now.date(),
        now,
    )
    .await
    .unwrap();
    assert!(matches!(written, repo::HeadWrite::Written(_)));
    let stored = crate::infra::storage::entity::sku::Entity::find()
        .secure()
        .scope_with(&d.scope)
        .filter(Condition::all().add(crate::infra::storage::entity::sku::Column::Code.eq("DUE")))
        .one(&d.state.db.conn().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.lifecycle, "published");
    assert_eq!(stored.lifecycle_next.as_deref(), Some("deprecated"));
    assert!(stored.lifecycle_next_from.is_some());

    for filter in [
        "lifecycle eq 'deprecated'",
        "lifecycle ne 'published'",
        "lifecycle in ('deprecated')",
        "lifecycle eq 'deprecated' and code eq 'DUE'",
        "contains(lifecycle,'deprecated')",
        "startswith(lifecycle,'dep')",
        "endswith(lifecycle,'cated')",
        "code eq 'DUE' and startswith(lifecycle,'dep')",
    ] {
        let (status, body) = d.get(&list(&[("$filter", filter)])).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {body}");
        assert_eq!(codes(&body), ["DUE"], "{filter}: {body}");
        assert_eq!(
            body["items"][0]["lifecycle"], "deprecated",
            "{filter}: {body}"
        );
    }
    for filter in [
        "lifecycle eq 'published'",
        "contains(lifecycle,'pub')",
        "endswith(lifecycle,'shed')",
    ] {
        let (status, stay) = d.get(&list(&[("$filter", filter)])).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {stay}");
        assert_eq!(codes(&stay), ["STAY"], "{filter}: {stay}");
    }

    for filter in ["lifecycle eq 'deprecated'", "startswith(lifecycle,'dep')"] {
        let (status, counted) = d.get(&counts(&[("$filter", filter)])).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {counted}");
        assert_eq!(counted["all"], 2, "{filter}: {counted}");
        assert_eq!(counted["published"], 1, "{filter}: {counted}");
        assert_eq!(counted["deprecated"], 1, "{filter}: {counted}");
    }

    for filter in [
        "lifecycle eq 'deprecated' or code eq 'none'",
        "not (lifecycle eq 'published')",
        "contains(lifecycle,'deprecated') or code eq 'none'",
        "not startswith(lifecycle,'dep')",
    ] {
        for uri in [list(&[("$filter", filter)]), counts(&[("$filter", filter)])] {
            let body = d.refused(&uri).await;
            assert_eq!(problem_code(&body), "INVALID_FILTER", "{filter}: {body}");
            let text = body.to_string();
            assert!(
                text.contains("under `or` or `not`")
                    && text.contains("`eq`, `ne`, `in`, `contains`, `startswith` or `endswith`"),
                "{filter}: {text}"
            );
        }
    }
}

/// A `lifecycle` term joined by `and` to another term narrows by the effective lifecycle whichever
/// side it stands on, and two `lifecycle` terms both narrow.
#[tokio::test]
async fn a_lifecycle_term_narrows_on_either_side_of_and() {
    let d = Door::new().await;
    for (code, lifecycle, ty) in [
        ("DEP", Lifecycle::Deprecated, SkuType::Recurring),
        ("OTH", Lifecycle::Published, SkuType::OneTime),
        ("PUB", Lifecycle::Published, SkuType::Recurring),
        ("RET", Lifecycle::Retired, SkuType::Recurring),
    ] {
        d.sku(Seed {
            ty,
            lifecycle,
            ..seed(code)
        })
        .await;
    }
    for (filter, expected) in [
        (
            "lifecycle eq 'published' and type eq 'recurring'",
            vec!["PUB"],
        ),
        (
            "type eq 'recurring' and lifecycle eq 'published'",
            vec!["PUB"],
        ),
        (
            "type eq 'recurring' and lifecycle eq 'published' and code ne 'none'",
            vec!["PUB"],
        ),
        (
            "lifecycle ne 'retired' and type eq 'recurring'",
            vec!["DEP", "PUB"],
        ),
        (
            "lifecycle in ('published', 'deprecated') and type eq 'recurring'",
            vec!["DEP", "PUB"],
        ),
        (
            "lifecycle ne 'retired' and lifecycle ne 'deprecated'",
            vec!["OTH", "PUB"],
        ),
    ] {
        let (status, body) = d.get(&list(&[("$filter", filter)])).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {body}");
        assert_eq!(codes(&body), expected, "{filter}: {body}");
    }
}

/// P-D-264: a text function on `lifecycle` narrows on either side of `and`, beside another
/// `lifecycle` term or text function too. Each case is checked in both orders, and the seeds make
/// every conjunct matter: the filter without any one of them keeps a row more, so a dropped
/// conjunct cannot pass for a kept one.
#[tokio::test]
async fn every_lifecycle_text_conjunct_narrows_in_either_order() {
    let d = Door::new().await;
    for (code, lifecycle, ty) in [
        ("DEP", Lifecycle::Deprecated, SkuType::Recurring),
        ("DRA", Lifecycle::Draft, SkuType::Recurring),
        ("ODE", Lifecycle::Deprecated, SkuType::OneTime),
        ("ODR", Lifecycle::Draft, SkuType::OneTime),
        ("OTH", Lifecycle::Published, SkuType::OneTime),
        ("PUB", Lifecycle::Published, SkuType::Recurring),
        ("RET", Lifecycle::Retired, SkuType::Recurring),
    ] {
        d.sku(Seed {
            ty,
            lifecycle,
            ..seed(code)
        })
        .await;
    }
    let page = |terms: Vec<&str>| {
        let filter = terms.join(" and ");
        let uri = list(&[("$filter", filter.as_str())]);
        let d = &d;
        async move {
            let (status, body) = d.get(&uri).await;
            assert_eq!(status, StatusCode::OK, "{filter}: {body}");
            codes(&body)
        }
    };
    for (terms, expected) in [
        (
            vec!["contains(lifecycle, 'pub')", "type eq 'recurring'"],
            vec!["PUB"],
        ),
        (
            vec!["startswith(lifecycle, 'd')", "endswith(lifecycle, 'ed')"],
            vec!["DEP", "ODE"],
        ),
        (
            vec!["type eq 'one_time'", "endswith(lifecycle, 'ed')"],
            vec!["ODE", "OTH"],
        ),
        (
            vec!["endswith(lifecycle, 'ed')", "lifecycle ne 'retired'"],
            vec!["DEP", "ODE", "OTH", "PUB"],
        ),
        (
            vec![
                "startswith(lifecycle, 'd')",
                "type eq 'one_time'",
                "endswith(lifecycle, 'ed')",
            ],
            vec!["ODE"],
        ),
    ] {
        let reversed: Vec<&str> = terms.iter().rev().copied().collect();
        for order in [terms.clone(), reversed] {
            assert_eq!(page(order.clone()).await, expected, "{order:?}");
        }
        for dropped in 0..terms.len() {
            let mut rest = terms.clone();
            let gone = rest.remove(dropped);
            let wider = page(rest).await;
            assert!(
                wider.len() > expected.len(),
                "without {gone} the seeds keep no row more: {wider:?}"
            );
        }
    }
}

/// P-D-264: `contains`, `startswith` and `endswith` on `lifecycle` keep the SKUs whose lifecycle
/// in force is one of the four tokens the text matches, case-sensitively: the same page as the
/// `in` over those tokens. A text that matches no token keeps nothing: an empty page, not a 400.
#[tokio::test]
async fn a_text_function_on_lifecycle_keeps_the_lifecycles_it_matches() {
    let d = Door::new().await;
    for (code, lifecycle) in [
        ("DEP", Lifecycle::Deprecated),
        ("DRA", Lifecycle::Draft),
        ("PUB", Lifecycle::Published),
        ("RET", Lifecycle::Retired),
    ] {
        d.sku(Seed {
            lifecycle,
            ..seed(code)
        })
        .await;
    }
    for (filter, same_as, expected) in [
        (
            "contains(lifecycle,'pub')",
            "lifecycle in ('published')",
            vec!["PUB"],
        ),
        (
            "startswith(lifecycle,'d')",
            "lifecycle in ('draft','deprecated')",
            vec!["DEP", "DRA"],
        ),
        (
            "endswith(lifecycle,'ed')",
            "lifecycle in ('published','deprecated','retired')",
            vec!["DEP", "PUB", "RET"],
        ),
        (
            "contains(lifecycle,'re')",
            "lifecycle in ('deprecated','retired')",
            vec!["DEP", "RET"],
        ),
        (
            "startswith(lifecycle,'draft')",
            "lifecycle in ('draft')",
            vec!["DRA"],
        ),
        (
            "contains(lifecycle,'')",
            "lifecycle in ('draft','published','deprecated','retired')",
            vec!["DEP", "DRA", "PUB", "RET"],
        ),
        // The function name, as the toolkit reads it, regardless of case.
        (
            "StartsWith(lifecycle,'d')",
            "lifecycle in ('draft','deprecated')",
            vec!["DEP", "DRA"],
        ),
    ] {
        assert_eq!(
            d.codes(&list(&[("$filter", filter)])).await,
            expected,
            "{filter}"
        );
        assert_eq!(
            d.codes(&list(&[("$filter", same_as)])).await,
            expected,
            "{same_as}"
        );
    }
    for filter in [
        "contains(lifecycle,'PUB')",
        "startswith(lifecycle,'Draft')",
        "endswith(lifecycle,'x')",
        "contains(lifecycle,'drafts')",
    ] {
        let (status, body) = d.get(&list(&[("$filter", filter)])).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {body}");
        assert!(codes(&body).is_empty(), "{filter}: {body}");
        let (status, counted) = d.get(&counts(&[("$filter", filter)])).await;
        assert_eq!(status, StatusCode::OK, "{filter}: {counted}");
        assert_eq!(counted["all"], 4, "{filter}: {counted}");
    }
}

/// P-D-259: a page serves each derived SKU's unit from one read of the referenced versions,
/// the same statements for 10 SKUs and for 100.
#[tokio::test]
async fn a_page_reads_derived_units_once_for_10_and_for_100_skus() {
    use crate::domain::derived::{self as rules, NewDerivedType, NewDerivedVersion};
    use crate::infra::storage::repo::derived_usage_type_repo as store;
    use crate::test_support::{products_statements, recorded_test_db, repo_connection};
    use bss_products_sdk::derived::{
        DerivedInput, DerivedUsageDeclaration, Expr, Granularity, GranuleFold, RoundMode,
    };
    let (db, _, _, dsn, recorder) = recorded_test_db().await;
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app_on_db(tenant, doors, resolved_usage_types(), "test", db).await;
    let (repo_db, scope) = repo_connection(&dsn, tenant).await;
    let conn = repo_db.conn().unwrap();
    let now = OffsetDateTime::now_utc();
    let created = store::create_type(
        &conn,
        &scope,
        tenant,
        NewDerivedType {
            code: "meter".into(),
            name: "Meter".into(),
        },
        Uuid::from_u128(7),
        now,
    )
    .await
    .unwrap();
    let declaration = DerivedUsageDeclaration {
        output_unit: "GB".into(),
        granularity: Granularity::Hour,
        inputs: vec![DerivedInput {
            name: "disk".into(),
            usage_type_ref: "usage:storage".into(),
            granule_fold: GranuleFold::Sum,
            max_hold_seconds: None,
            unit: "GB".into(),
        }],
        formula: Expr::Input("disk".into()),
        output_scale: 0,
        output_round: RoundMode::HalfEven,
    };
    store::insert_version(
        &conn,
        &scope,
        tenant,
        NewDerivedVersion {
            type_id: created.id,
            version: 1,
            declaration_json: serde_json::to_value(
                crate::api::rest::dto::ProductsDerivedDeclaration::from(&declaration),
            )
            .unwrap(),
            digest: rules::digest_hex(&declaration),
            created_by: Uuid::from_u128(7),
            created_at: now,
        },
    )
    .await
    .unwrap();
    for i in 0..100 {
        repo::insert_sku(
            &conn,
            &scope,
            tenant,
            NewSku {
                code: format!("U{i:03}"),
                name: format!("U{i:03}"),
                r#type: SkuType::Usage,
                category_id: None,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: Some("products.derived/meter@1".into()),
                unit: Some("GB".into()),
            },
            tenant,
            now,
        )
        .await
        .unwrap();
    }
    let page = |limit: u32| {
        let app = app.clone();
        let recorder = &recorder;
        async move {
            recorder.clear();
            let response = get(
                &app,
                tenant,
                &format!("/bss-products/v1/skus?limit={limit}"),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            let statements = products_statements(recorder);
            let version_reads = statements
                .iter()
                .filter(|sql| sql.contains("products_derived_usage_type_version"))
                .count();
            (body, statements.len(), version_reads)
        }
    };
    let (ten, ten_statements, ten_reads) = page(10).await;
    let (hundred, hundred_statements, hundred_reads) = page(100).await;
    assert_eq!(ten["items"].as_array().unwrap().len(), 10);
    assert_eq!(hundred["items"].as_array().unwrap().len(), 100);
    assert!(
        ten["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["unit"] == "GB")
    );
    assert_eq!(ten_statements, hundred_statements);
    assert_eq!(ten_reads, 1);
    assert_eq!(hundred_reads, 1);
}
