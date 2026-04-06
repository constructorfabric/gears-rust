use super::*;
use anyhow::Result;
use std::sync::Arc;

#[tokio::test]
async fn test_namespaced_locks() -> Result<()> {
    let lock_manager = LockManager::new("test_dsn".to_owned());

    // Unique key suffix (avoid conflicts in parallel)
    let test_id = format!(
        "test_ns_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let guard1 = lock_manager
        .lock("module1", &format!("{test_id}_key"))
        .await?;
    let guard2 = lock_manager
        .lock("module2", &format!("{test_id}_key"))
        .await?;

    assert!(!guard1.key().is_empty());
    assert!(!guard2.key().is_empty());

    guard1.release().await;
    guard2.release().await;
    Ok(())
}

#[tokio::test]
async fn test_try_lock_with_timeout() -> Result<()> {
    let lock_manager = Arc::new(LockManager::new("test_dsn".to_owned()));

    let test_id = format!(
        "test_timeout_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let _guard1 = lock_manager
        .lock("test_module", &format!("{test_id}_key"))
        .await?;

    // Different key should succeed quickly even with retries/timeouts
    let config = LockConfig {
        max_wait: Some(Duration::from_millis(200)),
        initial_backoff: Duration::from_millis(50),
        max_attempts: Some(3),
        ..Default::default()
    };

    let result = lock_manager
        .try_lock("test_module", &format!("{test_id}_different_key"), config)
        .await?;
    assert!(result.is_some(), "expected successful lock acquisition");
    Ok(())
}

#[tokio::test]
async fn test_try_lock_success() -> Result<()> {
    let lock_manager = LockManager::new("test_dsn".to_owned());

    let test_id = format!(
        "test_success_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let result = lock_manager
        .try_lock(
            "test_module",
            &format!("{test_id}_key"),
            LockConfig::default(),
        )
        .await?;
    assert!(result.is_some(), "expected lock acquisition");
    Ok(())
}

#[tokio::test]
async fn test_double_lock_same_key_errors() -> Result<()> {
    let lock_manager = LockManager::new("test_dsn".to_owned());

    let test_id = format!(
        "test_double_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let guard = lock_manager.lock("test_module", &test_id).await?;
    let err = lock_manager
        .lock("test_module", &test_id)
        .await
        .unwrap_err();
    match err {
        DbLockError::AlreadyHeld { lock_name } => {
            assert!(lock_name.contains(&test_id));
        }
        other => panic!("unexpected error: {other:?}"),
    }

    guard.release().await;
    Ok(())
}

#[tokio::test]
async fn test_try_lock_conflict_returns_none() -> Result<()> {
    let lock_manager = LockManager::new("test_dsn".to_owned());

    let key = format!(
        "test_conflict_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    let _guard = lock_manager.lock("module", &key).await?;
    let config = LockConfig {
        max_wait: Some(Duration::from_millis(100)),
        max_attempts: Some(2),
        ..Default::default()
    };
    let res = lock_manager.try_lock("module", &key, config).await?;
    assert!(res.is_none());
    Ok(())
}
