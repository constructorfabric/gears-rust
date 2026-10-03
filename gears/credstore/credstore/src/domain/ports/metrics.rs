//! Metrics vocabulary and recording port for credential-store operations.
//!
//! Defines bounded labels for outcomes and dependencies, plus the lifecycle
//! counters of ADR-0006: the write-intent counters
//! (`write_intents_reclaimed`, `write_intent_lost`,
//! `write_intent_reclaim_failed`), the store-cleanup counters
//! (`store_cleanup_enqueued`, `store_cleanup_failed`, by task op) and
//! `read_retry`. No inventory gauges: the shipped reaper's per-status
//! row-count gauges were `COUNT … GROUP BY` queries, forbidden by the
//! platform's no-`COUNT` rule, and are withdrawn rather than reimplemented.

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
            Self::PluginGet => "plugin_get",
            Self::PluginPut => "plugin_put",
            Self::PluginDeleteKey => "plugin_delete_key",
            Self::PluginDestroy => "plugin_destroy",
            Self::Evaluate => "evaluate",
            Self::GetTypeSchemaByUuid => "get_type_schema_by_uuid",
        }
    }
}

/// The kind of store-cleanup task (the `op` label of the `store_cleanup_*`
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
    /// `n` expired write intents were reclaimed (deleted by
    /// `reclaim_expired`).
    fn write_intents_reclaimed(&self, n: u64);
    /// A secret write's commit transaction found its own intent already
    /// reclaimed (the writer outlived its lease); the writer settled its
    /// version itself.
    fn write_intent_lost(&self);
    /// A reclaim pass (startup or piggyback after a write) failed; the
    /// intents stay for the next pass. A persistently rising value means
    /// expired intents are not being reclaimed.
    fn write_intent_reclaim_failed(&self);
    /// A store-cleanup task (`purge` or `destroy`) was enqueued in the
    /// transactional outbox.
    fn store_cleanup_enqueued(&self, op: CleanupOp);
    /// A store-cleanup delivery attempt failed and will be retried; a
    /// persistently rising value means a purge or destroy is stuck.
    fn store_cleanup_failed(&self, op: CleanupOp);
    /// A secret read found its version gone and re-read the row once.
    fn read_retry(&self, outcome: ReadRetryOutcome);
    /// Collection read (ADR-0005): a reference's reduced winner named a
    /// `secret_type_uuid` outside the set the request authorized per
    /// distinct type found in the candidate-reference query. The reference
    /// is dropped from the page rather than surfaced — a missing catalogue
    /// entry, not a false one — and this is the operational signal: it means
    /// the override-type-consistency invariant
    /// (`cpt-cf-credstore-fr-override-type-consistency`) was violated for
    /// that reference, which should never happen if every write went
    /// through the write path's own check.
    fn list_type_invariant_violation(&self);
    /// An audit event for a secret read or write could not be published
    /// (event broker absent, unavailable, slow or rejecting); the operation
    /// itself was unaffected (`cpt-cf-credstore-nfr-audit`). A persistently
    /// rising value means audit events are being lost.
    fn audit_publish_failed(&self);
    /// A secret read produced the permanent "unreadable" outcome: the plugin
    /// reported the version unreadable, or the version was gone although the
    /// record's pointer did not move. Persistently rising means records
    /// need a rewrite or delete (lost key, corrupt entry).
    fn secret_unreadable(&self);
}

#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;
impl CredStoreMetricsPort for NoopMetrics {
    fn read_outcome(&self, _: ReadOutcome) {}
    fn walkup_depth(&self, _: u64) {}
    fn dependency(&self, _: Dep, _: DepOp, _: Outcome, _: f64) {}
    fn cross_tenant_denied(&self) {}
    fn write_intents_reclaimed(&self, _: u64) {}
    fn write_intent_lost(&self) {}
    fn write_intent_reclaim_failed(&self) {}
    fn store_cleanup_enqueued(&self, _: CleanupOp) {}
    fn store_cleanup_failed(&self, _: CleanupOp) {}
    fn read_retry(&self, _: ReadRetryOutcome) {}
    fn list_type_invariant_violation(&self) {}
    fn audit_publish_failed(&self) {}
    fn secret_unreadable(&self) {}
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
    fn noop_metrics_port_is_inert() {
        let noop = NoopMetrics;
        noop.read_outcome(ReadOutcome::Miss);
        noop.walkup_depth(3);
        noop.dependency(Dep::Pdp, DepOp::Evaluate, Outcome::Success, 0.1);
        noop.cross_tenant_denied();
        noop.write_intents_reclaimed(2);
        noop.write_intent_lost();
        noop.write_intent_reclaim_failed();
        noop.store_cleanup_enqueued(CleanupOp::Purge);
        noop.store_cleanup_failed(CleanupOp::Destroy);
        noop.read_retry(ReadRetryOutcome::Recovered);
        noop.list_type_invariant_violation();
        noop.audit_publish_failed();
        noop.secret_unreadable();
    }
}
