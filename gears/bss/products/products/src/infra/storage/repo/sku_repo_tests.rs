#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::category::NewCategory;
use crate::domain::sku::NewSku;
use crate::infra::storage::repo::*;
use crate::infra::storage::repo::{HeadWrite, insert_category};
use crate::infra::storage::{RepoError, RepoRefusal};
use crate::test_support::test_db;
use bss_products_sdk::models::{Lifecycle, SkuType};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner};

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}
fn new_sku(code: &str, name: &str, cat: uuid::Uuid) -> NewSku {
    NewSku {
        code: code.into(),
        name: name.into(),
        r#type: SkuType::Usage,
        category_id: Some(cat),
        description: String::new(),
        sellable: true,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
    }
}

#[tokio::test]
async fn duplicate_code_and_name_are_refused_by_the_database_with_their_own_codes() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = insert_category(
        &conn,
        &scope,
        tenant,
        NewCategory {
            code: "hosting".into(),
            name: "Hosting".into(),
            is_default: true,
            sort_order: 0,
        },
        now(),
    )
    .await
    .unwrap();
    insert_sku(
        &conn,
        &scope,
        tenant,
        new_sku("STORAGE", "Storage", cat.id),
        tenant,
        now(),
    )
    .await
    .unwrap();
    assert!(matches!(
        insert_sku(
            &conn,
            &scope,
            tenant,
            new_sku("STORAGE", "Other", cat.id),
            tenant,
            now()
        )
        .await,
        Err(RepoError::Refused(RepoRefusal::SkuCodeTaken))
    ));
    assert!(matches!(
        insert_sku(
            &conn,
            &scope,
            tenant,
            new_sku("OTHER", "Storage", cat.id),
            tenant,
            now()
        )
        .await,
        Err(RepoError::Refused(RepoRefusal::SkuNameTaken))
    ));
}
#[tokio::test]
async fn the_lock_is_conditional_and_the_second_taker_gets_false() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("A", "A", cat), tenant, now())
        .await
        .unwrap();
    let owner = uuid::Uuid::new_v4();
    assert!(
        try_lock_sku(&conn, &scope, tenant, s.id, owner, s.revision)
            .await
            .unwrap()
    );
    assert!(
        !try_lock_sku(
            &conn,
            &scope,
            tenant,
            s.id,
            uuid::Uuid::new_v4(),
            s.revision
        )
        .await
        .unwrap()
    );
    unlock_sku(&conn, &scope, tenant, s.id, owner, None)
        .await
        .unwrap();
    assert!(
        try_lock_sku(
            &conn,
            &scope,
            tenant,
            s.id,
            uuid::Uuid::new_v4(),
            s.revision
        )
        .await
        .unwrap()
    );
}
#[tokio::test]
async fn a_stale_revision_does_not_write_and_a_published_sku_is_not_a_draft() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("A", "A", cat), tenant, now())
        .await
        .unwrap();
    let mut c = bss_products_sdk::models::SkuContent::from(&s);
    c.name = "B".into();
    assert!(matches!(
        update_sku_draft(&conn, &scope, tenant, s.id, s.revision + 5, &c, now())
            .await
            .unwrap(),
        HeadWrite::Unmatched
    ));
    assert!(matches!(
        set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            now()
        )
        .await
        .unwrap(),
        HeadWrite::Written(_)
    ));
    assert!(
        matches!(
            update_sku_draft(&conn, &scope, tenant, s.id, s.revision + 1, &c, now())
                .await
                .unwrap(),
            HeadWrite::Unmatched
        ),
        "the draft guard is in the WHERE clause"
    );
}
#[tokio::test]
async fn a_pending_sku_takes_no_ordinary_write_and_no_fence() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("A", "A", cat), tenant, now())
        .await
        .unwrap();
    assert!(
        try_lock_sku(
            &conn,
            &scope,
            tenant,
            s.id,
            uuid::Uuid::new_v4(),
            s.revision
        )
        .await
        .unwrap()
    );
    let c = bss_products_sdk::models::SkuContent::from(&s);
    assert!(matches!(
        update_sku_draft(&conn, &scope, tenant, s.id, s.revision, &c, now())
            .await
            .unwrap(),
        HeadWrite::Unmatched
    ));
    assert!(matches!(
        fence_sku(
            &conn,
            &scope,
            tenant,
            s.id,
            Fence::TypeChange,
            uuid::Uuid::new_v4(),
            now()
        )
        .await
        .unwrap(),
        HeadWrite::Unmatched
    ));
}
#[tokio::test]
async fn versions_append_and_resolve_as_of() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("A", "A", cat), tenant, now())
        .await
        .unwrap();
    let c = bss_products_sdk::models::SkuContent::from(&s);
    let d = |s: &str| {
        time::Date::parse(s, &time::format_description::well_known::Iso8601::DEFAULT).unwrap()
    };
    append_version(&conn, &scope, tenant, s.id, 1, d("2026-09-01"), &c, now())
        .await
        .unwrap();
    let mut c2 = c.clone();
    c2.gl_code = Some("4012".into());
    append_version(&conn, &scope, tenant, s.id, 2, d("2026-10-01"), &c2, now())
        .await
        .unwrap();
    let mut c3 = c2.clone();
    c3.gl_code = Some("4013".into());
    append_version(&conn, &scope, tenant, s.id, 3, d("2026-10-01"), &c3, now())
        .await
        .unwrap(); // same day: allowed, the higher version wins
    assert!(
        matches!(
            append_version(&conn, &scope, tenant, s.id, 4, d("2026-09-20"), &c3, now()).await,
            Err(RepoError::Refused(RepoRefusal::VersionOrder))
        ),
        "no insertion before the latest date"
    );
    assert_eq!(
        version_as_of(&conn, &scope, tenant, s.id, d("2026-09-15"))
            .await
            .unwrap()
            .unwrap()
            .published_version,
        1
    );
    assert_eq!(
        version_as_of(&conn, &scope, tenant, s.id, d("2026-10-01"))
            .await
            .unwrap()
            .unwrap()
            .published_version,
        3
    );
    assert!(
        version_as_of(&conn, &scope, tenant, s.id, d("2026-08-01"))
            .await
            .unwrap()
            .is_none()
    );
}

async fn seed_category(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: uuid::Uuid,
) -> uuid::Uuid {
    insert_category(
        runner,
        scope,
        tenant,
        NewCategory {
            code: "hosting".into(),
            name: "Hosting".into(),
            is_default: true,
            sort_order: 0,
        },
        now(),
    )
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn categories_are_ordered_revision_guarded_and_retire_only_when_unused() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = insert_category(
        &conn,
        &scope,
        tenant,
        NewCategory {
            code: "z".into(),
            name: "Z".into(),
            is_default: false,
            sort_order: 2,
        },
        now(),
    )
    .await
    .unwrap();
    let first = insert_category(
        &conn,
        &scope,
        tenant,
        NewCategory {
            code: "a".into(),
            name: "A".into(),
            is_default: true,
            sort_order: 1,
        },
        now(),
    )
    .await
    .unwrap();
    assert_eq!(
        list_categories(&conn, &scope, tenant)
            .await
            .unwrap()
            .iter()
            .map(|c| c.id)
            .collect::<Vec<_>>(),
        vec![first.id, cat.id]
    );
    assert!(matches!(
        insert_category(
            &conn,
            &scope,
            tenant,
            NewCategory {
                code: "z".into(),
                name: "Z2".into(),
                is_default: false,
                sort_order: 0
            },
            now()
        )
        .await,
        Err(RepoError::Refused(RepoRefusal::CategoryCodeTaken))
    ));
    let patch = crate::domain::category::CategoryPatch {
        name: Some("Renamed".into()),
        ..Default::default()
    };
    assert!(matches!(
        update_category(&conn, &scope, tenant, cat.id, 99, patch.clone(), now())
            .await
            .unwrap(),
        HeadWrite::Unmatched
    ));
    let HeadWrite::Written(updated) =
        update_category(&conn, &scope, tenant, cat.id, 1, patch, now())
            .await
            .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(updated.version, 2);
    assert_eq!(updated.name, "Renamed");
    insert_sku(
        &conn,
        &scope,
        tenant,
        new_sku("a", "a", cat.id),
        tenant,
        now(),
    )
    .await
    .unwrap();
    assert!(matches!(
        retire_category_if_unused(&conn, &scope, tenant, cat.id, now())
            .await
            .unwrap(),
        Some(HeadWrite::Unmatched)
    ));
    assert!(matches!(
        retire_category_if_unused(&conn, &scope, tenant, first.id, now())
            .await
            .unwrap(),
        Some(HeadWrite::Written(_))
    ));
    assert!(matches!(
        insert_sku(
            &conn,
            &scope,
            tenant,
            new_sku("b", "b", first.id),
            tenant,
            now()
        )
        .await,
        Err(RepoError::Refused(RepoRefusal::CategoryRetired))
    ));
    assert!(
        retire_category_if_unused(&conn, &scope, tenant, uuid::Uuid::new_v4(), now())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "One reference attempt is followed from reservation through fence refusal and release history"
)]
async fn references_block_both_fences_and_release_is_a_tombstone() {
    use crate::domain::references::RefKind;
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("a", "a", cat), tenant, now())
        .await
        .unwrap();
    set_lifecycle(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        now(),
    )
    .await
    .unwrap();
    let ref_id = uuid::Uuid::new_v4();
    let r = reserve_reference(
        &conn,
        &scope,
        tenant,
        s.id,
        "pricing",
        RefKind::PriceBookEntry,
        ref_id,
        tenant,
        now(),
    )
    .await
    .unwrap();
    assert!(matches!(
        reserve_reference(
            &conn,
            &scope,
            tenant,
            s.id,
            "pricing",
            RefKind::PriceBookEntry,
            ref_id,
            tenant,
            now()
        )
        .await,
        Err(RepoError::Refused(RepoRefusal::ReferenceExists))
    ));
    for kind in [Fence::Retire, Fence::TypeChange] {
        assert!(matches!(
            fence_sku(
                &conn,
                &scope,
                tenant,
                s.id,
                kind,
                uuid::Uuid::new_v4(),
                now()
            )
            .await
            .unwrap(),
            HeadWrite::Unmatched
        ));
    }
    assert_eq!(
        reference_summary(&conn, &scope, tenant, s.id)
            .await
            .unwrap(),
        crate::domain::references::ReferenceSummary {
            price_book_entries: 1,
            plans: 0,
            reserved: 1,
            by_owner: std::collections::BTreeMap::from([(
                "pricing".into(),
                std::collections::BTreeMap::from([
                    ("price_book_entry".into(), 1),
                    ("reserved".into(), 1)
                ])
            )]),
        }
    );
    assert_eq!(
        confirm_reference(&conn, &scope, tenant, r.id, now())
            .await
            .unwrap(),
        ConfirmOutcome::Confirmed
    );
    assert_eq!(
        confirm_reference(&conn, &scope, tenant, r.id, now())
            .await
            .unwrap(),
        ConfirmOutcome::AlreadyConfirmed
    );
    assert_eq!(
        reference_summary(&conn, &scope, tenant, s.id)
            .await
            .unwrap()
            .reserved,
        0
    );
    release_reference(
        &conn,
        &scope,
        tenant,
        r.id,
        tenant,
        Some("operator probe"),
        true,
        now(),
    )
    .await
    .unwrap();
    assert!(matches!(
        release_reference(
            &conn,
            &scope,
            tenant,
            r.id,
            uuid::Uuid::new_v4(),
            None,
            false,
            now()
        )
        .await
        .unwrap(),
        HeadWrite::Unmatched
    ));
    assert_eq!(
        confirm_reference(&conn, &scope, tenant, r.id, now())
            .await
            .unwrap(),
        ConfirmOutcome::Released
    );
    assert_eq!(
        confirm_reference(&conn, &scope, tenant, uuid::Uuid::new_v4(), now())
            .await
            .unwrap(),
        ConfirmOutcome::Missing
    );
    assert!(
        live_references(&conn, &scope, tenant, s.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        find_live_reference(
            &conn,
            &scope,
            tenant,
            "pricing",
            RefKind::PriceBookEntry,
            ref_id
        )
        .await
        .unwrap()
        .is_none()
    );
    let op = uuid::Uuid::new_v4();
    let HeadWrite::Written(fenced) =
        fence_sku(&conn, &scope, tenant, s.id, Fence::Retire, op, now())
            .await
            .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(fenced.lifecycle, Lifecycle::Published);
    assert!(fenced.retire_pending);
    assert!(matches!(
        unfence_sku(&conn, &scope, tenant, s.id, Some(uuid::Uuid::new_v4()))
            .await
            .unwrap(),
        HeadWrite::Unmatched
    ));
    let unit = uuid::Uuid::new_v4();
    assert!(
        try_lock_sku(&conn, &scope, tenant, s.id, unit, fenced.revision)
            .await
            .unwrap()
    );
    assert!(matches!(
        unfence_sku(&conn, &scope, tenant, s.id, None)
            .await
            .unwrap(),
        HeadWrite::Unmatched
    ));
    assert!(matches!(
        unlock_and_unfence(
            &conn,
            &scope,
            tenant,
            s.id,
            unit,
            uuid::Uuid::new_v4(),
            None,
            None
        )
        .await
        .unwrap(),
        HeadWrite::Unmatched
    ));
    let HeadWrite::Written(restored) =
        unlock_and_unfence(&conn, &scope, tenant, s.id, unit, op, None, None)
            .await
            .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(restored.lifecycle, Lifecycle::Published);
    assert_eq!(restored.pending_unit_id, None);
    let retry = reserve_reference(
        &conn,
        &scope,
        tenant,
        s.id,
        "pricing",
        RefKind::PriceBookEntry,
        ref_id,
        tenant,
        now(),
    )
    .await
    .unwrap();
    assert_ne!(retry.id, r.id);
    let old = find_reference(&conn, &scope, tenant, r.id)
        .await
        .unwrap()
        .unwrap();
    assert!(old.forced);
    assert_eq!(old.release_reason.as_deref(), Some("operator probe"));
}

#[tokio::test]
async fn type_fence_and_retire_completion_clear_only_the_matching_ownership() {
    let (db, scope, tenant, dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("a", "a", cat), tenant, now())
        .await
        .unwrap();
    set_lifecycle(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Draft],
        Lifecycle::Deprecated,
        now(),
    )
    .await
    .unwrap();
    let op = uuid::Uuid::new_v4();
    let HeadWrite::Written(f) =
        fence_sku(&conn, &scope, tenant, s.id, Fence::TypeChange, op, now())
            .await
            .unwrap()
    else {
        unreachable!()
    };
    assert!(f.type_change_pending);
    assert_eq!(f.lifecycle, Lifecycle::Deprecated);
    let HeadWrite::Written(f) = unfence_sku(&conn, &scope, tenant, s.id, Some(op))
        .await
        .unwrap()
    else {
        unreachable!()
    };
    assert!(!f.type_change_pending);
    assert_eq!(f.lifecycle, Lifecycle::Deprecated);
    fence_sku(&conn, &scope, tenant, s.id, Fence::Retire, op, now())
        .await
        .unwrap();
    let unit = uuid::Uuid::new_v4();
    assert!(
        try_lock_sku(&conn, &scope, tenant, s.id, unit, f.revision)
            .await
            .unwrap()
    );
    for (wrong_unit, wrong_op) in [(uuid::Uuid::new_v4(), op), (unit, uuid::Uuid::new_v4())] {
        assert!(matches!(
            unlock_and_unfence(
                &conn,
                &scope,
                tenant,
                s.id,
                wrong_unit,
                wrong_op,
                Some(unit),
                Some(crate::infra::storage::stored_now())
            )
            .await
            .unwrap(),
            HeadWrite::Unmatched
        ));
        let held = find_sku_fence(&conn, &scope, tenant, s.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(held.pending_unit_id, Some(unit));
        assert_eq!(held.fence_op_id, Some(op));
    }
    let fenced_revision = find_sku_fence(&conn, &scope, tenant, s.id)
        .await
        .unwrap()
        .unwrap()
        .revision;
    let retired_at = crate::infra::storage::stored_now();
    let HeadWrite::Written(f) = unlock_and_unfence(
        &conn,
        &scope,
        tenant,
        s.id,
        unit,
        op,
        Some(unit),
        Some(retired_at),
    )
    .await
    .unwrap() else {
        unreachable!()
    };
    assert_eq!(f.lifecycle, Lifecycle::Retired);
    // The retirement is a change of the row: its concurrency version (the ETag) and `updated_at` move.
    assert_eq!(f.revision, fenced_revision + 1);
    assert_eq!(f.updated_at, retired_at);
    assert_eq!(f.approved_by_unit_id, Some(unit));
    assert_eq!(crate::test_support::raw_i64(&dsn,"SELECT COUNT(*) AS v FROM products_sku WHERE fenced_at IS NOT NULL OR fence_op_id IS NOT NULL OR retire_pending != 0").await,0);
}

#[tokio::test]
async fn sku_queries_use_filters_cursor_and_scope_and_content_writes_increment_versions() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let a = insert_sku(
        &conn,
        &scope,
        tenant,
        new_sku("A", "Alpha", cat),
        tenant,
        now(),
    )
    .await
    .unwrap();
    insert_sku(
        &conn,
        &scope,
        tenant,
        new_sku("B", "Beta", cat),
        tenant,
        now(),
    )
    .await
    .unwrap();
    let q = SkuQuery {
        catalog_filter: Some(
            Condition::all()
                .add(sku::Column::Type.eq(SkuType::Usage.as_str()))
                .add(sku::Column::CategoryId.eq(cat)),
        ),
        lifecycle: Some(Lifecycle::Draft),
        limit: 1,
        after_code: None,
    };
    assert_eq!(list_skus(&conn, &scope, tenant, &q).await.unwrap().len(), 2);
    let q = SkuQuery {
        after_code: Some("A".into()),
        ..q
    };
    assert_eq!(
        list_skus(&conn, &scope, tenant, &q).await.unwrap()[0].code,
        "B"
    );
    let other = uuid::Uuid::new_v4();
    let foreign = AccessScope::for_tenant(other);
    assert!(
        find_sku(&conn, &foreign, tenant, a.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        find_category(&conn, &foreign, tenant, cat)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        set_lifecycle(
            &conn,
            &foreign,
            tenant,
            a.id,
            &[Lifecycle::Draft],
            Lifecycle::Published,
            now()
        )
        .await
        .unwrap(),
        HeadWrite::Unmatched
    ));
    // RT-05: with no category, the category lookup cannot refuse first, so the insert's own scope
    // guard is what refuses the tenant outside the scope.
    let uncategorized = NewSku {
        category_id: None,
        ..new_sku("C", "C", cat)
    };
    let refused = insert_sku(&conn, &foreign, tenant, uncategorized, tenant, now()).await;
    assert!(
        matches!(&refused, Err(RepoError::Db(m)) if m.starts_with("SKU scope:")),
        "{refused:?}"
    );
    let mut content = bss_products_sdk::models::SkuContent::from(&a);
    content.name = "Applied".into();
    let s = write_sku_content(&conn, &scope, tenant, a.id, &content, now())
        .await
        .unwrap();
    assert_eq!(s.published_version, 1);
    assert_eq!(s.revision, 2);
    assert_eq!(s.name, "Applied");
}

#[tokio::test]
async fn stale_unlock_cannot_clear_another_units_lock() {
    let (db, scope, tenant, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("A", "A", cat), tenant, now())
        .await
        .unwrap();
    let owner = uuid::Uuid::new_v4();
    let stale = uuid::Uuid::new_v4();
    assert!(
        try_lock_sku(&conn, &scope, tenant, s.id, owner, s.revision)
            .await
            .unwrap()
    );
    for approved_by in [None, Some(stale)] {
        assert!(matches!(
            unlock_sku(&conn, &scope, tenant, s.id, stale, approved_by)
                .await
                .unwrap(),
            HeadWrite::Unmatched
        ));
        let head = find_sku(&conn, &scope, tenant, s.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head.pending_unit_id, Some(owner));
        assert_eq!(head.approved_by_unit_id, None);
    }
    assert!(matches!(
        unlock_sku(&conn, &scope, tenant, s.id, owner, Some(owner))
            .await
            .unwrap(),
        HeadWrite::Written(_)
    ));
    let head = find_sku(&conn, &scope, tenant, s.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(head.pending_unit_id, None);
    assert_eq!(head.approved_by_unit_id, Some(owner));
    assert!(matches!(
        unlock_sku(&conn, &scope, tenant, s.id, owner, None)
            .await
            .unwrap(),
        HeadWrite::Unmatched
    ));
}

/// RS-22: `SkuContent` is the stored format of every version's append-only `content` (and of every
/// unit's proposal). Fixture rows keep reading through the repository: one as this build writes it,
/// one an older build wrote without the optional fields, and one a later build wrote with a field
/// this build does not know.
#[tokio::test]
async fn stored_content_fixtures_keep_reading() {
    use sea_orm::{ConnectionTrait, Database};
    let (db, scope, tenant, dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = seed_category(&conn, &scope, tenant).await;
    let s = insert_sku(&conn, &scope, tenant, new_sku("A", "A", cat), tenant, now())
        .await
        .unwrap();
    let date = time::Date::from_calendar_date(2026, time::Month::September, 1).unwrap();
    let c = bss_products_sdk::models::SkuContent::from(&s);
    append_version(&conn, &scope, tenant, s.id, 1, date, &c, now())
        .await
        .unwrap();
    let written = r#"{"code":"COMP-VCPU","name":"vCPU hour","type":"usage","category_id":null,"description":"","sellable":true,"gl_code":"4000","tax_category":"cloud","invoice_line_template":null,"billing_timing":"arrears","usage_type_ref":"gts.cf.core.uc.usage_record.v1~cf.bss.usage_type.cpu.v1","unit":"vCPU h"}"#;
    let older = r#"{"code":"A","name":"A","type":"recurring","category_id":null,"description":"","sellable":true}"#;
    let later = r#"{"code":"A","name":"A","type":"recurring","category_id":null,"description":"","sellable":true,"a_later_field":1}"#;
    // The table is append-only (a trigger refuses an UPDATE), so each fixture is a version row of
    // its own, copied from the one the repository wrote.
    let raw = Database::connect(&dsn).await.unwrap();
    for (n, fixture) in [(2, written), (3, older), (4, later)] {
        raw.execute_unprepared(&format!(
            "INSERT INTO products_sku_version \
             (sku_id, tenant_id, published_version, effective_from, content, created_at) \
             SELECT sku_id, tenant_id, {n}, effective_from, '{fixture}', created_at \
             FROM products_sku_version WHERE published_version = 1 AND {}",
            crate::test_support::id_matches("sku_id", s.id)
        ))
        .await
        .unwrap();
    }
    raw.close().await.ok();
    let versions = versions(&conn, &scope, tenant, s.id).await.unwrap();
    let read: Vec<_> = [2, 3, 4]
        .iter()
        .map(|n| {
            versions
                .iter()
                .find(|v| v.published_version == *n)
                .unwrap()
                .content
                .clone()
        })
        .collect();
    let stored: serde_json::Value = serde_json::from_str(written).unwrap();
    assert_eq!(serde_json::to_value(&read[0]).unwrap(), stored);
    assert_eq!(read[0].r#type, SkuType::Usage);
    assert_eq!(read[1].unit, None);
    assert_eq!(read[1].billing_timing, None);
    assert_eq!(read[2], read[1]);
}

/// A deprecation dated ahead stays published and serves `lifecycle_next`. Once the date has
/// passed, a read serves `deprecated` and leaves the stored lifecycle alone; the next head write
/// folds it first, so `set_lifecycle` sees the lifecycle in force (P-D-249).
#[tokio::test]
async fn a_dated_lifecycle_change_waits_for_its_date() {
    let (db, scope, tenant, dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let cat = insert_category(
        &conn,
        &scope,
        tenant,
        NewCategory {
            code: "hosting".into(),
            name: "Hosting".into(),
            is_default: true,
            sort_order: 0,
        },
        now(),
    )
    .await
    .unwrap();
    let s = insert_sku(
        &conn,
        &scope,
        tenant,
        new_sku("DATED", "Dated", cat.id),
        tenant,
        now(),
    )
    .await
    .unwrap();
    set_lifecycle(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Draft],
        Lifecycle::Published,
        now(),
    )
    .await
    .unwrap();
    let ahead = now().date().checked_add(time::Duration::days(30)).unwrap();
    let HeadWrite::Written(waiting) = set_lifecycle_next(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Published],
        Lifecycle::Deprecated,
        ahead,
        now(),
    )
    .await
    .unwrap() else {
        panic!("the dated change matched the published head");
    };
    assert_eq!(waiting.lifecycle, Lifecycle::Published);
    assert_eq!(
        waiting.lifecycle_next.map(|n| n.lifecycle),
        Some(Lifecycle::Deprecated)
    );
    let yesterday = now().date().checked_sub(time::Duration::days(1)).unwrap();
    let HeadWrite::Written(due) = set_lifecycle_next(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Published],
        Lifecycle::Deprecated,
        yesterday,
        now(),
    )
    .await
    .unwrap() else {
        panic!("replacing the pending change matched");
    };
    assert_eq!(due.lifecycle, Lifecycle::Deprecated);
    assert!(due.lifecycle_next.is_none());
    let stored = crate::test_support::raw_string_opt(
        &dsn,
        &format!(
            "SELECT lifecycle AS v FROM products_sku WHERE {}",
            crate::test_support::id_matches("id", s.id)
        ),
    )
    .await;
    assert_eq!(stored.as_deref(), Some("published"));
    assert!(matches!(
        set_lifecycle(
            &conn,
            &scope,
            tenant,
            s.id,
            &[Lifecycle::Published],
            Lifecycle::Retired,
            now()
        )
        .await
        .unwrap(),
        HeadWrite::Unmatched
    ));
    let HeadWrite::Written(retired) = set_lifecycle(
        &conn,
        &scope,
        tenant,
        s.id,
        &[Lifecycle::Deprecated],
        Lifecycle::Retired,
        now(),
    )
    .await
    .unwrap() else {
        panic!("the fold lets set_lifecycle see the lifecycle in force");
    };
    assert_eq!(retired.lifecycle, Lifecycle::Retired);
    assert!(retired.lifecycle_next.is_none());
}
