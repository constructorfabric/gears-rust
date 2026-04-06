use super::*;

#[test]
fn test_journal_mode_parsing() {
    assert_eq!(JournalMode::from_str("DELETE"), Some(JournalMode::Delete));
    assert_eq!(JournalMode::from_str("wal"), Some(JournalMode::Wal));
    assert_eq!(JournalMode::from_str("MEMORY"), Some(JournalMode::Memory));
    assert_eq!(JournalMode::from_str("invalid"), None);
}

#[test]
fn test_sync_mode_parsing() {
    assert_eq!(SyncMode::from_str("OFF"), Some(SyncMode::Off));
    assert_eq!(SyncMode::from_str("normal"), Some(SyncMode::Normal));
    assert_eq!(SyncMode::from_str("FULL"), Some(SyncMode::Full));
    assert_eq!(SyncMode::from_str("invalid"), None);
}

#[test]
fn test_pragmas_from_pairs() {
    let mut pairs = HashMap::new();
    pairs.insert("journal_mode".to_owned(), "WAL".to_owned());
    pairs.insert("synchronous".to_owned(), "NORMAL".to_owned());
    pairs.insert("busy_timeout".to_owned(), "5000".to_owned());
    pairs.insert("wal".to_owned(), "true".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);

    assert_eq!(pragmas.journal_mode, Some(JournalMode::Wal));
    assert_eq!(pragmas.synchronous, Some(SyncMode::Normal));
    assert_eq!(pragmas.busy_timeout_ms, Some(5000));
    assert_eq!(pragmas.wal_toggle, Some(true));
}

#[test]
fn test_pragmas_invalid_values() {
    let mut pairs = HashMap::new();
    pairs.insert("journal_mode".to_owned(), "INVALID".to_owned());
    pairs.insert("synchronous".to_owned(), "INVALID".to_owned());
    pairs.insert("busy_timeout".to_owned(), "-1".to_owned());
    pairs.insert("wal".to_owned(), "maybe".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);

    assert_eq!(pragmas.journal_mode, None);
    assert_eq!(pragmas.synchronous, None);
    assert_eq!(pragmas.busy_timeout_ms, None);
    assert_eq!(pragmas.wal_toggle, None);
}

#[test]
fn test_pragmas_case_insensitive() {
    let mut pairs = HashMap::new();
    pairs.insert("JOURNAL_MODE".to_owned(), "delete".to_owned());
    pairs.insert("SYNCHRONOUS".to_owned(), "off".to_owned());
    pairs.insert("WAL".to_owned(), "FALSE".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);

    assert_eq!(pragmas.journal_mode, Some(JournalMode::Delete));
    assert_eq!(pragmas.synchronous, Some(SyncMode::Off));
    assert_eq!(pragmas.wal_toggle, Some(false));
}

#[test]
fn test_pragmas_unknown_keys() {
    let mut pairs = HashMap::new();
    pairs.insert("unknown_param".to_owned(), "value".to_owned());
    pairs.insert("journal_mode".to_owned(), "WAL".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);

    assert_eq!(pragmas.journal_mode, Some(JournalMode::Wal));
    // Unknown params should be ignored without error
}

#[test]
fn test_pragmas_wal_numeric_toggle() {
    let mut pairs = HashMap::new();
    pairs.insert("wal".to_owned(), "1".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);
    assert_eq!(pragmas.wal_toggle, Some(true));

    let mut pairs = HashMap::new();
    pairs.insert("wal".to_owned(), "0".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);
    assert_eq!(pragmas.wal_toggle, Some(false));
}

#[test]
fn test_pragmas_busy_timeout_zero() {
    let mut pairs = HashMap::new();
    pairs.insert("busy_timeout".to_owned(), "0".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);
    assert_eq!(pragmas.busy_timeout_ms, Some(0));
}

#[test]
fn test_pragmas_busy_timeout_invalid() {
    let mut pairs = HashMap::new();
    pairs.insert("busy_timeout".to_owned(), "not_a_number".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);
    assert_eq!(pragmas.busy_timeout_ms, None);
}

#[test]
fn test_pragmas_partial_map() {
    let mut pairs = HashMap::new();
    pairs.insert("synchronous".to_owned(), "FULL".to_owned());

    let pragmas = Pragmas::from_pairs(&pairs);
    assert_eq!(pragmas.journal_mode, None);
    assert_eq!(pragmas.synchronous, Some(SyncMode::Full));
    assert_eq!(pragmas.busy_timeout_ms, None);
    assert_eq!(pragmas.wal_toggle, None);
}

#[test]
fn test_pragmas_empty_map() {
    let pairs = HashMap::new();

    let pragmas = Pragmas::from_pairs(&pairs);
    assert_eq!(pragmas.journal_mode, None);
    assert_eq!(pragmas.synchronous, None);
    assert_eq!(pragmas.busy_timeout_ms, None);
    assert_eq!(pragmas.wal_toggle, None);
}
