//! Every instant a door stores, and so echoes in its answer, is whole microseconds, as Postgres keeps
//! it: a write answers what it wrote (pricing D-453, the twin rule; phase 9 review B1). So a clock read in `src/`
//! is a date (`now_utc().date()`), a doc comment, the one helper that cuts it (`stored_now`), or a
//! named exception below. A door that reads `now_utc()` and stores it fails here, whether or not a
//! door test happens to cover it.
#![expect(
    clippy::unwrap_used,
    reason = "a census of the source tree: an unreadable file fails the test"
)]

use std::fs;
use std::path::{Path, PathBuf};

/// Clock reads that keep the clock's own precision, each with its reason.
const ALLOWED: &[&str] = &[
    // `stored_now` itself: it cuts.
    "src/infra/storage.rs",
    // The outbox payload's `occurred_at`: no read serves it, and the broker keeps its own time.
    "src/infra/events.rs",
    // A shipped migration: frozen, and its instant is written once by the upgrade.
    "src/infra/storage/migrations/m20260928_000010_clear_retired_defaults.rs",
];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && !name.ends_with("_tests.rs")
            && name != "test_support.rs"
        {
            out.push(path);
        }
    }
}

/// Whether a source line reads the clock at its own precision: it names the `now_utc` token (a
/// call, or the function passed by path, as `unwrap_or_else(OffsetDateTime::now_utc)` does), is not
/// a comment, and does not take the date alone (the phase 9 review's R29: the census matched only
/// the call and was blind to the path).
fn reads_the_clock(line: &str) -> bool {
    let code = line.trim_start();
    if code.starts_with("//") {
        return false;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices("now_utc").any(|(at, token)| {
        let before = code[..at].chars().next_back();
        let rest = &code[at + token.len()..];
        !before.is_some_and(word)
            && !rest.chars().next().is_some_and(word)
            && !rest.starts_with("().date()")
    })
}

/// R29's positive control: the matcher reports a clock read in each form it guards, and only those.
#[test]
fn the_census_sees_a_clock_read_in_every_form() {
    for stray in [
        "    let now = time::OffsetDateTime::now_utc();",
        "    .unwrap_or_else(OffsetDateTime::now_utc)",
        "    let at = clock.map_or_else(time::OffsetDateTime::now_utc, |c| c.now());",
        "    let t = now_utc(); // stored",
        "now_utc()",
    ] {
        assert!(reads_the_clock(stray), "a clock read: {stray}");
    }
    for kept in [
        "    let today = time::OffsetDateTime::now_utc().date();",
        "    // time::OffsetDateTime::now_utc() is cut by stored_now",
        "    /// `now_utc()` keeps nanoseconds",
        "    let at = crate::infra::storage::stored_now();",
        "    fn now_utc_cut() {}",
        "    let my_now_utc = 1;",
    ] {
        assert!(!reads_the_clock(kept), "not a stored clock read: {kept}");
    }
}

#[test]
fn every_clock_read_in_src_is_a_date_or_goes_through_stored_now() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut stray = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&file).unwrap();
        if ALLOWED.contains(&rel.as_str()) {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            if reads_the_clock(line) {
                stray.push(format!("{rel}:{}: {}", n + 1, line.trim_start()));
            }
        }
    }
    assert!(
        stray.is_empty(),
        "a stored instant must be cut to whole microseconds through stored_now(): {stray:#?}"
    );
}
