#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::infra::storage::repo::{AuditCommon, LifecycleMove, write_eventless_act_audit};
use crate::test_support::{products_statements, recorded_test_db, test_db, utc};
use sea_orm::Set;
use toolkit_db::secure::SecureInsertExt;
use toolkit_odata::CursorV1;

const TENANT: Uuid = Uuid::from_u128(0x7e_11);
const SKU: Uuid = Uuid::from_u128(0x5c_01);
const OTHER_SKU: Uuid = Uuid::from_u128(0x5c_02);
const UNIT: Uuid = Uuid::from_u128(0x0a_01);
const OTHER_UNIT: Uuid = Uuid::from_u128(0x0a_02);
const ACTOR: Uuid = Uuid::from_u128(0xac_01);

async fn unit(runner: &impl DBRunner, id: Uuid, tenant: Uuid, sku: Uuid, kind: &str) {
    let model = approval_unit::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant),
        kind: Set(kind.to_owned()),
        ref_type: Set("sku".to_owned()),
        ref_id: Set(sku),
        state: Set("pending".to_owned()),
        common_effective_date: Set(None),
        quorum_required: Set(1),
        generation: Set(1),
        submitted_by: Set(ACTOR),
        submitted_at: Set(utc(2026, 9, 27, 0, 0, 0)),
        decided_at: Set(None),
        decided_note: Set(None),
        snapshot: Set(serde_json::json!({})),
        snapshot_hash: Set("h".to_owned()),
        version: Set(1),
        submit_note: Set(None),
    };
    approval_unit::Entity::insert(model.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(tenant), &model)
        .unwrap()
        .exec(runner)
        .await
        .unwrap();
}

/// One audit row: `n` names it (its audit id is `n`, so its place in the history), written at
/// `second` past ten.
#[allow(
    clippy::too_many_arguments,
    reason = "each seeded row spells its whole identity"
)]
async fn row(
    runner: &impl DBRunner,
    n: u128,
    tenant: Uuid,
    kind: &str,
    subject: Uuid,
    second: u8,
    lifecycle: LifecycleMove,
    note: Option<&str>,
) {
    write_eventless_act_audit(
        runner,
        &AccessScope::for_tenant(tenant),
        AuditCommon {
            audit_id: Uuid::from_u128(n),
            tenant_id: tenant,
            actor_ref: ACTOR,
            action: format!("act.{n}"),
            subject_kind: kind.to_owned(),
            reason: note.map(ToOwned::to_owned),
            correlation_id: None,
            written_at: utc(2026, 9, 27, 10, 0, second),
            lifecycle,
        },
        subject,
        None,
    )
    .await
    .unwrap();
}

/// Every page of the history at `limit`, followed by its cursor, as `act.<n>` names.
async fn walk(runner: &impl DBRunner, limit: u64) -> Vec<String> {
    let mut names = Vec::new();
    let mut query = ODataQuery::default().with_limit(limit);
    loop {
        let page = page_sku_history(runner, TENANT, SKU, &query).await.unwrap();
        names.extend(page.items.iter().map(|e| e.action.clone()));
        let Some(next) = page.page_info.next_cursor else {
            break;
        };
        query = ODataQuery::default()
            .with_limit(limit)
            .with_cursor(CursorV1::decode(&next).unwrap());
        assert!(names.len() < 50, "the walk ends: {names:?}");
    }
    names
}

/// The history is the SKU's own rows and its units' rows, in the tenant, ordered by `audit_id`
/// alone whatever their `written_at` and the order they were inserted in; every page boundary
/// skips and repeats nothing; a unit row names its unit and kind; a row without the lifecycle
/// columns reads none.
#[tokio::test]
async fn the_history_orders_by_audit_id_across_every_page() {
    let (db, _, _, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    unit(&conn, UNIT, TENANT, SKU, "sku_retire").await;
    unit(&conn, OTHER_UNIT, TENANT, OTHER_SKU, "sku_publish").await;
    let moved = LifecycleMove::between(Lifecycle::Published, Lifecycle::Deprecated);
    // Three rows share one instant, inserted against their id order; the last id is the earliest
    // instant, and `written_at` decides nothing.
    row(&conn, 3, TENANT, "sku", SKU, 5, moved, None).await;
    row(
        &conn,
        1,
        TENANT,
        "approval_unit",
        UNIT,
        5,
        moved,
        Some("n1"),
    )
    .await;
    row(&conn, 2, TENANT, "sku", SKU, 5, moved, None).await;
    row(&conn, 9, TENANT, "sku", SKU, 1, LifecycleMove::NONE, None).await;
    row(&conn, 4, TENANT, "approval_unit", UNIT, 7, moved, None).await;
    // Not the SKU's: another SKU, another SKU's unit, another tenant, a category.
    row(&conn, 20, TENANT, "sku", OTHER_SKU, 5, moved, None).await;
    row(
        &conn,
        21,
        TENANT,
        "approval_unit",
        OTHER_UNIT,
        5,
        moved,
        None,
    )
    .await;
    row(
        &conn,
        22,
        Uuid::from_u128(0x7e_12),
        "sku",
        SKU,
        5,
        moved,
        None,
    )
    .await;
    row(
        &conn,
        23,
        TENANT,
        "category",
        SKU,
        5,
        LifecycleMove::NONE,
        None,
    )
    .await;

    let expected = ["act.1", "act.2", "act.3", "act.4", "act.9"];
    for limit in [1, 2, 3, 200] {
        assert_eq!(walk(&conn, limit).await, expected, "limit {limit}");
    }
    let page = page_sku_history(&conn, TENANT, SKU, &ODataQuery::default())
        .await
        .unwrap();
    assert_eq!(page.page_info.limit, 50, "the default page");
    let last = &page.items[4];
    assert_eq!(
        (last.from_lifecycle, last.to_lifecycle, last.unit_id),
        (None, None, None),
        "a row without the columns reads none"
    );
    let unit_row = &page.items[0];
    assert_eq!(
        (
            unit_row.unit_id,
            unit_row.unit_kind.as_deref(),
            unit_row.note.as_deref(),
            unit_row.from_lifecycle,
            unit_row.to_lifecycle,
            unit_row.actor,
            unit_row.at,
        ),
        (
            Some(UNIT),
            Some("sku_retire"),
            Some("n1"),
            Some(Lifecycle::Published),
            Some(Lifecycle::Deprecated),
            ACTOR,
            utc(2026, 9, 27, 10, 0, 5),
        )
    );
    assert_eq!(page.items[1].unit_kind, None, "a SKU row names no unit");
    let clamped = page_sku_history(&conn, TENANT, SKU, &ODataQuery::default().with_limit(5000))
        .await
        .unwrap();
    assert_eq!(clamped.page_info.limit, 200, "`$top` is clamped at 200");
}

/// One audit row of `sku` with its own id and instant, as an act writes it.
async fn act(runner: &impl DBRunner, audit_id: Uuid, sku: Uuid, name: &str, at: OffsetDateTime) {
    write_eventless_act_audit(
        runner,
        &AccessScope::for_tenant(TENANT),
        AuditCommon {
            audit_id,
            tenant_id: TENANT,
            actor_ref: ACTOR,
            action: name.to_owned(),
            subject_kind: "sku".to_owned(),
            reason: None,
            correlation_id: None,
            written_at: at,
            lifecycle: LifecycleMove::NONE,
        },
        sku,
        None,
    )
    .await
    .unwrap();
}

/// Every page of `sku`'s history at `limit`, as action names.
async fn walk_of(runner: &impl DBRunner, sku: Uuid, limit: u64) -> Vec<String> {
    let mut names = Vec::new();
    let mut query = ODataQuery::default().with_limit(limit);
    loop {
        let page = page_sku_history(runner, TENANT, sku, &query).await.unwrap();
        names.extend(page.items.iter().map(|e| e.action.clone()));
        let Some(next) = page.page_info.next_cursor else {
            break;
        };
        query = ODataQuery::default()
            .with_limit(limit)
            .with_cursor(CursorV1::decode(&next).unwrap());
        assert!(names.len() < 50, "the walk ends: {names:?}");
    }
    names
}

/// B-2: the history is in the order the acts committed — `audit_id`, a UUID v7 minted in the act's
/// transaction — wherever `written_at` disagrees. `written_at` is the instant the act began, taken
/// before its transaction and kept across a retry: an act that began first but committed second
/// (it resolved a usage type slowly, or lost a serialization race) reads after the act that
/// committed first. And on `SQLite` the RFC 3339 text of two instants in one second sorts against
/// time when one fraction is a prefix of the other (`…21.41868Z` after `…21.418681Z`).
#[tokio::test]
async fn the_history_follows_the_commit_order_where_written_at_disagrees() {
    let (db, _, _, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let second = utc(2026, 9, 27, 10, 0, 21);
    // B began 100 ms after A (`…21.2Z` against `…21.1Z`: text and time agree) and committed
    // first: its id is minted first.
    let b = Uuid::now_v7();
    let a = Uuid::now_v7();
    act(
        &conn,
        b,
        SKU,
        "b.began_second_committed_first",
        second + time::Duration::milliseconds(200),
    )
    .await;
    act(
        &conn,
        a,
        SKU,
        "a.began_first_committed_second",
        second + time::Duration::milliseconds(100),
    )
    .await;
    // One second, one fraction a prefix of the other: C began first and committed first.
    let c = Uuid::now_v7();
    let d = Uuid::now_v7();
    act(
        &conn,
        c,
        OTHER_SKU,
        "c.first",
        second + time::Duration::microseconds(418_680),
    )
    .await;
    act(
        &conn,
        d,
        OTHER_SKU,
        "d.second",
        second + time::Duration::microseconds(418_681),
    )
    .await;
    for limit in [1, 200] {
        assert_eq!(
            walk_of(&conn, SKU, limit).await,
            [
                "b.began_second_committed_first",
                "a.began_first_committed_second"
            ],
            "limit {limit}"
        );
        assert_eq!(
            walk_of(&conn, OTHER_SKU, limit).await,
            ["c.first", "d.second"],
            "limit {limit}"
        );
    }
}

/// A cursor minted when the history ordered by `(written_at, audit_id)` names a key the history no
/// longer has: it is refused as a query error (400), never read in another order.
#[tokio::test]
async fn a_cursor_of_the_written_at_order_is_refused() {
    let (db, _, _, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    for n in 1..=3 {
        row(&conn, n, TENANT, "sku", SKU, 5, LifecycleMove::NONE, None).await;
    }
    let page = page_sku_history(&conn, TENANT, SKU, &ODataQuery::default().with_limit(1))
        .await
        .unwrap();
    let mut cursor = CursorV1::decode(&page.page_info.next_cursor.unwrap()).unwrap();
    assert_eq!(cursor.s, "+audit_id", "the order a cursor carries now");
    cursor.s = "+written_at,+audit_id".to_owned();
    cursor.k = vec![
        "2026-09-27T10:00:05Z".to_owned(),
        Uuid::from_u128(1).to_string(),
    ];
    let stale = ODataQuery::default().with_limit(1).with_cursor(cursor);
    assert!(matches!(
        page_sku_history(&conn, TENANT, SKU, &stale).await,
        Err(SkuListError::Query(_))
    ));
}

async fn legacy(
    runner: &impl DBRunner,
    n: u128,
    kind: &str,
    subject: Uuid,
    action: &str,
    from: Option<&str>,
    to: Option<&str>,
) {
    let model = crate::infra::storage::entity::audit_log::ActiveModel {
        audit_id: Set(Uuid::from_u128(n)),
        tenant_id: Set(TENANT),
        actor_ref: Set(ACTOR),
        action: Set(action.to_owned()),
        subject_kind: Set(kind.to_owned()),
        subject_id: Set(Some(subject)),
        subject_revision: Set(None),
        error_code: Set(None),
        attempted_key: Set(None),
        reason: Set(None),
        correlation_id: Set(None),
        written_at: Set(utc(2026, 9, 27, 10, 0, u8::try_from(n).unwrap_or(1))),
        session_id: Set(None),
        ceremony_ref: Set(None),
        seal_state: Set("unsealed".to_owned()),
        chain_id: Set(None),
        seq: Set(None),
        prev_hash: Set(None),
        row_hash: Set(None),
        from_lifecycle: Set(from.map(str::to_owned)),
        to_lifecycle: Set(to.map(str::to_owned)),
    };
    crate::infra::storage::entity::audit_log::Entity::insert(model.clone())
        .secure()
        .scope_with_model(&AccessScope::for_tenant(TENANT), &model)
        .unwrap()
        .exec(runner)
        .await
        .unwrap();
}

/// A stored `retiring` is mapped at read (P-D-248). Parsing it would be a corrupt row, which the
/// history door serves as 500.
#[tokio::test]
async fn a_stored_retiring_move_is_mapped_and_never_a_corrupt_row() {
    let (db, _, _, _dsn) = test_db().await;
    let conn = db.conn().unwrap();
    let earlier = Uuid::from_u128(0x0a_11);
    let resumed = Uuid::from_u128(0x0a_12);
    unit(&conn, earlier, TENANT, SKU, "sku_retire").await;
    unit(&conn, resumed, TENANT, SKU, "sku_retire").await;
    unit(&conn, UNIT, TENANT, SKU, "sku_retire").await;
    legacy(
        &conn,
        1,
        "approval_unit",
        earlier,
        "approval.submit",
        Some("published"),
        Some("retiring"),
    )
    .await;
    legacy(
        &conn,
        2,
        "approval_unit",
        resumed,
        "approval.submit",
        Some("retiring"),
        Some("retiring"),
    )
    .await;
    legacy(
        &conn,
        3,
        "approval_unit",
        UNIT,
        "approval.submit",
        Some("deprecated"),
        Some("retiring"),
    )
    .await;
    legacy(
        &conn,
        4,
        "approval_unit",
        UNIT,
        "approval.vote",
        Some("retiring"),
        Some("retiring"),
    )
    .await;
    legacy(
        &conn,
        5,
        "approval_unit",
        UNIT,
        "approval.applied",
        Some("retiring"),
        Some("retired"),
    )
    .await;
    legacy(
        &conn,
        6,
        "approval_unit",
        UNIT,
        "approval.rejected",
        Some("retiring"),
        Some("deprecated"),
    )
    .await;
    legacy(
        &conn,
        7,
        "sku",
        SKU,
        "sku.unfence",
        Some("retiring"),
        Some("deprecated"),
    )
    .await;
    legacy(
        &conn,
        8,
        "sku",
        SKU,
        "sku.fence_expired",
        Some("retiring"),
        Some("published"),
    )
    .await;
    let page = page_sku_history(&conn, TENANT, SKU, &ODataQuery::default())
        .await
        .expect("a stored retiring is mapped, never a 500");
    let moves: Vec<_> = page
        .items
        .iter()
        .map(|e| {
            format!(
                "{} {}>{}",
                e.action,
                e.from_lifecycle.map_or("-", Lifecycle::as_str),
                e.to_lifecycle.map_or("-", Lifecycle::as_str)
            )
        })
        .collect();
    assert_eq!(
        moves,
        [
            "approval.submit published>published",
            "approval.submit published>published",
            "approval.submit deprecated>deprecated",
            "approval.vote deprecated>deprecated",
            "approval.applied deprecated>retired",
            "approval.rejected deprecated>deprecated",
            "sku.unfence deprecated>deprecated",
            "sku.fence_expired published>published",
        ]
    );
}

/// Unit A entered `retiring` on an earlier page. Unit B's submit and apply sit on the next page.
/// B's apply is served `published → retired`: the enter row is loaded with every `retiring` row
/// of the SKU, one statement, not only the page's units (P-D-248).
#[tokio::test]
async fn a_later_page_maps_a_legacy_retire_from_the_whole_sku() {
    let (db, _, _, _dsn, recorder) = recorded_test_db().await;
    let conn = db.conn().unwrap();
    let earlier = Uuid::from_u128(0x0b_01);
    let later = Uuid::from_u128(0x0b_02);
    unit(&conn, earlier, TENANT, SKU, "sku_retire").await;
    unit(&conn, later, TENANT, SKU, "sku_retire").await;
    legacy(
        &conn,
        1,
        "approval_unit",
        earlier,
        "approval.submit",
        Some("published"),
        Some("retiring"),
    )
    .await;
    legacy(
        &conn,
        2,
        "approval_unit",
        later,
        "approval.submit",
        Some("retiring"),
        Some("retiring"),
    )
    .await;
    legacy(
        &conn,
        3,
        "approval_unit",
        later,
        "approval.applied",
        Some("retiring"),
        Some("retired"),
    )
    .await;
    let first = page_sku_history(&conn, TENANT, SKU, &ODataQuery::default().with_limit(1))
        .await
        .unwrap();
    let cursor =
        CursorV1::decode(&first.page_info.next_cursor.expect("B is on the next page")).unwrap();
    recorder.clear();
    let page = page_sku_history(
        &conn,
        TENANT,
        SKU,
        &ODataQuery::default().with_cursor(cursor).with_limit(2),
    )
    .await
    .unwrap();
    let moves: Vec<_> = page
        .items
        .iter()
        .map(|e| {
            format!(
                "{} {}>{}",
                e.action,
                e.from_lifecycle.map_or("-", Lifecycle::as_str),
                e.to_lifecycle.map_or("-", Lifecycle::as_str)
            )
        })
        .collect();
    assert_eq!(
        moves,
        [
            "approval.submit published>published",
            "approval.applied published>retired",
        ],
        "{moves:#?}"
    );
    let sqls = products_statements(&recorder);
    let audit = sqls
        .iter()
        .filter(|sql| sql.to_lowercase().contains("products_audit_log"))
        .filter(|sql| sql.to_lowercase().starts_with("select"))
        .count();
    assert_eq!(
        audit, 3,
        "one page, the page's units, and one retiring load: {sqls:#?}"
    );
}
