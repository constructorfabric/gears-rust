// Updated: 2026-10-06 by Constructor Tech
//! Metrics vocabulary and recording port for credential-store operations.
//!
//! Defines bounded labels for outcomes and dependencies, plus the lifecycle
//! counters of ADR-0006: the write-intent counter
//! (`write_intents_healed`), the store-cleanup counters (`store_cleanup_recorded`,
//! `store_cleanup_failed`, by debt op), `write_commit_verified` (the
//! verification after an ambiguous commit) and `read_retry`. No inventory
//! gauges: counting rows is forbidden by the platform's no-`COUNT` rule.

use toolkit_macros::domain_model;

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    HitOwn,
    HitInherited,
    Miss,
    /// The decisive record's secret has expired (`SecretExpired`).
    Expired,
}
impl ReadOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HitOwn => "hit_own",
            Self::HitInherited => "hit_inherited",
            Self::Miss => "miss",
            Self::Expired => "expired",
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dep {
    TenantResolver,
    Plugin,
    Pdp,
    TypesRegistry,
}
impl Dep {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TenantResolver => "tenant_resolver",
            Self::Plugin => "plugin",
            Self::Pdp => "pdp",
            Self::TypesRegistry => "types_registry",
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepOp {
    GetAncestors,
    IsAncestor,
    PluginGet,
    PluginPut,
    PluginDeleteKey,
    PluginDestroy,
    Evaluate,
    GetTypeSchemaByUuid,
}
impl DepOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GetAncestors => "get_ancestors",
            Self::IsAncestor => "is_ancestor",
            Self::PluginGet => "plugin_get",
            Self::PluginPut => "plugin_put",
            Self::PluginDeleteKey => "plugin_delete_key",
            Self::PluginDestroy => "plugin_destroy",
            Self::Evaluate => "evaluate",
            Self::GetTypeSchemaByUuid => "get_type_schema_by_uuid",
        }
    }
}

/// The kind of store-cleanup debt (the `op` label of the `store_cleanup_*`
/// counters): a key purge (`delete_key`) or a version destroy.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupOp {
    Purge,
    Destroy,
}
impl CleanupOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Purge => "purge",
            Self::Destroy => "destroy",
        }
    }
}

/// The operation whose ambiguous commit was verified
/// ([`CredStoreMetricsPort::write_commit_verified`]).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOp {
    /// A secret write's commit transaction (create, replace, patch).
    Write,
    /// A record delete.
    Delete,
}
impl VerifyOp {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Delete => "delete",
        }
    }
}

/// What the verification transaction after an ambiguous commit found.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The commit had happened.
    Committed,
    /// The commit had not happened; the transaction ran again.
    NotCommitted,
    /// The attempt took no effect (503; its version's cleanup was recorded).
    NotApplied,
    /// The verification itself failed (503, nothing executed).
    Failed,
}
impl VerifyOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::NotCommitted => "not_committed",
            Self::NotApplied => "not_applied",
            Self::Failed => "failed",
        }
    }
}

/// Outcome of a secret read that found its version gone and re-read the row
/// once (ADR-0006, DESIGN section 4.6). `SecondMiss` is the 503 case.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadRetryOutcome {
    Recovered,
    SecondMiss,
}
impl ReadRetryOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recovered => "recovered",
            Self::SecondMiss => "second_miss",
        }
    }
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    NotFound,
    Error,
}
impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::NotFound => "not_found",
            Self::Error => "error",
        }
    }
}

pub trait CredStoreMetricsPort: Send + Sync + 'static {
    fn read_outcome(&self, outcome: ReadOutcome);
    fn walkup_depth(&self, depth: u64);
    fn dependency(&self, dep: Dep, op: DepOp, outcome: Outcome, secs: f64);
    fn cross_tenant_denied(&self);
    /// `n` expired write intents were removed by heal (the next write's
    /// commit transaction, or the failed-create heal).
    fn write_intents_healed(&self, n: u64);
    /// A store-cleanup debt (`purge` or `destroy`) was recorded in the
    /// transaction that made store content dead.
    fn store_cleanup_recorded(&self, op: CleanupOp);
    /// Executing a store-cleanup debt (immediately or at heal time) failed;
    /// the debt row stays and a later request that touches the record
    /// retries it. A persistently rising value means a purge or destroy is
    /// stuck.
    fn store_cleanup_failed(&self, op: CleanupOp);
    /// A commit of a secret write or a record delete was ambiguous and the
    /// request ran its one verification transaction. A rising `failed` means
    /// the database is unreliable around commits.
    fn write_commit_verified(&self, op: VerifyOp, outcome: VerifyOutcome);
    /// A secret read found its version gone and re-read the row once.
    fn read_retry(&self, outcome: ReadRetryOutcome);
    /// An audit event for a secret read or write could not be published
    /// (event broker absent, unavailable, slow or rejecting); the operation
    /// itself was unaffected (`cpt-cf-credstore-nfr-audit`). A persistently
    /// rising value means audit events are being lost.
    fn audit_publish_failed(&self);
}

#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;
impl CredStoreMetricsPort for NoopMetrics {
    fn read_outcome(&self, _: ReadOutcome) {}
    fn walkup_depth(&self, _: u64) {}
    fn dependency(&self, _: Dep, _: DepOp, _: Outcome, _: f64) {}
    fn cross_tenant_denied(&self) {}
    fn write_intents_healed(&self, _: u64) {}
    fn store_cleanup_recorded(&self, _: CleanupOp) {}
    fn store_cleanup_failed(&self, _: CleanupOp) {}
    fn write_commit_verified(&self, _: VerifyOp, _: VerifyOutcome) {}
    fn read_retry(&self, _: ReadRetryOutcome) {}
    fn audit_publish_failed(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_snake_case() {
        assert_eq!(ReadOutcome::HitInherited.as_str(), "hit_inherited");
        assert_eq!(Dep::TenantResolver.as_str(), "tenant_resolver");
        assert_eq!(DepOp::PluginGet.as_str(), "plugin_get");
        assert_eq!(Outcome::NotFound.as_str(), "not_found");
    }

    #[test]
    fn all_label_variants_render() {
        assert_eq!(ReadOutcome::HitOwn.as_str(), "hit_own");
        assert_eq!(ReadOutcome::Miss.as_str(), "miss");
        assert_eq!(Dep::Plugin.as_str(), "plugin");
        assert_eq!(Dep::Pdp.as_str(), "pdp");
        assert_eq!(Dep::TypesRegistry.as_str(), "types_registry");
        assert_eq!(DepOp::GetAncestors.as_str(), "get_ancestors");
        assert_eq!(DepOp::IsAncestor.as_str(), "is_ancestor");
        assert_eq!(DepOp::PluginPut.as_str(), "plugin_put");
        assert_eq!(DepOp::PluginDeleteKey.as_str(), "plugin_delete_key");
        assert_eq!(DepOp::PluginDestroy.as_str(), "plugin_destroy");
        assert_eq!(DepOp::Evaluate.as_str(), "evaluate");
        assert_eq!(
            DepOp::GetTypeSchemaByUuid.as_str(),
            "get_type_schema_by_uuid"
        );
        assert_eq!(Outcome::Success.as_str(), "success");
        assert_eq!(Outcome::Error.as_str(), "error");
        assert_eq!(ReadRetryOutcome::Recovered.as_str(), "recovered");
        assert_eq!(ReadRetryOutcome::SecondMiss.as_str(), "second_miss");
        assert_eq!(CleanupOp::Purge.as_str(), "purge");
        assert_eq!(CleanupOp::Destroy.as_str(), "destroy");
    }

    #[test]
    fn verification_labels_render() {
        assert_eq!(VerifyOp::Write.as_str(), "write");
        assert_eq!(VerifyOp::Delete.as_str(), "delete");
        assert_eq!(VerifyOutcome::Committed.as_str(), "committed");
        assert_eq!(VerifyOutcome::NotCommitted.as_str(), "not_committed");
        assert_eq!(VerifyOutcome::NotApplied.as_str(), "not_applied");
        assert_eq!(VerifyOutcome::Failed.as_str(), "failed");
    }

    #[test]
    fn noop_metrics_port_is_inert() {
        let noop = NoopMetrics;
        noop.read_outcome(ReadOutcome::Miss);
        noop.walkup_depth(3);
        noop.dependency(Dep::Pdp, DepOp::Evaluate, Outcome::Success, 0.1);
        noop.cross_tenant_denied();
        noop.write_intents_healed(2);
        noop.store_cleanup_recorded(CleanupOp::Purge);
        noop.store_cleanup_failed(CleanupOp::Destroy);
        noop.write_commit_verified(VerifyOp::Write, VerifyOutcome::Committed);
        noop.read_retry(ReadRetryOutcome::Recovered);
        noop.audit_publish_failed();
    }
}
