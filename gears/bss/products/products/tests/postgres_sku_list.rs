#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The SKU list and its counts on `PostgreSQL` (P-D-210, P-D-211): the text search folds Unicode
//! case through the ICU root collation — on a `C`-locale database too — and takes wildcards
//! literally, the null filters and the cursor order hold on the engine production runs, and the
//! counts group in one statement.
mod pg_support;

use bss_products::{
    domain::{category::NewCategory, sku::NewSku},
    infra::storage::repo::{self, SetFilter, SkuCounts, SkuListFilter},
};
use bss_products_sdk::models::{Lifecycle, SkuType};
use pg_support::Pg;
use sea_orm::DbBackend;
use time::OffsetDateTime;
use toolkit_db::{ConnectOpts, Db, secure::AccessScope, test_support::connect_with_recorder};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, SortDir};
use uuid::Uuid;

struct Fixture {
    db: Db,
    scope: AccessScope,
    tenant: Uuid,
}
impl Fixture {
    async fn new() -> (Pg, Self) {
        Self::on(Pg::applied().await).await
    }
    /// A fixture on a `C`-locale database, where the database's own `lower()` folds ASCII only.
    async fn in_c_locale() -> (Pg, Self) {
        Self::on(Pg::applied_in_c_locale().await).await
    }
    async fn on(pg: Pg) -> (Pg, Self) {
        let db = pg.db().await;
        let tenant = Uuid::new_v4();
        let f = Self {
            db,
            scope: AccessScope::for_tenant(tenant),
            tenant,
        };
        (pg, f)
    }
    async fn category(&self, code: &str) -> Uuid {
        repo::insert_category(
            &self.db.conn().unwrap(),
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
    async fn sku(
        &self,
        code: &str,
        name: &str,
        category: Option<Uuid>,
        lifecycle: Lifecycle,
        unit: Option<&str>,
    ) -> Uuid {
        let conn = self.db.conn().unwrap();
        let now = OffsetDateTime::now_utc();
        let s = repo::insert_sku(
            &conn,
            &self.scope,
            self.tenant,
            NewSku {
                code: code.into(),
                name: name.into(),
                r#type: SkuType::Usage,
                category_id: category,
                description: String::new(),
                sellable: true,
                gl_code: None,
                tax_category: None,
                invoice_line_template: None,
                billing_timing: None,
                usage_type_ref: None,
                unit: unit.map(Into::into),
            },
            self.tenant,
            now,
        )
        .await
        .unwrap();
        if lifecycle != Lifecycle::Draft {
            repo::set_lifecycle(
                &conn,
                &self.scope,
                self.tenant,
                s.id,
                &[Lifecycle::Draft],
                lifecycle,
                now,
            )
            .await
            .unwrap();
        }
        s.id
    }
    async fn page(&self, filter: SkuListFilter, query: &ODataQuery) -> Vec<String> {
        let page = repo::page_skus(
            &self.db.conn().unwrap(),
            &self.scope,
            self.tenant,
            DbBackend::Postgres,
            &filter,
            query,
        )
        .await
        .unwrap();
        page.items.into_iter().map(|s| s.code).collect()
    }
    async fn codes(&self, q: Option<&str>, filter: Option<&str>) -> Vec<String> {
        let mut query = ODataQuery::default();
        if let Some(raw) = filter {
            query = query.with_filter(toolkit_odata::parse_filter_string(raw).unwrap().into_expr());
        }
        let mut codes = self
            .page(
                SkuListFilter {
                    text: q.map(Into::into),
                    ..SkuListFilter::default()
                },
                &query,
            )
            .await;
        codes.sort();
        codes
    }
}
fn sorted(mut v: Vec<&str>) -> Vec<String> {
    v.sort_unstable();
    v.into_iter().map(str::to_owned).collect()
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn q_folds_unicode_case_and_takes_wildcards_literally_on_postgres() {
    let (_pg, f) = Fixture::new().await;
    f.sku("STOR-1", "Storage", None, Lifecycle::Draft, Some("GiB"))
        .await;
    f.sku("COMP", "Compute 50%_off", None, Lifecycle::Draft, None)
        .await;
    f.sku("DECOY", "Compute 50xyoff", None, Lifecycle::Draft, None)
        .await;
    f.sku("BACK", r"Back\slash", None, Lifecycle::Draft, None)
        .await;
    f.sku(
        "CYR",
        "\u{411}\u{435}\u{442}\u{430}",
        None,
        Lifecycle::Draft,
        None,
    ) // Beta, in Cyrillic
    .await;
    for (q, expected) in [
        ("stor", vec!["STOR-1"]),
        ("STORAGE", vec!["STOR-1"]),
        ("gib", vec!["STOR-1"]),
        ("%", vec!["COMP"]),
        ("_", vec!["COMP"]),
        ("50%_", vec!["COMP"]),
        (r"\", vec!["BACK"]),
        ("compute 50", vec!["COMP", "DECOY"]),
        ("\u{411}\u{435}\u{442}\u{430}", vec!["CYR"]),
        // Postgres folds Unicode: the other case of a Cyrillic letter matches here (SQLite's
        // `lower()` folds ASCII only; the SQLite door test pins the difference).
        ("\u{431}\u{435}\u{442}\u{430}", vec!["CYR"]),
        ("\u{411}\u{415}\u{422}\u{410}", vec!["CYR"]),
    ] {
        assert_eq!(f.codes(Some(q), None).await, sorted(expected), "q={q}");
    }
}

/// P-D-210 on a `C`-locale database — the deployed environment's `app` database, `initdb --locale=C`,
/// `CloudNativePG`'s default — where `lower()` folds ASCII only: `q` still folds Unicode case,
/// because both sides fold through the ICU root collation (`und-x-icu`), whatever the database's
/// locale.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn q_folds_unicode_case_on_a_c_locale_database() {
    use sea_orm::{ConnectionTrait, Statement};
    let (pg, f) = Fixture::in_c_locale().await;
    // The database is `C`, and there its own `lower()` does not fold Cyrillic.
    let raw = pg.raw().await;
    let row = raw
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT datctype, lower('\u{411}\u{415}\u{422}\u{410}') = '\u{431}\u{435}\u{442}\u{430}' AS folds \
             FROM pg_database WHERE datname = current_database()",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            row.try_get::<String>("", "datctype").unwrap(),
            row.try_get::<bool>("", "folds").unwrap()
        ),
        ("C".to_owned(), false),
        "the database's lower() folds ASCII only"
    );
    raw.close().await.unwrap();
    // Beta, in capital Cyrillic, as the code and the name; a Latin SKU beside it.
    f.sku(
        "\u{411}\u{415}\u{422}\u{410}",
        "\u{411}\u{415}\u{422}\u{410}",
        None,
        Lifecycle::Draft,
        None,
    )
    .await;
    f.sku("STOR", "Storage", None, Lifecycle::Draft, Some("GiB"))
        .await;
    for (q, expected) in [
        (
            "\u{431}\u{435}\u{442}\u{430}",
            vec!["\u{411}\u{415}\u{422}\u{410}"],
        ),
        (
            "\u{411}\u{435}\u{442}",
            vec!["\u{411}\u{415}\u{422}\u{410}"],
        ),
        (
            "\u{411}\u{415}\u{422}\u{410}",
            vec!["\u{411}\u{415}\u{422}\u{410}"],
        ),
        ("stor", vec!["STOR"]),
        ("gib", vec!["STOR"]),
    ] {
        assert_eq!(f.codes(Some(q), None).await, sorted(expected), "q={q}");
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn null_filters_the_order_and_the_counts_hold_on_postgres() {
    let (_pg, f) = Fixture::new().await;
    let x = f.category("x").await;
    f.sku("A", "zeta", Some(x), Lifecycle::Published, None)
        .await;
    f.sku("B", "alpha", None, Lifecycle::Draft, None).await;
    f.sku("C", "mu", None, Lifecycle::Deprecated, None).await;
    let d = f.sku("D", "beta", Some(x), Lifecycle::Draft, None).await;
    // D is in review: a pending unit locks it.
    let conn = f.db.conn().unwrap();
    let revision = repo::find_sku(&conn, &f.scope, f.tenant, d)
        .await
        .unwrap()
        .unwrap()
        .revision;
    assert!(
        repo::try_lock_sku(&conn, &f.scope, f.tenant, d, Uuid::new_v4(), revision)
            .await
            .unwrap()
    );
    assert_eq!(f.codes(None, Some("category_id eq null")).await, ["B", "C"]);
    assert_eq!(f.codes(None, Some("category_id ne null")).await, ["A", "D"]);
    assert_eq!(f.codes(None, Some("pending_unit_id ne null")).await, ["D"]);
    assert_eq!(
        f.codes(
            None,
            Some(&format!("category_id eq {x} and lifecycle eq 'draft'"))
        )
        .await,
        ["D"]
    );
    // Name order walks with the cursor on Postgres too.
    let mut query = ODataQuery::default()
        .with_order(ODataOrderBy(vec![OrderKey {
            field: "name".into(),
            dir: SortDir::Desc,
        }]))
        .with_limit(3);
    let first = repo::page_skus(
        &conn,
        &f.scope,
        f.tenant,
        DbBackend::Postgres,
        &SkuListFilter::default(),
        &query,
    )
    .await
    .unwrap();
    let names: Vec<_> = first.items.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, ["zeta", "mu", "beta"]);
    let cursor = first.page_info.next_cursor.unwrap();
    query = ODataQuery::default()
        .with_cursor(toolkit_odata::CursorV1::decode(&cursor).unwrap())
        .with_limit(3);
    let rest = f.page(SkuListFilter::default(), &query).await;
    assert_eq!(rest, ["B"]);
    // The counts: one grouped statement, in review across lifecycles.
    let counts = repo::count_skus(
        &conn,
        &f.scope,
        f.tenant,
        DbBackend::Postgres,
        &SkuListFilter::default(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        counts,
        SkuCounts {
            all: 4,
            draft: 2,
            published: 1,
            deprecated: 1,
            retired: 0,
            in_review: 1,
            archived: 0,
        }
    );
}

/// P-D-264 on Postgres: a text function on `lifecycle` is the `CASE`'s `in` over the lifecycles
/// it matches, at the top level and under `and`, and a text that matches none keeps nothing.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_text_function_on_lifecycle_filters_through_the_case_on_postgres() {
    let (_pg, f) = Fixture::new().await;
    let x = f.category("x").await;
    f.sku("A", "a", Some(x), Lifecycle::Published, None).await;
    f.sku("B", "b", None, Lifecycle::Draft, None).await;
    f.sku("C", "c", Some(x), Lifecycle::Deprecated, None).await;
    f.sku("D", "d", None, Lifecycle::Retired, None).await;
    for (filter, expected) in [
        ("contains(lifecycle,'pub')".to_owned(), vec!["A"]),
        ("startswith(lifecycle,'d')".to_owned(), vec!["B", "C"]),
        ("endswith(lifecycle,'ed')".to_owned(), vec!["A", "C", "D"]),
        (
            format!("category_id eq {x} and startswith(lifecycle,'d')"),
            vec!["C"],
        ),
        (
            "endswith(lifecycle,'ed') and lifecycle ne 'retired'".to_owned(),
            vec!["A", "C"],
        ),
        ("contains(lifecycle,'PUB')".to_owned(), vec![]),
        (
            format!("category_id eq {x} and endswith(lifecycle,'x')"),
            vec![],
        ),
    ] {
        assert_eq!(f.codes(None, Some(&filter)).await, expected, "{filter}");
    }
}

/// P-D-212 on Postgres: a usage filter's whole id set is one `uuid[]` bind, kept or negated, in
/// the list and in the counts, whatever its size.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_usage_set_filters_through_one_uuid_array_on_postgres() {
    let (_pg, f) = Fixture::new().await;
    let mut ids = Vec::new();
    for code in ["A", "B", "C", "D"] {
        ids.push(f.sku(code, code, None, Lifecycle::Draft, None).await);
    }
    for extra in [0_usize, 5000] {
        let mut priced = vec![ids[0], ids[2]];
        priced.extend((0..extra).map(|_| Uuid::new_v4()));
        let filter = |member: bool| SkuListFilter {
            priced: Some(SetFilter {
                member,
                ids: priced.clone(),
            }),
            in_plan: Some(SetFilter {
                member: false,
                ids: vec![ids[2]],
            }),
            ..SkuListFilter::default()
        };
        assert_eq!(
            f.page(filter(true), &ODataQuery::default()).await,
            ["A"],
            "{extra}"
        );
        assert_eq!(
            f.page(filter(false), &ODataQuery::default()).await,
            ["B", "D"],
            "{extra}"
        );
        let counts = repo::count_skus(
            &f.db.conn().unwrap(),
            &f.scope,
            f.tenant,
            DbBackend::Postgres,
            &filter(false),
            None,
        )
        .await
        .unwrap();
        assert_eq!((counts.all, counts.draft), (2, 2), "{extra}");
    }
    // An empty set keeps nothing, and its negation everything.
    let empty = |member| SkuListFilter {
        priced: Some(SetFilter {
            member,
            ids: Vec::new(),
        }),
        ..SkuListFilter::default()
    };
    assert!(f.page(empty(true), &ODataQuery::default()).await.is_empty());
    assert_eq!(f.page(empty(false), &ODataQuery::default()).await.len(), 4);
}

/// P-D-246 on Postgres: a picker's book set (kept for `priced_in`, negated for `not_priced_in`)
/// and its revision set (negated for `not_in_revision`) are each one `uuid[]` bind, beside the
/// usage sets, in the list and in the counts, whatever their size.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_picker_scope_filters_through_one_uuid_array_on_postgres() {
    let (pg, f) = Fixture::new().await;
    let (watched, recorder) = connect_with_recorder(
        &pg.url(true),
        ConnectOpts {
            max_conns: Some(2),
            min_conns: Some(0),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for code in ["A", "B", "C", "D"] {
        ids.push(f.sku(code, code, None, Lifecycle::Draft, None).await);
    }
    let mut first_list = Vec::new();
    for extra in [0_usize, 5000] {
        let mut book = vec![ids[0], ids[1]];
        book.extend((0..extra).map(|_| Uuid::new_v4()));
        let filter = |member: bool| SkuListFilter {
            book: Some(SetFilter {
                member,
                ids: book.clone(),
            }),
            revision: Some(SetFilter {
                member: false,
                ids: vec![ids[1], ids[2]],
            }),
            ..SkuListFilter::default()
        };
        recorder.clear();
        let page = repo::page_skus(
            &watched.conn().unwrap(),
            &f.scope,
            f.tenant,
            DbBackend::Postgres,
            &filter(true),
            &ODataQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            page.items
                .into_iter()
                .map(|sku| sku.code)
                .collect::<Vec<_>>(),
            ["A"],
            "priced_in and not_in_revision: {extra}"
        );
        let list_sql = uuid_array_sql(&recorder);
        recorder.clear();
        let page = repo::page_skus(
            &watched.conn().unwrap(),
            &f.scope,
            f.tenant,
            DbBackend::Postgres,
            &filter(false),
            &ODataQuery::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            page.items
                .into_iter()
                .map(|sku| sku.code)
                .collect::<Vec<_>>(),
            ["D"],
            "not_priced_in and not_in_revision: {extra}"
        );
        let outside = uuid_array_sql(&recorder);
        assert!(
            outside.iter().any(|sql| sql.contains("NOT (")),
            "a non-member set is negated: {outside:?}"
        );
        assert_eq!(
            outside
                .iter()
                .map(|sql| sql.matches("uuid[]").count())
                .sum::<usize>(),
            list_sql
                .iter()
                .map(|sql| sql.matches("uuid[]").count())
                .sum::<usize>(),
            "{extra}"
        );
        recorder.clear();
        let counts = repo::count_skus(
            &watched.conn().unwrap(),
            &f.scope,
            f.tenant,
            DbBackend::Postgres,
            &filter(true),
            None,
        )
        .await
        .unwrap();
        assert_eq!((counts.all, counts.draft), (1, 1), "{extra}");
        let count_sql = uuid_array_sql(&recorder);
        assert!(
            !list_sql.is_empty() && !count_sql.is_empty(),
            "{extra}: list {list_sql:?} counts {count_sql:?}"
        );
        for sql in list_sql.iter().chain(&count_sql) {
            assert_eq!(
                sql.matches("uuid[]").count(),
                sql.matches("CAST(").count(),
                "each uuid[] is one cast: {sql}"
            );
            assert!(sql.contains("AS uuid[]"), "{sql}");
        }
        if extra == 5000 {
            assert_eq!(
                list_sql, first_list,
                "the statement does not grow with the set"
            );
        } else {
            first_list = list_sql;
        }
    }
}

fn uuid_array_sql(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<String> {
    recorder
        .events()
        .into_iter()
        .filter(|event| event.sql.contains("uuid[]"))
        .map(|event| event.sql)
        .collect()
}

/// P-D-245 on Postgres: the batch SKU read binds the whole id set as one `uuid[]`, and a missing
/// id is absent, for a handful of ids and for a set far past a per-id bind list.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn skus_for_write_binds_one_uuid_array_on_postgres() {
    let (pg, f) = Fixture::new().await;
    let mut ids = Vec::new();
    for code in ["A", "B", "C"] {
        ids.push(f.sku(code, code, None, Lifecycle::Draft, None).await);
    }
    let (watched, recorder) = connect_with_recorder(
        &pg.url(true),
        ConnectOpts {
            max_conns: Some(2),
            min_conns: Some(0),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    let conn = watched.conn().unwrap();
    let mut first = Vec::new();
    for extra in [0_usize, 5000] {
        let mut asked = vec![ids[2], ids[0], Uuid::now_v7()];
        asked.extend((0..extra).map(|_| Uuid::new_v4()));
        recorder.clear();
        let found = repo::find_skus(&conn, DbBackend::Postgres, &f.scope, f.tenant, &asked)
            .await
            .unwrap();
        let got: Vec<Uuid> = found.into_iter().map(|sku| sku.id).collect();
        assert!(
            got.contains(&ids[0]) && got.contains(&ids[2]),
            "{extra}: {got:?}"
        );
        assert_eq!(got.len(), 2, "{extra}");
        let sql = uuid_array_sql(&recorder);
        assert_eq!(sql.len(), 1, "{extra}: {sql:?}");
        assert_eq!(sql[0].matches("uuid[]").count(), 1, "{}", sql[0]);
        assert!(
            (sql[0].contains("CAST(?") || sql[0].contains("CAST($"))
                && sql[0].contains("AS uuid[]"),
            "{}",
            sql[0]
        );
        if extra == 5000 {
            assert_eq!(sql, first, "the statement does not grow with the set");
        } else {
            first = sql;
        }
    }
}
