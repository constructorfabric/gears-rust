#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;

#[test]
fn default_leaves_the_runtime_slot_empty() {
    let gear = BssProductsGear::default();
    assert!(gear.runtime.load_full().is_none());
}

/// A configured gear registers every implemented operation.
#[tokio::test]
async fn configured_gear_registers_implemented_routes() -> anyhow::Result<()> {
    use sea_orm_migration::MigratorTrait;
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
    let (gear, ctx) = skeleton_harness().await?;
    assert!(gear.runtime.load_full().is_some());
    assert_eq!(
        crate::infra::storage::migrations::Migrator::migrations().len(),
        17,
        "the schema guard, coordination and the fifteen PriceBook migrations (000009: the unit's \
         note, P-D-219; 000010: no retired default, P-D-220; 000011: retire_pending, P-D-248; \
         000012: the derived usage types, P-D-231; 000013: a derived SKU stores no unit, P-D-259; \
         000014: the archive mark, P-D-263; 000015: the quota catalog, P-D-274)"
    );
    let openapi = OpenApiRegistryImpl::new();
    let router = gear.register_rest(&ctx, Router::new(), &openapi)?;
    assert!(router.has_routes());
    let api = serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default())?)?;
    let mut actual: Vec<&str> = api["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|p| p.as_object().unwrap().values())
        .filter_map(|op| op["operationId"].as_str())
        .collect();
    actual.sort_unstable();
    let mut expected = vec![
        "bss_products.create_category",
        "bss_products.list_categories",
        "bss_products.get_category",
        "bss_products.update_category",
        "bss_products.retire_category",
        "bss_products.archive_category",
        "bss_products.unarchive_category",
        "bss_products.create_sku",
        "bss_products.list_skus",
        "bss_products.count_skus",
        "bss_products.get_sku",
        "bss_products.update_sku_draft",
        "bss_products.delete_sku_draft",
        "bss_products.sku_versions",
        "bss_products.sku_version_as_of",
        "bss_products.sku_references",
        "bss_products.archive_sku",
        "bss_products.unarchive_sku",
        "bss_products.sku_history",
        "bss_products.submit_sku",
        "bss_products.change_sku",
        "bss_products.retire_sku",
        "bss_products.unfence_sku",
        "bss_products.list_approval_units",
        "bss_products.count_approval_units",
        "bss_products.get_approval_unit",
        "bss_products.approve_unit",
        "bss_products.reject_unit",
        "bss_products.withdraw_unit",
        "bss_products.get_approval_policy",
        "bss_products.put_approval_policy",
        "bss_products.delete_approval_policy_override",
        "bss_products.reserve_reference",
        "bss_products.confirm_reference",
        "bss_products.release_reference",
        "bss_products.browse",
        "bss_products.list_usage_types",
        "bss_products.create_derived_usage_type",
        "bss_products.create_derived_usage_type_version",
        "bss_products.list_derived_usage_types",
        "bss_products.get_derived_usage_type",
        "bss_products.get_derived_usage_type_version",
        "bss_products.create_quota_adapter_target",
        "bss_products.list_quota_adapter_targets",
        "bss_products.get_quota_adapter_target",
        "bss_products.run_quota_discovery",
        "bss_products.create_quota_candidate",
        "bss_products.list_quota_candidates",
        "bss_products.get_quota_candidate",
        "bss_products.submit_quota_candidate",
        "bss_products.approve_quota_candidate",
        "bss_products.reject_quota_candidate",
        "bss_products.list_quota_definitions",
        "bss_products.get_quota_definition",
    ];
    expected.sort_unstable();
    assert_eq!(actual, expected);
    // The actual lifecycle entry must honor the retained cancellation token, and it stops the
    // outbox pipeline before it returns (RS-12): the runtime holds none after.
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let gear = Arc::new(gear);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        Arc::clone(&gear).serve(cancel),
    )
    .await??;
    let runtime = gear
        .runtime
        .load_full()
        .expect("the runtime outlives serve");
    assert!(
        runtime.pipeline.lock().await.is_none(),
        "serve stops the pipeline"
    );
    Ok(())
}

/// D4 (P-D-210): every query parameter the gear serves is declared with its type — `limit` an
/// integer, `include_released`, `priced` and `in_plan` booleans, the rest strings (the builder has no uuid or date
/// format) — and the SKU list publishes its `OData` vocabulary: the filter fields without
/// `updated_at`, the order fields without any nullable or filter-only field.
#[tokio::test]
async fn served_query_parameters_are_typed_and_the_list_publishes_its_odata_vocabulary()
-> anyhow::Result<()> {
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
    let (gear, ctx) = skeleton_harness().await?;
    let openapi = OpenApiRegistryImpl::new();
    let _router = gear.register_rest(&ctx, Router::new(), &openapi)?;
    let api = serde_json::to_value(openapi.build_openapi(&OpenApiInfo::default())?)?;
    let mut seen = 0;
    for (path, item) in api["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            for p in op["parameters"].as_array().into_iter().flatten() {
                if p["in"] != "query" {
                    continue;
                }
                seen += 1;
                let name = p["name"].as_str().unwrap();
                let expected = match name {
                    "limit" => "integer",
                    "include_released" | "priced" | "in_plan" => "boolean",
                    _ => "string",
                };
                assert_eq!(
                    p["schema"]["type"], expected,
                    "{method} {path} `{name}`: {p}"
                );
                assert!(
                    p["description"].as_str().is_some_and(|d| d != name),
                    "{method} {path} `{name}` says what it is: {p}"
                );
            }
        }
    }
    assert!(seen > 15, "the census read the query parameters: {seen}");
    let list = &api["paths"]["/bss-products/v1/skus"]["get"];
    let mut filter: Vec<&str> = list["x-odata-filter"]["allowedFields"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    filter.sort_unstable();
    assert_eq!(
        filter,
        [
            "archived",
            "category_id",
            "code",
            "id",
            "lifecycle",
            "name",
            "pending_unit_id",
            "retire_pending",
            "type"
        ],
        "{list}"
    );
    // P-D-249, P-D-264: the CASE serves `eq`, `ne` and `in`, and a text function as the `in` of
    // the lifecycles it matches. The toolkit publishes every operator a string field parses, and
    // the door's own description says how each is served.
    let filter_text = list["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "$filter")
        .unwrap()["description"]
        .as_str()
        .unwrap();
    assert!(
        filter_text.contains("- lifecycle: eq|ne|contains|startswith|endswith|in\n"),
        "{filter_text}"
    );
    let description = list["description"].as_str().unwrap();
    assert!(
        description.contains("joined by `and`")
            && description.contains("`contains`, `startswith` or `endswith` as the `in`")
            && !description.contains("`endswith` on it, is 400"),
        "{description}"
    );
    let mut order: Vec<&str> = list["x-odata-orderby"]["allowedFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    order.sort_unstable();
    assert_eq!(
        order,
        [
            "code asc",
            "code desc",
            "id asc",
            "id desc",
            "name asc",
            "name desc",
            "updated_at asc",
            "updated_at desc"
        ],
        "{list}"
    );
    let counts = &api["paths"]["/bss-products/v1/skus/counts"]["get"];
    assert!(counts["x-odata-orderby"].is_null(), "{counts}");
    // P-D-215: the category list publishes its vocabulary; `status`, `is_default` and `archived`
    // (P-D-263) only filter.
    let categories = &api["paths"]["/bss-products/v1/categories"]["get"];
    let mut filter: Vec<&str> = categories["x-odata-filter"]["allowedFields"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    filter.sort_unstable();
    assert_eq!(
        filter,
        [
            "archived",
            "code",
            "id",
            "is_default",
            "name",
            "sort_order",
            "status"
        ],
        "{categories}"
    );
    let mut order: Vec<&str> = categories["x-odata-orderby"]["allowedFields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    order.sort_unstable();
    assert_eq!(
        order,
        [
            "code asc",
            "code desc",
            "id asc",
            "id desc",
            "name asc",
            "name desc",
            "sort_order asc",
            "sort_order desc"
        ],
        "{categories}"
    );
    // P-D-272: the quota definition list publishes its five terms, each served by `eq` only.
    let definitions = &api["paths"]["/bss-products/v1/quota-definitions"]["get"];
    assert_eq!(
        definitions["x-odata-filter"]["allowedFields"],
        serde_json::json!({
            "sku_id": ["eq"],
            "sku_version": ["eq"],
            "subject_type_ref": ["eq"],
            "resource_type_ref": ["eq"],
            "profile_id": ["eq"]
        }),
        "{definitions}"
    );
    assert!(definitions["x-odata-orderby"].is_null(), "{definitions}");
    // P-D-213, P-D-214: the history and the versions publish no OData vocabulary.
    for path in [
        "/bss-products/v1/skus/{id}/history",
        "/bss-products/v1/skus/{id}/versions",
        "/bss-products/v1/skus/{id}/versions/as-of",
    ] {
        let op = &api["paths"][path]["get"];
        assert!(op.is_object(), "{path} is served");
        assert!(op["x-odata-filter"].is_null(), "{path}: {op}");
        assert!(op["x-odata-orderby"].is_null(), "{path}: {op}");
    }
    Ok(())
}

async fn skeleton_harness() -> anyhow::Result<(BssProductsGear, GearCtx)> {
    use crate::infra::events::{PARTITIONS, PendingBrokerProducer, QUEUE_NAME};
    use toolkit::contracts::DatabaseCapability;
    use toolkit_db::{ConnectOpts, DBProvider, connect_db};

    struct NoConfig;
    impl toolkit::config::ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
            None
        }
    }
    let gear = BssProductsGear::default();
    let db = connect_db(
        "sqlite::memory:",
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await?;
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations()).await?;
    let pipeline = toolkit_db::outbox::Outbox::builder(db.clone())
        .table_prefix(OUTBOX_TABLE_PREFIX)?
        .queue(QUEUE_NAME, toolkit_db::outbox::Partitions::of(PARTITIONS))
        .leased(PendingBrokerProducer)
        .start()
        .await?;
    let db = DBProvider::new(db);
    let api_state = Arc::new(crate::api::rest::ApiState {
        db: db.clone(),
        sink: crate::infra::broker::EventSink::Interim(Arc::clone(pipeline.outbox())),
        usage_type_catalog: Arc::new(crate::infra::usage_types::UnconfiguredUsageTypes),
        usage_type_catalog_source: USAGE_TYPE_SOURCE_UNCONFIGURED,
        idempotency_retention_hours: ProductsConfig::default()
            .resolved_idempotency_retention_hours(),
        fence_ttl_minutes: 30,
        reference_principals: std::collections::BTreeMap::new(),
        hub: Arc::new(toolkit::ClientHub::new()),
        actor_names: crate::api::rest::ApiState::names_from(&Arc::new(toolkit::ClientHub::new())),
    });
    let quota = build_quota_state(&api_state.hub, &api_state, &ProductsConfig::default());
    gear.runtime.store(Some(Arc::new(ProductsRuntime {
        enforcer: Arc::new(crate::test_support::flat_in_enforcer(uuid::Uuid::new_v4())),
        api_state,
        quota,
        pipeline: tokio::sync::Mutex::new(Some(OutboxLifetime::Interim(pipeline))),
    })));
    let ctx = GearCtx::new(
        "bss-products",
        uuid::Uuid::new_v4(),
        Arc::new(NoConfig),
        Arc::new(toolkit::ClientHub::new()),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(db);
    Ok((gear, ctx))
}

#[tokio::test]
async fn registered_products_client_reads_drafts_and_hides_foreign_rows() {
    use crate::infra::storage::repo;
    use crate::test_support::*;
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = repo::insert_category(
        &conn,
        &scope,
        tenant,
        crate::domain::category::NewCategory {
            code: "C".into(),
            name: "Category".into(),
            is_default: false,
            sort_order: 0,
        },
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    let sku = seed_rest_sku(&conn, &scope, tenant, cat.id, "DRAFT").await;
    let hub = toolkit::ClientHub::new();
    register_products_client(&hub, db.db(), Arc::new(flat_in_enforcer(tenant)));
    let client = hub.get::<dyn bss_products_sdk::ProductsClient>().unwrap();
    let ctx = authed_ctx(tenant);
    let found = client.get_sku(&ctx, tenant, sku.id).await.unwrap();
    assert_eq!(found, sku);
    assert!(matches!(
        client.get_sku(&ctx, uuid::Uuid::new_v4(), sku.id).await,
        Err(toolkit_canonical_errors::CanonicalError::NotFound { .. })
    ));
    assert!(matches!(
        client.get_sku(&ctx, tenant, uuid::Uuid::new_v4()).await,
        Err(toolkit_canonical_errors::CanonicalError::NotFound { .. })
    ));
}

// ------------------------------------------------------------------ P-D-217: closed sets

const SKU_TYPE: &[&str] = &["recurring", "usage", "one_time", "bundle"];
const LIFECYCLE: &[&str] = &["draft", "published", "deprecated", "retired"];
const TIMING: &[&str] = &["advance", "arrears"];
const CATEGORY_STATUS: &[&str] = &["active", "retired"];
const UNIT_STATE: &[&str] = &["pending", "approved", "rejected", "withdrawn"];
const UNIT_KIND: &[&str] = &["sku_publish", "sku_change", "sku_retire"];
const DECISION: &[&str] = &["approve", "reject"];
const VOTE_OUTCOME: &[&str] = &["pending", "applied", "rejected", "withdrawn"];
const REFERENCE_KIND: &[&str] = &["price_book_entry", "plan_item", "sold_as"];
const REFERENCE_STATE: &[&str] = &["reserved", "confirmed", "released"];

/// P-D-217: (schema, field, the exact values in order, nullable).
type Closed = (&'static str, &'static str, &'static [&'static str], bool);
const CLOSED: &[Closed] = &[
    ("SkuDto", "type", SKU_TYPE, false),
    ("SkuDto", "lifecycle", LIFECYCLE, false),
    ("LifecycleNextDto", "lifecycle", LIFECYCLE, false),
    ("SkuDto", "billing_timing", TIMING, true),
    ("SkuContentDto", "type", SKU_TYPE, false),
    ("SkuContentDto", "billing_timing", TIMING, true),
    ("ProductsCategoryDto", "status", CATEGORY_STATUS, false),
    ("ProductsSkuHistoryEntry", "from_lifecycle", LIFECYCLE, true),
    ("ProductsSkuHistoryEntry", "to_lifecycle", LIFECYCLE, true),
    ("UnitDto", "state", UNIT_STATE, false),
    // The phase 9 review's theme C: no CHECK holds the kind, but the repository reads it through
    // its closed set, so a row outside it is a corrupt row (500), never served.
    ("UnitDto", "kind", UNIT_KIND, false),
    ("DecisionDto", "decision", DECISION, false),
    ("VoteReceipt", "outcome", VOTE_OUTCOME, false),
    ("ReferenceDto", "kind", REFERENCE_KIND, false),
    ("ReferenceDto", "state", REFERENCE_STATE, false),
    ("ReferenceReceipt", "kind", REFERENCE_KIND, false),
    ("ReferenceReceipt", "state", REFERENCE_STATE, false),
];

/// P-D-217: response fields that stay `string`. No CHECK guards the stored set (the audit
/// `action` and `unit_kind`, the approval unit's `ref_type`, a reference's `owner`, and a derived usage
/// type's declaration, stored as JSON and served in the request's own strings, P-D-231), the value is not
/// this gear's (a usage type's `kind`, the collector's), it names the wired catalog (`source`), or
/// the kept `/browse` envelope carries the catalog port's vocabulary verbatim (`CatalogSku`: "not
/// an enum").
const KEPT_STRING: &[(&str, &str)] = &[
    ("ProductsSkuHistoryEntry", "action"),
    ("ProductsSkuHistoryEntry", "unit_kind"),
    ("UnitDto", "ref_type"),
    ("ReferenceDto", "owner"),
    ("ReferenceReceipt", "owner"),
    ("ProductsUsageTypeDto", "kind"),
    ("ProductsUsageTypeList", "source"),
    ("SkuRow", "lifecycle_state"),
    ("SkuRow", "sku_type"),
    ("ProductsDerivedDeclaration", "granularity"),
    ("ProductsDerivedDeclaration", "output_round"),
    ("ProductsDerivedInput", "granule_fold"),
    ("ProductsDerivedExpr", "op"),
    ("ProductsDerivedExpr", "mode"),
];

/// Request fields over the same sets: `string`, so the door's own code refuses a bad value.
const REQUEST_STRING: &[(&str, &str)] = &[
    ("SkuRequest", "type"),
    ("SkuRequest", "billing_timing"),
    ("SkuPatchRequest", "type"),
    ("SkuPatchRequest", "lifecycle"),
    ("SkuPatchRequest", "billing_timing"),
    ("ReserveRequest", "kind"),
    ("ApprovalPolicyRequest", "kind"),
    ("ProductsDerivedDeclaration", "granularity"),
    ("ProductsDerivedDeclaration", "output_round"),
    ("ProductsDerivedInput", "granule_fold"),
    ("ProductsDerivedExpr", "op"),
    ("ProductsDerivedExpr", "mode"),
];

async fn served_spec() -> anyhow::Result<serde_json::Value> {
    use toolkit::api::{OpenApiInfo, OpenApiRegistryImpl};
    let (gear, ctx) = skeleton_harness().await?;
    let openapi = OpenApiRegistryImpl::new();
    let _router = gear.register_rest(&ctx, Router::new(), &openapi)?;
    Ok(serde_json::to_value(
        openapi.build_openapi(&OpenApiInfo::default())?,
    )?)
}
fn component<'a>(api: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    &api["components"]["schemas"][name]
}
/// The property `field` of component `schema`: its own, or an inline `allOf` part's (a flatten).
fn property<'a>(
    api: &'a serde_json::Value,
    schema: &str,
    field: &str,
) -> Option<&'a serde_json::Value> {
    let s = component(api, schema);
    std::iter::once(s)
        .chain(s["allOf"].as_array().into_iter().flatten())
        .find_map(|part| part["properties"].get(field))
}
fn referenced<'a>(
    api: &'a serde_json::Value,
    node: &'a serde_json::Value,
) -> &'a serde_json::Value {
    node["$ref"]
        .as_str()
        .and_then(|r| r.strip_prefix("#/components/schemas/"))
        .map_or(node, |name| component(api, name))
}
/// The values of the `enum` a property names (inline, by `$ref`, or by `allOf`/`oneOf`/`anyOf`
/// beside `null`) and whether it admits `null`; an error names what the property is instead.
fn enum_of(api: &serde_json::Value, p: &serde_json::Value) -> Result<(Vec<String>, bool), String> {
    let mut nullable = p["type"]
        .as_array()
        .is_some_and(|t| t.iter().any(|t| t == "null"));
    let mut target = None;
    for key in ["oneOf", "anyOf", "allOf"] {
        for branch in p[key].as_array().into_iter().flatten() {
            if branch["type"] == "null" {
                nullable = true;
            } else {
                target = Some(referenced(api, branch));
            }
        }
    }
    let target = target.unwrap_or_else(|| referenced(api, p));
    let values = target["enum"]
        .as_array()
        .ok_or_else(|| format!("no enum: {p}"))?;
    let string = target["type"] == "string"
        || target["type"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t == "string"));
    if !string {
        return Err(format!("the enum is not a string: {target}"));
    }
    Ok((
        values
            .iter()
            .map(|v| v.as_str().unwrap_or("<not a string>").to_owned())
            .collect(),
        nullable,
    ))
}
/// A plain `string` property: no `enum` and no reference, only `string` (and `null`).
fn plain_string(p: &serde_json::Value) -> bool {
    let string = p["type"] == "string"
        || p["type"].as_array().is_some_and(|t| {
            t.iter().any(|t| t == "string") && t.iter().all(|t| t == "string" || t == "null")
        });
    string && p.get("enum").is_none() && p.get("$ref").is_none()
}
fn schema_refs(node: &serde_json::Value, out: &mut std::collections::BTreeSet<String>) {
    match node {
        serde_json::Value::Object(map) => {
            if let Some(r) = map.get("$ref").and_then(serde_json::Value::as_str) {
                out.insert(r.trim_start_matches("#/components/schemas/").to_owned());
            }
            map.values().for_each(|v| schema_refs(v, out));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| schema_refs(v, out)),
        _ => {}
    }
}
fn has_enum(node: &serde_json::Value) -> bool {
    match node {
        serde_json::Value::Object(map) => map.contains_key("enum") || map.values().any(has_enum),
        serde_json::Value::Array(items) => items.iter().any(has_enum),
        _ => false,
    }
}

/// P-D-217: every closed set on a RESPONSE schema is an `enum` holding exactly the stored tokens.
#[tokio::test]
async fn every_closed_set_on_a_response_schema_is_an_enum_of_its_stored_tokens()
-> anyhow::Result<()> {
    let api = served_spec().await?;
    let mut wrong = Vec::new();
    for &(schema, field, values, nullable) in CLOSED {
        let Some(p) = property(&api, schema, field) else {
            wrong.push(format!("{schema}.{field}: no such property"));
            continue;
        };
        match enum_of(&api, p) {
            Ok((served, served_nullable)) => {
                if served != values || served_nullable != nullable {
                    wrong.push(format!(
                        "{schema}.{field}: {served:?} (nullable {served_nullable}), want \
                         {values:?} (nullable {nullable})"
                    ));
                }
            }
            Err(why) => wrong.push(format!("{schema}.{field}: {why}")),
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} closed sets are not their enum:\n{}",
        wrong.len(),
        CLOSED.len(),
        wrong.join("\n")
    );
    Ok(())
}

/// P-D-217: the fields no CHECK guards, and the values that are not this gear's, stay strings.
#[tokio::test]
async fn the_fields_no_check_guards_stay_strings_on_the_responses() -> anyhow::Result<()> {
    let api = served_spec().await?;
    for &(schema, field) in KEPT_STRING {
        let p = property(&api, schema, field).unwrap_or_else(|| panic!("{schema}.{field}"));
        assert!(plain_string(p), "{schema}.{field}: {p}");
    }
    Ok(())
}

/// P-D-217: no request body reaches an `enum`, so each door keeps its own refusal code.
#[tokio::test]
async fn request_bodies_keep_strings_so_the_doors_keep_their_codes() -> anyhow::Result<()> {
    let api = served_spec().await?;
    let mut pending = std::collections::BTreeSet::new();
    for op in api["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|ops| ops.as_object().unwrap().values())
    {
        schema_refs(&op["requestBody"], &mut pending);
    }
    let mut seen = std::collections::BTreeSet::new();
    while let Some(name) = pending.pop_first() {
        if seen.insert(name.clone()) {
            let mut next = std::collections::BTreeSet::new();
            schema_refs(component(&api, &name), &mut next);
            pending.extend(next.difference(&seen).cloned());
        }
    }
    let enums: Vec<_> = seen
        .iter()
        .filter(|name| has_enum(component(&api, name)))
        .collect();
    assert!(
        enums.is_empty(),
        "a request body reaches an enum: {enums:?}"
    );
    for &(schema, field) in REQUEST_STRING {
        assert!(seen.contains(schema), "{schema} is a request body");
        let p = property(&api, schema, field).unwrap_or_else(|| panic!("{schema}.{field}"));
        assert!(plain_string(p), "{schema}.{field}: {p}");
    }
    Ok(())
}

/// A registered usage-type catalog that never answers.
struct HangingUsageTypes;
#[async_trait::async_trait]
impl bss_products_sdk::usage_types::UsageTypeCatalog for HangingUsageTypes {
    async fn resolve(
        &self,
        _: &toolkit_security::SecurityContext,
        _: &str,
    ) -> bss_products_sdk::usage_types::UsageTypeAnswer {
        std::future::pending().await
    }
    async fn list(
        &self,
        _: &toolkit_security::SecurityContext,
        _: Option<&str>,
        _: Option<&str>,
        _: u32,
        _: Option<&str>,
    ) -> Result<
        bss_products_sdk::usage_types::UsageTypePage,
        toolkit_canonical_errors::CanonicalError,
    > {
        std::future::pending().await
    }
}

/// RS-42: a registered catalog is bounded by `usage_type_resolver_timeout_ms` as the collector
/// adapter is: a resolve that outlives it is `Unavailable` and a list a 503, so a hanging catalog
/// never hangs a submit, an approve or the pick-list.
#[tokio::test(start_paused = true)]
async fn a_registered_usage_type_catalog_is_bounded_by_the_resolver_timeout() {
    use bss_products_sdk::usage_types::{UsageTypeAnswer, UsageTypeCatalog};
    struct NoConfig;
    impl toolkit::config::ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
            None
        }
    }
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn UsageTypeCatalog>(Arc::new(HangingUsageTypes));
    let ctx = GearCtx::new(
        "bss-products",
        uuid::Uuid::new_v4(),
        Arc::new(NoConfig),
        hub,
        tokio_util::sync::CancellationToken::new(),
    );
    let cfg = ProductsConfig {
        usage_type_resolver_timeout_ms: 50,
        ..ProductsConfig::default()
    };
    let (catalog, source) = super::resolve_usage_type_catalog(&ctx, &cfg);
    assert_eq!(source, USAGE_TYPE_SOURCE_REGISTRY);
    let caller = crate::test_support::authed_ctx(uuid::Uuid::new_v4());
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        catalog.resolve(&caller, "storage"),
    )
    .await
    .expect("the resolve is bounded");
    assert!(matches!(answer, UsageTypeAnswer::Unavailable), "{answer:?}");
    let listed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        catalog.list(&caller, None, None, 50, None),
    )
    .await
    .expect("the list is bounded");
    assert_eq!(listed.unwrap_err().status_code(), 503);
}

/// P-D-227 (ask 42): the counts op declares its 503, as every products op does, its narrowing and
/// its answer; the list names its order and its refusals.
#[tokio::test]
async fn the_unit_reads_say_how_they_count_and_order() -> anyhow::Result<()> {
    let api = served_spec().await?;
    let counts = &api["paths"]["/bss-products/v1/approval-units/counts"]["get"];
    assert!(
        !counts["responses"]["503"].is_null(),
        "the counts declare 503: {counts}"
    );
    let names = |op: &serde_json::Value| -> Vec<String> {
        op["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["in"] == "query")
            .map(|p| p["name"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(names(counts), ["state", "kind", "ref_id"]);
    let text = counts["description"].as_str().unwrap_or_default();
    for said in [
        "by_state",
        "by_kind",
        "total",
        "one grouped statement",
        // The phase 9 review's theme C: a kind products does not record is refused.
        "a kind other than sku_publish, sku_change or sku_retire (on kind)",
    ] {
        assert!(text.contains(said), "the counts say {said}: {text}");
    }
    assert_eq!(
        counts["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ProductsApprovalUnitCounts",
        "{counts}"
    );
    let list = &api["paths"]["/bss-products/v1/approval-units"]["get"];
    assert!(names(list).iter().any(|n| n == "$orderby"), "{list}");
    // The phase 9 review's theme I (R66): the order is declared through the toolkit, so the
    // contract lists the one field it takes, in both directions; `$orderby` is one parameter, and
    // the counts take no order.
    assert_eq!(
        list["x-odata-orderby"]["allowedFields"],
        serde_json::json!(["submitted_at asc", "submitted_at desc"]),
        "{list}"
    );
    assert_eq!(
        names(list).iter().filter(|n| *n == "$orderby").count(),
        1,
        "{list}"
    );
    assert!(counts["x-odata-orderby"].is_null(), "{counts}");
    let text = list["description"].as_str().unwrap_or_default();
    for said in [
        "submitted_at desc",
        "ORDER_WITH_CURSOR",
        "INVALID_ORDERBY_FIELD",
    ] {
        assert!(text.contains(said), "the list says {said}: {text}");
    }
    Ok(())
}

/// P-D-228 (ask 28): every unit carries `caller_can_approve`, a required boolean whose text says
/// it judges Approve only and not the grant, and the list's text names it; the reject door no
/// longer claims `SOD_VIOLATION`, which only the approve answers.
#[tokio::test]
async fn every_unit_says_whether_its_reader_may_approve_it() -> anyhow::Result<()> {
    let api = served_spec().await?;
    let list = api["paths"]["/bss-products/v1/approval-units"]["get"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(list.contains("caller_can_approve"), "{list}");
    let flag = property(&api, "UnitDto", "caller_can_approve").expect("the flag");
    assert_eq!(flag["type"], "boolean", "{flag}");
    let required = component(&api, "UnitDto")["required"].as_array().unwrap();
    for name in [
        "caller_can_approve",
        "caller_can_reject",
        "caller_can_withdraw",
    ] {
        assert!(required.iter().any(|r| r == name), "{name} {required:?}");
        assert_eq!(
            property(&api, "UnitDto", name).expect(name)["type"],
            "boolean"
        );
    }
    let said = flag["description"].as_str().unwrap_or_default();
    assert!(
        said.contains("Approve only") && said.contains("403"),
        "the flag says what it judges: {said}"
    );
    let reject = api["paths"]["/bss-products/v1/approval-units/{id}/reject"]["post"]["description"]
        .as_str()
        .unwrap_or_default();
    assert!(!reject.contains("SOD_VIOLATION"), "{reject}");
    assert!(reject.contains("separation of duties"), "{reject}");
    Ok(())
}

/// P-D-247 (ask 56): the picker declares its `Cache-Control` on its 200. Ask 46 and P-D-246
/// (ask 52): the SKU list's text names `$filter=id in (…)` as the multi-id read with its bounds,
/// and the list and the counts declare and name the picker keys `priced_in`, `not_priced_in` and
/// `not_in_revision`.
#[tokio::test]
async fn the_picker_reads_say_how_they_cache_and_narrow() -> anyhow::Result<()> {
    let api = served_spec().await?;
    let header = &api["paths"]["/bss-products/v1/usage-types"]["get"]["responses"]["200"]["headers"]
        ["Cache-Control"];
    assert!(
        header.is_object(),
        "the 200 declares Cache-Control: {header}"
    );
    let said = header["description"].as_str().unwrap_or_default();
    assert!(said.contains("private, max-age=60"), "{said}");
    for path in ["/bss-products/v1/skus", "/bss-products/v1/skus/counts"] {
        let op = &api["paths"][path]["get"];
        let names: Vec<&str> = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["in"] == "query")
            .filter_map(|p| p["name"].as_str())
            .collect();
        for key in ["priced_in", "not_priced_in", "not_in_revision"] {
            assert!(names.contains(&key), "{path} declares {key}: {names:?}");
        }
        let text = op["description"].as_str().unwrap_or_default();
        for said in [
            "priced_in",
            "not_priced_in",
            "not_in_revision",
            "plan read",
            "USAGE_FORBIDDEN",
            "USAGE_UNAVAILABLE",
        ] {
            assert!(text.contains(said), "{path} says {said}: {text}");
        }
    }
    let list = api["paths"]["/bss-products/v1/skus"]["get"]["description"]
        .as_str()
        .unwrap_or_default();
    for said in ["$filter=id in (", "$top", "200", "8 KiB"] {
        assert!(list.contains(said), "the list says {said}: {list}");
    }
    Ok(())
}

/// A gear's hub as `init` leaves it: a PDP that allows `tenant` in its own tenant and a legacy
/// types-registry client that accepts the gear's label schemas; no `TypesRegistryApi`, no
/// discovery plugin.
async fn initialized(
    tenant: uuid::Uuid,
) -> (
    BssProductsGear,
    Arc<toolkit::ClientHub>,
    crate::test_support::TestDsn,
) {
    use toolkit::Gear;
    use toolkit::contracts::DatabaseCapability;
    struct NoConfig;
    impl toolkit::config::ConfigProvider for NoConfig {
        fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
            None
        }
    }
    let gear = BssProductsGear::default();
    let dsn = crate::test_support::TestDsn::new("products-quota-init-");
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(4),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(&db, gear.migrations())
        .await
        .unwrap();
    let hub = Arc::new(toolkit::ClientHub::new());
    hub.register::<dyn authz_resolver_sdk::AuthZResolverApi>(Arc::new(
        crate::test_support::Spanning {
            tenants: vec![tenant],
        },
    ));
    hub.register::<dyn types_registry_sdk::TypesRegistryClient>(Arc::new(
        crate::test_support::Accepting,
    ));
    let ctx = GearCtx::new(
        "bss-products",
        uuid::Uuid::new_v4(),
        Arc::new(NoConfig),
        hub.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .with_db(toolkit_db::DBProvider::new(db));
    gear.init(&ctx).await.unwrap();
    (gear, hub, dsn)
}

/// After the gear's own init, the hub's one `dyn ProductsQuotaCatalogClientV1` is Products'
/// client over the gear's database: it reads as its caller, so an empty catalog answers an empty
/// page and an unknown version 404. Before the init nothing answers.
#[tokio::test]
async fn the_hubs_quota_catalog_is_products_client_after_the_gears_init() {
    use bss_products_sdk::quota::catalog::{
        QuotaAvailabilityFilter, QuotaDefinitionListRequest, QuotaVersion,
    };
    let empty = toolkit::ClientHub::new();
    assert!(
        empty
            .get::<dyn bss_products_sdk::ProductsQuotaCatalogClientV1>()
            .is_err()
    );
    let tenant = uuid::Uuid::new_v4();
    let (_gear, hub, _dsn) = initialized(tenant).await;
    let client = hub
        .get::<dyn bss_products_sdk::ProductsQuotaCatalogClientV1>()
        .unwrap();
    let caller = crate::test_support::authed_ctx(tenant);
    let page = client
        .list_quota_definitions(
            &caller,
            QuotaDefinitionListRequest {
                sku_id: uuid::Uuid::new_v4(),
                sku_version: QuotaVersion::new(1).unwrap(),
                subject_type_ref: None,
                resource_type_ref: None,
                profile_id: None,
                locale: None,
                availability: QuotaAvailabilityFilter::All,
                limit: None,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert!(page.items.is_empty() && page.next_cursor.is_none());
    let missing = client
        .get_quota_definition(
            &caller,
            uuid::Uuid::new_v4(),
            QuotaVersion::new(1).unwrap(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            missing,
            toolkit::api::canonical_prelude::CanonicalError::NotFound { .. }
        ),
        "{missing:?}"
    );
}

/// The router the gear's own `register_rest` serves over a fresh database whose hub holds no
/// types-registry and no discovery plugin, under a PDP that allows the database's tenant; the
/// first router keeps the outbox running.
async fn production_router() -> (Router, Router, crate::test_support::TestDsn, uuid::Uuid) {
    let (db, _, tenant, dsn) = crate::test_support::test_db().await;
    let (holder, state) = crate::test_support::rest_app_on_db(
        tenant,
        |_, _| Router::new(),
        crate::test_support::resolved_usage_types(),
        "test",
        db,
    )
    .await;
    let (router, _) =
        BssProductsGear::registered_rest(state, crate::test_support::flat_in_enforcer(tenant));
    (holder, router, dsn, tenant)
}

/// The gear-built router closes discovery without an adapter: a stored target's discovery is 501
/// `QUOTA_DISCOVERY_CLOSED` before any read or write, and no snapshot is stored.
#[tokio::test]
async fn the_production_wiring_closes_discovery_before_any_effect() {
    let (_holder, app, dsn, tenant) = production_router().await;
    let (db, scope) = crate::test_support::repo_connection(&dsn, tenant).await;
    let target = crate::infra::storage::repo::quota_intake_repo::insert_target(
        &db.conn().unwrap(),
        &scope.tenant_only(),
        tenant,
        crate::domain::quota::NewQuotaTarget {
            plugin_instance_id: crate::quota_test_support::TEST_PLUGIN.to_owned(),
            target_key: "region-1".to_owned(),
            name: "Region 1".to_owned(),
        },
        uuid::Uuid::new_v4(),
        crate::infra::storage::stored_now(),
    )
    .await
    .unwrap();
    let response = crate::test_support::post(
        &app,
        tenant,
        &format!(
            "/bss-products/v1/quota-adapter-targets/{}/discoveries",
            target.id
        ),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(response.status(), 501);
    let body = crate::test_support::body_json(response).await;
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.starts_with("QUOTA_DISCOVERY_CLOSED")),
        "{body}"
    );
    assert_eq!(
        crate::test_support::raw_i64(&dsn, "SELECT COUNT(*) AS v FROM products_quota_snapshot")
            .await,
        0
    );
}

/// The gear-built router answers 503 `QUOTA_REGISTRY_UNAVAILABLE` for a target create when the
/// hub holds no types-registry, and binds nothing.
#[tokio::test]
async fn the_production_wiring_answers_503_without_a_registry() {
    let (_holder, app, dsn, tenant) = production_router().await;
    let response = crate::test_support::post(
        &app,
        tenant,
        "/bss-products/v1/quota-adapter-targets",
        serde_json::json!({
            "plugin_instance_id": crate::quota_test_support::TEST_PLUGIN,
            "target_key": "region-1",
            "name": "Region 1"
        }),
    )
    .await;
    assert_eq!(response.status(), 503);
    let body = crate::test_support::body_json(response).await;
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.starts_with("QUOTA_REGISTRY_UNAVAILABLE")),
        "{body}"
    );
    assert_eq!(
        crate::test_support::raw_i64(
            &dsn,
            "SELECT COUNT(*) AS v FROM products_quota_adapter_target"
        )
        .await,
        0
    );
}
