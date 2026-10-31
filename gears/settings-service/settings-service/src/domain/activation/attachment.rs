// @cpt-dod:cpt-cf-settings-service-dod-activation-read-history:p1
//! The reports the history attaches under the change they answer: which of the
//! `activation` records found for a page the caller may see, and how they group.
//!
//! An `activation` record is the account of one subscriber's report on one
//! change. It is kept for a caller only where the caller could read the key at
//! the report's tenant; the key-level gates (the `read` decision, the
//! administrative domain, the caller's effective access) are the history's own
//! and hold for the caller at every tenant of the page as they hold at the
//! history's tenant, so only the tenant is tested here.

use std::collections::HashMap;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::audit::StoredAuditRecord;
use crate::domain::error::DomainError;
use crate::domain::resolution::TenantHierarchy;

/// The reports of one history page, grouped under the id of the change record
/// each answers.
#[domain_model]
#[derive(Debug, Default)]
pub struct Attachments {
    by_change: HashMap<Uuid, Vec<StoredAuditRecord>>,
}

impl Attachments {
    /// The reports attached under the change record `change`, newest first;
    /// empty when nothing answers it.
    #[must_use]
    pub fn of(&self, change: Uuid) -> &[StoredAuditRecord] {
        self.by_change.get(&change).map_or(&[], Vec::as_slice)
    }

    /// Whether any attached report is recorded with a personal-data actor, so
    /// the reader's entitlement decides what the actor shows.
    #[must_use]
    pub fn has_pii_actor(&self) -> bool {
        self.by_change
            .values()
            .flatten()
            .any(|report| report.actor_classification == crate::audit::ActorClassification::Pii)
    }
}

/// Decides, per distinct report tenant of a page, whether it lies inside the
/// caller's subtree and outside any standalone tenant under the caller.
struct Visibility<'a> {
    hierarchy: &'a dyn TenantHierarchy,
    caller: Uuid,
    target: Uuid,
    decided: HashMap<Uuid, bool>,
    standalone: HashMap<Uuid, bool>,
}

impl Visibility<'_> {
    /// Keep (`true`) or drop (`false`) a report at `tenant`.
    ///
    /// # Errors
    /// [`DomainError::Unavailable`] when the Tenant Resolver cannot answer: the
    /// read is refused rather than served with the report kept unchecked or
    /// dropped unseen.
    async fn keeps(&mut self, tenant: Uuid) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-1
        if tenant == self.target {
            return Ok(true);
        }
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-1
        if let Some(decided) = self.decided.get(&tenant) {
            return Ok(*decided);
        }
        let decided = self.decide(tenant).await?;
        self.decided.insert(tenant, decided);
        Ok(decided)
    }

    async fn decide(&mut self, tenant: Uuid) -> Result<bool, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-2
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-3
        // The chain, root to self, once per distinct tenant of the page. A
        // tenant the resolver does not know is on no chain of the caller's.
        let chain = match self.hierarchy.chain(tenant).await {
            Ok(chain) => chain,
            Err(DomainError::NotFound { .. }) => return Ok(false),
            Err(err) => return Err(err),
        };
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-3
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-2
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-4
        let Some(at) = chain.iter().position(|id| *id == self.caller) else {
            return Ok(false);
        };
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-4
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-5
        // Every tenant below the caller on the chain, the report's own
        // included: nothing of a standalone tenant's state is shown from above.
        for below in &chain[at + 1..] {
            let standalone = if let Some(known) = self.standalone.get(below) {
                *known
            } else {
                // A tenant the resolver does not know cannot be shown from
                // above: the report is dropped, as an unknown chain drops it.
                let known = match self.hierarchy.is_standalone(*below).await {
                    Ok(known) => known,
                    Err(DomainError::NotFound { .. }) => true,
                    Err(err) => return Err(err),
                };
                self.standalone.insert(*below, known);
                known
            };
            if standalone {
                return Ok(false);
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-5
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-6
        Ok(true)
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-visibility:p1:inst-ar-visibility-6
    }
}

/// Keep the `reports` the caller may see and group them under the change they
/// answer, each group newest first by `occurred_at`, the report's `id` breaking
/// a tie in the order the history itself breaks one (descending).
///
/// `target` is the history's tenant `X`; `caller` the caller's own tenant.
///
/// # Errors
/// [`DomainError::Unavailable`] when the Tenant Resolver cannot answer for a
/// report's tenant.
pub async fn attach(
    hierarchy: &dyn TenantHierarchy,
    caller: Uuid,
    target: Uuid,
    reports: Vec<StoredAuditRecord>,
) -> Result<Attachments, DomainError> {
    let mut visibility = Visibility {
        hierarchy,
        caller,
        target,
        decided: HashMap::new(),
        standalone: HashMap::new(),
    };
    let mut by_change: HashMap<Uuid, Vec<StoredAuditRecord>> = HashMap::new();
    for report in reports {
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-4
        // A report without a tenant or without a change names nothing a
        // caller could be shown: dropped.
        let (Some(tenant), Some(change)) = (report.tenant_id, report.change_ref) else {
            continue;
        };
        if !visibility.keeps(tenant).await? {
            continue;
        }
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-4
        // @cpt-begin:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-5
        by_change.entry(change).or_default().push(report);
        // @cpt-end:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-5
    }
    // @cpt-begin:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-6
    for group in by_change.values_mut() {
        group.sort_by(|a, b| {
            b.occurred_at
                .cmp(&a.occurred_at)
                .then_with(|| b.id.cmp(&a.id))
        });
    }
    // @cpt-end:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-6
    // @cpt-begin:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-8
    Ok(Attachments { by_change })
    // @cpt-end:cpt-cf-settings-service-algo-activation-read-attach:p1:inst-ar-attach-8
}

#[cfg(test)]
#[path = "attachment_tests.rs"]
mod attachment_tests;
