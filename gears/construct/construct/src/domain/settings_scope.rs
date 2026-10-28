//! Why a policy scope does not let Subject Settings reach one key's row. Kept
//! apart from `subject_settings`, so the gear-wide `DomainError` can carry the
//! refusal without depending on that feature module.

use toolkit_macros::domain_model;
use uuid::Uuid;

/// A predicate kind that cannot be checked against a key without the database.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UncheckablePredicate {
    Group,
    GroupSubtree,
    TenantSubtree,
    /// A filter kind this gear does not know.
    Unknown,
}

impl std::fmt::Display for UncheckablePredicate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Group => "group",
            Self::GroupSubtree => "group-subtree",
            Self::TenantSubtree => "tenant-subtree",
            Self::Unknown => "unknown",
        })
    }
}

/// Why a policy scope does not let a read or a write reach one key's row.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeRefusal {
    /// The scope does not cover the key, whether or not a row exists.
    NotCovered,
    /// The scope holds a predicate that cannot be checked against the key
    /// without the database. Treated as not covering, so the read or write
    /// fails closed.
    Undecidable(UncheckablePredicate),
}

/// A read or a write refused because the caller's policy scope leaves the
/// subject's settings out. It names the caller, the caller's tenant, the
/// subject, the subject's tenant and why, so the log tells a scope refusal from
/// a policy denial: on Record Intake's path the subject's tenant is the one the
/// connector names, not the connector's own.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsOutOfScope {
    pub caller: Uuid,
    pub caller_tenant: Uuid,
    pub tenant_id: Uuid,
    pub subject_id: Uuid,
    pub refusal: ScopeRefusal,
}

impl std::fmt::Display for SettingsOutOfScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "caller {} of tenant {} may not read or change the settings of subject {} in \
             tenant {}: ",
            self.caller, self.caller_tenant, self.subject_id, self.tenant_id
        )?;
        match self.refusal {
            ScopeRefusal::NotCovered => f.write_str("the policy scope does not cover them"),
            ScopeRefusal::Undecidable(predicate) => write!(
                f,
                "the policy scope holds a {predicate} predicate, which cannot be checked \
                 against the key, so it does not count as covering them"
            ),
        }
    }
}
