//! Postgres-only regression test for SQL-level cross-tenant BOLA on reads.
//!
//! Proves the degraded flat-`In` PDP scope is compiled into the SecureORM WHERE
//! clause: a tenant-A scope reading tenant-B's chart of accounts yields ZERO
//! rows — independent of the caller-supplied `tenant_id` filter — and a by-id
//! read of a B-owned account returns `None`. This closes the gap where the
//! highest-value isolation guarantee was only covered at e2e (live cluster),
//! never at the Rust layer. Ignored by default; run with
//! `cargo test -p bss-ledger --test postgres_bola -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use bss_ledger::domain::model::AccountRow;
use bss_ledger::infra::storage::migrations::Migrator;
use bss_ledger::infra::storage::repo::ReferenceRepo;
use bss_ledger_sdk::ODataQuery;
use sea_orm::Database;
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError, connect_db};
use uuid::Uuid;

/// A minimal OPEN `AR`/`DR`/`USD` chart-of-accounts row for `tenant`.
fn account_row(tenant: Uuid) -> AccountRow {
    AccountRow {
        account_id: Uuid::new_v4(),
        tenant_id: tenant,
        legal_entity_id: tenant,
        account_class: "AR".to_owned(),
        currency: "USD".to_owned(),
        revenue_stream: None,
        normal_side: "DR".to_owned(),
        may_go_negative: false,
        lifecycle_state: "OPEN".to_owned(),
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn cross_tenant_reference_reads_are_sql_scoped_to_empty() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let db = Database::connect(&url).await.unwrap();
    Migrator::up(&db, None).await.unwrap();

    // Repo connection runs with search_path=bss (as the gear does in prod) so
    // its unqualified entity queries resolve into the bss schema.
    let repo_url = format!("{url}?options=-c%20search_path%3Dbss,public");
    let tdb = connect_db(&repo_url, ConnectOpts::default()).await.unwrap();
    let provider = DBProvider::<DbError>::new(tdb);
    let repo = ReferenceRepo::new(provider);

    // Two tenants, each seeded with exactly one account.
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let a_account = account_row(tenant_a);
    let b_account = account_row(tenant_b);
    let a_id = a_account.account_id;
    let b_id = b_account.account_id;
    repo.insert_account(a_account).await.expect("seed A");
    repo.insert_account(b_account).await.expect("seed B");

    let scope_a = AccessScope::for_tenant(tenant_a);

    // Positive control: A's own scope sees A's own account — so an empty result
    // below means "scoped out", not "the store is simply empty".
    let own = repo
        .list_accounts(&scope_a, tenant_a, &ODataQuery::default())
        .await
        .expect("list own");
    assert_eq!(own.items.len(), 1, "A's scope must see A's own account");
    assert_eq!(own.items[0].account_id, a_id);

    // BOLA: A's scope asking for B's accounts yields ZERO. B genuinely HAS an
    // account (a populated outsider), so this proves the scope predicate
    // overrides the caller-supplied `tenant_id = B` filter at the SQL layer.
    let cross = repo
        .list_accounts(&scope_a, tenant_b, &ODataQuery::default())
        .await
        .expect("list cross");
    assert!(
        cross.items.is_empty(),
        "A's scope must NOT see B's accounts (SQL-level BOLA); got {} row(s)",
        cross.items.len()
    );

    // BOLA on a by-id read: A's scope cannot resolve a B-owned account by id.
    let b_via_a = repo.find_account(&scope_a, b_id).await.expect("find cross");
    assert!(
        b_via_a.is_none(),
        "A's scope must not resolve a B-owned account id"
    );
    // Positive control: A's scope resolves its own account by id.
    let a_via_a = repo.find_account(&scope_a, a_id).await.expect("find own");
    assert_eq!(a_via_a.map(|r| r.account_id), Some(a_id));

    // Symmetric: B's scope cannot see A's accounts either.
    let scope_b = AccessScope::for_tenant(tenant_b);
    let cross_b = repo
        .list_accounts(&scope_b, tenant_a, &ODataQuery::default())
        .await
        .expect("list cross b");
    assert!(
        cross_b.items.is_empty(),
        "B's scope must NOT see A's accounts"
    );
}
