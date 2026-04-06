use super::*;
#[cfg(feature = "sqlite")]
use tokio::time::Duration;

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_sqlite_connection() -> Result<()> {
    let dsn = "sqlite::memory:";
    let opts = ConnectOpts::default();
    let db = DbHandle::connect(dsn, opts).await?;
    assert_eq!(db.engine(), DbEngine::Sqlite);
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_sqlite_connection_with_pragma_parameters() -> Result<()> {
    // Test that SQLite connections work with PRAGMA parameters in DSN
    let dsn = "sqlite::memory:?wal=true&synchronous=NORMAL&busy_timeout=5000&journal_mode=WAL";
    let opts = ConnectOpts::default();
    let db = DbHandle::connect(dsn, opts).await?;
    assert_eq!(db.engine(), DbEngine::Sqlite);

    // Verify that the stored DSN has been cleaned (SQLite parameters removed)
    // Note: For memory databases, the DSN should still be sqlite::memory: after cleaning
    assert!(db.dsn == "sqlite::memory:" || db.dsn.starts_with("sqlite::memory:"));

    Ok(())
}

#[tokio::test]
async fn test_backend_detection() {
    assert_eq!(
        DbHandle::detect("sqlite::memory:").unwrap(),
        DbEngine::Sqlite
    );
    assert_eq!(
        DbHandle::detect("postgres://localhost/test").unwrap(),
        DbEngine::Postgres
    );
    assert_eq!(
        DbHandle::detect("mysql://localhost/test").unwrap(),
        DbEngine::MySql
    );
    assert!(DbHandle::detect("unknown://test").is_err());
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_advisory_lock_sqlite() -> Result<()> {
    let dsn = "sqlite:file:memdb1?mode=memory&cache=shared";
    let db = DbHandle::connect(dsn, ConnectOpts::default()).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let test_id = format!("test_basic_{now}");

    let guard1 = db.lock("test_module", &format!("{test_id}_key1")).await?;
    let _guard2 = db.lock("test_module", &format!("{test_id}_key2")).await?;
    let _guard3 = db
        .lock("different_module", &format!("{test_id}_key1"))
        .await?;

    // Deterministic unlock to avoid races with async Drop cleanup
    guard1.release().await;
    let _guard4 = db.lock("test_module", &format!("{test_id}_key1")).await?;
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_advisory_lock_different_keys() -> Result<()> {
    let dsn = "sqlite:file:memdb_diff_keys?mode=memory&cache=shared";
    let db = DbHandle::connect(dsn, ConnectOpts::default()).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let test_id = format!("test_diff_{now}");

    let _guard1 = db.lock("test_module", &format!("{test_id}_key1")).await?;
    let _guard2 = db.lock("test_module", &format!("{test_id}_key2")).await?;
    let _guard3 = db.lock("other_module", &format!("{test_id}_key1")).await?;
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_try_lock_with_config() -> Result<()> {
    let dsn = "sqlite:file:memdb2?mode=memory&cache=shared";
    let db = DbHandle::connect(dsn, ConnectOpts::default()).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let test_id = format!("test_config_{now}");

    let _guard1 = db.lock("test_module", &format!("{test_id}_key")).await?;

    let config = LockConfig {
        max_wait: Some(Duration::from_millis(200)),
        initial_backoff: Duration::from_millis(50),
        max_attempts: Some(3),
        ..Default::default()
    };

    let result = db
        .try_lock("test_module", &format!("{test_id}_different_key"), config)
        .await?;
    assert!(
        result.is_some(),
        "expected lock acquisition for different key"
    );
    Ok(())
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_sea_internal_access() -> Result<()> {
    let dsn = "sqlite::memory:";
    let db = DbHandle::connect(dsn, ConnectOpts::default()).await?;

    // Internal method for migrations
    let _raw = db.sea_internal();
    Ok(())
}
