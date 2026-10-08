//! The derived usage type doors (P-D-231), through the real router over a migrated database.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::router;
use crate::domain::recognized::UsageTypeAnswer;
use crate::test_support::{
    StubUsageTypes, UnreachableUsageTypes, body_json, drop_table, get, id_matches, problem_code,
    raw_i64, raw_string_opt, repo_connection, rest_app, rest_app_with_catalog, tenant_user,
};
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

const BASE: &str = "/bss-products/v1/derived-usage-types";
const RAM_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.ram_mb.v1";
const CPU_REF: &str = "gts.cf.core.uc.usage_record.v1~cf.test.usage.cpu_mhz.v1";
/// The cloudlet's digest, taken outside the code (`domain::derived_tests::CLOUDLET_DIGEST`).
const CLOUDLET_DIGEST: &str = "9af0a695c790874e75592010b587407a3d90878dab200005bc8697626a11c421";

fn share(name: &str, divisor: &str) -> Value {
    json!({"op":"ceil","arg":{"op":"div_const","arg":{"op":"input","name":name},"divisor":divisor}})
}
/// The cloudlet of decision 1 on the wire.
fn cloudlet() -> Value {
    json!({
        "output_unit": "cloudlet\u{b7}hour",
        "granularity": "hour",
        "inputs": [
            {"name":"ram_mb","usage_type_ref":RAM_REF,"granule_fold":"peak","unit":"MB"},
            {"name":"cpu_mhz","usage_type_ref":CPU_REF,"granule_fold":"peak","unit":"MHz"}
        ],
        "formula": {"op":"max","args":[share("ram_mb","128"), share("cpu_mhz","400")]},
        "output_scale": 0,
        "output_round": "half_even"
    })
}
fn create(code: &str, declaration: Value) -> Value {
    let mut body = json!({"code": code, "name": format!("{code} name")});
    body["declaration"] = declaration;
    body
}

/// One request with the caller's tenant and the given headers.
async fn send(
    app: &axum::Router,
    tenant: Uuid,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .extension(tenant_user(tenant))
        .header("Content-Type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let body = body.map_or_else(axum::body::Body::empty, |b| {
        axum::body::Body::from(b.to_string())
    });
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The `declaration` violation's detail, as the 400 carries it.
fn declaration_detail(body: &Value) -> String {
    crate::test_support::violation_for(body, "declaration").unwrap_or_default()
}

/// The stored row of a version, as the database holds it: the declaration and the digest.
async fn stored_version(dsn: &str, type_id: Uuid, version: u32) -> (String, String) {
    let row = |column: &str| {
        format!(
            "SELECT {column} AS v FROM products_derived_usage_type_version WHERE {} AND version = \
             {version}",
            id_matches("type_id", type_id)
        )
    };
    (
        raw_string_opt(dsn, &row("CAST(declaration_json AS TEXT)"))
            .await
            .unwrap(),
        raw_string_opt(dsn, &row("digest")).await.unwrap(),
    )
}

/// Create → version 1 with its id; a new version → version 2, while version 1, as the door serves
/// it and as the database holds it, stays byte for byte what it was.
#[tokio::test]
async fn create_gives_version_1_and_a_new_version_leaves_version_1_as_it_was() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (status, one) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{one}");
    let id: Uuid = serde_json::from_value(one["id"].clone()).unwrap();
    assert_eq!(one["code"], "cloudlets");
    assert_eq!(one["version"], 1);
    assert_eq!(one["digest"], CLOUDLET_DIGEST);
    assert_eq!(one["declaration"], cloudlet(), "the declaration as written");
    let v1_url = format!("{BASE}/cloudlets/versions/1");
    let (status, v1_read) = send(&app, tenant, Method::GET, &v1_url, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{v1_read}");
    assert_eq!(v1_read, one, "the read is the create's answer");
    let v1_row = stored_version(&dsn, id, 1).await;
    let mut next = cloudlet();
    next["formula"]["args"][1] = share("cpu_mhz", "500");
    let (status, two) = send(
        &app,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": next})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{two}");
    assert_eq!(two["id"], one["id"]);
    assert_eq!(two["version"], 2);
    assert_ne!(two["digest"], one["digest"]);
    assert_eq!(
        two["meter_ref"]["usage_type_id"],
        "products.derived/cloudlets@2"
    );
    let (_, v1_again) = send(&app, tenant, Method::GET, &v1_url, None, &[]).await;
    assert_eq!(v1_again, v1_read, "version 1 is served as it was");
    assert_eq!(
        stored_version(&dsn, id, 1).await,
        v1_row,
        "and stored as it was"
    );
    let (status, t) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/cloudlets"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["id"], one["id"]);
    let versions: Vec<_> = t["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["version"].clone(), v["digest"].clone()))
        .collect();
    assert_eq!(
        versions,
        [
            (json!(1), one["digest"].clone()),
            (json!(2), two["digest"].clone())
        ]
    );
}

/// What a pricing author copies (decision 5, O-2): the meter reference, the canonical unit and the
/// accrual policy version, carrying the digest.
#[tokio::test]
async fn the_version_read_carries_the_meter_ref_unit_and_accrual() {
    let tenant = Uuid::new_v4();
    let (app, _dsn) = rest_app(tenant, router).await;
    send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    let (status, v) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/cloudlets/versions/1"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        v["meter_ref"],
        json!({"usage_type_id": "products.derived/cloudlets@1", "version": "1"})
    );
    assert_eq!(v["canonical_unit"], "cloudlet\u{b7}hour");
    assert_eq!(
        v["accrual_policy_version"],
        format!("derived-v1:{CLOUDLET_DIGEST}")
    );
    for missing in ["2", "01", "0", "x"] {
        let (status, b) = send(
            &app,
            tenant,
            Method::GET,
            &format!("{BASE}/cloudlets/versions/{missing}"),
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{missing}: {b}");
    }
    let (status, _) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/other"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Decision 3: a read answers the STORED digest. A version written with a digest the declaration
/// would not hash to is served with that digest, never a recomputed one.
#[tokio::test]
async fn a_read_answers_the_stored_digest() {
    use crate::domain::derived::{NewDerivedType, NewDerivedVersion};
    use crate::infra::storage::repo::derived_usage_type_repo as store;
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    let now = time::OffsetDateTime::now_utc();
    let t = store::create_type(
        &conn,
        &scope,
        tenant,
        NewDerivedType {
            code: "pinned".into(),
            name: "Pinned".into(),
        },
        Uuid::from_u128(7),
        now,
    )
    .await
    .unwrap();
    let stored = "f".repeat(64);
    store::insert_version(
        &conn,
        &scope,
        tenant,
        NewDerivedVersion {
            type_id: t.id,
            version: 1,
            declaration_json: cloudlet(),
            digest: stored.clone(),
            created_by: Uuid::from_u128(7),
            created_at: now,
        },
    )
    .await
    .unwrap();
    let (status, v) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/pinned/versions/1"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["digest"], stored);
    assert_eq!(v["accrual_policy_version"], format!("derived-v1:{stored}"));
    let (_, t) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/pinned"),
        None,
        &[],
    )
    .await;
    assert_eq!(t["versions"][0]["digest"], stored);
}

/// A second type with the tenant's code is 409 `DERIVED_CODE_TAKEN`, naming the derived resource,
/// and writes nothing.
#[tokio::test]
async fn a_duplicate_code_is_derived_code_taken() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (status, _) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, b) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{b}");
    assert_eq!(problem_code(&b), "DERIVED_CODE_TAKEN");
    assert_eq!(
        b["context"]["resource_type"],
        crate::authz::labels::DERIVED_USAGE_TYPE,
        "{b}"
    );
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type"
        )
        .await,
        1
    );
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type_version"
        )
        .await,
        1
    );
}

/// The declaration with one change made by `edit`.
fn edited(edit: impl FnOnce(&mut Value)) -> Value {
    let mut d = cloudlet();
    edit(&mut d);
    d
}

/// Every declaration refusal, the SDK's and the shape's, reaches the door as 400
/// `DERIVED_DECLARATION_INVALID` naming its rule, on the derived resource, and writes nothing.
#[tokio::test]
async fn every_declaration_refusal_is_derived_declaration_invalid_naming_its_rule() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let deep = {
        let mut e = json!({"op":"input","name":"ram_mb"});
        for _ in 0..40 {
            e = json!({"op":"ceil","arg":e});
        }
        json!({"op":"max","args":[e, {"op":"input","name":"cpu_mhz"}]})
    };
    let wide = {
        let mut args: Vec<Value> = (0..257)
            .map(|_| json!({"op":"input","name":"ram_mb"}))
            .collect();
        args.push(json!({"op":"input","name":"cpu_mhz"}));
        json!({"op":"max","args":args})
    };
    let cases: Vec<(&str, Value)> = vec![
        ("empty_unit", edited(|d| d["output_unit"] = json!(" "))),
        (
            "unit_too_long",
            edited(|d| d["output_unit"] = json!("u".repeat(65))),
        ),
        ("scale_too_large", edited(|d| d["output_scale"] = json!(13))),
        (
            "too_few_inputs",
            edited(|d| {
                d["inputs"] = json!([]);
                d["formula"] = json!({"op":"const","value":"1"});
            }),
        ),
        (
            "invalid_input_name",
            edited(|d| d["inputs"][0]["name"] = json!("Ram")),
        ),
        (
            "duplicate_input",
            edited(|d| d["inputs"][1]["name"] = json!("ram_mb")),
        ),
        (
            "empty_input_ref",
            edited(|d| d["inputs"][0]["usage_type_ref"] = json!(" ")),
        ),
        (
            "input_ref_too_long",
            edited(|d| d["inputs"][0]["usage_type_ref"] = json!("r".repeat(513))),
        ),
        (
            "derived_input",
            edited(|d| d["inputs"][0]["usage_type_ref"] = json!("products.derived/other@1")),
        ),
        (
            "hold_missing",
            edited(|d| d["inputs"][0]["granule_fold"] = json!("time_weighted")),
        ),
        (
            "hold_not_allowed",
            edited(|d| d["inputs"][0]["max_hold_seconds"] = json!(60)),
        ),
        (
            "hold_out_of_range",
            edited(|d| {
                d["inputs"][0]["granule_fold"] = json!("time_weighted");
                d["inputs"][0]["max_hold_seconds"] = json!(86_401);
            }),
        ),
        (
            "unknown_input",
            edited(|d| d["formula"]["args"][0] = share("disk_gb", "128")),
        ),
        (
            "unused_input",
            edited(|d| {
                d["inputs"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"name":"disk_gb","usage_type_ref":RAM_REF,"granule_fold":"sum","unit":"GB"}));
            }),
        ),
        (
            "division_by_zero",
            edited(|d| d["formula"]["args"][0] = share("ram_mb", "0")),
        ),
        (
            "too_few_operands",
            edited(|d| {
                d["formula"] = json!({"op":"max","args":[
                    {"op":"add","left":{"op":"input","name":"ram_mb"},"right":{"op":"input","name":"cpu_mhz"}}
                ]});
            }),
        ),
        ("too_deep", edited(|d| d["formula"] = deep.clone())),
        ("too_many_nodes", edited(|d| d["formula"] = wide.clone())),
        (
            "unknown_granularity",
            edited(|d| d["granularity"] = json!("day")),
        ),
        (
            "unknown_fold",
            edited(|d| d["inputs"][1]["granule_fold"] = json!("avg")),
        ),
        (
            "unknown_round_mode",
            edited(|d| d["output_round"] = json!("banker")),
        ),
        (
            "unknown_operator",
            edited(|d| d["formula"]["op"] = json!("pow")),
        ),
        (
            "invalid_decimal",
            edited(|d| d["formula"]["args"][0] = share("ram_mb", "12x")),
        ),
        (
            "malformed_expression",
            edited(|d| {
                d["formula"]["args"][0] = json!({"op":"add","left":{"op":"input","name":"ram_mb"}});
            }),
        ),
    ];
    assert_eq!(cases.len(), 24, "18 SDK rules and 6 shape rules");
    for (i, (rule, declaration)) in cases.into_iter().enumerate() {
        let (status, b) = send(
            &app,
            tenant,
            Method::POST,
            BASE,
            Some(create(&format!("case-{i}"), declaration)),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rule}: {b}");
        assert_eq!(
            problem_code(&b),
            "DERIVED_DECLARATION_INVALID",
            "{rule}: {b}"
        );
        assert!(
            declaration_detail(&b).starts_with(&format!("{rule}: ")),
            "{rule}: {b}"
        );
        assert_eq!(
            b["context"]["resource_type"],
            crate::authz::labels::DERIVED_USAGE_TYPE,
            "{rule}: {b}"
        );
    }
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type"
        )
        .await,
        0
    );
    // The same rules judge a new version.
    send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    let (status, b) = send(
        &app,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": edited(|d| d["output_scale"] = json!(13))})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{b}");
    assert!(
        declaration_detail(&b).starts_with("scale_too_large: "),
        "{b}"
    );
}

/// The identity of a create: a code off its pattern or a blank name is 400 `VALIDATION`, a code
/// over 64 or a name over 200 characters 400 `FIELD_TOO_LONG` (P-D-225); a body without a
/// declaration is 400.
#[tokio::test]
async fn the_identity_and_the_body_are_judged() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    for (body, subject, code) in [
        (create("Cloudlets", cloudlet()), "code", "VALIDATION"),
        (
            create(&"a".repeat(65), cloudlet()),
            "code",
            "FIELD_TOO_LONG",
        ),
        (
            json!({"code":"ok","name":" ","declaration":cloudlet()}),
            "name",
            "VALIDATION",
        ),
        (
            json!({"code":"ok","name":"n".repeat(201),"declaration":cloudlet()}),
            "name",
            "FIELD_TOO_LONG",
        ),
        (json!({"code":"ok","name":"N"}), "body", "VALIDATION"),
    ] {
        let (status, b) = send(&app, tenant, Method::POST, BASE, Some(body), &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{subject}: {b}");
        assert_eq!(problem_code(&b), code, "{subject}: {b}");
        assert!(
            crate::test_support::violation_for(&b, subject).is_some(),
            "{subject}: {b}"
        );
    }
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type"
        )
        .await,
        0
    );
}

/// A catalog, the status it gives a create, and the code that status carries.
type CatalogCase = (
    Arc<dyn bss_products_sdk::usage_types::UsageTypeCatalog>,
    StatusCode,
    &'static str,
);

/// The catalog's answers (P-D-184, P-D-207): an input it does not know is 400
/// `USAGE_TYPE_UNRESOLVED` on that input's ref, an unreachable catalog 503 `USAGE_TYPE_UNAVAILABLE`,
/// a catalog refusing the caller 403 `USAGE_TYPE_FORBIDDEN`; none writes anything.
#[tokio::test]
async fn the_catalog_answers_unresolved_400_unreachable_503_denied_403() {
    let resolved = UsageTypeAnswer::Resolved(crate::test_support::probe_binding());
    let cases: Vec<CatalogCase> = vec![
        (
            Arc::new(StubUsageTypes::scripted([
                resolved,
                UsageTypeAnswer::Unresolved,
            ])),
            StatusCode::BAD_REQUEST,
            "USAGE_TYPE_UNRESOLVED",
        ),
        (
            Arc::new(UnreachableUsageTypes),
            StatusCode::SERVICE_UNAVAILABLE,
            "USAGE_TYPE_UNAVAILABLE",
        ),
        (
            Arc::new(StubUsageTypes::always(UsageTypeAnswer::Forbidden)),
            StatusCode::FORBIDDEN,
            "USAGE_TYPE_FORBIDDEN",
        ),
    ];
    for (catalog, expected, code) in cases {
        let tenant = Uuid::new_v4();
        let (app, dsn) = rest_app_with_catalog(tenant, router, catalog, "test").await;
        let (status, b) = send(
            &app,
            tenant,
            Method::POST,
            BASE,
            Some(create("cloudlets", cloudlet())),
            &[],
        )
        .await;
        assert_eq!(status, expected, "{code}: {b}");
        assert!(b.to_string().contains(code), "{code}: {b}");
        if code == "USAGE_TYPE_UNRESOLVED" {
            assert!(
                crate::test_support::violation_for(&b, "declaration.inputs.cpu_mhz.usage_type_ref")
                    .is_some(),
                "{b}"
            );
            assert!(
                crate::test_support::violation_for(&b, "declaration.inputs.ram_mb.usage_type_ref")
                    .is_none(),
                "{b}"
            );
        }
        assert_eq!(
            raw_i64(
                &dsn,
                "SELECT COUNT(*) AS v FROM products_derived_usage_type"
            )
            .await,
            0,
            "{code}"
        );
    }
}

/// Another tenant reads nothing: its list is empty and the type and the version are 404 to it.
#[tokio::test]
async fn a_foreign_tenant_reads_nothing() {
    let tenant = Uuid::new_v4();
    let (app, _dsn) = rest_app(tenant, router).await;
    send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    let other = Uuid::new_v4();
    let list = body_json(get(&app, other, BASE).await).await;
    assert_eq!(list["items"], json!([]), "{list}");
    for path in [
        format!("{BASE}/cloudlets"),
        format!("{BASE}/cloudlets/versions/1"),
    ] {
        let (status, b) = send(&app, other, Method::GET, &path, None, &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {b}");
    }
    let (status, own) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/cloudlets"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{own}");
}

/// An `Idempotency-Key` replays the first answer and writes once; the same key with another body is
/// 409 `IDEMPOTENCY_CONFLICT`. A new version's key replays too.
#[tokio::test]
async fn an_idempotent_replay_answers_the_first_receipt_and_writes_once() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let key = [("Idempotency-Key", "create-cloudlets")];
    let (status, first) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, again) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{again}");
    assert_eq!(again, first);
    let (status, b) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("other", cloudlet())),
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{b}");
    assert_eq!(problem_code(&b), "IDEMPOTENCY_CONFLICT");
    let version_key = [("Idempotency-Key", "version-2")];
    let url = format!("{BASE}/cloudlets/versions");
    let body =
        json!({"declaration": edited(|d| d["formula"]["args"][1] = share("cpu_mhz", "500"))});
    let (_, two) = send(
        &app,
        tenant,
        Method::POST,
        &url,
        Some(body.clone()),
        &version_key,
    )
    .await;
    let (_, replayed) = send(&app, tenant, Method::POST, &url, Some(body), &version_key).await;
    assert_eq!(replayed, two);
    assert_eq!(two["version"], 2);
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type"
        )
        .await,
        1
    );
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type_version"
        )
        .await,
        2
    );
    assert_eq!(
        raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await,
        2
    );
}

#[tokio::test]
async fn a_path_off_the_meter_id_pattern_is_a_fixed_404() {
    let tenant = Uuid::new_v4();
    let (app, _dsn) = rest_app(tenant, router).await;
    let code = "Z".repeat(80);
    let (status, body) = send(
        &app,
        tenant,
        Method::GET,
        &format!("{BASE}/{code}"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let text = body.to_string();
    assert!(!text.contains(&code), "{text}");
    assert!(text.contains("derived usage type"), "{text}");
    assert_eq!(body["detail"], "derived usage type");
}

/// A new version of a code the tenant does not hold is 404 and asks no catalog.
#[tokio::test]
async fn a_version_of_an_unknown_code_is_404() {
    let tenant = Uuid::new_v4();
    let stub = Arc::new(StubUsageTypes::always(UsageTypeAnswer::Resolved(
        crate::test_support::probe_binding(),
    )));
    let (app, _dsn) = rest_app_with_catalog(tenant, router, stub.clone(), "test").await;
    let (status, b) = send(
        &app,
        tenant,
        Method::POST,
        &format!("{BASE}/missing/versions"),
        Some(json!({"declaration": cloudlet()})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(stub.asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// One audit row per create and per version, in the write's transaction: `subject_kind`
/// `derived_usage_type`, the type's uuid as the subject and the version as its revision.
#[tokio::test]
async fn each_create_and_version_writes_one_audit_row_with_the_uuid_subject() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (_, one) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    let id: Uuid = serde_json::from_value(one["id"].clone()).unwrap();
    send(
        &app,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": edited(|d| d["formula"]["args"][1] = share("cpu_mhz", "500"))})),
        &[],
    )
    .await;
    for (revision, action) in [
        (1, "derived_usage_type.create"),
        (2, "derived_usage_type.version"),
    ] {
        assert_eq!(
            raw_i64(
                &dsn,
                &format!(
                    "SELECT COUNT(*) AS v FROM products_audit_log WHERE subject_kind = \
                     'derived_usage_type' AND {} AND subject_revision = {revision} AND action = \
                     '{action}' AND seal_state = 'unsealed' AND {}",
                    id_matches("subject_id", id),
                    id_matches("actor_ref", tenant_user(tenant).subject_id()),
                )
            )
            .await,
            1,
            "{action}"
        );
    }
    assert_eq!(
        raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_audit_log").await,
        2
    );
}

/// The audit row shares the write's transaction: with the audit table gone, a create and a new
/// version are each a 500 and leave no row of their own.
#[tokio::test]
async fn an_audit_failure_rolls_the_write_back() {
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    drop_table(&dsn, "products_audit_log").await;
    let (status, b) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("rollback", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{b}");
    let (status, b) = send(
        &app,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": cloudlet()})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{b}");
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type"
        )
        .await,
        1
    );
    assert_eq!(
        raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_derived_usage_type_version"
        )
        .await,
        1
    );
}

/// The list pages as the SKU list does: 50 by default, clamped at 200, by code, and a cursor walks
/// every type exactly once. A query key the list does not take is 400.
#[tokio::test]
async fn the_list_pages_50_by_default_200_at_most_and_walks_by_cursor() {
    use crate::domain::derived::{NewDerivedType, NewDerivedVersion};
    use crate::infra::storage::repo::derived_usage_type_repo as store;
    let tenant = Uuid::new_v4();
    let (app, dsn) = rest_app(tenant, router).await;
    let (db, scope) = repo_connection(&dsn, tenant).await;
    let conn = db.conn().unwrap();
    let now = time::OffsetDateTime::now_utc();
    for i in 0..205 {
        let t = store::create_type(
            &conn,
            &scope,
            tenant,
            NewDerivedType {
                code: format!("t{i:03}"),
                name: format!("T{i}"),
            },
            Uuid::from_u128(7),
            now,
        )
        .await
        .unwrap();
        for version in 1..=(1 + u32::from(i % 2 == 0)) {
            store::insert_version(
                &conn,
                &scope,
                tenant,
                NewDerivedVersion {
                    type_id: t.id,
                    version,
                    declaration_json: cloudlet(),
                    digest: "a".repeat(64),
                    created_by: Uuid::from_u128(7),
                    created_at: now,
                },
            )
            .await
            .unwrap();
        }
    }
    let page = body_json(get(&app, tenant, BASE).await).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 50, "{page}");
    assert_eq!(page["page_info"]["limit"], 50);
    assert_eq!(page["items"][0]["code"], "t000");
    assert_eq!(page["items"][0]["latest_version"], 2);
    assert_eq!(page["items"][1]["latest_version"], 1);
    let page = body_json(get(&app, tenant, &format!("{BASE}?limit=500")).await).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 200, "clamped");
    assert_eq!(page["page_info"]["limit"], 200);
    let mut seen = Vec::new();
    let mut url = format!("{BASE}?limit=60");
    loop {
        let page = body_json(get(&app, tenant, &url).await).await;
        seen.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["code"].as_str().unwrap().to_owned()),
        );
        let Some(next) = page["page_info"]["next_cursor"].as_str() else {
            break;
        };
        url = format!("{BASE}?limit=60&cursor={next}");
    }
    let expected: Vec<String> = (0..205).map(|i| format!("t{i:03}")).collect();
    assert_eq!(seen, expected, "every type once, by code");
    for refused in [
        "$filter=code eq 'x'",
        "$orderby=code desc",
        "$select=code",
        "q=x",
    ] {
        let (status, b) = send(
            &app,
            tenant,
            Method::GET,
            &format!("{BASE}?{}", refused.replace(' ', "%20")),
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}: {b}");
    }
}

/// The writes ask `author` on `derived_usage_type`, the reads `sku:read` (O-3): a PDP that allows
/// only the other is a 403 on each.
#[tokio::test]
async fn writes_ask_derived_author_and_reads_ask_sku_read() {
    use crate::test_support::test_db;
    struct Only(&'static str, &'static str, Uuid);
    #[async_trait::async_trait]
    impl authz_resolver_sdk::AuthZResolverApi for Only {
        async fn evaluate(
            &self,
            _: toolkit_security::PlatformSecurityContext,
            req: authz_resolver_sdk::models::EvaluationRequest,
        ) -> Result<
            authz_resolver_sdk::models::EvaluationResponse,
            toolkit::api::canonical_prelude::CanonicalError,
        > {
            use authz_resolver_sdk::{
                constraints::{Constraint, InPredicate, Predicate},
                models::{EvaluationResponse, EvaluationResponseContext},
            };
            Ok(EvaluationResponse {
                decision: req.resource.resource_type == self.0 && req.action.name == self.1,
                context: EvaluationResponseContext {
                    constraints: vec![Constraint {
                        predicates: vec![Predicate::In(InPredicate::new(
                            toolkit_security::pep_properties::OWNER_TENANT_ID,
                            vec![self.2],
                        ))],
                    }],
                    deny_reason: None,
                },
            })
        }
    }
    let tenant = Uuid::new_v4();
    let (db, _, _, _dsn) = test_db().await;
    // The fixture's router holds the outbox; the routers under test share its state.
    let (_fixture, state) = crate::test_support::rest_app_on_db(
        tenant,
        router,
        crate::test_support::resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let app_with = |label: &'static str, action: &'static str| {
        router(state.clone(), &toolkit::api::OpenApiRegistryImpl::new()).layer(axum::Extension(
            authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Only(label, action, tenant))),
        ))
    };
    let author = app_with(crate::authz::labels::DERIVED_USAGE_TYPE, "author");
    let (status, b) = send(
        &author,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let (status, _) = send(&author, tenant, Method::GET, BASE, None, &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "author is not read");
    let reader = app_with(crate::authz::labels::SKU, "read");
    let (status, _) = send(
        &reader,
        tenant,
        Method::GET,
        &format!("{BASE}/cloudlets/versions/1"),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, b) = send(
        &reader,
        tenant,
        Method::POST,
        BASE,
        Some(create("other", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(
        b["context"]["resource_type"],
        crate::authz::labels::DERIVED_USAGE_TYPE,
        "{b}"
    );
    let (status, b) = send(
        &reader,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": cloudlet()})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{b}");
    assert_eq!(
        b["context"]["resource_type"],
        crate::authz::labels::DERIVED_USAGE_TYPE,
        "{b}"
    );
    let sku_author = app_with(crate::authz::labels::SKU, "author");
    let (status, b) = send(
        &sku_author,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": cloudlet()})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{b}");
    let (status, b) = send(
        &author,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": cloudlet()})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{b}");
    let (status, _) = send(
        &sku_author,
        tenant,
        Method::POST,
        BASE,
        Some(create("third", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "sku:author is not derived_usage_type:author"
    );
}

/// A type with two versions lists `latest` equal to `GET …/versions/{latest_version}`,
/// including the formula, and keeps `latest_version` (P-D-257).
#[tokio::test]
async fn the_list_carries_the_latest_version_equal_to_the_version_read() {
    let tenant = Uuid::new_v4();
    let (app, _) = rest_app(tenant, router).await;
    let (status, one) = send(
        &app,
        tenant,
        Method::POST,
        BASE,
        Some(create("cloudlets", cloudlet())),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{one}");
    let mut second = cloudlet();
    second["formula"]["args"][0]["arg"]["divisor"] = json!("64");
    let (status, two) = send(
        &app,
        tenant,
        Method::POST,
        &format!("{BASE}/cloudlets/versions"),
        Some(json!({"declaration": second})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{two}");
    assert_eq!(two["version"], 2);
    let page = body_json(get(&app, tenant, BASE).await).await;
    let item = &page["items"][0];
    assert_eq!(item["code"], "cloudlets", "{page}");
    assert_eq!(item["latest_version"], 2, "{item}");
    let version = body_json(get(&app, tenant, &format!("{BASE}/cloudlets/versions/2")).await).await;
    assert_eq!(
        item["latest"], version,
        "latest is the version read, byte for byte"
    );
    assert_eq!(item["latest"]["declaration"]["formula"]["op"], "max");
    assert_eq!(
        item["latest"]["declaration"]["formula"]["args"][0]["arg"]["divisor"],
        "64"
    );
}

/// The list makes the same number of statements for 10 types and for 100: one page read and one
/// grouped read of the latest version rows (P-D-257). The pin is 2.
#[tokio::test]
async fn the_list_reads_the_latest_version_in_the_same_statements_for_10_and_100_types() {
    use crate::domain::derived::{NewDerivedType, NewDerivedVersion};
    use crate::infra::storage::repo::derived_usage_type_repo as store;
    use crate::test_support::{
        products_statements, recorded_test_db, resolved_usage_types, rest_app_on_db,
    };

    async fn recorded(n: usize) -> (axum::Router, Uuid, toolkit_db::test_support::QueryRecorder) {
        let (db, scope, tenant, _dsn, recorder) = recorded_test_db().await;
        let conn = db.conn().unwrap();
        let now = time::OffsetDateTime::now_utc();
        for i in 0..n {
            let code = format!("t{i:03}");
            let t = store::create_type(
                &conn,
                &scope,
                tenant,
                NewDerivedType {
                    code,
                    name: format!("T{i}"),
                },
                Uuid::from_u128(7),
                now,
            )
            .await
            .unwrap();
            let versions = if i % 2 == 0 { 2 } else { 1 };
            for version in 1..=versions {
                store::insert_version(
                    &conn,
                    &scope,
                    tenant,
                    NewDerivedVersion {
                        type_id: t.id,
                        version,
                        declaration_json: cloudlet(),
                        digest: "a".repeat(64),
                        created_by: Uuid::from_u128(7),
                        created_at: now,
                    },
                )
                .await
                .unwrap();
            }
        }
        let (app, _) = rest_app_on_db(tenant, router, resolved_usage_types(), "test", db).await;
        recorder.clear();
        (app, tenant, recorder)
    }
    async fn listed(
        app: &axum::Router,
        tenant: Uuid,
        recorder: &toolkit_db::test_support::QueryRecorder,
        n: usize,
    ) -> Vec<String> {
        recorder.clear();
        let (status, body) = send(
            app,
            tenant,
            Method::GET,
            &format!("{BASE}?limit=200"),
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["items"].as_array().unwrap().len(), n, "{body}");
        products_statements(recorder)
    }
    let (ten_app, ten_tenant, ten_rec) = recorded(10).await;
    let (hundred_app, hundred_tenant, hundred_rec) = recorded(100).await;
    let ten = listed(&ten_app, ten_tenant, &ten_rec, 10).await;
    let hundred = listed(&hundred_app, hundred_tenant, &hundred_rec, 100).await;
    let version_reads = |sqls: &[String]| {
        sqls.iter()
            .filter(|sql| sql.contains("products_derived_usage_type_version"))
            .count()
    };
    for (i, sql) in hundred.iter().enumerate() {
        eprintln!("statement {i}: {sql}");
    }
    assert_eq!(
        ten.len(),
        hundred.len(),
        "10 types {ten:#?}\n100 types {hundred:#?}"
    );
    assert_eq!(version_reads(&ten), 1, "one version read for 10: {ten:#?}");
    assert_eq!(
        version_reads(&hundred),
        1,
        "one version read for 100: {hundred:#?}"
    );
    assert_eq!(
        ten.len(),
        2,
        "pin: one page read and one grouped latest-version read: {ten:#?}"
    );
}
