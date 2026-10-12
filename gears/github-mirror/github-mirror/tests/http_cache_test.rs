#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use github_mirror::infra::github::cache::{CacheKey, CachedResponse, HttpCache};
use github_mirror::infra::github::compression::Compression;
use github_mirror::infra::storage::sea_orm_repo::SeaOrmHttpCache;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

const URL: &str = "https://api.github.com/repos/acme/widget/issues";

fn entry() -> CachedResponse {
    CachedResponse {
        body: r#"[{"id":1,"title":"an issue"}]"#.to_owned(),
        etag: Some("W/\"abc\"".to_owned()),
        last_modified: None,
        next_page: Some("https://api.github.com/repos/acme/widget/issues?page=2".to_owned()),
    }
}

async fn store(compression: Compression) -> SeaOrmHttpCache {
    let db = common::inmem_db().await;
    SeaOrmHttpCache::new(Arc::new(DBProvider::<DbError>::new(db)), compression)
}

#[tokio::test]
async fn a_gzipped_entry_round_trips_through_the_database() {
    let cache = store(Compression::Gzip).await;
    let tenant = Uuid::new_v4();
    let key = CacheKey::compute("GET", URL, "application/json");

    assert!(
        cache
            .get(&AccessScope::for_tenant(tenant), &key)
            .await
            .unwrap()
            .is_none()
    );

    cache
        .put(&AccessScope::for_tenant(tenant), tenant, &key, URL, entry())
        .await
        .unwrap();
    let loaded = cache
        .get(&AccessScope::for_tenant(tenant), &key)
        .await
        .unwrap()
        .expect("entry");
    assert_eq!(loaded, entry(), "compression must be invisible to callers");
}

#[tokio::test]
async fn an_uncompressed_entry_round_trips_too() {
    let cache = store(Compression::None).await;
    let tenant = Uuid::new_v4();
    let key = CacheKey::compute("GET", URL, "application/json");

    cache
        .put(&AccessScope::for_tenant(tenant), tenant, &key, URL, entry())
        .await
        .unwrap();
    assert_eq!(
        cache
            .get(&AccessScope::for_tenant(tenant), &key)
            .await
            .unwrap(),
        Some(entry())
    );
}

#[tokio::test]
async fn entries_do_not_cross_tenants() {
    let cache = store(Compression::Gzip).await;
    let key = CacheKey::compute("GET", URL, "application/json");
    let owner = Uuid::new_v4();

    cache
        .put(&AccessScope::for_tenant(owner), owner, &key, URL, entry())
        .await
        .unwrap();
    assert!(
        cache
            .get(&AccessScope::for_tenant(Uuid::new_v4()), &key)
            .await
            .unwrap()
            .is_none(),
        "another tenant must not read this entry"
    );
    assert!(
        cache
            .get(&AccessScope::for_tenant(owner), &key)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn clearing_by_prefix_drops_only_the_matching_repository() {
    let cache = store(Compression::Gzip).await;
    let tenant = Uuid::new_v4();

    let widget = CacheKey::compute("GET", URL, "application/json");
    let other_url = "https://api.github.com/repos/acme/gadget/issues";
    let gadget = CacheKey::compute("GET", other_url, "application/json");

    cache
        .put(
            &AccessScope::for_tenant(tenant),
            tenant,
            &widget,
            URL,
            entry(),
        )
        .await
        .unwrap();
    cache
        .put(
            &AccessScope::for_tenant(tenant),
            tenant,
            &gadget,
            other_url,
            entry(),
        )
        .await
        .unwrap();

    let removed = cache
        .clear(
            &AccessScope::for_tenant(tenant),
            &["https://api.github.com/repos/acme/widget"],
        )
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(
        cache
            .get(&AccessScope::for_tenant(tenant), &widget)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .get(&AccessScope::for_tenant(tenant), &gadget)
            .await
            .unwrap()
            .is_some(),
        "the other repository's entries must survive"
    );
}

/// Every edge of the prefix match, in one pass: the prefix URL itself, a
/// child under `/`, a query under `?`, and two siblings that merely start
/// with the same text. Anything that matched on text alone would take the
/// siblings with it.
#[tokio::test]
async fn clearing_a_prefix_stops_at_a_path_or_query_boundary() {
    let cache = store(Compression::Gzip).await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let prefix = "https://api.github.com/repos/acme/widget";

    let urls = [
        (prefix, true),
        ("https://api.github.com/repos/acme/widget/issues", true),
        ("https://api.github.com/repos/acme/widget?page=2", true),
        (
            "https://api.github.com/repos/acme/widget-fork/issues",
            false,
        ),
        ("https://api.github.com/repos/acme/widgets/issues", false),
    ];

    for (url, _) in urls {
        let key = CacheKey::compute("GET", url, "application/json");
        cache.put(&scope, tenant, &key, url, entry()).await.unwrap();
    }

    let removed = cache.clear(&scope, &[prefix]).await.unwrap();
    assert_eq!(removed, 3, "the prefix itself, its child and its query");

    for (url, cleared) in urls {
        let key = CacheKey::compute("GET", url, "application/json");
        let found = cache.get(&scope, &key).await.unwrap().is_some();
        assert_eq!(
            found,
            !cleared,
            "{url} must {} a clear of {prefix}",
            if cleared { "not survive" } else { "survive" }
        );
    }
}

/// A repository name may contain `_` or `%`, which `LIKE` reads as "any one
/// character" and "any run of characters". Unescaped, a clear of `my_repo`
/// would take `myXrepo` with it.
#[tokio::test]
async fn a_metacharacter_in_a_prefix_matches_only_itself() {
    let cache = store(Compression::Gzip).await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let prefix = "https://api.github.com/repos/acme/my_re%po";

    let urls = [
        ("https://api.github.com/repos/acme/my_re%po/issues", true),
        ("https://api.github.com/repos/acme/myXre%po/issues", false),
        ("https://api.github.com/repos/acme/my_reZZZpo/issues", false),
        ("https://api.github.com/repos/acme/myXreZZZpo/issues", false),
    ];

    for (url, _) in urls {
        let key = CacheKey::compute("GET", url, "application/json");
        cache.put(&scope, tenant, &key, url, entry()).await.unwrap();
    }

    let removed = cache.clear(&scope, &[prefix]).await.unwrap();
    assert_eq!(
        removed, 1,
        "only the repository actually named in the prefix"
    );

    for (url, cleared) in urls {
        let key = CacheKey::compute("GET", url, "application/json");
        let found = cache.get(&scope, &key).await.unwrap().is_some();
        assert_eq!(
            found,
            !cleared,
            "{url} must {} a clear of {prefix}",
            if cleared { "not survive" } else { "survive" }
        );
    }
}

#[tokio::test]
async fn a_row_keeps_the_compression_it_was_written_with() {
    let db = common::inmem_db().await;
    let provider = Arc::new(DBProvider::<DbError>::new(db));
    let writer = SeaOrmHttpCache::new(Arc::clone(&provider), Compression::Gzip);
    let reader = SeaOrmHttpCache::new(provider, Compression::None);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let key = CacheKey::compute("GET", URL, "application/json");

    writer
        .put(&scope, tenant, &key, URL, entry())
        .await
        .unwrap();
    assert_eq!(
        reader.get(&scope, &key).await.unwrap(),
        Some(entry()),
        "the row records gzip, so a cache configured for none still decodes it"
    );
}

#[tokio::test]
async fn a_tampered_body_is_a_miss_not_an_error() {
    use github_mirror::infra::storage::entity::http_cache;
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait};
    use toolkit_db::secure::SecureUpdateExt;

    let db = common::inmem_db().await;
    let cache = SeaOrmHttpCache::new(
        Arc::new(DBProvider::<DbError>::new(db.clone())),
        Compression::Gzip,
    );
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let key = CacheKey::compute("GET", URL, "application/json");
    cache.put(&scope, tenant, &key, URL, entry()).await.unwrap();

    let conn = db.conn().unwrap();
    http_cache::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(sea_orm::Condition::all().add(http_cache::Column::CacheKey.eq(key.as_str())))
        .col_expr(http_cache::Column::ContentHash, Expr::value("0000"))
        .exec(&conn)
        .await
        .unwrap();

    assert_eq!(
        cache.get(&scope, &key).await.unwrap(),
        None,
        "a body that fails its integrity check is dropped, not surfaced"
    );
}

/// One clear covers several prefixes in one statement, and a clear with
/// nothing to clear must delete nothing: an empty `Condition::any()` matches
/// every row, so the guard in front of it is the whole tenant's cache.
#[tokio::test]
async fn a_clear_takes_every_prefix_it_is_given_and_an_empty_list_takes_none() {
    let cache = store(Compression::Gzip).await;
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    let urls = [
        "https://api.github.com/repos/acme/widget/issues",
        "https://api.github.com/repos/acme/gadget/issues",
        "https://api.github.com/repos/acme/spanner/issues",
    ];
    for url in urls {
        let key = CacheKey::compute("GET", url, "application/json");
        cache.put(&scope, tenant, &key, url, entry()).await.unwrap();
    }

    let removed = cache.clear(&scope, &[]).await.unwrap();
    assert_eq!(removed, 0, "a clear with no prefix must remove nothing");
    for url in urls {
        let key = CacheKey::compute("GET", url, "application/json");
        assert!(
            cache.get(&scope, &key).await.unwrap().is_some(),
            "{url} must survive a clear that named nothing"
        );
    }

    let removed = cache
        .clear(
            &scope,
            &[
                "https://api.github.com/repos/acme/widget",
                "https://api.github.com/repos/acme/spanner",
            ],
        )
        .await
        .unwrap();
    assert_eq!(removed, 2, "both prefixes go in one statement");

    let survivor = CacheKey::compute("GET", urls[1], "application/json");
    assert!(
        cache.get(&scope, &survivor).await.unwrap().is_some(),
        "the repository no prefix named must survive"
    );
    for url in [urls[0], urls[2]] {
        let key = CacheKey::compute("GET", url, "application/json");
        assert!(cache.get(&scope, &key).await.unwrap().is_none(), "{url}");
    }
}

/// The `compression` column says how the body was written. A row whose column
/// disagrees with its bytes is the case the integrity check cannot catch on
/// its own: gzip bytes read as plain text are not valid UTF-8, and plain text
/// read as gzip has no header. Either way the entry is a miss and the next
/// sync fetches it again, rather than an error the caller sees.
#[tokio::test]
async fn a_row_whose_compression_column_lies_is_a_miss_not_an_error() {
    use github_mirror::infra::storage::entity::http_cache;
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait};
    use toolkit_db::secure::SecureUpdateExt;

    for (wrote_with, stamped) in [(Compression::Gzip, "none"), (Compression::None, "gzip")] {
        let db = common::inmem_db().await;
        let cache =
            SeaOrmHttpCache::new(Arc::new(DBProvider::<DbError>::new(db.clone())), wrote_with);
        let tenant = Uuid::new_v4();
        let scope = AccessScope::for_tenant(tenant);
        let key = CacheKey::compute("GET", URL, "application/json");
        cache.put(&scope, tenant, &key, URL, entry()).await.unwrap();

        let conn = db.conn().unwrap();
        http_cache::Entity::update_many()
            .secure()
            .scope_with(&scope)
            .filter(sea_orm::Condition::all().add(http_cache::Column::CacheKey.eq(key.as_str())))
            .col_expr(http_cache::Column::Compression, Expr::value(stamped))
            .exec(&conn)
            .await
            .unwrap();

        assert_eq!(
            cache.get(&scope, &key).await.unwrap(),
            None,
            "a body written {wrote_with:?} and stamped {stamped} must read as a miss"
        );
    }
}

/// A `compression` value no build knows is not a miss but a refusal: it means
/// the row was written by something this code does not understand, and
/// guessing at its bytes is worse than saying so.
#[tokio::test]
async fn a_compression_value_this_build_does_not_know_is_refused() {
    use github_mirror::infra::storage::entity::http_cache;
    use sea_orm::sea_query::Expr;
    use sea_orm::{ColumnTrait, EntityTrait};
    use toolkit_db::secure::SecureUpdateExt;

    let db = common::inmem_db().await;
    let cache = SeaOrmHttpCache::new(
        Arc::new(DBProvider::<DbError>::new(db.clone())),
        Compression::Gzip,
    );
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let key = CacheKey::compute("GET", URL, "application/json");
    cache.put(&scope, tenant, &key, URL, entry()).await.unwrap();

    let conn = db.conn().unwrap();
    http_cache::Entity::update_many()
        .secure()
        .scope_with(&scope)
        .filter(sea_orm::Condition::all().add(http_cache::Column::CacheKey.eq(key.as_str())))
        .col_expr(http_cache::Column::Compression, Expr::value("brotli"))
        .exec(&conn)
        .await
        .unwrap();

    let outcome = cache.get(&scope, &key).await;
    assert!(
        outcome.is_err() || outcome.is_ok_and(|entry| entry.is_none()),
        "an unknown compression must not be served as a body"
    );
}
