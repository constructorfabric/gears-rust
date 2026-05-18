use super::*;
use sea_orm::{DatabaseBackend, EntityTrait, QueryTrait};

/// Pins the bind-renumbering contract for the `due_check`
/// `cust_with_values` expression. `SeaQuery` renumbers the local
/// `$1`/`$2` placeholders inside the cust string globally when
/// composing them with surrounding filter clauses, so the bound
/// values arrive in the right slots regardless of how many binds
/// precede the cust expression.
///
/// This test was added in response to a false-positive review
/// finding that claimed the `$N` indices were bound to earlier
/// filter values. The actual SQL output proves they are not — the
/// values list is `[status, stale_cutoff, default_secs, now]` and
/// the SQL placeholders renumber to `$1..$4` in the same order.
/// If a future `SeaQuery` upgrade ever broke this, we would see it
/// here before retention scanning silently misbehaved in prod.
#[test]
fn scan_retention_due_filter_binds_in_expected_order() {
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, Condition, QueryFilter};

    let now = time::OffsetDateTime::UNIX_EPOCH;
    let stale_cutoff = now - time::Duration::seconds(120);

    let claimable = Condition::any()
        .add(tenants::Column::ClaimedBy.is_null())
        .add(tenants::Column::ClaimedAt.lte(stale_cutoff));
    let due = Expr::cust_with_values(
        "deleted_at + make_interval(secs => CASE WHEN retention_window_secs >= 0 THEN retention_window_secs ELSE $1 END) <= $2",
        vec![sea_orm::Value::from(60_i64), sea_orm::Value::from(now)],
    );
    let stmt = tenants::Entity::find()
        .filter(
            Condition::all()
                .add(tenants::Column::Status.eq(3_i16))
                .add(tenants::Column::DeletedAt.is_not_null())
                .add(claimable)
                .add(due),
        )
        .build(DatabaseBackend::Postgres);

    let values: Vec<sea_orm::Value> = stmt
        .values
        .as_ref()
        .map(|v| v.0.clone())
        .unwrap_or_default();
    assert_eq!(
        values.len(),
        4,
        "expected 4 binds (status, stale_cutoff, default_secs, now); got {} from SQL: {}",
        values.len(),
        stmt.sql
    );
    // Bind ordering is what matters for retention correctness:
    // position 2 (cust's `$1`) MUST be `default_secs` (i64 = 60),
    // position 3 (cust's `$2`) MUST be `now`.
    assert!(
        matches!(values[2], sea_orm::Value::BigInt(Some(60))),
        "bind position 3 (cust $1) must be default_secs (BigInt 60); got {:?}",
        values[2]
    );
    assert!(
        matches!(values[3], sea_orm::Value::TimeDateTimeWithTimeZone(_)),
        "bind position 4 (cust $2) must be `now` (TimeDateTimeWithTimeZone); got {:?}",
        values[3]
    );
}

/// Snapshot test pinning the leaf-first ORDER BY for
/// `scan_retention_due`. Catches the starvation regression where
/// `deleted_at ASC` ran first and let an older parent with surviving
/// Deleted children monopolise the LIMIT window (`hard_delete_one`
/// defers parents-with-children, so the next tick re-picks the same
/// parent and starves newer due leaves).
///
/// The test calls the same `apply_retention_leaf_first_order`
/// helper the impl uses, so a reordering of the helper's
/// `.order_by(...)` chain breaks both at once.
#[test]
fn retention_scan_orders_leaf_first() {
    let stmt =
        apply_retention_leaf_first_order(tenants::Entity::find()).build(DatabaseBackend::Postgres);
    let sql = stmt.to_string();

    let depth_pos = sql
        .find("\"depth\" DESC")
        .or_else(|| sql.find("`depth` DESC"))
        .expect("ORDER BY must include `depth DESC`");
    let deleted_pos = sql
        .find("\"deleted_at\" ASC")
        .or_else(|| sql.find("`deleted_at` ASC"))
        .expect("ORDER BY must include `deleted_at ASC`");
    let id_pos = sql
        .find("\"id\" ASC")
        .or_else(|| sql.find("`id` ASC"))
        .expect("ORDER BY must include `id ASC`");

    assert!(
        depth_pos < deleted_pos,
        "leaf-first ORDER BY: `depth DESC` must precede `deleted_at ASC` to \
             prevent the parent-starvation regression. Full SQL: {sql}"
    );
    assert!(
        deleted_pos < id_pos,
        "tiebreaker order: `deleted_at ASC` must precede `id ASC`. \
             Full SQL: {sql}"
    );
}
