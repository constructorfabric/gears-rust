use super::*;

#[test]
fn test_extract_sqlite_pragmas_basic() {
    let dsn = "sqlite:///path/to/db.sqlite?wal=true&synchronous=NORMAL&other_param=value";
    let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, "sqlite:///path/to/db.sqlite?other_param=value");
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs.get("wal"), Some(&"true".to_owned()));
    assert_eq!(pairs.get("synchronous"), Some(&"NORMAL".to_owned()));
    assert!(!pairs.contains_key("other_param"));
}

#[test]
fn test_extract_sqlite_pragmas_all_params() {
    let dsn = "sqlite:///test.db?wal=false&synchronous=OFF&busy_timeout=5000&journal_mode=DELETE";
    let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, "sqlite:///test.db");
    assert_eq!(pairs.len(), 4);
    assert_eq!(pairs.get("wal"), Some(&"false".to_owned()));
    assert_eq!(pairs.get("synchronous"), Some(&"OFF".to_owned()));
    assert_eq!(pairs.get("busy_timeout"), Some(&"5000".to_owned()));
    assert_eq!(pairs.get("journal_mode"), Some(&"DELETE".to_owned()));
}

#[test]
fn test_extract_sqlite_pragmas_case_insensitive() {
    let dsn = "sqlite:///test.db?WAL=true&SYNCHRONOUS=normal&Journal_Mode=wal";
    let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, "sqlite:///test.db");
    assert_eq!(pairs.len(), 3);
    assert_eq!(pairs.get("wal"), Some(&"true".to_owned()));
    assert_eq!(pairs.get("synchronous"), Some(&"normal".to_owned()));
    assert_eq!(pairs.get("journal_mode"), Some(&"wal".to_owned()));
}

#[test]
fn test_extract_sqlite_pragmas_no_sqlite_params() {
    let dsn = "sqlite:///test.db?other=value&another=param";
    let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, dsn);
    assert!(pairs.is_empty());
}

#[test]
fn test_extract_sqlite_pragmas_only_sqlite_params() {
    let dsn = "sqlite:///test.db?wal=true&synchronous=NORMAL";
    let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, "sqlite:///test.db");
    assert_eq!(pairs.len(), 2);
}

#[test]
fn test_extract_sqlite_pragmas_invalid_url() {
    let dsn = "/plain/file/path.db";
    let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, dsn);
    assert!(pairs.is_empty());
}

#[test]
fn test_extract_sqlite_pragmas_convenience() {
    let dsn = "sqlite:///test.db?wal=true&other=value";
    let (clean_dsn, _) = extract_sqlite_pragmas(dsn);

    assert_eq!(clean_dsn, "sqlite:///test.db?other=value");
}

#[test]
fn test_is_memory_dsn() {
    assert!(is_memory_dsn("sqlite::memory:"));
    assert!(is_memory_dsn("sqlite://memory:"));
    assert!(is_memory_dsn("sqlite:///test.db?mode=memory"));
    assert!(is_memory_dsn("sqlite:///test.db?other=value&mode=memory"));

    assert!(!is_memory_dsn("sqlite:///test.db"));
    assert!(!is_memory_dsn("sqlite:///test.db?mode=file"));
    assert!(!is_memory_dsn("/plain/path.db"));
}

#[test]
fn test_is_memory_dsn_case_insensitive() {
    assert!(is_memory_dsn("sqlite:///test.db?MODE=MEMORY"));
    assert!(is_memory_dsn("sqlite:///test.db?mode=Memory"));
}
