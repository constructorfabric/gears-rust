//! `SQLite` DSN parsing and cleaning utilities.
//!
//! Everything here follows the grammar `sqlx` itself uses in
//! `SqliteConnectOptions::from_str`: strip the `sqlite://` / `sqlite:` scheme, split at the
//! first `?`, and form-decode only the query. A `SQLite` DSN is **not** a WHATWG URL, so it is
//! never round-tripped through [`url::Url`] — doing so turns a Windows drive letter into a host
//! and rewrites backslashes.

use std::collections::HashMap;

/// Split a `SQLite` DSN into its database part and its query part.
///
/// The database part is returned verbatim (still percent-encoded); the query part is [`None`]
/// when the DSN carries no `?`. Mirrors `sqlx`'s scheme trimming exactly, including the fact
/// that a DSN without a scheme is treated as a bare path.
// `pub(crate)` is intentional API hygiene even though the parent `sqlite` module is private
// (clippy::redundant_pub_crate would prefer `pub`), matching `sqlite::path`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn split_sqlite_dsn(dsn: &str) -> (&str, Option<&str>) {
    let rest = dsn
        .trim_start_matches("sqlite://")
        .trim_start_matches("sqlite:");

    match rest.split_once('?') {
        Some((database, params)) => (database, Some(params)),
        None => (rest, None),
    }
}

/// Extract Toolkit PRAGMA parameters without rewriting the `SQLite` path.
///
/// This follows `sqlx`'s DSN grammar: split at the first `?`, treat the left
/// side as a path, and form-decode the query. Non-PRAGMA pairs stay unchanged.
///
/// Returns:
/// - the DSN with Toolkit PRAGMA parameters removed;
/// - decoded PRAGMA pairs with lowercase keys.
pub fn extract_sqlite_pragmas(dsn: &str) -> (String, HashMap<String, String>) {
    const SQLITE_PRAGMA_PARAMS: &[&str] = &["wal", "synchronous", "busy_timeout", "journal_mode"];

    let Some((path, query)) = dsn.split_once('?') else {
        return (dsn.to_owned(), HashMap::new());
    };

    let mut extracted_pairs = HashMap::new();
    let mut remaining_pairs = Vec::new();

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let Some((key, value)) = url::form_urlencoded::parse(pair.as_bytes()).next() else {
            continue;
        };
        let key_lower = key.to_lowercase();
        if SQLITE_PRAGMA_PARAMS.contains(&key_lower.as_str()) {
            extracted_pairs.insert(key_lower, value.into_owned());
        } else {
            remaining_pairs.push(pair);
        }
    }

    if remaining_pairs.is_empty() {
        (path.to_owned(), extracted_pairs)
    } else {
        (
            format!("{path}?{}", remaining_pairs.join("&")),
            extracted_pairs,
        )
    }
}

/// Check whether a `SQLite` DSN selects an in-memory database.
///
/// Memory is exactly what `sqlx` treats as memory: a database part of `:memory:`
/// (`sqlite::memory:`, `sqlite://:memory:`) or a `mode=memory` query. Note that
/// `sqlite://memory:` is *not* memory — `sqlx` opens a file literally named `memory:`.
///
/// The `mode` comparison is case-insensitive where `sqlx` is case-sensitive; `sqlx` rejects
/// `mode=MEMORY` outright, so the extra leniency only affects DSNs that cannot connect anyway.
pub fn is_memory_dsn(dsn: &str) -> bool {
    let (database, params) = split_sqlite_dsn(dsn);

    if database == ":memory:" {
        return true;
    }

    params.is_some_and(|params| {
        url::form_urlencoded::parse(params.as_bytes()).any(|(key, value)| {
            key.eq_ignore_ascii_case("mode") && value.eq_ignore_ascii_case("memory")
        })
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
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
        let dsn =
            "sqlite:///test.db?wal=false&synchronous=OFF&busy_timeout=5000&journal_mode=DELETE";
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
    fn percent_encoded_pragmas_are_decoded_without_rewriting_other_pairs() {
        let dsn = "sqlite:///test.db?w%61l=f%61lse&busy_timeout=5%3000&vfs=custom%2Fvfs";
        let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

        assert_eq!(clean_dsn, "sqlite:///test.db?vfs=custom%2Fvfs");
        assert_eq!(pairs.get("wal"), Some(&"false".to_owned()));
        assert_eq!(pairs.get("busy_timeout"), Some(&"5000".to_owned()));
    }

    #[test]
    fn test_is_memory_dsn() {
        assert!(is_memory_dsn("sqlite::memory:"));
        assert!(is_memory_dsn("sqlite://:memory:"));
        assert!(is_memory_dsn(":memory:"));
        assert!(is_memory_dsn("sqlite:///test.db?mode=memory"));
        assert!(is_memory_dsn("sqlite:///test.db?other=value&mode=memory"));

        assert!(!is_memory_dsn("sqlite:///test.db"));
        assert!(!is_memory_dsn("sqlite:///test.db?mode=file"));
        assert!(!is_memory_dsn("/plain/path.db"));
    }

    #[test]
    fn memory_without_leading_colon_is_a_file_named_memory() {
        // `sqlx` trims `sqlite://` and opens a file literally called `memory:`.
        assert!(!is_memory_dsn("sqlite://memory:"));
    }

    #[test]
    fn split_sqlite_dsn_matches_sqlx_scheme_trimming() {
        assert_eq!(
            split_sqlite_dsn("sqlite:///abs/app.db"),
            ("/abs/app.db", None)
        );
        assert_eq!(
            split_sqlite_dsn("sqlite://./rel/app.db"),
            ("./rel/app.db", None)
        );
        assert_eq!(split_sqlite_dsn("sqlite:app.db"), ("app.db", None));
        assert_eq!(split_sqlite_dsn("/plain/app.db"), ("/plain/app.db", None));
        assert_eq!(
            split_sqlite_dsn("sqlite://C:\\dir\\app.db?mode=rwc"),
            ("C:\\dir\\app.db", Some("mode=rwc"))
        );
    }

    #[test]
    fn test_is_memory_dsn_case_insensitive() {
        assert!(is_memory_dsn("sqlite:///test.db?MODE=MEMORY"));
        assert!(is_memory_dsn("sqlite:///test.db?mode=Memory"));
        assert!(is_memory_dsn("sqlite:///test.db?mode=mem%6Fry"));
    }

    #[test]
    fn windows_backslash_path_survives_pragma_extraction() {
        // Generic URL parsing rewrites this native path; `sqlx` does not.
        let dsn = "sqlite://C:\\Users\\runner\\AppData\\app.db?mode=rwc&wal=true";
        let (clean_dsn, pairs) = extract_sqlite_pragmas(dsn);

        assert_eq!(
            clean_dsn,
            "sqlite://C:\\Users\\runner\\AppData\\app.db?mode=rwc"
        );
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs.get("wal"), Some(&"true".to_owned()));
    }

    #[test]
    fn windows_backslash_path_is_not_memory() {
        assert!(!is_memory_dsn(
            "sqlite://C:\\Users\\runner\\AppData\\app.db?mode=rwc"
        ));
        assert!(is_memory_dsn(
            "sqlite://C:\\Users\\runner\\AppData\\app.db?mode=memory"
        ));
    }

    #[test]
    fn plain_path_with_pragmas_is_cleaned_too() {
        // `sqlx` accepts plain paths as DSNs too.
        let (clean_dsn, pairs) = extract_sqlite_pragmas("/plain/file/path.db?wal=true");
        assert_eq!(clean_dsn, "/plain/file/path.db");
        assert_eq!(pairs.get("wal"), Some(&"true".to_owned()));
    }

    #[test]
    fn trailing_question_mark_and_empty_pairs_are_dropped() {
        let (clean_dsn, pairs) = extract_sqlite_pragmas("sqlite:///test.db?");
        assert_eq!(clean_dsn, "sqlite:///test.db");
        assert!(pairs.is_empty());

        let (clean_dsn, pairs) = extract_sqlite_pragmas("sqlite:///test.db?&&wal=true&");
        assert_eq!(clean_dsn, "sqlite:///test.db");
        assert_eq!(pairs.get("wal"), Some(&"true".to_owned()));
    }
}
