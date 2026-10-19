//! The metrics ports: the test binding of every port, and the finite label
//! sets the DESIGN names (§7 *Feature Metrics*).

use std::path::Path;
use std::sync::Arc;

use super::{
    LifecycleMetrics, NoMetrics, ReadMetrics, SecurityMetrics, StepUpOperation, WriteMetrics,
    labels,
};
use crate::domain::resolution::{scope_class, scope_kind};
use crate::domain::stepup::StepUpRefusal;

/// Compiles only for a type that is every metrics port at once.
fn is_every_port<T: WriteMetrics + LifecycleMetrics + ReadMetrics + SecurityMetrics>() {}

#[test]
fn the_test_binding_implements_every_metrics_port_and_counts_nothing() {
    is_every_port::<NoMetrics>();

    // Each port is object-safe, so a binding can be handed around as the
    // trait object the services hold.
    let write: Arc<dyn WriteMetrics> = Arc::new(NoMetrics);
    let lifecycle: Arc<dyn LifecycleMetrics> = Arc::new(NoMetrics);
    let read: Arc<dyn ReadMetrics> = Arc::new(NoMetrics);
    let security: Arc<dyn SecurityMetrics> = Arc::new(NoMetrics);

    write.value_write("committed");
    write.step_up(StepUpOperation::Set, "verified");
    write.purge_released();
    write.purge_release_failed();
    write.change("set", "committed");
    lifecycle.needs_review("admin_authored", 1);
    lifecycle.retention_pass("ok", 0);
    lifecycle.service_up(true);
    lifecycle.declarations("admin_authored", "active", 1);
    lifecycle.values("global", "platform", 1);
    read.lookup("hit", "global");
    read.read("cache", "global", 0.001);
    read.resolve_failure("unavailable");
    security.authz_denial("read", "denied");
    security.secret_use("allowed");
    security.audit_event("mutation");
}

#[test]
fn the_label_sets_are_the_designs_finite_sets() {
    assert_eq!(labels::OUTCOMES, ["committed", "rejected"]);
    assert_eq!(labels::CHANGE_TYPES, ["set", "revert", "remove", "clone"]);
    assert_eq!(labels::LOOKUP_RESULTS, ["hit", "miss"]);
    assert_eq!(labels::READ_SOURCES, ["cache", "resolve"]);
    assert_eq!(labels::RESOLVE_FAILURES, ["unavailable", "internal"]);
    assert_eq!(
        labels::AUTHZ_REASONS,
        ["denied", "unavailable", "constraints"]
    );
    assert_eq!(
        labels::AUTHZ_OPERATIONS,
        ["read", "write", "create", "update", "delete", "delegate"],
        "`read_unmasked` is a probe, not an operation of the counted enforcement point"
    );
    assert_eq!(labels::SECRET_USE_RESULTS, ["allowed", "denied"]);
    assert_eq!(labels::AUDIT_KINDS, ["mutation", "secret_use"]);
    assert_eq!(labels::VERIFIED, "verified");
    assert_eq!(labels::SERVICE_PRINCIPAL, "service_principal");
}

#[test]
fn the_scope_and_inventory_label_sets_are_the_designs() {
    assert_eq!(
        [
            scope_class::GLOBAL,
            scope_class::CASCADING,
            scope_class::LOCAL
        ],
        ["global", "cascading", "local"]
    );
    assert_eq!(
        [scope_kind::PLATFORM, scope_kind::TENANT],
        ["platform", "tenant"]
    );
    assert_eq!(
        crate::domain::declaration::SOURCES,
        ["admin_authored", "module_contributed"]
    );
    assert_eq!(crate::domain::declaration::STATUSES, ["active", "retired"]);
}

#[test]
fn every_refusal_code_is_a_step_up_result_label() {
    // The refusal codes are the `result` values beside `verified` and
    // `service_principal`; a refusal added without a label breaks here.
    let every = [
        StepUpRefusal::Missing,
        StepUpRefusal::NotConfigured,
        StepUpRefusal::Unavailable(String::new()),
        StepUpRefusal::Signature(String::new()),
        StepUpRefusal::SubjectMismatch,
        StepUpRefusal::AuthTimeMissing,
        StepUpRefusal::Stale,
        StepUpRefusal::Assurance,
        StepUpRefusal::Claims(String::new()),
    ];
    let codes: Vec<&str> = every.iter().map(StepUpRefusal::code).collect();
    assert_eq!(codes, labels::STEP_UP_REFUSALS);
}

/// Every call on a metrics port in the production sources, as the string
/// literals among its arguments.
/// `calls` counts every such call, literal-carrying or not.
fn literal_labels(dir: &Path, out: &mut Vec<(String, String)>, calls: &mut usize) {
    for entry in std::fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            literal_labels(&path, out, calls);
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_owned();
        if path.extension().is_none_or(|ext| ext != "rs")
            || name.ends_with("_tests.rs")
            || name == "test_support.rs"
            || name == "ports.rs"
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a readable source file");
        for method in [
            "value_write",
            "step_up",
            "change",
            "lookup",
            "read",
            "resolve_failure",
            "authz_denial",
            "secret_use",
            "audit_event",
            "retention_pass",
            "declarations",
            "values",
        ] {
            let needle = format!("metrics.{method}(");
            let mut rest = text.as_str();
            while let Some(at) = rest.find(&needle) {
                *calls += 1;
                let after = &rest[at + needle.len()..];
                let end = after.find(')').unwrap_or(after.len());
                let mut args = &after[..end];
                while let Some(open) = args.find('"') {
                    let Some(len) = args[open + 1..].find('"') else {
                        break;
                    };
                    out.push((method.to_owned(), args[open + 1..open + 1 + len].to_owned()));
                    args = &args[open + 2 + len..];
                }
                rest = &after[end..];
            }
        }
    }
}

/// The values a metrics port method may be handed as a literal.
fn allowed_for(method: &str) -> Vec<&'static str> {
    let classes = [
        scope_class::GLOBAL,
        scope_class::CASCADING,
        scope_class::LOCAL,
    ];
    let kinds = [scope_kind::PLATFORM, scope_kind::TENANT];
    match method {
        "value_write" => labels::OUTCOMES.to_vec(),
        "step_up" => [
            &labels::STEP_UP_REFUSALS[..],
            &[labels::VERIFIED, labels::SERVICE_PRINCIPAL],
        ]
        .concat(),
        "change" => [&labels::CHANGE_TYPES[..], &labels::OUTCOMES].concat(),
        "lookup" => [&labels::LOOKUP_RESULTS[..], &classes].concat(),
        "read" => [&labels::READ_SOURCES[..], &classes].concat(),
        "resolve_failure" => labels::RESOLVE_FAILURES.to_vec(),
        "authz_denial" => [&labels::AUTHZ_OPERATIONS[..], &labels::AUTHZ_REASONS].concat(),
        "secret_use" => labels::SECRET_USE_RESULTS.to_vec(),
        "audit_event" => labels::AUDIT_KINDS.to_vec(),
        "retention_pass" => labels::RETENTION_RESULTS.to_vec(),
        "declarations" => [
            &crate::domain::declaration::SOURCES[..],
            &crate::domain::declaration::STATUSES,
        ]
        .concat(),
        "values" => [&classes[..], &kinds].concat(),
        other => panic!("no allow-list for the port method `{other}`"),
    }
}

#[test]
fn production_code_passes_only_the_designs_label_values_to_the_metrics_ports() {
    let mut found = Vec::new();
    let mut calls = 0;
    literal_labels(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
        &mut calls,
    );
    // The call sites pass the `labels` constants, so few literals are left to
    // check; the scan still has to be seeing the calls.
    assert!(
        calls >= 20,
        "the scan finds the production call sites: {calls}"
    );
    // Each literal is checked against the set of the method it is passed to,
    // not against one flat union.
    let stray: Vec<&(String, String)> = found
        .iter()
        .filter(|(method, literal)| !allowed_for(method).contains(&literal.as_str()))
        .collect();
    assert!(
        stray.is_empty(),
        "a label value outside the DESIGN's finite set of its port method: {stray:?}"
    );
}

#[test]
fn the_retention_result_labels_are_the_designs() {
    assert_eq!(labels::RETENTION_RESULTS, ["ok", "failed"]);
}

#[test]
fn every_authz_operation_the_handlers_name_is_a_counted_operation() {
    // The handler modules keep their own action consts — the action
    // vocabulary — so each must be one the counter's finite set holds.
    let mut sources = Vec::new();
    production_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api/rest"),
        &mut sources,
    );
    let mut found = Vec::new();
    for (name, text) in &sources {
        if !name.ends_with("_handlers.rs") && !name.ends_with("/handlers.rs") {
            continue;
        }
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("const ") else {
                continue;
            };
            let Some((ident, value)) = rest.split_once(": &str = \"") else {
                continue;
            };
            if ["READ", "WRITE", "CREATE", "UPDATE", "DELETE", "DELEGATE"].contains(&ident) {
                found.push((
                    name.clone(),
                    ident.to_owned(),
                    value.trim_end_matches("\";").to_owned(),
                ));
            }
        }
    }
    assert!(found.len() >= 10, "the scan finds the consts: {found:?}");
    let stray: Vec<_> = found
        .iter()
        .filter(|(_, _, value)| !labels::AUTHZ_OPERATIONS.contains(&value.as_str()))
        .collect();
    assert!(stray.is_empty(), "not in AUTHZ_OPERATIONS: {stray:?}");
}

/// Every production source file under `dir`, as its path and text.
fn production_sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("a readable source directory") {
        let path = entry.expect("an entry").path();
        if path.is_dir() {
            production_sources(&path, out);
            continue;
        }
        let name = path.to_string_lossy().into_owned();
        if path.extension().is_some_and(|ext| ext == "rs")
            && !name.ends_with("_tests.rs")
            && !name.ends_with("test_support.rs")
        {
            out.push((
                name,
                std::fs::read_to_string(&path).expect("a readable source file"),
            ));
        }
    }
}

#[test]
fn the_gears_router_installs_no_http_latency_histogram_of_its_own() {
    // REST latency is the host's `http.server.request.duration` by
    // `http.route`; a second layer in the gear would double-count it. The
    // histograms the gear owns are the reader boundary's and the actor-name
    // lookup's.
    let mut sources = Vec::new();
    production_sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut sources,
    );
    assert!(sources.len() > 50, "the scan reads the sources");
    let offenders: Vec<&str> = sources
        .iter()
        .filter(|(name, text)| {
            !name.ends_with("infra/read_metrics.rs")
                && !name.ends_with("infra/actor_name_metrics.rs")
                && !name.ends_with("domain/ports_tests.rs")
                && (text.contains("http.server.request.duration")
                    || text.contains("f64_histogram")
                    || text.contains("u64_histogram"))
        })
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}
