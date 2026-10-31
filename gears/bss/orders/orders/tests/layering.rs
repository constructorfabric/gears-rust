//! Layering gate (n40, n41, n59, n60): the domain and infra layers never reach into the REST
//! adapter's `throttle` module. The per-caller limiter and the canonical error builders live in
//! `infra::throttle` and `infra::error_mapping`; `api::rest::throttle` keeps only the HTTP parts.
//!
//! Integration tier, no Docker: a grep over the source tree, in the style of
//! `engine_single_writer.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "repository files must exist; fail loudly"
)]
use std::path::{Path, PathBuf};

const FORBIDDEN: &str = "api::rest::throttle";

/// Whether `text` reaches the REST `throttle` module in any import form: the full path, a
/// grouped import (`api::rest::{throttle, ..}` or `api::{rest::throttle}`), a glob over `rest`
/// (`api::rest::*` would pull `throttle` in), or a `rest::throttle` path under a shorter prefix. Whitespace
/// is ignored so a line-wrapped group still matches.
fn reaches_rest_throttle(text: &str) -> bool {
    let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if flat.contains("rest::throttle") || flat.contains(FORBIDDEN) || flat.contains("rest::*") {
        return true;
    }
    // `rest::{..throttle..}` / `rest::{self, throttle}`: a brace group opened right after
    // `rest::` that names `throttle` before it closes.
    flat.match_indices("rest::{").any(|(at, pat)| {
        let group = &flat[at + pat.len()..];
        let end = group.find('}').unwrap_or(group.len());
        group[..end]
            .split(',')
            .any(|part| part == "throttle" || part.starts_with("throttle::"))
    })
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            // Test directories may name the REST path (the re-export keeps them compiling).
            if path.file_name().is_some_and(|n| n == "tests") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn offenders(sub: &str) -> Vec<String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(sub);
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files
        .into_iter()
        .filter(|f| {
            let name = f.file_name().unwrap().to_string_lossy();
            // `*_tests.rs` siblings are test code.
            !name.ends_with("_tests.rs")
        })
        .filter(|f| reaches_rest_throttle(&std::fs::read_to_string(f).unwrap()))
        .map(|f| f.display().to_string())
        .collect()
}

#[test]
fn domain_never_imports_the_rest_throttle_module() {
    let found = offenders("domain");
    assert!(found.is_empty(), "domain reaches `{FORBIDDEN}`: {found:?}");
}

#[test]
fn infra_never_imports_the_rest_throttle_module() {
    let found = offenders("infra");
    assert!(found.is_empty(), "infra reaches `{FORBIDDEN}`: {found:?}");
}

/// The limiter identity is the bounded metric label: `preview` and `submit`, as before the enum.
#[test]
fn per_caller_limiter_tokens_are_the_metric_labels() {
    use bss_orders::api::rest::throttle::CallerLimiter;
    assert_eq!(CallerLimiter::Preview.token(), "preview");
    assert_eq!(CallerLimiter::Submit.token(), "submit");
}

/// The gate-diagnostics row building and insert live in `infra::storage::repo::gate_outcome`
/// (n172): no non-test file under `src/infra/gate/` names `secure_insert_many` or the
/// `gate_outcome` entity.
#[test]
fn gate_outcome_rows_are_written_only_by_storage() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/infra/gate");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let found: Vec<String> = files
        .into_iter()
        .filter(|f| {
            !f.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_tests.rs")
        })
        .filter(|f| {
            let text = std::fs::read_to_string(f).unwrap();
            text.contains("secure_insert_many") || text.contains("entity::gate_outcome")
        })
        .map(|f| f.display().to_string())
        .collect();
    assert!(
        found.is_empty(),
        "gate code writes `bss_orders__gate_outcome` itself: {found:?}"
    );
}

/// The strengthened matcher catches grouped, glob and shortened forms of the same edge, and
/// leaves the two known out-of-scope `api::rest` edges alone.
#[test]
fn the_throttle_matcher_catches_grouped_and_shortened_forms() {
    for planted in [
        "use crate::api::rest::throttle::PerOrderLimiter;",
        "use crate::api::rest::{throttle, error};",
        "use crate::api::rest::{\n    error,\n    throttle::CallerLimiter,\n};",
        "use crate::api::{rest::throttle};",
        "use crate::api::{rest::{throttle}};",
        "use crate::api::rest::*;",
        "use rest::throttle::X;",
    ] {
        assert!(reaches_rest_throttle(planted), "missed: {planted}");
    }
    for allowed in [
        "|| crate::api::rest::wire::ROUTES",
        "let problem = crate::api::rest::error::settled_refusal_problem(reason, data);",
        "use crate::infra::throttle::CallerLimiter;",
    ] {
        assert!(!reaches_rest_throttle(allowed), "false positive: {allowed}");
    }
}
