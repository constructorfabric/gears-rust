// Created: 2026-04-07 by Constructor Tech
// Updated: 2026-04-14 by Constructor Tech
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

extern crate rustc_ast;

use clippy_utils::diagnostics::span_lint_and_then;
use rustc_ast::Item;
use rustc_lint::{EarlyContext, EarlyLintPass, LintContext};
use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static SCANNED_FILES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Known path prefixes for module directories, longest-first
/// so that `gears/system/` matches before `gears/`.
/// Top-level dirs (`examples/`, `apps/`, `plugins/`) are included so they
/// go through the same segment-boundary check and config-driven exclusion.
const MODULE_PREFIXES: &[&str] = &[
    "libs/",
    "gears/system/",
    "gears/",
    "examples/",
    "apps/",
    "plugins/",
];

const DEFAULT_MAX_INLINE_TEST_LINES: usize = 100;

#[derive(Default, serde::Deserialize)]
struct Config {
    #[serde(default)]
    excluded_paths: Vec<String>,
    /// Maximum number of test lines allowed inline before the linter enforces
    /// moving them to a separate file. Set to 0 to always require separation.
    /// Default: 100.
    #[serde(default)]
    max_inline_test_lines: Option<usize>,
}

struct De1101TestsInSeparateFiles {
    excluded_set: HashSet<String>,
    max_inline_test_lines: usize,
}

impl De1101TestsInSeparateFiles {
    pub fn new() -> Self {
        let config: Config = dylint_linting::config_or_default(env!("CARGO_PKG_NAME"));
        Self {
            excluded_set: config.excluded_paths.into_iter().collect(),
            max_inline_test_lines: config
                .max_inline_test_lines
                .unwrap_or(DEFAULT_MAX_INLINE_TEST_LINES),
        }
    }

    fn is_in_scope(&self, normalized_path: &str) -> bool {
        // Try to extract a module key (e.g. "libs/toolkit", "gears/system/oagw",
        // "examples/oop-gears").
        for prefix in MODULE_PREFIXES {
            if let Some(pos) = normalized_path.find(prefix) {
                // Ensure match is at a path segment boundary, not inside
                // a compound directory name like "oop-gears/".
                if pos > 0 && normalized_path.as_bytes()[pos - 1] != b'/' {
                    continue;
                }
                let rest = &normalized_path[pos + prefix.len()..];
                let seg_end = rest.find('/').unwrap_or(rest.len());
                let key = &normalized_path[pos..pos + prefix.len() + seg_end];
                return !self.excluded_set.contains(key);
            }
        }

        true
    }
}

dylint_linting::impl_pre_expansion_lint! {
    /// DE1101: Tests must be in separate files
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
    /// - dedicated unit-test files named `{source_stem}_tests.rs`
    ///
    /// Test code is forbidden inline inside production source files when:
    /// - the inline test block exceeds `max_inline_test_lines` (default: 100), OR
    /// - a companion `{source_stem}_tests.rs` file already exists (tests must not
    ///   be split across two files)
    ///
    /// Additionally:
    /// - test module reference must resolve to `{source_stem}_tests.rs`
    /// - if `#[path = "..."]` is used, its value must be `{source_stem}_tests.rs`
    /// - if no `#[path]`, the module name must be `{source_stem}_tests`
    pub DE1101_TESTS_IN_SEPARATE_FILES,
    Deny,
    "tests must live in separate files, not inline in production files (DE1101)",
    De1101TestsInSeparateFiles::new()
}

/// The kind of test-declaration violation found in a source file.
enum TestViolation {
    /// Inline test code (`#[test]` or `#[cfg(test)] mod tests { ... }`) in a production file.
    InlineTestCode,
    /// Inline test code when a companion `_tests.rs` file already exists — always denied.
    InlineTestCodeWithCompanion,
    /// `#[path = "..."]` value does not match `{source_stem}_tests.rs`.
    WrongPathAttr { expected: String, actual: String },
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

        if !self.is_in_scope(&normalized) {
            return;
        }

        let should_scan = SCANNED_FILES.with(|files| files.borrow_mut().insert(normalized.clone()));
        if !should_scan {
            return;
        }

        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };

        if is_allowed_test_file(&normalized) {
            return;
        }

        let source_stem = file_stem(&normalized);
        let has_companion = has_companion_test_file(&path, source_stem.as_deref());
        let violations = find_test_violations(
            &source,
            source_stem.as_deref(),
            has_companion,
            self.max_inline_test_lines,
        );

        for violation in violations {
            match violation {
                TestViolation::InlineTestCode => {
                    span_lint_and_then(
                        cx,
                        DE1101_TESTS_IN_SEPARATE_FILES,
                        item.span,
                        "test code must be moved to a separate test file (DE1101)",
                        |diag| {
                            diag.help(format!(
                                "move the test into `tests/*.rs` or an out-of-line `*_tests.rs` module (inline test block exceeds {} lines)",
                                self.max_inline_test_lines,
                            ));
                        },
                    );
                }
                TestViolation::InlineTestCodeWithCompanion => {
                    span_lint_and_then(
                        cx,
                        DE1101_TESTS_IN_SEPARATE_FILES,
                        item.span,
                        "test code must not be added back to a file that already has a companion test file (DE1101)",
                        |diag| {
                            diag.help(
                                "a `*_tests.rs` companion file already exists; add tests there instead",
                            );
                        },
                    );
                }
                TestViolation::WrongPathAttr { expected, actual } => {
                    span_lint_and_then(
                        cx,
                        DE1101_TESTS_IN_SEPARATE_FILES,
                        item.span,
                        format!(
                            "test module path `{actual}.rs` must reference `{expected}.rs` to match the source file (DE1101)",
                        ),
                        |diag| {
                            diag.help(format!(
                                "use `#[path = \"{expected}.rs\"]` or remove `#[path]`"
                            ));
                        },
                    );
                }
            }
        }
    }
}

fn is_allowed_test_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);

    path.contains("/tests/") || file_name.ends_with("_tests.rs")
}

/// Extract the file stem from a normalized path.
/// `"/foo/bar/handler.rs"` → `Some("handler")`
///
/// Returns `None` for special entry-point files (`lib.rs`, `main.rs`, `mod.rs`)
/// where enforcing `{stem}_tests` naming would be meaningless.
fn file_stem(path: &str) -> Option<String> {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name.strip_suffix(".rs")?;
    match stem {
        "lib" | "main" | "mod" | "tests" | "test" => None,
        _ => Some(stem.to_string()),
    }
}

/// Check whether a companion `{stem}_tests.rs` file exists next to the source file.
fn has_companion_test_file(source_path: &str, source_stem: Option<&str>) -> bool {
    let Some(stem) = source_stem else {
        return false;
    };
    let parent = match source_path.rfind('/').or_else(|| source_path.rfind('\\')) {
        Some(pos) => &source_path[..=pos],
        None => "",
    };
    let companion = format!("{parent}{stem}_tests.rs");
    std::path::Path::new(&companion).exists()
}

/// Count the number of lines in an inline `#[cfg(test)] mod ... { ... }` block,
/// starting from the line containing the opening `{`.
fn count_inline_test_block_lines(lines: &[&str], open_brace_line: usize) -> usize {
    let mut depth = 0usize;
    let mut count = 0usize;

    for line in &lines[open_brace_line..] {
        count += 1;
        for ch in line.chars() {
            if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return count;
                }
            }
        }
    }

    count
}

/// Scan source text for test-declaration violations.
///
/// Returns all violations found (inline code, wrong test file name).
///
/// - `has_companion`: whether a `{stem}_tests.rs` file exists alongside this file.
///   If true, any inline test code is unconditionally denied.
/// - `max_inline_lines`: the threshold below which inline test blocks are tolerated.
fn find_test_violations(
    source: &str,
    source_stem: Option<&str>,
    has_companion: bool,
    max_inline_lines: usize,
) -> Vec<TestViolation> {
    let lines: Vec<&str> = source.lines().collect();
    let mut violations = Vec::new();
    let mut reported_inline = false;
    let mut reported_naming = false;

    for (index, line) in lines.iter().enumerate() {
        if is_comment_or_blank_line(line) {
            continue;
        }

        let compact_line = compact(line);

        // A bare `#[test]` / `#[tokio::test]` in a production file.
        if !reported_inline && is_direct_test_attr(&compact_line) {
            if has_companion {
                violations.push(TestViolation::InlineTestCodeWithCompanion);
            } else {
                violations.push(TestViolation::InlineTestCode);
            }
            reported_inline = true;
            continue;
        }

        if !is_cfg_test_attr(&compact_line) {
            continue;
        }

        // Found `#[cfg(test)]` — scan ahead for the declaration that follows.
        let mut next = index + 1;
        let mut path_attr_value: Option<String> = None;

        while let Some(candidate) = lines.get(next) {
            let trimmed = candidate.trim();
            let candidate_compact = compact(candidate);

            if is_comment_or_blank_line(candidate) {
                next += 1;
                continue;
            }

            // Collect attributes between `#[cfg(test)]` and the item.
            if candidate_compact.starts_with("#[") {
                if is_path_attr(&candidate_compact) {
                    path_attr_value = extract_path_attr_value(trimmed);
                }
                next += 1;
                continue;
            }

            // Out-of-line mod declaration (e.g. `mod foo_tests;`).
            // Without #[path]: any module name is accepted.
            // With #[path]: value must be `{stem}_tests.rs` or `{stem}_test.rs`.
            if is_out_of_line_mod_decl(trimmed) {
                if let (Some(stem), Some(pv)) = (source_stem, &path_attr_value)
                    && !reported_naming
                {
                    let expected = format!("{stem}_tests");
                    let filename = pv.rsplit('/').next().unwrap_or(pv);
                    let actual = filename.strip_suffix(".rs").unwrap_or(filename);

                    if actual != expected {
                        violations.push(TestViolation::WrongPathAttr {
                            expected,
                            actual: actual.to_string(),
                        });
                        reported_naming = true;
                    }
                }
                break;
            }

            // `extern crate` alias after `#[cfg(test)]` is allowed.
            if is_extern_crate_alias(trimmed) {
                break;
            }

            // Anything else is inline test code — check threshold.
            if !reported_inline {
                if has_companion {
                    violations.push(TestViolation::InlineTestCodeWithCompanion);
                    reported_inline = true;
                } else {
                    // Count the lines in the inline test block.
                    let block_lines = count_inline_test_block_lines(&lines, next);
                    // Include the #[cfg(test)] line and any attributes above the block.
                    let total_test_lines = (next - index) + block_lines;

                    if total_test_lines > max_inline_lines {
                        violations.push(TestViolation::InlineTestCode);
                    }
                    // Mark as reported either way to avoid re-triggering on
                    // bare `#[test]` lines inside this allowed inline block.
                    reported_inline = true;
                }
            }
            break;
        }
    }

    violations
}

fn is_comment_or_blank_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with("//")
}

fn compact(line: &str) -> String {
    // Strip trailing line comments before removing whitespace, so that
    // `#[cfg(test)] // comment` compacts to `#[cfg(test)]` and is detected.
    let without_comment = match line.find("//") {
        Some(pos) => &line[..pos],
        None => line,
    };
    without_comment
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

fn is_direct_test_attr(line: &str) -> bool {
    let trimmed = line.trim_start();
    let is_attr = trimmed.starts_with("#[");

    trimmed.starts_with("#[test")
        || trimmed.starts_with("#[tokio::test")
        || (is_attr && trimmed.contains("::test]"))
        || (is_attr && trimmed.contains("::test("))
}

/// Returns true for `#[cfg(test)]`, `#[cfg(test, ...)]`, `#[cfg(any(test, ...))]`,
/// `#[cfg(all(test, ...))]`.
/// Does NOT match `#[cfg(not(test))]` or feature names containing "test".
fn is_cfg_test_attr(line: &str) -> bool {
    let Some(inner) = line
        .strip_prefix("#[cfg(")
        .and_then(|rest| rest.strip_suffix(")]"))
    else {
        return false;
    };

    contains_test_cfg_operand(inner)
}

fn contains_test_cfg_operand(input: &str) -> bool {
    split_top_level_args(input).into_iter().any(|arg| {
        if arg == "test" {
            return true;
        }

        if let Some(inner) = arg
            .strip_prefix("all(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return contains_test_cfg_operand(inner);
        }

        if let Some(inner) = arg
            .strip_prefix("any(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return contains_test_cfg_operand(inner);
        }

        false
    })
}

fn split_top_level_args(input: &str) -> Vec<&str> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (index, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(input[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    args.push(input[start..].trim());
    args
}

/// Returns `true` when the compacted line is a `#[path = "..."]` attribute.
fn is_path_attr(compact_line: &str) -> bool {
    compact_line.starts_with("#[path=")
}

/// Extract the string value from a `#[path = "..."]` attribute.
/// Works on the original (non-compacted) line to preserve the value intact.
fn extract_path_attr_value(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("#[path")?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Strip a leading visibility qualifier from a line, including `pub(in path)`.
fn strip_visibility(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("pub(in ")
        // Find the closing ')' and skip past it plus any trailing space.
        && let Some(close) = rest.find(')')
    {
        let after = &rest[close + 1..];
        return after.strip_prefix(' ').unwrap_or(after);
    }
    line.strip_prefix("pub(crate) ")
        .or_else(|| line.strip_prefix("pub(super) "))
        .or_else(|| line.strip_prefix("pub(self) "))
        .or_else(|| line.strip_prefix("pub "))
        .unwrap_or(line)
}

/// Extract the module name from an out-of-line mod declaration.
/// `"mod foo_tests;"` → `Some("foo_tests")`
/// `"pub(crate) mod foo_tests;"` → `Some("foo_tests")`
#[cfg(test)]
fn extract_mod_name(line: &str) -> Option<&str> {
    if !line.ends_with(';') {
        return None;
    }
    let name_with_semi = strip_visibility(line).strip_prefix("mod ")?;
    Some(name_with_semi.trim_end_matches(';').trim())
}

fn is_out_of_line_mod_decl(line: &str) -> bool {
    line.ends_with(';') && strip_visibility(line).starts_with("mod ")
}

fn is_extern_crate_alias(line: &str) -> bool {
    line.starts_with("extern crate ") && line.ends_with(';')
}

#[cfg(test)]
mod tests {
    use super::{
        count_inline_test_block_lines, extract_mod_name, extract_path_attr_value,
        find_test_violations, is_cfg_test_attr, is_path_attr,
    };

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

    #[test]
    fn test_is_cfg_test_attr_matches_supported_forms() {
        assert!(is_cfg_test_attr("#[cfg(test)]"));
        assert!(is_cfg_test_attr("#[cfg(test,feature=\"foo\")]"));
        assert!(is_cfg_test_attr("#[cfg(all(test,feature=\"foo\"))]"));
        assert!(is_cfg_test_attr("#[cfg(any(feature=\"foo\",test))]"));
    }

    #[test]
    fn test_is_cfg_test_attr_rejects_unsupported_forms() {
        assert!(!is_cfg_test_attr("#[cfg(not(test))]"));
        assert!(!is_cfg_test_attr("#[cfg(feature=\"test\")]"));
        assert!(!is_cfg_test_attr("#[cfg(any(feature=\"test\",unix))]"));
    }

    #[test]
    fn test_is_path_attr() {
        assert!(is_path_attr("#[path=\"foo.rs\"]"));
        assert!(is_path_attr("#[path=\"some/path.rs\"]"));
        assert!(!is_path_attr("#[cfg(test)]"));
        assert!(!is_path_attr("#[derive(Debug)]"));
    }

    #[test]
    fn test_extract_path_attr_value() {
        assert_eq!(
            extract_path_attr_value(r#"#[path = "foo_tests.rs"]"#),
            Some("foo_tests.rs".to_string())
        );
        assert_eq!(
            extract_path_attr_value(r#"#[path="bar.rs"]"#),
            Some("bar.rs".to_string())
        );
        assert_eq!(extract_path_attr_value(r#"#[cfg(test)]"#), None);
    }

    #[test]
    fn test_extract_mod_name() {
        assert_eq!(extract_mod_name("mod foo_tests;"), Some("foo_tests"));
        assert_eq!(extract_mod_name("pub mod foo_tests;"), Some("foo_tests"));
        assert_eq!(
            extract_mod_name("pub(crate) mod foo_tests;"),
            Some("foo_tests")
        );
        assert_eq!(extract_mod_name("mod tests;"), Some("tests"));
        assert_eq!(extract_mod_name("mod tests { }"), None);
        assert_eq!(extract_mod_name("fn main() {}"), None);
    }

    #[test]
    fn test_find_violations_correct_name_no_issues() {
        let source = r#"
#[cfg(test)]
mod handler_tests;

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"), false, 100);
        assert!(violations.is_empty(), "expected no violations");
    }

    #[test]
    fn test_find_violations_any_mod_name_without_path_ok() {
        // Without #[path], any module name is accepted.
        let source = r#"
#[cfg(test)]
mod tests;

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"), false, 100);
        assert!(
            violations.is_empty(),
            "any mod name should be accepted without #[path]"
        );
    }

    #[test]
    fn test_find_violations_path_attr_wrong_value() {
        let source = r#"
#[cfg(test)]
#[path = "dto_tests.rs"]
mod tests;

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"), false, 100);
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            super::TestViolation::WrongPathAttr { expected, actual }
            if expected == "handler_tests" && actual == "dto_tests"
        ));
    }

    #[test]
    fn test_find_violations_path_attr_correct_value() {
        let source = r#"
#[cfg(test)]
#[path = "handler_tests.rs"]
mod tests;

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"), false, 100);
        assert!(violations.is_empty(), "expected no violations");
    }

    #[test]
    fn test_find_violations_inline_code_over_threshold() {
        let source = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn foo() {}
}

fn main() {}
"#;
        // Threshold 3: the test block is 4 lines (mod tests { ... }), trigger.
        let violations = find_test_violations(source, Some("handler"), false, 3);
        assert!(
            violations
                .iter()
                .any(|v| matches!(v, super::TestViolation::InlineTestCode))
        );
    }

    #[test]
    fn test_find_violations_inline_code_under_threshold() {
        let source = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn foo() {}
}

fn main() {}
"#;
        // Threshold 100: the test block is ~6 lines total, allow.
        let violations = find_test_violations(source, Some("handler"), false, 100);
        assert!(
            violations.is_empty(),
            "expected no violations under threshold"
        );
    }

    #[test]
    fn test_find_violations_inline_code_with_companion() {
        let source = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn foo() {}
}

fn main() {}
"#;
        // Even tiny inline tests are denied when companion exists.
        let violations = find_test_violations(source, Some("handler"), true, 100);
        assert!(
            violations
                .iter()
                .any(|v| matches!(v, super::TestViolation::InlineTestCodeWithCompanion))
        );
    }

    #[test]
    fn test_count_inline_test_block_lines() {
        let source = "mod tests {\n    #[test]\n    fn foo() {}\n}\n";
        let lines: Vec<&str> = source.lines().collect();
        assert_eq!(count_inline_test_block_lines(&lines, 0), 4);
    }

    #[test]
    fn test_count_inline_test_block_lines_nested() {
        let source = "mod tests {\n    fn foo() {\n        if true {\n        }\n    }\n}\n";
        let lines: Vec<&str> = source.lines().collect();
        assert_eq!(count_inline_test_block_lines(&lines, 0), 6);
    }
}
