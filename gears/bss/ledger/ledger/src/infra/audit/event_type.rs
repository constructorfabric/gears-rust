//! The fixed catalogue of secured-audit event types. The `as_str` tokens are
//! the canonical wire codes — they MUST match the `chk_secured_audit_event_type`
//! CHECK in migration 000009 (a code not in the set is rejected by the DB).

/// A secured-audit event type. The 12 variants match the migration's CHECK set.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AuditEventType {
    /// A conflicting concurrent change was captured for later resolution.
    ConflictCapture,
    /// Mutable metadata on a record changed.
    MetadataChange,
    /// A cross-tenant access took place.
    CrossTenantAccess,
    /// A manual adjustment was posted.
    ManualAdjustment,
    /// PII was erased (GDPR right-to-erasure).
    Erasure,
    /// A previously-erased subject was re-identified.
    ReIdentification,
    /// An account lifecycle state changed (open/close/suspend).
    AccountLifecycleChange,
    /// An exception (e.g. a hold) was resolved.
    ExceptionResolution,
    /// A scope freeze was set or cleared.
    FreezeSetClear,
    /// A configuration value changed.
    ConfigChange,
    /// Data was restored from backup.
    RestoreEvent,
    /// A closed fiscal period was reopened.
    PeriodReopen,
}

impl AuditEventType {
    /// Every event-type wire token, in the migration CHECK-set order. Pinned
    /// equal to `chk_secured_audit_event_type` by the migration's drift test.
    pub const ALL: &'static [&'static str] = &[
        "conflict-capture",
        "metadata-change",
        "cross-tenant-access",
        "manual-adjustment",
        "erasure",
        "re-identification",
        "account-lifecycle-change",
        "exception-resolution",
        "freeze-set-clear",
        "config-change",
        "restore-event",
        "period-reopen",
    ];

    /// Stable wire token for this event type (matches the migration CHECK set).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConflictCapture => "conflict-capture",
            Self::MetadataChange => "metadata-change",
            Self::CrossTenantAccess => "cross-tenant-access",
            Self::ManualAdjustment => "manual-adjustment",
            Self::Erasure => "erasure",
            Self::ReIdentification => "re-identification",
            Self::AccountLifecycleChange => "account-lifecycle-change",
            Self::ExceptionResolution => "exception-resolution",
            Self::FreezeSetClear => "freeze-set-clear",
            Self::ConfigChange => "config-change",
            Self::RestoreEvent => "restore-event",
            Self::PeriodReopen => "period-reopen",
        }
    }
}
