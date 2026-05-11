//! Coverage guard (mock-reference-alignment design D5): the mock test tree must
//! mirror the scenario tree 1:1. Every in-scope scenario has exactly one test that
//! references it via a `/// Scenario: <path>` doc comment; the only permitted
//! absences are the explicit `OUT_OF_SCOPE` entries (auth + pure-HTTP-transport
//! guardrails, covered at the HTTP/integration layer).

#![cfg(test)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Scenarios the in-process mock deliberately does NOT implement (auth +
/// transport-edge concerns). These live at the HTTP/integration layer and are the
/// only scenarios permitted to lack a mock test.
const OUT_OF_SCOPE: &[&str] = &[
    "auth/1.01-negative-missing-bearer-token.md",
    "auth/1.02-negative-invalid-bearer-token.md",
    "auth/1.03-negative-insufficient-permission-produce.md",
    "auth/1.04-negative-insufficient-permission-consume.md",
    "auth/1.05-negative-cross-tenant-anonymous-group.md",
    "consumer/subscriptions/1.06-negative-join-unauthorized-topic.md",
    "consumer/stream/1.07-guardrail-stream-accept-json-rejected.md",
    "consumer/stream/1.08-guardrail-sse-from-stream-endpoint.md",
    "consumer/stream/1.09-positive-sse-event-stream.md",
    "consumer/stream/1.10-negative-stream-rejects-timeout-collect-params.md",
    "errors/1.02-negative-401-unauthenticated.md",
    "errors/1.03-negative-403-unauthorized.md",
];

fn collect_scenarios(dir: &Path, base: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("read scenarios dir") {
        let p = entry.unwrap().path();
        if p.is_dir() {
            collect_scenarios(&p, base, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("md") {
            if p.file_name().and_then(|n| n.to_str()) == Some("INDEX.md") {
                continue;
            }
            let rel = p
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel);
        }
    }
}

fn collect_referenced(dir: &Path, out: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(dir).expect("read tests dir") {
        let p = entry.unwrap().path();
        if p.is_dir() {
            collect_referenced(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            let txt = std::fs::read_to_string(&p).unwrap();
            for line in txt.lines() {
                let trimmed = line.trim_start();
                // Real doc comments START with the marker; a string literal that merely
                // contains it (e.g. this guard's own source) does not.
                if let Some(rest) = trimmed.strip_prefix("/// Scenario:") {
                    let path = rest.trim();
                    if !path.is_empty() {
                        out.insert(path.to_string());
                    }
                }
            }
        }
    }
}

#[test]
fn every_in_scope_scenario_has_exactly_one_test() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let scen_root = PathBuf::from(manifest).join("../scenarios");
    let tests_root = PathBuf::from(manifest).join("src/mock/tests");

    let mut scenarios = BTreeSet::new();
    collect_scenarios(&scen_root, &scen_root, &mut scenarios);

    let oos: BTreeSet<String> = OUT_OF_SCOPE.iter().map(|s| s.to_string()).collect();
    // Every OUT_OF_SCOPE entry must name a real scenario (catches typos / renames).
    let stale_oos: Vec<&String> = oos.difference(&scenarios).collect();
    assert!(
        stale_oos.is_empty(),
        "OUT_OF_SCOPE names non-existent scenarios: {stale_oos:#?}"
    );

    let expected: BTreeSet<String> = scenarios.difference(&oos).cloned().collect();

    let mut referenced = BTreeSet::new();
    collect_referenced(&tests_root, &mut referenced);

    let missing: Vec<&String> = expected.difference(&referenced).collect();
    let dangling: Vec<&String> = referenced.difference(&expected).collect();

    assert!(
        missing.is_empty() && dangling.is_empty(),
        "scenario↔test mirror is broken.\n  MISSING tests for in-scope scenarios: {missing:#?}\n  DANGLING `/// Scenario:` refs (no such in-scope scenario): {dangling:#?}",
    );
}
