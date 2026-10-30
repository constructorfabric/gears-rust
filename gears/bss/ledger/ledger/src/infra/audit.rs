//! Secured audit (Slice 6 Phase 2 Group 2A store + the Slice 3 ↔ Slice 6 sink seam).
//!
//! Slice 6 (audit) owns the append-only `secured_audit_record` store
//! with its own per-tenant tamper-evidence hash chain (`event_type` / `retrieval`
//! / `store`). Each appended record is born sealed (`row_hash` / `prev_hash`
//! non-NULL) and is never updated.
//!
//! Slice 3 depends on the secured-audit append only through a local **port**
//! ([`secured_audit_sink::SecuredAuditSink`]) whose method signature mirrors
//! `store::SecuredAuditStore::append` (design §2.1). The wired implementation is
//! [`secured_audit_sink::NoopSecuredAuditSink`] — it records nothing durable
//! (logs + emits a metric only). Slice 3 uses it for the `unknown_final` refund
//! disposition (clear `REFUND_CLEARING` → documented loss line + an audit record,
//! design §4.4 / K-1) and — in Phase 3 — the attempted-write-off capture (§6 A4).

pub mod event_type;
pub mod retrieval;
pub mod secured_audit_sink;
pub mod store;
