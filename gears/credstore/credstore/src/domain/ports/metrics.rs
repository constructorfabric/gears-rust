use toolkit_macros::domain_model;

#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct SecretCounts {
    pub private: i64,
    pub tenant: i64,
    pub shared: i64,
    pub provisioning: i64,
    pub deprovisioning: i64,
    pub tenants: i64,
}

#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    HitOwn,
    HitInherited,
    Miss,
}
impl ReadOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HitOwn => "hit_own",
            Self::HitInherited => "hit_inherited",
            Self::Miss => "miss",
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
    PluginDelete,
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
            Self::PluginDelete => "plugin_delete",
            Self::Evaluate => "evaluate",
            Self::GetTypeSchemaByUuid => "get_type_schema_by_uuid",
        }
    }
}

/// Value-fingerprint fence verdict for a read (see
/// `docs/features/001-value-fingerprint-fence.md`). `Legacy` is an
/// out-of-band seeded row served on trust (no fingerprint yet); `Mismatch`
/// is the fail-closed anti-enumeration miss — the alertable signal.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceVerify {
    Ok,
    Legacy,
    Mismatch,
}
impl FenceVerify {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Legacy => "legacy",
            Self::Mismatch => "mismatch",
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
    fn record_inventory(&self, counts: SecretCounts);
    fn read_outcome(&self, outcome: ReadOutcome);
    fn walkup_depth(&self, depth: u64);
    fn dependency(&self, dep: Dep, op: DepOp, outcome: Outcome, secs: f64);
    fn provisioning_reaped(&self, n: u64);
    /// Stuck delete-saga rows completed by the reaper (backend value deleted,
    /// row removed).
    fn deprovisioning_reaped(&self, n: u64);
    /// Records a create-saga provisioning-row rollback after a failed backend
    /// write. `outcome` is `Error` when the rollback itself failed (the
    /// reference stays wedged until reaped) — the signal worth alerting on.
    fn provisioning_rollback(&self, outcome: Outcome);
    fn cross_tenant_denied(&self);
    /// Records the fence verdict of a read (`ok`/`legacy`/`mismatch`);
    /// `mismatch` is the fail-closed 404 worth alerting on.
    fn fence_verify(&self, outcome: FenceVerify);
    /// Records a lazy fingerprint backfill attempt: `Success` = stamped,
    /// `NotFound` = CAS no-op (a concurrent PUT already stamped), `Error` =
    /// the best-effort UPDATE failed (retried on a later read/sweep).
    fn fence_backfill(&self, outcome: Outcome);
}

#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopMetrics;
impl CredStoreMetricsPort for NoopMetrics {
    fn record_inventory(&self, _: SecretCounts) {}
    fn read_outcome(&self, _: ReadOutcome) {}
    fn walkup_depth(&self, _: u64) {}
    fn dependency(&self, _: Dep, _: DepOp, _: Outcome, _: f64) {}
    fn provisioning_reaped(&self, _: u64) {}
    fn deprovisioning_reaped(&self, _: u64) {}
    fn provisioning_rollback(&self, _: Outcome) {}
    fn cross_tenant_denied(&self) {}
    fn fence_verify(&self, _: FenceVerify) {}
    fn fence_backfill(&self, _: Outcome) {}
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
        assert_eq!(DepOp::PluginDelete.as_str(), "plugin_delete");
        assert_eq!(DepOp::Evaluate.as_str(), "evaluate");
        assert_eq!(
            DepOp::GetTypeSchemaByUuid.as_str(),
            "get_type_schema_by_uuid"
        );
        assert_eq!(Outcome::Success.as_str(), "success");
        assert_eq!(Outcome::Error.as_str(), "error");
    }

    #[test]
    fn noop_metrics_port_is_inert() {
        let noop = NoopMetrics;
        noop.record_inventory(SecretCounts::default());
        noop.read_outcome(ReadOutcome::Miss);
        noop.walkup_depth(3);
        noop.dependency(Dep::Pdp, DepOp::Evaluate, Outcome::Success, 0.1);
        noop.provisioning_reaped(2);
        noop.cross_tenant_denied();
    }
}
