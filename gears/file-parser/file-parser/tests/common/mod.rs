//! Shared helpers for the parser integration tests.

use std::path::PathBuf;

/// Path to an e2e fixture: `<repo root>/testing/e2e/testdata/<subdir>/<filename>`.
///
/// `CARGO_MANIFEST_DIR` is `<repo root>/gears/file-parser/file-parser`, so the
/// repo root is three levels up.
pub fn fixture_path(subdir: &str, filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("CARGO_MANIFEST_DIR is at least three levels deep")
        .join("testing/e2e/testdata")
        .join(subdir)
        .join(filename)
}
