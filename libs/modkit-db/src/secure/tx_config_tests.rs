use super::*;

#[test]
fn test_default_tx_config() {
    let cfg = TxConfig::default();
    assert!(cfg.isolation.is_none());
    assert!(cfg.access_mode.is_none());
}

#[test]
fn test_tx_config_with_isolation() {
    let cfg = TxConfig::with_isolation(TxIsolationLevel::Serializable);
    assert_eq!(cfg.isolation, Some(TxIsolationLevel::Serializable));
    assert!(cfg.access_mode.is_none());
}

#[test]
fn test_tx_config_read_only() {
    let cfg = TxConfig::read_only();
    assert!(cfg.isolation.is_none());
    assert_eq!(cfg.access_mode, Some(TxAccessMode::ReadOnly));
}

#[test]
fn test_tx_config_serializable() {
    let cfg = TxConfig::serializable();
    assert_eq!(cfg.isolation, Some(TxIsolationLevel::Serializable));
    assert!(cfg.access_mode.is_none());
}

#[test]
fn test_isolation_level_conversion() {
    assert!(matches!(
        IsolationLevel::from(TxIsolationLevel::ReadUncommitted),
        IsolationLevel::ReadUncommitted
    ));
    assert!(matches!(
        IsolationLevel::from(TxIsolationLevel::ReadCommitted),
        IsolationLevel::ReadCommitted
    ));
    assert!(matches!(
        IsolationLevel::from(TxIsolationLevel::RepeatableRead),
        IsolationLevel::RepeatableRead
    ));
    assert!(matches!(
        IsolationLevel::from(TxIsolationLevel::Serializable),
        IsolationLevel::Serializable
    ));
}

#[test]
fn test_access_mode_conversion() {
    assert!(matches!(
        AccessMode::from(TxAccessMode::ReadOnly),
        AccessMode::ReadOnly
    ));
    assert!(matches!(
        AccessMode::from(TxAccessMode::ReadWrite),
        AccessMode::ReadWrite
    ));
}
