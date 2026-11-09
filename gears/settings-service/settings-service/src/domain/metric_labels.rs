//! The label values the metrics ports take.
//!
//! Each is a finite set the DESIGN names (§7 *Feature Metrics*): nothing a
//! request carries — a key, a tenant, an actor, a value, a path — is ever a
//! label. The sets are types, so a label outside them cannot be written: the
//! compiler refuses it where a free string would have been counted silently.
//! `ALL` lists a set in the order the DESIGN does, for a gauge that publishes
//! every member each time.

use super::stepup::StepUpRefusal;

/// Declare a label set: its variants, the text each is exported as, and `ALL`.
macro_rules! label_set {
    (
        $(#[$meta:meta])*
        $name:ident { $( $(#[$vmeta:meta])* $variant:ident => $text:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $name {
            /// Every member, in the order the DESIGN lists them.
            pub const ALL: &[Self] = &[ $( Self::$variant ),+ ];

            /// The exported label value.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $text ),+
                }
            }
        }
    };
}

label_set! {
    /// How a change ended: `settings_changes_total` and
    /// `settings_value_writes_total`.
    Outcome {
        /// The change was durably committed.
        Committed => "committed",
        /// The change was refused or its commit failed.
        Rejected => "rejected",
    }
}

label_set! {
    /// The kinds of change `settings_changes_total` breaks down.
    ChangeType {
        /// A stored or adopted value (`create` and `change` both count it).
        Set => "set",
        /// A revert to the inherited value.
        Revert => "revert",
        /// A removal of the override.
        Remove => "remove",
        /// A clone of one scope's value onto another.
        Clone => "clone",
    }
}

label_set! {
    /// What a cache lookup found.
    LookupResult {
        /// Answered from the cache.
        Hit => "hit",
        /// Fell through to the walk.
        Miss => "miss",
    }
}

label_set! {
    /// Where a reader-boundary read was answered from.
    ReadSource {
        /// Served from the cache.
        Cache => "cache",
        /// Resolved by the walk.
        Resolve => "resolve",
    }
}

label_set! {
    /// Why a reader-boundary read failed.
    ResolveFailure {
        /// A dependency that cannot answer.
        Unavailable => "unavailable",
        /// A failure that is the service's own.
        Internal => "internal",
    }
}

label_set! {
    /// The action an enforcement point is asked for. `read_unmasked` is not
    /// among them: that entitlement is a probe, answered by masking, never
    /// counted.
    Operation {
        /// Read a value or a listing.
        Read => "read",
        /// Write an existing value.
        Write => "write",
        /// Create one.
        Create => "create",
        /// Update one.
        Update => "update",
        /// Delete one.
        Delete => "delete",
        /// Delegate access.
        Delegate => "delegate",
    }
}

label_set! {
    /// Why an enforcement point refused.
    AuthzReason {
        /// The decision point said no.
        Denied => "denied",
        /// The decision point could not be reached.
        Unavailable => "unavailable",
        /// The returned constraints could not be compiled.
        Constraints => "constraints",
    }
}

label_set! {
    /// How an audit-retention pass ended.
    RetentionResult {
        /// Ran to its end.
        Ok => "ok",
        /// A failure stopped it early.
        Failed => "failed",
    }
}

label_set! {
    /// How a machine secret use ended.
    SecretUseResult {
        /// The plaintext was handed out.
        Allowed => "allowed",
        /// The use was refused.
        Denied => "denied",
    }
}

label_set! {
    /// What an audit record records.
    AuditKind {
        /// A mutation (a staging included).
        Mutation => "mutation",
        /// A machine secret use.
        SecretUse => "secret_use",
    }
}

label_set! {
    /// Where a declaration came from.
    DeclarationSource {
        /// Authored by an administrator.
        AdminAuthored => "admin_authored",
        /// Contributed by a gear.
        ModuleContributed => "module_contributed",
    }
}

label_set! {
    /// A declaration's scope class.
    ScopeClass {
        /// One value for the whole platform, set at the root tenant only.
        Global => "global",
        /// Inherited down the tenant tree, nearest override wins.
        Cascading => "cascading",
        /// One value per tenant, never inherited.
        Local => "local",
    }
}

impl ScopeClass {
    /// The class a stored declaration names; `None` for anything else, which
    /// the table's CHECK constraint makes unreachable.
    #[must_use]
    pub fn parse(stored: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|class| class.as_str() == stored)
    }
}

label_set! {
    /// Whether a row sits at the platform or at a tenant.
    ScopeKind {
        /// A row at the root tenant.
        Platform => "platform",
        /// A row at any other tenant.
        Tenant => "tenant",
    }
}

label_set! {
    /// How the step-up check of a write ended: the proof held, the caller had
    /// no person behind it, or the refusal's own code.
    StepUpResult {
        /// A verified assertion.
        Verified => "verified",
        /// A caller with no person behind it, refused for want of an assertion.
        ServicePrincipal => "service_principal",
        /// No token was presented.
        Missing => "missing",
        /// The `AuthN` resolver could not be reached from the hub.
        NotConfigured => "not_configured",
        /// The resolver was reached but could not answer.
        Unavailable => "unavailable",
        /// The resolver did not authenticate the token.
        Signature => "signature",
        /// The token's subject is not the session's.
        SubjectMismatch => "subject_mismatch",
        /// `auth_time` is absent.
        AuthTimeMissing => "auth_time_missing",
        /// `auth_time` is older than the freshness window.
        Stale => "stale",
        /// `acr` or `amr` does not meet the required assurance.
        Assurance => "assurance",
        /// A pinned claim is not carried.
        Claims => "claims",
    }
}

impl From<&StepUpRefusal> for StepUpResult {
    fn from(refusal: &StepUpRefusal) -> Self {
        match refusal {
            StepUpRefusal::Missing => Self::Missing,
            StepUpRefusal::NotConfigured => Self::NotConfigured,
            StepUpRefusal::Unavailable(_) => Self::Unavailable,
            StepUpRefusal::Signature(_) => Self::Signature,
            StepUpRefusal::SubjectMismatch => Self::SubjectMismatch,
            StepUpRefusal::AuthTimeMissing => Self::AuthTimeMissing,
            StepUpRefusal::Stale => Self::Stale,
            StepUpRefusal::Assurance => Self::Assurance,
            StepUpRefusal::Claims(_) => Self::Claims,
        }
    }
}

#[cfg(test)]
#[path = "metric_labels_tests.rs"]
mod metric_labels_tests;
