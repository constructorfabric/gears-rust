// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
use std::path::Path;

use super::*;

fn write(path: &Path, content: &str) {
    std::fs::write(path, content).expect("write token file");
}

fn file_store(path: &Path) -> TokenStore {
    TokenStore::new(TokenSource::File(path.to_path_buf())).expect("a token file is read lazily")
}

fn inline(token: &str) -> anyhow::Result<TokenStore> {
    TokenStore::new(TokenSource::Inline(VaultToken::from(token)))
}

// -- source selection ----------------------------------------------------------

#[test]
fn from_config_picks_the_single_configured_source() {
    let inline = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("t")),
        ..VaultCredStorePluginConfig::default()
    };
    assert_eq!(
        TokenSource::from_config(&inline).expect("ok").kind(),
        "token"
    );

    let file = VaultCredStorePluginConfig {
        token_file: Some("/vault/secrets/token".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    let source = TokenSource::from_config(&file).expect("ok");
    assert_eq!(source.kind(), "token_file");
    assert!(matches!(&source, TokenSource::File(p) if p == Path::new("/vault/secrets/token")));

    let env = VaultCredStorePluginConfig {
        token_env: Some("VAULT_TOKEN".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    assert_eq!(
        TokenSource::from_config(&env).expect("ok").kind(),
        "token_env"
    );
}

#[test]
fn from_config_rejects_zero_or_several_sources() {
    assert!(TokenSource::from_config(&VaultCredStorePluginConfig::default()).is_err());
    let two = VaultCredStorePluginConfig {
        token: Some(VaultToken::from("t")),
        token_env: Some("X".to_owned()),
        ..VaultCredStorePluginConfig::default()
    };
    assert!(TokenSource::from_config(&two).is_err());
}

#[test]
fn debug_output_never_shows_the_token() {
    let source = TokenSource::Inline(VaultToken::from("s.super-secret"));
    assert!(!format!("{source:?}").contains("s.super-secret"));
}

// -- inline --------------------------------------------------------------------

#[tokio::test]
async fn inline_token_is_trimmed_and_served() {
    let store = inline("  s.abc\n").expect("ok");
    assert_eq!(&*store.current().await.expect("token"), "s.abc");
}

#[test]
fn inline_token_must_be_usable() {
    assert!(inline("").is_err());
    assert!(inline("   ").is_err());
    assert!(inline("two words").is_err());
    assert!(inline("tab\tinside").is_err());
    assert!(inline("caf\u{e9}").is_err());
}

#[test]
fn startup_errors_never_contain_the_token() {
    let msg = format!("{:#}", inline("bad token value").err().expect("rejected"));
    assert!(!msg.contains("bad token value"), "{msg}");
}

#[tokio::test]
async fn inline_token_is_never_refreshed() {
    let store = inline("s.abc").expect("ok");
    assert!(store.refreshed_after_rejection("s.abc").await.is_none());
}

// -- environment ---------------------------------------------------------------

#[tokio::test]
async fn env_token_is_read_at_startup_and_trimmed() {
    temp_env::async_with_vars(
        [("VAULT_CREDSTORE_TEST_TOKEN_ENV", Some("s.from-env\n"))],
        async {
            let store = TokenStore::new(TokenSource::Env(
                "VAULT_CREDSTORE_TEST_TOKEN_ENV".to_owned(),
            ))
            .expect("variable is set");
            assert_eq!(&*store.current().await.expect("token"), "s.from-env");
        },
    )
    .await;
}

#[test]
fn missing_or_empty_env_token_fails_startup_naming_the_variable() {
    temp_env::with_vars(
        [
            ("VAULT_CREDSTORE_TEST_UNSET_ENV", None::<&str>),
            ("VAULT_CREDSTORE_TEST_EMPTY_ENV", Some("")),
        ],
        || {
            for name in [
                "VAULT_CREDSTORE_TEST_UNSET_ENV",
                "VAULT_CREDSTORE_TEST_EMPTY_ENV",
            ] {
                let err = TokenStore::new(TokenSource::Env(name.to_owned()))
                    .err()
                    .expect("rejected");
                assert!(format!("{err:#}").contains(name), "{err:#}");
            }
        },
    );
}

#[tokio::test]
async fn env_token_is_not_changed_by_a_rejection() {
    temp_env::async_with_vars(
        [("VAULT_CREDSTORE_TEST_TOKEN_ENV2", Some("s.same"))],
        async {
            let store = TokenStore::new(TokenSource::Env(
                "VAULT_CREDSTORE_TEST_TOKEN_ENV2".to_owned(),
            ))
            .expect("ok");
            assert!(store.refreshed_after_rejection("s.same").await.is_none());
        },
    )
    .await;
}

// -- file ----------------------------------------------------------------------

#[tokio::test]
async fn file_token_is_read_lazily_and_trimmed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");

    // The sidecar has not written the file yet: the store still builds.
    let store = file_store(&path);
    assert_eq!(
        store.current().await.unwrap_err(),
        TokenError::Unreadable(std::io::ErrorKind::NotFound)
    );

    write(&path, "s.first\n");
    assert_eq!(&*store.current().await.expect("token"), "s.first");
}

#[tokio::test]
async fn file_token_is_cached_until_a_rejection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    write(&path, "s.old");
    let store = file_store(&path);
    assert_eq!(&*store.current().await.expect("token"), "s.old");

    write(&path, "s.new");
    assert_eq!(
        &*store.current().await.expect("token"),
        "s.old",
        "the file is not polled"
    );
}

#[tokio::test]
async fn rejection_re_reads_the_file_and_returns_a_rotated_token() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    write(&path, "s.old");
    let store = file_store(&path);
    store.current().await.expect("token");

    write(&path, "s.new\n");
    let fresh = store.refreshed_after_rejection("s.old").await;
    assert_eq!(fresh.as_deref(), Some("s.new"));
    assert_eq!(
        &*store.current().await.expect("token"),
        "s.new",
        "cache follows"
    );
}

#[tokio::test]
async fn rejection_with_an_unchanged_file_offers_nothing_new() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    write(&path, "s.same");
    let store = file_store(&path);
    store.current().await.expect("token");

    assert!(store.refreshed_after_rejection("s.same").await.is_none());
}

#[tokio::test]
async fn rejection_with_an_unreadable_file_offers_nothing_and_keeps_the_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    write(&path, "s.old");
    let store = file_store(&path);
    store.current().await.expect("token");

    // Mid-rotation states: truncated, then gone.
    write(&path, "");
    assert!(store.refreshed_after_rejection("s.old").await.is_none());
    std::fs::remove_file(&path).expect("remove");
    assert!(store.refreshed_after_rejection("s.old").await.is_none());
    assert_eq!(&*store.current().await.expect("token"), "s.old");
}

#[tokio::test]
async fn a_concurrently_refreshed_cache_still_yields_the_newer_token() {
    // Request A used `s.old`; request B already rotated the cache to `s.new`.
    // A's rejection re-reads the file, finds `s.new` differs from what A sent,
    // and A retries with it.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("token");
    write(&path, "s.old");
    let store = file_store(&path);
    store.current().await.expect("token");
    write(&path, "s.new");
    assert!(store.refreshed_after_rejection("s.old").await.is_some());
    assert_eq!(
        store.refreshed_after_rejection("s.old").await.as_deref(),
        Some("s.new")
    );
}

#[test]
fn token_errors_describe_the_cause_without_a_token() {
    for err in [
        TokenError::NotSet,
        TokenError::Unreadable(std::io::ErrorKind::PermissionDenied),
        TokenError::Empty,
        TokenError::Malformed,
    ] {
        assert!(!err.to_string().is_empty());
    }
}
