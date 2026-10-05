#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
mod seam_support;
use bss_pricing_sdk::read::{PriceModel, PricingReadV1};
use seam_support::ReadFixture;

#[tokio::test]
async fn approved_money_is_exact_and_authorized() {
    let f = ReadFixture::new().await;
    let price = f
        .provider
        .price(&f.ctx, f.price_query.clone())
        .await
        .unwrap();
    match price.model {
        PriceModel::PerUnit { unit_amount } => assert_eq!(unit_amount.to_string(), "0.047"),
        other => panic!("unexpected price model: {other:?}"),
    }
    assert!(
        f.provider
            .price(&f.denied_ctx, f.price_query.clone())
            .await
            .is_err()
    );
}

/// D-520, amended: the typed read says a cancelled price is cancelled. It stays readable by id with
/// its money as approved, and it is never the price in force: resolve on a day inside its window
/// still binds the approved price before it.
#[tokio::test]
async fn a_cancelled_price_reads_cancelled_by_id_and_is_never_in_force() {
    use bss_pricing_sdk::read::{PriceQuery, PriceState};
    let f = ReadFixture::new().await;
    let approved = f
        .provider
        .price(&f.ctx, f.price_query.clone())
        .await
        .unwrap();
    assert_eq!(approved.state, PriceState::Approved);
    let cancelled = seam_support::put(
        &f.fixture,
        approved.price_book_entry_id,
        seam_support::Row {
            price: serde_json::json!({"rate":"0.09"}),
            from: "2026-09-10",
            state: "cancelled",
            version_no: 2,
            ..seam_support::Row::default()
        },
    )
    .await;
    let read = f
        .provider
        .price(
            &f.ctx,
            PriceQuery {
                price_id: cancelled,
                ..f.price_query.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(read.state, PriceState::Cancelled);
    assert_eq!(read.effective_from, seam_support::date("2026-09-10"));
    match read.model {
        PriceModel::PerUnit { unit_amount } => assert_eq!(unit_amount.to_string(), "0.09"),
        other => panic!("unexpected price model: {other:?}"),
    }
    assert_eq!(f.resolve_query.date, seam_support::date("2026-09-15"));
    let resolved = f
        .provider
        .resolve(&f.ctx, f.resolve_query.clone())
        .await
        .unwrap();
    let binding = resolved.cells[0].binding.as_ref().unwrap();
    assert_eq!(binding.price.price_id, approved.price_id);
    assert_eq!(binding.price.state, PriceState::Approved);
}

#[tokio::test]
async fn rest_and_sdk_resolve_the_same_stored_matrix() {
    let f = ReadFixture::new().await;
    let sdk = f
        .provider
        .resolve(&f.ctx, f.resolve_query.clone())
        .await
        .unwrap();
    let (status, rest) = seam_support::resolve(
        &f.fixture,
        &format!(
            "plan_revision_id={}&date={}",
            f.resolve_query.revision_id, f.resolve_query.date
        ),
    )
    .await;
    assert_eq!(status, 200, "{rest}");
    assert_eq!(
        sdk.cells.len(),
        rest["items"][0]["chains"].as_array().unwrap().len()
    );
    let binding = sdk.cells[0].binding.as_ref().unwrap();
    assert_eq!(
        binding.price.price_id.to_string(),
        rest["items"][0]["chains"][0]["binding"]["price_id"]
    );
    assert_eq!(binding.unit.as_deref(), Some("cloudlet_hour"));
}

#[tokio::test]
async fn read_actions_are_distinct_and_no_actor_name_bypasses_pdp() {
    use bss_pricing_sdk::read::PlanQuery;
    let f = ReadFixture::new().await;
    let plan_reader = plan_support::holding(&f.fixture, "plan:read");
    let price_reader = plan_support::holding(&f.fixture, "price:read");
    let bindings = f
        .provider
        .resolve(&plan_reader, f.resolve_query.clone())
        .await
        .unwrap();
    let plan = PlanQuery {
        catalog: f.price_query.catalog.clone(),
        plan_id: bindings.plan_id,
    };
    assert_eq!(
        f.provider
            .current_revision(&plan_reader, plan.clone())
            .await
            .unwrap()
            .revision_id,
        bindings.revision_id
    );
    assert_eq!(
        f.provider
            .price(&plan_reader, f.price_query.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    f.provider
        .price(&price_reader, f.price_query.clone())
        .await
        .unwrap();
    assert_eq!(
        f.provider
            .resolve(&price_reader, f.resolve_query.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.provider
            .current_revision(&price_reader, plan)
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.provider
            .price(
                &toolkit_security::SecurityContext::anonymous(),
                f.price_query.clone()
            )
            .await
            .unwrap_err()
            .status_code(),
        401
    );
    assert_eq!(
        f.provider
            .resolve(
                &toolkit_security::SecurityContext::anonymous(),
                f.resolve_query.clone()
            )
            .await
            .unwrap_err()
            .status_code(),
        401
    );
    let system = plan_support::holding(&f.fixture, "bss-rating.system");
    assert_eq!(
        f.provider
            .price(&system, f.price_query.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    assert_eq!(
        f.provider
            .resolve(&system, f.resolve_query.clone())
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    let mut foreign = f.price_query.clone();
    foreign.catalog.tenant_id = uuid::Uuid::new_v4();
    assert_eq!(
        f.provider
            .price(&f.ctx, foreign)
            .await
            .unwrap_err()
            .status_code(),
        403
    );
    let mut foreign = f.resolve_query.clone();
    foreign.catalog.tenant_id = uuid::Uuid::new_v4();
    assert_eq!(
        f.provider
            .resolve(&f.ctx, foreign)
            .await
            .unwrap_err()
            .status_code(),
        403
    );
}

#[tokio::test]
async fn closed_money_stays_readable_and_nonapproved_prices_are_indistinguishable() {
    use bss_pricing::infra::storage::repo::price_repo;
    let f = ReadFixture::new().await;
    let original = f
        .provider
        .price(&f.ctx, f.price_query.clone())
        .await
        .unwrap();
    let conn = f.fixture.db.conn().unwrap();
    let scope = plan_support::scope(&f.fixture);
    let tenant = f.ctx.subject_tenant_id();
    let row = price_repo::find(&conn, &scope, tenant, original.price_id)
        .await
        .unwrap()
        .unwrap();
    price_repo::set_window(
        &conn,
        &scope,
        tenant,
        row.id,
        row.version,
        Some(seam_support::date("2026-10-01")),
        true,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    update_by_id(
        &f.fixture,
        "UPDATE pricing_price SET closed_explicitly = TRUE WHERE id = ?",
        original.price_id,
    )
    .await;
    seam_support::put(
        &f.fixture,
        original.price_book_entry_id,
        seam_support::Row {
            price: serde_json::json!({"rate":"0.09"}),
            from: "2026-10-01",
            version_no: 2,
            ..seam_support::Row::default()
        },
    )
    .await;
    let closed = f
        .provider
        .price(&f.ctx, f.price_query.clone())
        .await
        .unwrap();
    assert_eq!(closed.money_digest, original.money_digest);
    assert_eq!(closed.model, original.model);
    assert_eq!(closed.ends_on, Some(seam_support::date("2026-10-01")));
    for (index, state) in ["draft", "pending", "rejected"].into_iter().enumerate() {
        let id = seam_support::put(
            &f.fixture,
            original.price_book_entry_id,
            seam_support::Row {
                price: serde_json::json!({"rate":"0.05"}),
                state,
                version_no: i32::try_from(index).unwrap() + 3,
                ..seam_support::Row::default()
            },
        )
        .await;
        let query = bss_pricing_sdk::read::PriceQuery {
            price_id: id,
            ..f.price_query.clone()
        };
        assert_eq!(
            f.provider
                .price(&f.ctx, query)
                .await
                .unwrap_err()
                .status_code(),
            404
        );
    }
    let foreign_fixture = ReadFixture::new().await;
    for price_id in [uuid::Uuid::new_v4(), foreign_fixture.price_query.price_id] {
        assert_eq!(
            f.provider
                .price(
                    &f.ctx,
                    bss_pricing_sdk::read::PriceQuery {
                        price_id,
                        ..f.price_query.clone()
                    }
                )
                .await
                .unwrap_err()
                .status_code(),
            404
        );
    }
}

#[tokio::test]
async fn unit_and_dimension_identity_are_in_binding_digest_and_selections_are_checked() {
    use bss_pricing_sdk::digest::selected_bindings_digest;
    let f = ReadFixture::new().await;
    let mut resolved = f
        .provider
        .resolve(&f.ctx, f.resolve_query.clone())
        .await
        .unwrap();
    let selected = vec![resolved.cells[0].selection.clone()];
    let before = selected_bindings_digest(&resolved, &selected).unwrap();
    let money = resolved.cells[0]
        .binding
        .as_ref()
        .unwrap()
        .price
        .money_digest;
    resolved.cells[0].binding.as_mut().unwrap().unit = Some("cloudlet_second".into());
    assert_ne!(
        before,
        selected_bindings_digest(&resolved, &selected).unwrap()
    );
    assert_eq!(
        money,
        resolved.cells[0]
            .binding
            .as_ref()
            .unwrap()
            .price
            .money_digest
    );
    resolved.cells[0].binding.as_mut().unwrap().dimension_key = Some("region".into());
    assert_ne!(
        before,
        selected_bindings_digest(&resolved, &selected).unwrap()
    );
    assert!(
        selected_bindings_digest(&resolved, &[selected[0].clone(), selected[0].clone()]).is_err()
    );
    let mut unknown = selected[0].clone();
    unknown.item_id = uuid::Uuid::new_v4();
    assert!(selected_bindings_digest(&resolved, &[unknown]).is_err());
    resolved.cells[0].binding = None;
    assert!(selected_bindings_digest(&resolved, &selected).is_err());
}

#[tokio::test]
async fn sdk_pins_validate_explicit_item_and_dimension() {
    use bss_pricing_sdk::read::PricePin;
    let f = ReadFixture::new().await;
    let resolved = f
        .provider
        .resolve(&f.ctx, f.resolve_query.clone())
        .await
        .unwrap();
    let mut query = f.resolve_query.clone();
    let pin = PricePin {
        item_id: resolved.cells[0].selection.item_id,
        dimension_value: None,
        price_id: f.price_query.price_id,
    };
    query.pins.push(pin.clone());
    assert_eq!(
        f.provider.resolve(&f.ctx, query.clone()).await.unwrap(),
        resolved
    );
    query.pins.push(pin);
    assert_eq!(
        f.provider
            .resolve(&f.ctx, query.clone())
            .await
            .unwrap_err()
            .status_code(),
        400
    );
    query.pins.truncate(1);
    query.pins[0].item_id = uuid::Uuid::new_v4();
    assert_eq!(
        f.provider
            .resolve(&f.ctx, query)
            .await
            .unwrap_err()
            .status_code(),
        400
    );
}

#[tokio::test]
async fn missing_descriptors_remain_nullable_on_rest_and_typed_failure_on_sdk() {
    use bss_pricing::api::pricing_read::PricingReadProvider;
    let w = seam_support::world().await;
    let provider = PricingReadProvider::new(
        w.f.state.clone(),
        std::sync::Arc::new(plan_support::entry_support::enforcer_for(
            w.f.ctx.subject_tenant_id(),
        )),
    );
    let query = bss_pricing_sdk::read::ResolveQuery {
        catalog: bss_pricing_sdk::read::CatalogRef {
            tenant_id: w.f.ctx.subject_tenant_id(),
        },
        revision_id: w.revision,
        date: seam_support::date("2026-09-15"),
        item_id: None,
        pins: vec![],
    };
    let (status, rest) = seam_support::resolve(
        &w.f,
        &format!("plan_revision_id={}&date=2026-09-15", w.revision),
    )
    .await;
    assert_eq!(status, 200);
    assert!(rest["items"][0]["sku_version"].is_null());
    let error = provider.resolve(&w.f.ctx, query).await.unwrap_err();
    let body = serde_json::to_value(toolkit_canonical_errors::Problem::from_error(&error).unwrap())
        .unwrap();
    assert_eq!(
        body["context"]["violations"][0]["type"], "INCOMPLETE_COMMERCIAL_INPUTS",
        "{body}"
    );
    assert_eq!(body["context"]["violations"][0]["subject"], "sku_version");
}

#[tokio::test]
async fn current_revision_promotes_due_revision_even_without_ticker() {
    use bss_pricing::infra::storage::{
        entity::plan_revision,
        repo::{plan_repo, plan_revision_repo},
    };
    use bss_pricing_sdk::read::PlanQuery;
    let f = ReadFixture::new().await;
    let resolved = f
        .provider
        .resolve(&f.ctx, f.resolve_query.clone())
        .await
        .unwrap();
    let query = PlanQuery {
        catalog: f.price_query.catalog.clone(),
        plan_id: resolved.plan_id,
    };
    let scope = plan_support::scope(&f.fixture);
    let conn = f.fixture.db.conn().unwrap();
    let tenant = f.ctx.subject_tenant_id();
    let old = plan_revision_repo::find(&conn, &scope, tenant, resolved.revision_id)
        .await
        .unwrap()
        .unwrap();
    let id = uuid::Uuid::now_v7();
    let future = time::OffsetDateTime::now_utc().date() + time::Duration::days(3);
    plan_revision_repo::insert(
        &conn,
        &scope,
        plan_revision::Model {
            id,
            rev_no: 2,
            state: "draft".into(),
            available_from: Some(future),
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            ..old
        },
    )
    .await
    .unwrap();
    let unit = plan_support::lock(&f.fixture, id).await;
    plan_revision_repo::schedule(
        &conn,
        &scope,
        tenant,
        id,
        unit,
        time::OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    assert_eq!(
        f.provider
            .current_revision(&f.ctx, query.clone())
            .await
            .unwrap()
            .revision_id,
        resolved.revision_id
    );
    update_by_id(
        &f.fixture,
        "UPDATE pricing_plan_revision SET available_from = '2020-01-01' WHERE id = ?",
        id,
    )
    .await;
    let counts = seam_support::written(&f.fixture).await;
    assert_eq!(
        f.provider
            .current_revision(&f.ctx, query.clone())
            .await
            .unwrap()
            .revision_id,
        id
    );
    assert_eq!(
        plan_repo::find(&conn, &scope, tenant, query.plan_id)
            .await
            .unwrap()
            .unwrap()
            .published_rev,
        Some(2)
    );
    let after = seam_support::written(&f.fixture).await;
    assert!(after[0] > counts[0]);
    assert!(after[2] > counts[2]);
    assert_eq!(
        f.provider
            .current_revision(&f.ctx, query)
            .await
            .unwrap()
            .revision_id,
        id
    );
    assert_eq!(
        seam_support::written(&f.fixture).await,
        after,
        "switch emits and audits once"
    );
}

async fn update_by_id(f: &plan_support::Fixture, sql: &str, id: uuid::Uuid) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let changed = Database::connect(&f.dsn)
        .await
        .unwrap()
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            sql,
            [id.into()],
        ))
        .await
        .unwrap();
    assert_eq!(changed.rows_affected(), 1);
}

#[tokio::test]
async fn dimension_matrix_default_fallback_and_uncovered_cells_match_rest() {
    let f = ReadFixture::new().await;
    let initial = f
        .provider
        .price(&f.ctx, f.price_query.clone())
        .await
        .unwrap();
    seam_support::dimension(&f.fixture, "region", &["eu", "us"]).await;
    update_by_id(
        &f.fixture,
        "UPDATE pricing_price_book_entry SET dimension_key = 'region' WHERE id = ?",
        initial.price_book_entry_id,
    )
    .await;
    let own = seam_support::put(
        &f.fixture,
        initial.price_book_entry_id,
        seam_support::Row {
            dim: Some("eu"),
            price: serde_json::json!({"rate":"0.052"}),
            version_no: 2,
            ..seam_support::Row::default()
        },
    )
    .await;
    let resolved = f
        .provider
        .resolve(&f.ctx, f.resolve_query.clone())
        .await
        .unwrap();
    let (status, rest) = seam_support::resolve(
        &f.fixture,
        &format!(
            "plan_revision_id={}&date={}",
            f.resolve_query.revision_id, f.resolve_query.date
        ),
    )
    .await;
    assert_eq!(status, 200);
    let chains = rest["items"][0]["chains"].as_array().unwrap();
    assert_eq!(resolved.cells.len(), chains.len());
    for (cell, chain) in resolved.cells.iter().zip(chains) {
        assert_eq!(
            serde_json::to_value(&cell.selection.dimension_value).unwrap(),
            chain["dim_value"]
        );
        let binding = cell.binding.as_ref().unwrap();
        assert_eq!(binding.dimension_key.as_deref(), Some("region"));
        assert_eq!(
            binding.price.price_id.to_string(),
            chain["binding"]["price_id"]
        );
        assert_eq!(
            binding.via_default,
            cell.selection.dimension_value.as_deref() == Some("us")
        );
    }
    assert_eq!(
        resolved.cells[1].binding.as_ref().unwrap().price.price_id,
        own
    );
    let mut query = f.resolve_query.clone();
    query.date = seam_support::date("2026-08-01");
    let uncovered = f.provider.resolve(&f.ctx, query).await.unwrap();
    assert_eq!(uncovered.cells.len(), 3);
    assert!(uncovered.cells.iter().all(|c| c.binding.is_none()));
}
