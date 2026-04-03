#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;

use rustc_ast::Item;
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static SCANNED_FILES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

dylint_linting::declare_pre_expansion_lint! {
    /// DE1101: Resource-group tests must be in separate files
    ///
    /// Applies only to `cf-resource-group`.
    ///
    /// ### Why
    ///
    /// Keeping tests in separate files makes it easier to:
    /// - filter test files out when counting lines of code
    /// - navigate the codebase for both humans and LLMs because files stay smaller
    /// - keep production logic and test code separated by file type
    ///
    /// Test files should never be the place where production logic lives.
    ///
    /// Test code is allowed in:
    /// - integration tests under `tests/`
    /// - dedicated unit-test files such as `*_test.rs` or `*_tests.rs`
    ///
    /// Test code is forbidden inline inside production source files under `src/`.
    pub DE1101_TESTS_IN_SEPARATE_FILES,
    Deny,
    "resource-group tests must live in separate files, not inline in production files (DE1101)"
}

impl EarlyLintPass for De1101TestsInSeparateFiles {
    fn check_crate_post(&mut self, _cx: &EarlyContext<'_>, _krate: &rustc_ast::Crate) {
        SCANNED_FILES.with(|files| files.borrow_mut().clear());
    }

    fn check_item(&mut self, cx: &EarlyContext<'_>, item: &Item) {
        let Some(path) = lint_utils::filename_str(cx.sess().source_map(), item.span) else {
            return;
        };

        let normalized = path.replace('\\', "/");

        // Deduplicate: skip if this file was already scanned in this crate pass.
        // Dedup happens before read_to_string to avoid N reads for N items per file.
        let should_scan = SCANNED_FILES.with(|files| files.borrow_mut().insert(normalized.clone()));
        if !should_scan {
            return;
        }

        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };

        if !is_resource_group_file(&path, &source) {
            return;
        }

        if is_allowed_test_file(&normalized) {
            return;
        }

        if !contains_inline_test_code(&source) {
            return;
        }

        cx.span_lint(DE1101_TESTS_IN_SEPARATE_FILES, item.span, |diag| {
            diag.primary_message(
                "resource-group test code must be moved to a separate test file (DE1101)",
            );
            diag.help(
                "move the test into `tests/*.rs` or an out-of-line `*_test.rs`/`*_tests.rs` module",
            );
        });
    }
}

fn is_allowed_test_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);

    path.contains("/tests/") || file_name.ends_with("_test.rs") || file_name.ends_with("_tests.rs")
}

fn is_resource_group_file(path: &str, source: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/modules/system/resource-group/resource-group/")
        || extract_simulated_dir(source)
            .map(|dir| dir.replace('\\', "/"))
            .map(|dir| dir.contains("/modules/system/resource-group/resource-group/"))
            .unwrap_or(false)
}

fn contains_inline_test_code(source: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        let compact_line = compact(line);

        if is_direct_test_attr(&compact_line) {
            return true;
        }

        if !is_cfg_test_attr(&compact_line) {
            continue;
        }

        let mut next = index + 1;
        while let Some(candidate) = lines.get(next) {
            let trimmed = candidate.trim();
            let candidate_compact = compact(candidate);

            if trimmed.is_empty() || trimmed.starts_with("//") {
                next += 1;
                continue;
            }

            if candidate_compact.starts_with("#[") {
                next += 1;
                continue;
            }

            if trimmed.starts_with("mod ") && trimmed.ends_with(';') {
                break;
            }

            return true;
        }
    }

    false
}

fn compact(line: &str) -> String {
    line.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn is_direct_test_attr(line: &str) -> bool {
    line.starts_with("#[test")
        || line.contains("::test]")
        || line.contains("::test(")
        || line.starts_with("#[tokio::test")
}

/// Returns true for `#[cfg(test)]`, `#[cfg(test, ...)]`, `#[cfg(any(test, ...))]`,
/// `#[cfg(all(test, ...))]`.
/// Does NOT match `#[cfg(not(test))]` or feature names containing "test".
fn is_cfg_test_attr(line: &str) -> bool {
    line == "#[cfg(test)]"
        || line.starts_with("#[cfg(test,")
        || line.starts_with("#[cfg(any(test")
        || line.starts_with("#[cfg(all(test")
}

fn extract_simulated_dir(source: &str) -> Option<&str> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("// simulated_dir=") {
            return trimmed.strip_prefix("// simulated_dir=");
        }
        if !trimmed.is_empty() && !trimmed.starts_with("//") && !trimmed.starts_with("#!") {
            break;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn ui_examples() {
        dylint_testing::ui_test_examples(env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn test_comment_annotations_match_stderr() {
        let ui_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui");
        lint_utils::test_comment_annotations_match_stderr(
            &ui_dir,
            "DE1101",
            "tests must be in separate files",
        );
    }
}
