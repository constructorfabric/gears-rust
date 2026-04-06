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
    /// DE1101: Tests must be in separate files
    ///
    /// Applies across the repository.
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
    /// Test code is forbidden inline inside production source files.
    ///
    /// Additionally:
    /// - test module reference must resolve to `{source_stem}_tests.rs`
    /// - if `#[path = "..."]` is used, its value must be `{source_stem}_tests.rs`
    /// - if no `#[path]`, the module name must be `{source_stem}_tests`
    pub DE1101_TESTS_IN_SEPARATE_FILES,
    Deny,
    "tests must live in separate files, not inline in production files (DE1101)"
}

/// The kind of test-declaration violation found in a source file.
enum TestViolation {
    /// Inline test code (`#[test]` or `#[cfg(test)] mod tests { ... }`) in a production file.
    InlineTestCode,
    /// Test file reference does not resolve to `{source_stem}_tests`.
    /// When `has_path_attr` is true, the `#[path]` value was checked;
    /// otherwise the module name was checked.
    WrongTestFileName {
        expected: String,
        actual: String,
        has_path_attr: bool,
    },
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
        let violations = find_test_violations(&source, source_stem.as_deref());

        for violation in violations {
            match violation {
                TestViolation::InlineTestCode => {
                    cx.span_lint(DE1101_TESTS_IN_SEPARATE_FILES, item.span, |diag| {
                        diag.primary_message(
                            "test code must be moved to a separate test file (DE1101)",
                        );
                        diag.help(
                            "move the test into `tests/*.rs` or an out-of-line `*_tests.rs` module",
                        );
                    });
                }
                TestViolation::WrongTestFileName {
                    expected,
                    actual,
                    has_path_attr,
                } => {
                    cx.span_lint(DE1101_TESTS_IN_SEPARATE_FILES, item.span, |diag| {
                        if has_path_attr {
                            diag.primary_message(format!(
                                "test module path `{actual}.rs` must reference `{expected}.rs` to match the source file (DE1101)",
                            ));
                            diag.help(format!(
                                "use `#[path = \"{expected}.rs\"]` or remove `#[path]` and use `mod {expected};`"
                            ));
                        } else {
                            diag.primary_message(format!(
                                "test module `{actual}` must be named `{expected}` to match the source file (DE1101)",
                            ));
                            diag.help(format!("rename to `mod {expected};`"));
                        }
                    });
                }
            }
        }
    }
}

fn is_allowed_test_file(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);

    path.contains("/tests/") || file_name.ends_with("_test.rs") || file_name.ends_with("_tests.rs")
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

/// Scan source text for test-declaration violations.
///
/// Returns all violations found (inline code, wrong test file name).
fn find_test_violations(source: &str, source_stem: Option<&str>) -> Vec<TestViolation> {
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
            violations.push(TestViolation::InlineTestCode);
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
            if is_out_of_line_mod_decl(trimmed) {
                if let Some(stem) = source_stem {
                    if !reported_naming {
                        let expected = format!("{stem}_tests");
                        let (actual, has_path) = if let Some(ref pv) = path_attr_value {
                            // Use filename from #[path] value
                            let filename = pv.rsplit('/').next().unwrap_or(pv);
                            let name = filename.strip_suffix(".rs").unwrap_or(filename);
                            (name.to_string(), true)
                        } else {
                            // Use the module name
                            let name = extract_mod_name(trimmed).unwrap_or("");
                            (name.to_string(), false)
                        };

                        if actual != expected {
                            violations.push(TestViolation::WrongTestFileName {
                                expected,
                                actual,
                                has_path_attr: has_path,
                            });
                            reported_naming = true;
                        }
                    }
                }
                break;
            }

            // `extern crate` alias after `#[cfg(test)]` is allowed.
            if is_extern_crate_alias(trimmed) {
                break;
            }

            // Anything else is inline test code.
            if !reported_inline {
                violations.push(TestViolation::InlineTestCode);
                reported_inline = true;
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
    line.chars().filter(|ch| !ch.is_whitespace()).collect()
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

        if let Some(inner) = arg.strip_prefix("all(").and_then(|rest| rest.strip_suffix(')')) {
            return contains_test_cfg_operand(inner);
        }

        if let Some(inner) = arg.strip_prefix("any(").and_then(|rest| rest.strip_suffix(')')) {
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
    compact_line.starts_with("#[path=") || compact_line.starts_with("#[path=\"")
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

/// Extract the module name from an out-of-line mod declaration.
/// `"mod foo_tests;"` → `Some("foo_tests")`
/// `"pub(crate) mod foo_tests;"` → `Some("foo_tests")`
fn extract_mod_name(line: &str) -> Option<&str> {
    if !line.ends_with(';') {
        return None;
    }

    let without_visibility = line
        .strip_prefix("pub ")
        .or_else(|| line.strip_prefix("pub(crate) "))
        .or_else(|| line.strip_prefix("pub(super) "))
        .or_else(|| line.strip_prefix("pub(self) "))
        .unwrap_or(line);

    let name_with_semi = without_visibility.strip_prefix("mod ")?;
    Some(name_with_semi.trim_end_matches(';').trim())
}

fn is_out_of_line_mod_decl(line: &str) -> bool {
    if !line.ends_with(';') {
        return false;
    }

    let without_visibility = line
        .strip_prefix("pub ")
        .or_else(|| line.strip_prefix("pub(crate) "))
        .or_else(|| line.strip_prefix("pub(super) "))
        .or_else(|| line.strip_prefix("pub(self) "))
        .unwrap_or(line);

    without_visibility.starts_with("mod ")
}

fn is_extern_crate_alias(line: &str) -> bool {
    line.starts_with("extern crate ") && line.ends_with(';')
}

#[cfg(test)]
mod tests {
    use super::{
        extract_mod_name, extract_path_attr_value, find_test_violations, is_cfg_test_attr,
        is_path_attr,
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
        let violations = find_test_violations(source, Some("handler"));
        assert!(violations.is_empty(), "expected no violations");
    }

    #[test]
    fn test_find_violations_wrong_name() {
        let source = r#"
#[cfg(test)]
mod tests;

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"));
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            super::TestViolation::WrongTestFileName { expected, actual, has_path_attr }
            if expected == "handler_tests" && actual == "tests" && !has_path_attr
        ));
    }

    #[test]
    fn test_find_violations_path_attr_wrong_value() {
        let source = r#"
#[cfg(test)]
#[path = "dto_tests.rs"]
mod tests;

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"));
        assert_eq!(violations.len(), 1);
        assert!(matches!(
            &violations[0],
            super::TestViolation::WrongTestFileName { expected, actual, has_path_attr }
            if expected == "handler_tests" && actual == "dto_tests" && *has_path_attr
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
        let violations = find_test_violations(source, Some("handler"));
        assert!(violations.is_empty(), "expected no violations");
    }

    #[test]
    fn test_find_violations_inline_code() {
        let source = r#"
#[cfg(test)]
mod tests {
    #[test]
    fn foo() {}
}

fn main() {}
"#;
        let violations = find_test_violations(source, Some("handler"));
        assert!(violations
            .iter()
            .any(|v| matches!(v, super::TestViolation::InlineTestCode)));
    }
}
