use super::*;
use std::io::Write;

/// Helper: run concurrent write test through a given `MakeWriter` and verify output.
fn assert_concurrent_writes<'a, W>(writer: &'a W, log_path: &Path)
where
    W: fmt::MakeWriter<'a> + Sync,
    W::Writer: Write,
{
    const NUM_THREADS: usize = 8;
    const LINES_PER_THREAD: usize = 500;
    const TOTAL_LINES: usize = NUM_THREADS * LINES_PER_THREAD;

    std::thread::scope(|s| {
        for thread_id in 0..NUM_THREADS {
            s.spawn(move || {
                for line_no in 0..LINES_PER_THREAD {
                    let mut handle = writer.make_writer();
                    writeln!(handle, "thread={thread_id} line={line_no}")
                        .expect("write must not fail");
                }
            });
        }
    });

    let content = std::fs::read_to_string(log_path).expect("failed to read log file");
    let lines: Vec<&str> = content.lines().collect();

    assert_eq!(
        lines.len(),
        TOTAL_LINES,
        "expected {TOTAL_LINES} lines but found {} ({} records {})",
        lines.len(),
        lines.len().abs_diff(TOTAL_LINES),
        if lines.len() < TOTAL_LINES {
            "lost"
        } else {
            "extra"
        },
    );

    // Verify every line is intact (no interleaved bytes)
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.starts_with("thread=") && line.contains(" line="),
            "corrupted line {i}: {line:?}",
        );
    }
}

#[test]
fn concurrent_writes_are_not_dropped() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let log_path = dir.path().join("test.log");

    let writer = create_rotating_writer_at_path(&log_path, 50 * 1024 * 1024, None, Some(1))
        .expect("failed to create rotating writer");

    assert_concurrent_writes(&writer, &log_path);
}

#[test]
fn concurrent_writes_through_routed_writer() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let log_path = dir.path().join("routed.log");

    let rot = create_rotating_writer_at_path(&log_path, 50 * 1024 * 1024, None, Some(1))
        .expect("failed to create rotating writer");

    let router = MultiFileRouter {
        default: Some(rot),
        by_prefix: Vec::new(),
    };

    assert_concurrent_writes(&router, &log_path);
}

/// Helper: create a `RotWriter` for a temp path and return (writer, path).
fn tmp_writer(dir: &Path, name: &str) -> (RotWriter, std::path::PathBuf) {
    let p = dir.join(name);
    let w = create_rotating_writer_at_path(&p, 50 * 1024 * 1024, None, Some(1))
        .expect("failed to create rotating writer");
    (w, p)
}

#[test]
fn resolve_for_picks_longest_matching_prefix() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");

    let (broad, broad_path) = tmp_writer(dir.path(), "broad.log");
    let (specific, specific_path) = tmp_writer(dir.path(), "specific.log");

    // Write markers so we can tell them apart
    broad.0.lock().write_all(b"BROAD\n").unwrap();
    specific.0.lock().write_all(b"SPECIFIC\n").unwrap();

    let router = MultiFileRouter {
        default: None,
        by_prefix: vec![
            ("hyperspot::api_gateway".into(), specific),
            ("hyperspot".into(), broad),
        ],
    };

    // "hyperspot::api_gateway::handler" should match the longer prefix
    let mut handle = router
        .resolve_for("hyperspot::api_gateway::handler")
        .expect("should resolve");
    handle.write_all(b"routed\n").unwrap();
    handle.flush().unwrap();

    let specific_content = std::fs::read_to_string(&specific_path).unwrap();
    assert!(
        specific_content.contains("routed"),
        "expected write to land in specific log, got: {specific_content:?}"
    );

    let broad_content = std::fs::read_to_string(&broad_path).unwrap();
    assert!(
        !broad_content.contains("routed"),
        "write should NOT appear in broad log, got: {broad_content:?}"
    );
}

/// Verifies that `build_file_router` sorts `by_prefix` by descending length so
/// that the longest (most-specific) prefix wins even when the caller registers
/// a broad prefix before a specific one.
#[test]
fn build_file_router_sorts_prefixes_longest_match_wins() {
    use crate::bootstrap::config::SectionFile;

    let dir = tempfile::tempdir().expect("failed to create temp dir");

    let broad_section = Section {
        console_format: ConsoleFormat::default(),
        console_level: None,
        section_file: Some(SectionFile {
            file: "broad.log".to_owned(),
            file_level: None,
        }),
        max_age_days: None,
        max_backups: Some(1),
        max_size_mb: None,
    };
    let specific_section = Section {
        console_format: ConsoleFormat::default(),
        console_level: None,
        section_file: Some(SectionFile {
            file: "specific.log".to_owned(),
            file_level: None,
        }),
        max_age_days: None,
        max_backups: Some(1),
        max_size_mb: None,
    };

    // Register broad BEFORE specific (reverse of preference order) so that
    // build_file_router's sort step is what makes the specific prefix win.
    let config = ConfigData {
        default_section: None,
        crate_sections: vec![
            ("hyperspot".to_owned(), &broad_section),
            ("hyperspot::api_gateway".to_owned(), &specific_section),
        ],
    };

    let router = build_file_router(&config, dir.path());

    let mut handle = router
        .resolve_for("hyperspot::api_gateway::handler")
        .expect("should resolve");
    handle.write_all(b"routed\n").unwrap();
    handle.flush().unwrap();

    let specific_content = std::fs::read_to_string(dir.path().join("specific.log")).unwrap();
    assert!(
        specific_content.contains("routed"),
        "expected write to land in specific log, got: {specific_content:?}"
    );

    let broad_content = std::fs::read_to_string(dir.path().join("broad.log")).unwrap();
    assert!(
        !broad_content.contains("routed"),
        "write should NOT appear in broad log, got: {broad_content:?}"
    );
}

#[test]
fn resolve_for_exact_match() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let (writer, _) = tmp_writer(dir.path(), "exact.log");

    let router = MultiFileRouter {
        default: None,
        by_prefix: vec![("hyperspot".into(), writer)],
    };

    // Exact match
    assert!(
        router.resolve_for("hyperspot").is_some(),
        "exact target should match"
    );
    // Submodule match
    assert!(
        router.resolve_for("hyperspot::sub").is_some(),
        "submodule target should match"
    );
    // Non-prefix string must NOT match
    assert!(
        router.resolve_for("hyperspot_extra").is_none(),
        "non-prefix target should not match"
    );
    assert!(
        router.resolve_for("other").is_none(),
        "unrelated target should not match"
    );
}

#[test]
fn resolve_for_falls_back_to_default() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let (default_writer, default_path) = tmp_writer(dir.path(), "default.log");

    default_writer.0.lock().write_all(b"DEFAULT\n").unwrap();

    let router = MultiFileRouter {
        default: Some(default_writer),
        by_prefix: vec![],
    };

    // Unknown target should fall back to default
    let mut handle = router
        .resolve_for("unknown_crate::module")
        .expect("should fall back to default");
    handle.write_all(b"fallback\n").unwrap();
    handle.flush().unwrap();

    let content = std::fs::read_to_string(&default_path).unwrap();
    assert!(
        content.contains("fallback"),
        "expected write to land in default log, got: {content:?}"
    );
}
