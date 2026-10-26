#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use chrono::{Duration, Utc};
use github_mirror::infra::github::cache::{CacheKey, CachedResponse, HttpCache};
use github_mirror::infra::github::compression::Compression;
use github_mirror::infra::storage::fs_cache::FilesystemHttpCache;
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

fn key(url: &str) -> CacheKey {
    CacheKey::compute("GET", url, "application/json")
}

fn cache_in(dir: &Path, compression: Compression) -> FilesystemHttpCache {
    FilesystemHttpCache::new(dir.to_path_buf(), compression)
}

async fn put(cache: &FilesystemHttpCache, tenant: Uuid, url: &str) {
    cache
        .put(
            &AccessScope::for_tenant(tenant),
            tenant,
            &key(url),
            url,
            entry(),
        )
        .await
        .unwrap();
}

async fn cached(cache: &FilesystemHttpCache, tenant: Uuid, url: &str) -> bool {
    cache
        .get(&AccessScope::for_tenant(tenant), tenant, &key(url))
        .await
        .unwrap()
        .is_some()
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for child in std::fs::read_dir(dir).unwrap() {
        let path = child.unwrap().path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[tokio::test]
async fn a_gzipped_entry_round_trips_through_files() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::Gzip);
    let tenant = Uuid::new_v4();

    assert!(!cached(&cache, tenant, URL).await);
    put(&cache, tenant, URL).await;

    let loaded = cache
        .get(&AccessScope::for_tenant(tenant), tenant, &key(URL))
        .await
        .unwrap()
        .expect("entry");
    assert_eq!(loaded, entry(), "compression must be invisible to callers");
}

#[tokio::test]
async fn an_uncompressed_entry_round_trips_too() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();

    put(&cache, tenant, URL).await;
    assert_eq!(
        cache
            .get(&AccessScope::for_tenant(tenant), tenant, &key(URL))
            .await
            .unwrap(),
        Some(entry())
    );
}

#[tokio::test]
async fn files_land_under_the_tenant_folder_with_a_meta_companion() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();

    put(&cache, tenant, URL).await;

    let key = key(URL);
    let bucket = dir.path().join(tenant.to_string()).join(&key.as_str()[..2]);
    let body = bucket.join(key.as_str());
    let meta = bucket.join(format!("{}.meta.json", key.as_str()));
    assert!(body.is_file(), "body at {}", body.display());
    assert!(meta.is_file(), "meta at {}", meta.display());

    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
    assert_eq!(meta["url"], URL);
    assert_eq!(meta["etag"], "W/\"abc\"");
    assert_eq!(meta["compression"], "none");
    assert!(meta["fetched_at"].is_string());
    assert_eq!(
        files_under(dir.path()).len(),
        2,
        "no temporary files may be left behind"
    );
}

#[tokio::test]
async fn entries_do_not_cross_tenants() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::Gzip);
    let owner = Uuid::new_v4();
    let stranger = Uuid::new_v4();

    put(&cache, owner, URL).await;
    assert!(
        !cached(&cache, stranger, URL).await,
        "another tenant must not read this entry"
    );
    assert!(cached(&cache, owner, URL).await);
}

#[tokio::test]
async fn a_deny_all_scope_reads_nothing_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();

    put(&cache, tenant, URL).await;
    assert!(
        cache
            .get(&AccessScope::deny_all(), tenant, &key(URL))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        cache
            .put(&AccessScope::deny_all(), tenant, &key(URL), URL, entry())
            .await
            .is_err()
    );
    assert_eq!(
        cache
            .clear(
                &AccessScope::deny_all(),
                tenant,
                &["https://api.github.com/repos/acme"]
            )
            .await
            .unwrap(),
        0
    );
    assert!(
        cached(&cache, tenant, URL).await,
        "the denied clear must leave the entry"
    );
}

#[tokio::test]
async fn a_rewrite_replaces_the_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::Gzip);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    put(&cache, tenant, URL).await;
    let newer = CachedResponse {
        body: r#"[{"id":2}]"#.to_owned(),
        etag: Some("W/\"def\"".to_owned()),
        last_modified: None,
        next_page: None,
    };
    cache
        .put(&scope, tenant, &key(URL), URL, newer.clone())
        .await
        .unwrap();

    assert_eq!(
        cache.get(&scope, tenant, &key(URL)).await.unwrap(),
        Some(newer)
    );
    assert_eq!(files_under(dir.path()).len(), 2);
}

#[tokio::test]
async fn clearing_by_prefix_drops_only_the_matching_repository() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::Gzip);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    let urls = [
        ("https://api.github.com/repos/acme/widget", true),
        ("https://api.github.com/repos/acme/widget/issues", true),
        ("https://api.github.com/repos/acme/widget?page=2", true),
        (
            "https://api.github.com/repos/acme/widget-fork/issues",
            false,
        ),
        ("https://api.github.com/repos/acme/widgets/issues", false),
        ("https://api.github.com/repos/acme/gadget/issues", false),
    ];
    for (url, _) in urls {
        put(&cache, tenant, url).await;
    }

    let removed = cache
        .clear(
            &scope,
            tenant,
            &["https://api.github.com/repos/acme/widget"],
        )
        .await
        .unwrap();
    assert_eq!(removed, 3, "the prefix itself, its child and its query");

    for (url, cleared) in urls {
        assert_eq!(
            cached(&cache, tenant, url).await,
            !cleared,
            "{url} must {} the clear",
            if cleared { "not survive" } else { "survive" }
        );
    }
    assert_eq!(
        files_under(dir.path()).len(),
        6,
        "three entries of two files each remain"
    );
}

#[tokio::test]
async fn a_clear_takes_every_prefix_and_an_empty_list_takes_none() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    let urls = [
        "https://api.github.com/repos/acme/widget/issues",
        "https://api.github.com/repos/acme/gadget/issues",
        "https://api.github.com/repos/acme/spanner/issues",
    ];
    for url in urls {
        put(&cache, tenant, url).await;
    }

    assert_eq!(cache.clear(&scope, tenant, &[]).await.unwrap(), 0);
    for url in urls {
        assert!(cached(&cache, tenant, url).await, "{url}");
    }

    let removed = cache
        .clear(
            &scope,
            tenant,
            &[
                "https://api.github.com/repos/acme/widget",
                "https://api.github.com/repos/acme/spanner",
            ],
        )
        .await
        .unwrap();
    assert_eq!(removed, 2);
    assert!(cached(&cache, tenant, urls[1]).await);
    assert!(!cached(&cache, tenant, urls[0]).await);
    assert!(!cached(&cache, tenant, urls[2]).await);
}

#[tokio::test]
async fn a_clear_of_one_tenant_leaves_the_other_tenants_entries() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let one = Uuid::new_v4();
    let two = Uuid::new_v4();

    put(&cache, one, URL).await;
    put(&cache, two, URL).await;

    let removed = cache
        .clear(
            &AccessScope::for_tenant(one),
            one,
            &["https://api.github.com/repos/acme/widget"],
        )
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(!cached(&cache, one, URL).await);
    assert!(cached(&cache, two, URL).await);
}

#[tokio::test]
async fn expiring_drops_only_entries_fetched_before_the_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let old_url = "https://api.github.com/repos/acme/widget/issues";
    let new_url = "https://api.github.com/repos/acme/gadget/issues";

    put(&cache, tenant, old_url).await;
    let between = Utc::now() + Duration::seconds(1);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    put(&cache, tenant, new_url).await;

    assert_eq!(
        cache
            .expire(&scope, tenant, Utc::now() - Duration::days(1))
            .await
            .unwrap(),
        0,
        "nothing is a day old yet"
    );
    assert_eq!(cache.expire(&scope, tenant, between).await.unwrap(), 1);
    assert!(!cached(&cache, tenant, old_url).await);
    assert!(cached(&cache, tenant, new_url).await);
}

#[tokio::test]
async fn a_tampered_body_is_a_miss_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();

    put(&cache, tenant, URL).await;
    let key = key(URL);
    let body = dir
        .path()
        .join(tenant.to_string())
        .join(&key.as_str()[..2])
        .join(key.as_str());
    std::fs::write(&body, b"[]").unwrap();

    assert_eq!(
        cache
            .get(&AccessScope::for_tenant(tenant), tenant, &key)
            .await
            .unwrap(),
        None,
        "a body that fails its integrity check is dropped, not surfaced"
    );
}

#[tokio::test]
async fn a_body_without_its_meta_file_is_a_miss() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();

    put(&cache, tenant, URL).await;
    let key = key(URL);
    let meta = dir
        .path()
        .join(tenant.to_string())
        .join(&key.as_str()[..2])
        .join(format!("{}.meta.json", key.as_str()));
    std::fs::remove_file(&meta).unwrap();

    assert!(!cached(&cache, tenant, URL).await);
    assert_eq!(
        cache
            .expire(&AccessScope::for_tenant(tenant), tenant, Utc::now())
            .await
            .unwrap(),
        0,
        "an orphaned body has no fetched_at to judge, so it is left alone"
    );
}

#[tokio::test]
async fn a_tenant_with_no_folder_yet_clears_and_expires_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);

    assert_eq!(
        cache
            .clear(&scope, tenant, &["https://api.github.com/repos/acme"])
            .await
            .unwrap(),
        0
    );
    assert_eq!(cache.expire(&scope, tenant, Utc::now()).await.unwrap(), 0);
    assert!(!cached(&cache, tenant, URL).await);
}

#[tokio::test]
async fn size_counts_the_body_and_meta_files_below_the_prefixes() {
    let dir = tempfile::tempdir().unwrap();
    let cache = cache_in(dir.path(), Compression::None);
    let tenant = Uuid::new_v4();
    let scope = AccessScope::for_tenant(tenant);
    let widget = "https://api.github.com/repos/acme/widget";
    let issues = format!("{widget}/issues");
    let pulls = format!("{widget}/pulls?page=2");
    put(&cache, tenant, &issues).await;
    put(&cache, tenant, &pulls).await;
    put(
        &cache,
        tenant,
        "https://api.github.com/repos/acme/other/issues",
    )
    .await;

    let widget_keys = [key(&issues), key(&pulls)];
    let expected: u64 = files_under(dir.path())
        .iter()
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            widget_keys.iter().any(|k| name.starts_with(k.as_str()))
        })
        .map(|path| std::fs::metadata(path).unwrap().len())
        .sum();
    assert!(expected > 0);
    assert_eq!(
        cache.size(&scope, tenant, &[widget]).await.unwrap(),
        expected
    );
    assert_eq!(cache.size(&scope, tenant, &[]).await.unwrap(), 0);
    let stranger = Uuid::new_v4();
    assert_eq!(
        cache
            .size(&AccessScope::for_tenant(stranger), stranger, &[widget])
            .await
            .unwrap(),
        0
    );
}
