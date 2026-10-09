//! File walker with the same skip/include rules as the Python orchestrator.

use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use walkdir::WalkDir;

const SKIP_DIRS: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "__pycache__",
    ".cargo",
    "dist",
    "build",
];
const TEST_DIRS: &[&str] = &["tests", "benches"];
const SCAN_EXT: &[&str] = &["rs", "md", "json", "toml", "yaml", "yml"];

fn test_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:^|_)tests?\.rs$").expect("static regex"))
}

/// True when `rel` is a Rust test or bench file (Cargo `tests/`/`benches/` directory
/// or filename matches `*_test.rs` / `*_tests.rs` / `test.rs` / `tests.rs`).
pub fn is_test_file(rel: &Path) -> bool {
    for c in rel.components() {
        if let Some(s) = c.as_os_str().to_str()
            && TEST_DIRS.contains(&s)
        {
            return true;
        }
    }
    if let Some(name) = rel.file_name().and_then(|s| s.to_str())
        && test_file_re().is_match(name)
    {
        return true;
    }
    false
}

fn ext_of(p: &Path) -> Option<String> {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

/// Walk `root`, yielding (absolute path, relative path) for every file we want to scan.
/// Applies skip rules: SKIP_DIRS, test-file filter (off by `include_tests`),
/// docs filter (on by `skip_docs` — both `*.md` anywhere and any file under a `docs/` directory).
pub struct Walker {
    root: PathBuf,
    include_tests: bool,
    skip_docs: bool,
}

impl Walker {
    /// Creates a walker over `root` with the given test and docs filters.
    pub fn new(root: PathBuf, include_tests: bool, skip_docs: bool) -> Self {
        Self {
            root,
            include_tests,
            skip_docs,
        }
    }

    /// Yields `(absolute, root-relative)` paths of every scannable file that passes the skip rules.
    pub fn iter(&self) -> impl Iterator<Item = (PathBuf, PathBuf)> {
        let root = self.root.clone();
        let include_tests = self.include_tests;
        let skip_docs = self.skip_docs;
        WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                !SKIP_DIRS.iter().any(|d| d == &name)
            })
            .filter_map(Result::ok)
            .filter_map(move |entry| {
                if !entry.file_type().is_file() {
                    return None;
                }
                let abs = entry.into_path();
                let rel = abs.strip_prefix(&root).ok()?.to_path_buf();
                let ext = ext_of(&abs)?;
                if !SCAN_EXT.contains(&ext.as_str()) {
                    return None;
                }
                if !include_tests && is_test_file(&rel) {
                    return None;
                }
                if skip_docs {
                    if ext == "md" {
                        return None;
                    }
                    if rel.components().any(|c| c.as_os_str() == "docs") {
                        return None;
                    }
                }
                Some((abs, rel))
            })
    }
}
