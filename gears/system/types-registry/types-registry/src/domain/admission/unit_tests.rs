//! Tests for bounded refusal summaries persisted in `operation_item.error_payload`.
//! Both diagnostic counts and path lengths are caller-controlled.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use gts::SchemaComparison;
use gts::schema_evolution::{CompatibilityDiagnostic, CompatibilityFinding};

use super::{MAX_REPORTED_DIAGNOSTICS, MAX_REPORTED_PATH_BYTES, diagnostics_summary};

/// A comparison carrying `count` backward diagnostics, each at `path`.
fn comparison(count: usize, path: &str) -> SchemaComparison {
    SchemaComparison {
        backward_diagnostics: (0..count)
            .map(|_| CompatibilityDiagnostic {
                path: path.to_owned(),
                finding: CompatibilityFinding::PropertyAdded,
                detail: String::new(),
            })
            .collect(),
        forward_diagnostics: Vec::new(),
        candidate_object_levels: Vec::new(),
    }
}

/// An empty list says so, rather than producing an empty message a reader would
/// take for a missing field.
#[test]
fn no_diagnostics_reports_the_absence_of_evidence() {
    assert_eq!(
        diagnostics_summary(&comparison(0, "$")),
        "no diagnostic evidence"
    );
}

/// Under the cap every diagnostic is named and nothing is counted: ADR-0003 wants
/// the offending levels, and a short list is the case where it gets all of them.
#[test]
fn a_short_list_names_every_diagnostic_and_counts_nothing() {
    let summary = diagnostics_summary(&comparison(3, "$.a"));
    assert_eq!(summary.matches("PropertyAdded at $.a").count(), 3);
    assert!(!summary.contains("more"), "got {summary}");
}

/// Report the capped prefix and count omitted diagnostics.
#[test]
fn a_long_list_is_capped_and_the_remainder_is_counted() {
    let total = MAX_REPORTED_DIAGNOSTICS + 7;
    let summary = diagnostics_summary(&comparison(total, "$.a"));
    assert_eq!(
        summary.matches("PropertyAdded at $.a").count(),
        MAX_REPORTED_DIAGNOSTICS,
    );
    assert!(summary.ends_with("; and 7 more"), "got {summary}");
}

/// A single long property name must not bypass the payload bound.
#[test]
fn one_diagnostic_with_an_enormous_path_still_produces_a_bounded_message() {
    let huge = "$.".to_owned() + &"x".repeat(100_000);
    let summary = diagnostics_summary(&comparison(1, &huge));
    assert!(
        summary.len() < 4 * MAX_REPORTED_PATH_BYTES,
        "one diagnostic produced {} bytes",
        summary.len(),
    );
    assert!(summary.contains("...(truncated)"), "got {summary}");
}

/// The message's size is a function of this file's constants, not of the candidate:
/// the worst case is every entry at the full path cap.
#[test]
fn the_message_is_bounded_by_the_two_constants_together() {
    let huge = "$.".to_owned() + &"y".repeat(MAX_REPORTED_PATH_BYTES * 2);
    let summary = diagnostics_summary(&comparison(MAX_REPORTED_DIAGNOSTICS + 1, &huge));
    // Each entry contributes the truncated path, the finding name, the marker and
    // the separator. A generous per-entry allowance still bounds the whole.
    let ceiling = MAX_REPORTED_DIAGNOSTICS * (MAX_REPORTED_PATH_BYTES + 128);
    assert!(
        summary.len() <= ceiling,
        "{} bytes exceeds the {ceiling}-byte ceiling",
        summary.len(),
    );
}

/// Truncation must preserve UTF-8 when the byte cap falls inside a character.
#[test]
fn a_multi_byte_path_is_cut_on_a_character_boundary() {
    // `\u{2603}` (snowman) is three UTF-8 bytes, so no repetition lands the 200-byte
    // cap on a boundary without walking back — the case a byte slice would panic on.
    let path = "\u{2603}".repeat(200);
    assert!(path.len() > MAX_REPORTED_PATH_BYTES);
    let summary = diagnostics_summary(&comparison(1, &path));
    assert!(summary.contains("...(truncated)"), "got {summary}");
    // Reaching here at all means no byte-boundary panic; assert the prefix survived
    // as whole characters.
    let kept = summary
        .split_once(" at ")
        .expect("the summary names a path")
        .1
        .trim_end_matches("...(truncated)");
    assert!(
        kept.chars().all(|c| c == '\u{2603}'),
        "the cut split a character: {kept:?}",
    );
    assert!(kept.len() <= MAX_REPORTED_PATH_BYTES);
}

/// A path exactly at the cap is not truncated: the marker must mean "there was
/// more", or a reader cannot trust a pointer that carries it.
#[test]
fn a_path_at_the_cap_is_left_alone() {
    let path = "z".repeat(MAX_REPORTED_PATH_BYTES);
    let summary = diagnostics_summary(&comparison(1, &path));
    assert!(!summary.contains("truncated"), "got {summary}");
    assert!(summary.ends_with(&path), "got {summary}");
}
