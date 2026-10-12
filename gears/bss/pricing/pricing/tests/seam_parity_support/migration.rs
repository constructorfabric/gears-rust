//! Upgrade from the committed pre-seam chain; no golden regeneration.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use crate::{plan_support as p, seam_support as s};
use bss_pricing::{
    domain::reference_op::{OpKind, RefKind},
    infra::reference_work::{self, Ref, Work},
};
use bss_pricing::{
    infra::storage::repo::{price_book_entry_repo, reference_op_repo},
    module::BssPricingGear,
};
use sea_orm::EntityTrait;
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
use serde_json::json;
use std::sync::Arc;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::secure::SecureEntityExt;
use toolkit_db::{DBProvider, DbError, migration_runner::run_migrations_for_testing};
use uuid::Uuid;

async fn execute(raw: &sea_orm::DatabaseConnection, sql: &str, values: Vec<sea_orm::Value>) {
    let backend = raw.get_database_backend();
    let mut n = 0;
    let sql = if backend == DbBackend::Postgres {
        sql.chars()
            .map(|c| {
                if c == '?' {
                    n += 1;
                    format!("${n}")
                } else {
                    c.to_string()
                }
            })
            .collect::<String>()
    } else {
        sql.into()
    };
    raw.execute_raw(Statement::from_sql_and_values(backend, sql, values))
        .await
        .unwrap();
}
pub async fn dump(dsn: &str) -> String {
    let raw = Database::connect(dsn).await.unwrap();
    if raw.get_database_backend() == DbBackend::Postgres {
        crate::schema_dump::postgres_dump(&raw).await
    } else {
        crate::schema_dump::sqlite_dump(&raw).await
    }
}
/// Both arguments are separate databases: the first empty, the second freshly fully migrated.
#[expect(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "ordered migration proof retains before/after state in one scenario"
)]
pub async fn upgrade(
    db: DBProvider<DbError>,
    dsn: p::entry_support::TestDsn,
    fresh: p::entry_support::TestDsn,
) {
    // Through 000017, plus the outbox migrations appended after 000020. 000018, 000019 and
    // 000020 stay out so the second run applies them in production order.
    let prior = BssPricingGear::default()
        .migrations()
        .into_iter()
        .filter(|m| {
            let name = m.name();
            !(name.contains("000018")
                || name.contains("000019")
                || name.contains("000020")
                || name.contains("000021")
                || name.contains("000022")
                || name.contains("000023"))
        })
        .collect();
    let applied = run_migrations_for_testing(&db.db(), prior).await.unwrap();
    assert!(
        applied
            .applied_names
            .iter()
            .any(|s| s == "m20260929_000017_revision_scheduled")
    );
    assert!(!applied.applied_names.iter().any(|s| {
        s.contains("000018")
            || s.contains("000019")
            || s.contains("000020")
            || s.contains("000021")
            || s.contains("000022")
            || s.contains("000023")
    }));
    let catalog = Arc::new(p::Catalog::default());
    let sku = catalog.sku(bss_products_sdk::models::SkuType::Usage);
    let tenant = Uuid::now_v7();
    let ctx = crate::plan_support::entry_support::user_of(tenant);
    let book = Uuid::now_v7();
    let plan_id = Uuid::now_v7();
    let revision = Uuid::now_v7();
    let now = time::OffsetDateTime::now_utc();
    let entry = Uuid::new_v4();
    let price = Uuid::new_v4();
    let item = Uuid::new_v4();
    let raw = Database::connect(&dsn).await.unwrap();
    execute(&raw,"INSERT INTO pricing_price_book (id,tenant_id,code,name,currency,version,created_at,updated_at) VALUES (?,?,?,?,?,1,?,?)",vec![book.into(),tenant.into(),"LEGACY".into(),"Legacy".into(),"EUR".into(),now.into(),now.into()]).await;
    execute(&raw,"INSERT INTO pricing_plan (id,tenant_id,code,name,published_rev,version,created_by,created_at,updated_at) VALUES (?,?,?,?,1,1,?,?,?)",vec![plan_id.into(),tenant.into(),"LEGACY".into(),"Legacy".into(),ctx.subject_id().into(),now.into(),now.into()]).await;
    execute(&raw,"INSERT INTO pricing_plan_revision (id,tenant_id,plan_id,rev_no,book_id,state,version,created_by,created_at,updated_at) VALUES (?,?,?,1,?,'published',1,?,?,?)",vec![revision.into(),tenant.into(),plan_id.into(),book.into(),ctx.subject_id().into(),now.into(),now.into()]).await;
    execute(&raw,"INSERT INTO pricing_price_book_entry (id,tenant_id,book_id,sku_id,charge_kind,model,reservation_id,reference_state,version,created_at,updated_at) VALUES (?,?,?,?,'usage','per_unit',?,'confirmed',1,?,?)",vec![entry.into(),tenant.into(),book.into(),sku.into(),Uuid::new_v4().into(),now.into(),now.into()]).await;
    execute(&raw,"INSERT INTO pricing_plan_item (id,tenant_id,revision_id,sku_id,price_book_entry_id,treatment,reservation_id,reference_state,version,created_by,created_at,updated_at) VALUES (?,?,?,?,?,'paid',?,'confirmed',1,?,?,?)",vec![item.into(),tenant.into(),revision.into(),sku.into(),entry.into(),Uuid::new_v4().into(),ctx.subject_id().into(),now.into(),now.into()]).await;
    execute(&raw,"INSERT INTO pricing_price (id,tenant_id,price_book_entry_id,version_no,price_json,eligibility,effective_from,state,created_by,version,created_at,updated_at) VALUES (?,?,?,1,?,'all','2026-09-01','approved',?,1,?,?)",vec![price.into(),tenant.into(),entry.into(),json!({"rate":"1"}).into(),ctx.subject_id().into(),now.into(),now.into()]).await;
    let mut operations = vec![];
    for (kind, id, target) in [
        (
            RefKind::Entry,
            entry,
            json!({"price_book_entry":{"book_id":book,"input":{"sku_id":sku,"model":"per_unit","period":null,"dimension_key":null,"invoice_line_override":null}}}),
        ),
        (
            RefKind::PlanItem,
            item,
            json!({"plan_item":{"revision_id":revision,"input":{"sku_id":sku,"price_book_entry_id":entry,"treatment":"paid","included_qty":null,"qty_min":null}}}),
        ),
    ] {
        let old =
            json!({"target":target,"correlation":Uuid::new_v4(),"refusal":null,"receipt":null});
        let work: Work = serde_json::from_value(old.clone()).unwrap();
        let mut op = reference_work::new_op(
            &ctx,
            Ref {
                kind,
                id,
                sku_id: sku,
            },
            &work,
            OpKind::Create,
            Some(Uuid::new_v4()),
            None,
            now,
        )
        .unwrap();
        op.outcome = Some(old.to_string());
        let saved = reference_op_repo::insert(
            &db.conn().unwrap(),
            &toolkit_db::secure::AccessScope::for_tenant(tenant),
            op,
        )
        .await
        .unwrap();
        operations.push(saved);
    }
    let result = run_migrations_for_testing(&db.db(), BssPricingGear::default().migrations())
        .await
        .unwrap();
    assert_eq!(
        result.applied_names,
        [
            "m20260930_000018_usage_rating_policy",
            "m20260930_000019_commercial_receipts",
            "m20261002_000020_plan_summary",
            "m20261002_000021_policy_references_sku",
            "m20261003_000022_price_cancel_and_end",
            "m20261003_000023_book_archive"
        ]
    );
    let f = p::Fixture::on(db, tenant, dsn, catalog).await;
    assert!(
        run_migrations_for_testing(&f.db.db(), BssPricingGear::default().migrations())
            .await
            .unwrap()
            .applied_names
            .is_empty()
    );
    let stored = price_book_entry_repo::find(&f.db.conn().unwrap(), &p::scope(&f), tenant, entry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stored.usage_policy_id,
            stored.usage_policy_version,
            stored.usage_policy_digest
        ),
        (None, None, None)
    );
    assert!(
        p::items(&f, revision)
            .await
            .iter()
            .any(|row| row.id == item)
    );
    let stored_price = bss_pricing::infra::storage::repo::price_repo::find(
        &f.db.conn().unwrap(),
        &p::scope(&f),
        tenant,
        price,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored_price.id, price);
    assert_eq!(stored_price.state, "approved");
    for old in operations {
        let stored =
            reference_op_repo::find(&f.db.conn().unwrap(), &p::scope(&f), tenant, old.op_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(stored, old);
        let _: Work = serde_json::from_str(stored.outcome.as_ref().unwrap()).unwrap();
    }
    let read = f
        .call(
            "GET",
            &format!("/price-book-entries/{entry}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(read.0, 200, "{read:?}");
    assert_eq!(read.1["usage_rating_policy"], serde_json::Value::Null);
    let resolved = s::resolve(&f, &format!("plan_revision_id={revision}&date=2026-10-01")).await;
    assert_eq!(resolved.0, 200, "{resolved:?}");
    assert!(resolved.1.to_string().contains(&price.to_string()));
    assert_eq!(s::commercial_counts(&f).await, (0, 0));
    assert!(
        bss_pricing::infra::storage::entity::hold::Entity::find()
            .secure()
            .scope_with(&p::scope(&f))
            .all(&f.db.conn().unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    let missing = f
        .call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"sku_id":sku,"model":"per_unit"}),
            None,
            Some("new-policyless"),
        )
        .await;
    assert_eq!(missing.0, 400, "{missing:?}");
    assert!(missing.1.to_string().contains("MISSING_RATING_POLICY"));
    let upgraded = dump(&f.dsn).await;
    assert_eq!(upgraded, dump(&fresh).await, "fresh install equals upgrade");
    for table in ["pricing_plan_item", "pricing_price"] {
        let sql = if raw.get_database_backend() == DbBackend::Postgres {
            format!(
                "SELECT column_name AS name FROM information_schema.columns WHERE table_schema='bss' AND table_name='{table}'"
            )
        } else {
            format!("PRAGMA table_info({table})")
        };
        let rows = raw
            .query_all_raw(Statement::from_string(raw.get_database_backend(), sql))
            .await
            .unwrap();
        assert!(!rows.is_empty());
        assert!(
            rows.iter()
                .all(|r| !r.try_get::<String>("", "name").unwrap().contains("policy"))
        );
    }
    // The full dump is compared above; quote the actual composite-key/index DDL for the handoff.
    for line in upgraded.lines().filter(|l| {
        l.contains("pricing_entry_policy_fk")
            || l.contains("pricing_price_book_entry_key")
            || l.contains("PRIMARY KEY (tenant_id")
            || l.contains("FOREIGN KEY (tenant_id")
            || l.contains("UNIQUE (tenant_id")
    }) {
        eprintln!("{line}");
    }
}
