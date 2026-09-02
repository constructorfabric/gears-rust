//! Regression cases for `check-test-container-pins`.
//!
//! Fixtures are inline `&str` snippets rather than files under `tests/`: a
//! fixture *file* would be tracked, so `git ls-files` would hand it to the very
//! checker it exists to test. String literals are not calls, so this file being
//! scanned is a non-issue.

use super::*;

/// Rules that fired, as `line:column rule-prefix`, for terse assertions.
fn rules(source: &str) -> Vec<String> {
    scan_source(source)
        .expect("fixture must parse")
        .into_iter()
        .map(|v| format!("{}:{}", v.line, v.column))
        .collect()
}

fn count(source: &str) -> usize {
    scan_source(source).expect("fixture must parse").len()
}

#[test]
fn canonical_default_constructors_are_rejected() {
    assert_eq!(count("fn f() { let _ = Postgres::default(); }"), 1);
    assert_eq!(count("fn f() { let _ = Mysql::default(); }"), 1);
}

#[test]
fn aliased_postgres_is_rejected() {
    // The form the old regex could not see: renamed on import, called by the
    // local name.
    let source = r#"
use testcontainers_modules::postgres::Postgres as Pg;

fn f() {
    let _ = Pg::default();
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn aliased_generic_image_is_rejected() {
    let source = r#"
use testcontainers::GenericImage as Img;

fn f() {
    let _ = Img::new("mariadb", "11.8");
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn grouped_and_nested_use_renames_are_rejected() {
    let source = r#"
use testcontainers_modules::{mysql::Mysql as My, postgres::Postgres as Pg};

fn f() {
    let _ = (Pg::default(), My::default());
}
"#;
    assert_eq!(count(source), 2);
}

#[test]
fn generic_image_with_a_non_literal_image_name_is_rejected() {
    // The second form the old regex missed: the image name is an expression,
    // not the bare string literal the pattern anchored on.
    let source = r#"
fn f(tag: String) {
    let _ = GenericImage::new("mariadb".to_owned(), tag);
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn generic_image_is_rejected_whatever_the_arguments() {
    let source = r#"
fn f(image: String, tag: String) {
    let _ = GenericImage::new(image, tag);
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn fully_qualified_paths_are_rejected() {
    let source = r#"
fn f() {
    let _ = testcontainers_modules::postgres::Postgres::default();
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn type_aliases_are_rejected() {
    let source = r#"
use testcontainers_modules::postgres::Postgres;

type Pg = Postgres;

fn f() {
    let _ = Pg::default();
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn aliases_declared_inside_an_inline_test_module_are_rejected() {
    let source = r#"
#[cfg(test)]
mod tests {
    use testcontainers_modules::postgres::Postgres as Pg;

    #[test]
    fn t() {
        let _ = Pg::default();
    }
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn formatting_does_not_hide_a_violation() {
    // Line breaks, interleaved comments and stray whitespace are exactly what
    // a line-oriented regex is fragile about.
    let source = r#"
fn f() {
    let _ = Postgres
        // a comment in the middle of the path
        ::  default (
        );
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn a_violation_reports_its_line_and_column() {
    let source = "fn f() {\n    let _ = Postgres::default();\n}\n";
    assert_eq!(rules(source), ["2:13"]);
}

#[test]
fn glob_imports_do_not_hide_the_canonical_name() {
    let source = r#"
use testcontainers_modules::postgres::*;

fn f() {
    let _ = Postgres::default();
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn centralized_helpers_are_clean() {
    let source = r#"
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

struct Harness {
    _container: ContainerAsync<GenericImage>,
}

fn image() -> GenericImage {
    test_containers::mariadb()
}

async fn f() {
    let _ = test_containers::postgres().start().await;
    let _ = test_containers::postgres_tagged("16-alpine");
}
"#;
    assert_eq!(count(source), 0);
}

#[test]
fn unrelated_default_and_new_calls_are_clean() {
    let source = r#"
fn f() {
    let _ = Config::default();
    let _ = String::new();
    let _ = Postgres::builder();
}
"#;
    assert_eq!(count(source), 0);
}

#[test]
fn the_banned_names_inside_string_literals_are_clean() {
    // This file itself relies on it, and so does pins.rs's own documentation.
    let source = r#"
fn f() {
    let _ = "Postgres::default()";
    let _ = "GenericImage::new(\"mariadb\", tag)";
}
"#;
    assert_eq!(count(source), 0);
}

#[test]
fn parenthesized_default_callee_is_rejected() {
    // `(Postgres::default)()` is still a call to the banned constructor; the
    // parens must not hide the path from the visitor.
    let source = r#"
fn f() {
    let _ = (Postgres::default)();
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn parenthesized_generic_image_callee_is_rejected() {
    let source = r#"
fn f() {
    let _ = (GenericImage::new)("mariadb", "11.8");
}
"#;
    assert_eq!(count(source), 1);
}

#[test]
fn unparseable_source_is_an_error_not_a_violation() {
    // `check` downgrades this to a warning and skips the file: the workspace
    // tracks deliberately-broken trybuild UI fixtures.
    assert!(scan_source("fn f( {").is_err());
}
