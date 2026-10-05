//! Every index DESIGN §3.7 names exists on Postgres with the columns and predicate it states,
//! and the approved-start index is unique per chain: per entry and per dimension value, with
//! the default chain its own chain, and only among approved prices.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod pg_support;
use bss_pricing::infra::storage::{
    RepoError,
    entity::{price_book, price_book_entry},
    repo::{book_repo, price_book_entry_repo, price_repo},
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit_db::{DBProvider, DbError, secure::AccessScope};
use uuid::Uuid;

/// `(index, the definition's tail after "USING btree ")` as DESIGN §3.7 declares them.
///
/// The approval queue index is created by the shared `bss_approval::ddl` under the name
/// `ix_<prefix>approval_unit_queue`; DESIGN §3.7 called it `pricing_approval_queue` until this
/// test found the difference.
const DESIGN_INDEXES: &[(&str, &str)] = &[
    (
        "ix_pricing_approval_unit_queue",
        "(tenant_id, state, kind, submitted_at)",
    ),
    (
        "pricing_price_book_entry_key",
        // D-427 added model; migration 18 added the immutable policy digest (DESIGN §3.7).
        "(book_id, sku_id, charge_kind, COALESCE(period, ''::text), model, COALESCE(usage_policy_digest, ''::text))",
    ),
    (
        "pricing_price_approved_start",
        "(price_book_entry_id, COALESCE(dim_value, ''::text), effective_from) WHERE ((state = 'approved'::text) AND (change_kind = 'set'::text))",
    ),
    (
        "pricing_price_chain",
        "(price_book_entry_id, dim_value, effective_from) WHERE (state = 'approved'::text)",
    ),
    (
        "pricing_reference_op_due",
        "(state, next_attempt_at) WHERE (state <> 'done'::text)",
    ),
    ("idx_pricing_audit_tenant_time", "(tenant_id, written_at)"),
    (
        "idx_pricing_audit_subject",
        "(tenant_id, subject_kind, subject_id, written_at)",
    ),
    (
        "idx_pricing_audit_actor",
        "(tenant_id, actor_ref, written_at)",
    ),
    ("idx_pricing_idempotency_expires", "(tenant_id, expires_at)"),
    ("pricing_plan_code", "(tenant_id, code)"),
    ("pricing_plan_revision_no", "(plan_id, rev_no)"),
    (
        "pricing_plan_revision_open",
        "(plan_id) WHERE (state = ANY (ARRAY['draft'::text, 'pending'::text]))",
    ),
    (
        "pricing_plan_revision_published",
        "(plan_id) WHERE (state = 'published'::text)",
    ),
    // D-446 (m20260929_000017): one scheduled revision per plan.
    (
        "pricing_plan_revision_scheduled",
        "(plan_id) WHERE (state = 'scheduled'::text)",
    ),
    ("pricing_plan_item_sku", "(revision_id, sku_id)"),
];
/// The unique keys DESIGN §3.7 declares, inline or as a partial unique index, by the columns (and
/// the predicate) they cover.
const DESIGN_UNIQUE: &[(&str, &str)] = &[
    ("pricing_price_book", "(tenant_id, code)"),
    ("pricing_price_book", "(tenant_id, id)"),
    ("pricing_price", "(price_book_entry_id, version_no)"),
    ("pricing_plan", "(tenant_id, code)"),
    ("pricing_plan_revision", "(plan_id, rev_no)"),
    (
        "pricing_plan_revision",
        "(plan_id) WHERE (state = ANY (ARRAY['draft'::text, 'pending'::text]))",
    ),
    (
        "pricing_plan_revision",
        "(plan_id) WHERE (state = 'published'::text)",
    ),
    (
        "pricing_plan_revision",
        "(plan_id) WHERE (state = 'scheduled'::text)",
    ),
    ("pricing_plan_item", "(revision_id, sku_id)"),
];

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_every_index_design_names_exists_as_declared() {
    let pg = pg_support::Pg::applied().await;
    let raw = pg.raw().await;
    let prices = raw
        .query_all_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT indexname AS n, indexdef AS d FROM pg_indexes WHERE schemaname = 'bss'"
                .to_owned(),
        ))
        .await
        .unwrap();
    let indexes: Vec<(String, String)> = prices
        .iter()
        .map(|r| {
            (
                r.try_get::<String>("", "n").unwrap(),
                r.try_get::<String>("", "d").unwrap(),
            )
        })
        .collect();
    let mut missing = Vec::new();
    for (name, tail) in DESIGN_INDEXES {
        match indexes.iter().find(|(n, _)| n == name) {
            Some((_, def)) => assert!(
                def.ends_with(&format!("USING btree {tail}")),
                "{name}: {def}"
            ),
            None => missing.push(*name),
        }
    }
    assert!(
        missing.is_empty(),
        "DESIGN §3.7 names absent indexes: {missing:?}"
    );
    for (table, columns) in DESIGN_UNIQUE {
        assert!(
            indexes
                .iter()
                .any(|(_, def)| def.starts_with("CREATE UNIQUE INDEX")
                    && def.contains(&format!(" ON bss.{table} USING btree {columns}"))),
            "{table} {columns}"
        );
    }
}

#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn postgres_one_approved_start_per_chain_and_only_among_approved_prices() {
    let pg = pg_support::Pg::applied().await;
    let provider = DBProvider::<DbError>::new(pg.db().await);
    let conn = provider.conn().unwrap();
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let now = time::OffsetDateTime::now_utc();
    let book = book_repo::insert(
        &conn,
        &scope,
        price_book::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            code: "standard".into(),
            name: "Standard".into(),
            currency: "EUR".into(),
            valid_from: None,
            valid_until: None,
            description: None,
            version: 1,
            created_at: now,
            updated_at: now,
            archived_at: None,
            archived_by: None,
        },
    )
    .await
    .unwrap();
    let mut entries = Vec::new();
    for _ in 0..2 {
        entries.push(
            price_book_entry_repo::insert(
                &conn,
                &scope,
                price_book_entry::Model {
                    id: Uuid::new_v4(),
                    tenant_id: tenant,
                    book_id: book.id,
                    sku_id: Uuid::new_v4(),
                    charge_kind: "usage".into(),
                    period: None,
                    model: "per_unit".into(),
                    usage_policy_id: None,
                    usage_policy_version: None,
                    usage_policy_digest: None,
                    usage_sku_version: None,
                    dimension_key: None,
                    invoice_line_override: None,
                    reservation_id: Uuid::new_v4(),
                    reference_state: "confirmed".into(),
                    version: 1,
                    created_at: now,
                    updated_at: now,
                },
            )
            .await
            .unwrap(),
        );
    }
    let start = time::Date::from_calendar_date(2031, time::Month::March, 1).unwrap();
    let mut version = 0;
    let mut price = |entry: &price_book_entry::Model, dim: Option<&str>, state: &str| {
        version += 1;
        let mut r = price_template(entry);
        r.version_no = version;
        r.dim_value = dim.map(str::to_owned);
        r.state = state.into();
        r.effective_from = start;
        r
    };
    let (a, b) = (&entries[0], &entries[1]);
    price_repo::insert(&conn, &scope, price(a, None, "approved"))
        .await
        .unwrap();
    for (what, candidate) in [
        (
            "a value chain starts on the same day",
            price(a, Some("eu"), "approved"),
        ),
        ("another entry's default chain", price(b, None, "approved")),
        ("a draft on the taken start", price(a, None, "draft")),
        (
            "a rejected price on the taken start",
            price(a, None, "rejected"),
        ),
    ] {
        price_repo::insert(&conn, &scope, candidate)
            .await
            .unwrap_or_else(|e| panic!("{what}: {e:?}"));
    }
    for (what, candidate) in [
        ("the default chain again", price(a, None, "approved")),
        ("the value chain again", price(a, Some("eu"), "approved")),
    ] {
        let refused = price_repo::insert(&conn, &scope, candidate).await;
        assert!(
            matches!(
                refused,
                Err(RepoError::Conflict {
                    code: "WINDOW_OVERLAP"
                })
            ),
            "{what}: {refused:?}"
        );
    }
}

fn price_template(
    p: &price_book_entry::Model,
) -> bss_pricing::infra::storage::entity::price::Model {
    let now = time::OffsetDateTime::now_utc();
    bss_pricing::infra::storage::entity::price::Model {
        id: Uuid::new_v4(),
        tenant_id: p.tenant_id,
        price_book_entry_id: p.id,
        version_no: 1,
        dim_value: None,
        price_json: serde_json::json!({"rate":"0.10"}),
        min_fee: None,
        eligibility: "all".into(),
        effective_from: now.date(),
        effective_to: None,
        keep_for_bound: false,
        closed_explicitly: false,
        temporary_until: None,
        paired_price_id: None,
        return_of_price_id: None,
        change_kind: "set".into(),
        target_price_id: None,
        cancelled_by_unit_id: None,
        state: "draft".into(),
        pending_unit_id: None,
        approved_by_unit_id: None,
        note: None,
        created_by: Uuid::new_v4(),
        approved_at: None,
        version: 1,
        created_at: now,
        updated_at: now,
    }
}
