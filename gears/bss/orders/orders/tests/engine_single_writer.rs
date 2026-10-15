//! S2-04 single-writer gate (DESIGN §4.1: "No component other than the engine MAY write the
//! aggregate, version, line, resolved-total, transition-audit or idempotency tables"; Foundation
//! step 19: slices "do not update the aggregate independently").
//!
//! Every production call of an aggregate/version/audit/registry/claim/outbox writer must sit in
//! the engine's transaction adapter or in the storage layer that defines it. A slice handler,
//! worker or REST adapter added later that calls one directly fails this scan. Slices receive
//! only `ChildDocuments`, which exposes child-document writers and no aggregate handle.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Repository files must exist; fail loudly.
use std::path::{Path, PathBuf};

/// Writer entry points and aggregate handles reserved to the engine (and the storage modules
/// defining them). A slice reaches child documents only through `ChildDocuments`, whose method
/// names may coincide with `LockedOrder` writers; it can never name or obtain a `LockedOrder`,
/// so those coinciding names are covered by forbidding the handle and every way to acquire it.
const WRITERS: &[&str] = &[
    // The aggregate handle itself and every way to obtain one.
    "LockedOrder",
    "lock_authorized(",
    "lock_target_order(",
    "insert_authorized_order(",
    "apply_authorized_arrangement(",
    // Raw aggregate/version writes that bypass the handle.
    "insert_order(",
    "insert_order_version(",
    "order::Entity::insert",
    "order::Entity::update",
    "order_version::Entity::insert",
    "bss_orders__order ",
    // Transition audit, registry, durable executions, claims and the outbox.
    "append_committed(",
    "append_committed_all(",
    "append_resolved_refusal(",
    "append_unresolved_refusal(",
    "advance_audit_sequence(",
    "finish_execution(",
    "maintain_claims(",
    "release_claims(",
    "begin_commercial_attempt(",
    "begin_fulfillment_control(",
    "replace_commercial_attempt(",
    "replace_fulfillment_control(",
    "replace_idempotency(",
    "reg::resolve(",
    "idempotency::resolve(",
    "Settlement::Success",
    "Settlement::Refused",
    "TxEvents",
];
/// Production locations allowed to call them: the engine adapter, the storage layer that
/// defines them, the event module that defines the enqueue, and the D-188/D-198 registry.
const PERMITTED: &[&str] = &[
    "src/infra/engine/",
    "src/infra/storage/repo/",
    "src/infra/storage/entity/",
    "src/infra/storage/scoped.rs",
    "src/infra/events/",
    "src/domain/idempotency.rs",
    // S2-03's discovered-target lock for workers: it locks and rechecks, and writes nothing.
    "src/infra/maintenance/mod.rs",
];

/// The reserved writers a production source text names.
fn violations(text: &str) -> Vec<&'static str> {
    WRITERS
        .iter()
        .copied()
        .filter(|writer| text.contains(writer))
        .collect()
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}
fn is_test_source(relative: &str) -> bool {
    relative.contains("/tests/")
        || relative.ends_with("_tests.rs")
        || relative.ends_with("/tests.rs")
        || relative.contains("test_support")
        || relative.ends_with("test_pdp.rs")
        || relative.ends_with("test_registry.rs")
}

#[test]
fn only_the_engine_and_storage_layer_call_aggregate_writers() {
    let mut files = vec![];
    rust_files(&root().join("src"), &mut files);
    let mut checked = 0;
    for file in files {
        let relative = file
            .strip_prefix(root())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_source(&relative) || PERMITTED.iter().any(|p| relative.starts_with(p)) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        let found = violations(&text);
        assert!(
            found.is_empty(),
            "{relative} names engine-only writers {found:?}: route it through the engine"
        );
        checked += 1;
    }
    assert!(checked > 10, "the scan saw the gear's production modules");
}

#[test]
fn the_slice_handle_exposes_child_documents_and_never_the_aggregate() {
    let documents = std::fs::read_to_string(root().join("src/infra/engine/documents.rs")).unwrap();
    let handle = &documents[documents.find("impl<'l, 'a> ChildDocuments").unwrap()
        ..documents.find("pub trait DocumentWriter").unwrap()];
    for forbidden in [
        "pub fn row(",
        "pub fn locked(",
        "pub fn transaction(",
        "pub async fn replace(",
        "insert_order_version",
        "advance_audit_sequence",
        "append_",
        "maintain_claims",
        "enqueue",
    ] {
        assert!(
            !handle.contains(forbidden),
            "ChildDocuments must not expose `{forbidden}`"
        );
    }
    assert!(handle.contains("pub async fn insert_fulfillment_grant("));
}

#[test]
fn the_scan_flags_a_slice_that_takes_the_aggregate_or_writes_it() {
    // A slice handler that locks the aggregate itself, or writes it raw, is caught.
    for stray in [
        "let locked = scoped::lock_authorized(tx, &target).await?;",
        "fn f(l: &LockedOrder<'_, DbTx<'_>>) {}",
        "repo::insert_order(tx, &scope, row).await?;",
        "conn.execute_unprepared(\"UPDATE bss_orders__order SET state='x'\").await?;",
        "fn g(e: &TxEvents) {}",
    ] {
        assert!(!violations(stray).is_empty(), "not flagged: {stray}");
    }
    // Child-document contributions through the bounded handle are the permitted path.
    let slice = "docs.add_draft_line(row).await?; docs.replace_order_admin(&old, new).await?;";
    assert!(violations(slice).is_empty());
}

#[test]
fn an_event_detail_receives_a_sealed_enqueue_and_never_the_aggregate() {
    let documents = std::fs::read_to_string(root().join("src/infra/engine/documents.rs")).unwrap();
    let spec = &documents[documents.find("pub trait EventSpec").unwrap()
        ..documents.find("pub struct EventEnqueue").unwrap()];
    assert!(
        !spec.contains("LockedOrder"),
        "EventSpec must not receive the aggregate"
    );
    assert!(
        !spec.contains("TxEvents"),
        "EventSpec must not receive the raw sink"
    );
    let handle = &documents[documents.find("pub struct EventEnqueue").unwrap()
        ..documents.find("impl<'l, 'a> EventEnqueue").unwrap()];
    assert!(
        !handle.contains("pub events") && !handle.contains("pub locked"),
        "EventEnqueue fields stay private to the engine"
    );
}
