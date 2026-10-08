//! Detached semantic observations and transaction-local checks of their entry identities.
use crate::{
    api::rest::authoring::support::{self, DoorError},
    domain::usage_policy::validate_meter_policy,
    infra::{
        reference_registry,
        storage::{
            entity::{plan_item, price, price_book_entry},
            repo::{plan_item_repo, price_book_entry_repo, price_repo, usage_policy_repo},
        },
        usage_policy_wire::{MeterEvidence, UsageRatingPolicyInput},
    },
};
use bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1;
use bss_products_sdk::models::Sku;
use std::collections::BTreeMap;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{
    DbConn,
    secure::{AccessScope, DBRunner},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A provider 5xx other than 500 is an outage: the generic 503. A 500 is a permanent fault and is forwarded.
/// 400 and 403 are forwarded unchanged.
fn provider_outage(error: CanonicalError) -> CanonicalError {
    let status = error.status_code();
    if (500..600).contains(&status) && status != 500 {
        tracing::error!(status, error = %error, "UsageMeterSemanticsV1 provider outage");
        CanonicalError::service_unavailable().create()
    } else {
        error
    }
}

/// Resolve a provider or report E1 unconfigured. No production default is supplied.
/// # Errors
/// Unconfigured dependency, configured outage or the provider's definite refusal.
pub async fn resolve(
    hub: &toolkit::ClientHub,
    ctx: &SecurityContext,
    input: &UsageRatingPolicyInput,
    sku: &Sku,
) -> Result<MeterEvidence, CanonicalError> {
    let provider = hub.get::<dyn UsageMeterSemanticsV1>().map_err(|_| {
        CanonicalError::from(bss_pricing_sdk::meter_semantics::UnconfiguredMeterSemantics)
    })?;
    let content = bss_pricing_sdk::terms::UsageRatingPolicyInput::from(input);
    let usage_type_ref = sku.usage_type_ref.as_deref().unwrap_or("");
    if usage_type_ref.trim().is_empty() {
        return Err(support::invalid(
            "usage_rating_policy",
            "UNCONFIGURED_DEPENDENCY",
        ));
    }
    let evidence = provider
        .resolve(ctx, crate::domain::usage_policy::meter_ref(usage_type_ref))
        .await
        .map_err(provider_outage)?;
    validate(&content, sku, &evidence)?;
    Ok(evidence.into())
}
/// Verify SKU identity and complete immutable evidence without dependency calls.
/// # Errors
/// A mismatching SKU or declaration is a typed input refusal.
pub fn validate(
    policy: &bss_pricing_sdk::terms::UsageRatingPolicyInput,
    sku: &Sku,
    evidence: &bss_pricing_sdk::meter_semantics::MeterSemantics,
) -> Result<(), CanonicalError> {
    let sku_ref = sku.usage_type_ref.as_deref().unwrap_or("");
    validate_meter_policy(
        policy,
        sku_ref,
        sku.unit.as_deref().unwrap_or_default(),
        evidence,
    )
    .map_err(|e| support::invalid("usage_rating_policy", e.code))
}
/// One entry observation. The transaction must still hold precisely this entry generation.
#[derive(Clone)]
pub struct ObservedEntry {
    entry: price_book_entry::Model,
    pub policy: super::usage_policy_wire::UsageRatingPolicy,
    pub evidence: MeterEvidence,
}
/// Observations are request-local and never used by historical reads.
#[derive(Clone, Default)]
pub struct Observations {
    local_entries: Vec<price_book_entry::Model>,
    pub(crate) selection: Selection,
    entries: BTreeMap<Uuid, Result<ObservedEntry, CanonicalError>>,
    skus: BTreeMap<Uuid, Result<Sku, CanonicalError>>,
    versions: BTreeMap<Uuid, Result<Vec<bss_products_sdk::models::SkuVersion>, CanonicalError>>,
}
/// The local selection that supplied a detached capture, including removals and re-pointing.
#[derive(Clone, Default)]
pub enum Selection {
    #[default]
    None,
    Revision {
        id: Uuid,
        items: Vec<plan_item::Model>,
    },
    Prices {
        ids: Vec<Uuid>,
        scope: AccessScope,
        rows: Vec<price::Model>,
    },
    Publish {
        book: Uuid,
        ids: Option<Vec<Uuid>>,
        rows: Vec<price::Model>,
    },
}
impl Selection {
    /// Read only the local selection; no provider call belongs in this check.
    async fn matches(&self, conn: &impl DBRunner, tenant: Uuid) -> Result<bool, DoorError> {
        let children = AccessScope::for_tenant(tenant);
        match self {
            Self::None => Ok(true),
            Self::Revision { id, items } => {
                let current = plan_item_repo::for_revision(conn, &children, tenant, *id).await?;
                let identity = |rows: &[plan_item::Model]| {
                    rows.iter()
                        .map(|i| (i.id, (i.sku_id, i.price_book_entry_id)))
                        .collect::<BTreeMap<_, _>>()
                };
                Ok(identity(items) == identity(&current))
            }
            Self::Prices { ids, scope, rows } => {
                let current = price_repo::find_many(conn, scope, tenant, ids).await?;
                Ok(price_selection(rows) == price_selection(&current))
            }
            Self::Publish { book, ids, rows } => {
                let entries =
                    price_book_entry_repo::for_book(conn, &children, tenant, *book).await?;
                let current = price_repo::for_entries(
                    conn,
                    &children,
                    tenant,
                    &entries.iter().map(|e| e.id).collect::<Vec<_>>(),
                )
                .await?;
                Ok(price_selection(rows)
                    == price_selection(&publish_selection(current, ids.as_deref())))
            }
        }
    }
}
/// Match the publication door's implicit draft selection, or its explicit draft IDs.
pub(crate) fn publish_selection(
    rows: Vec<price::Model>,
    ids: Option<&[Uuid]>,
) -> Vec<price::Model> {
    rows.into_iter()
        .filter(|p| {
            p.state == "draft"
                && p.pending_unit_id.is_none()
                && ids.is_none_or(|ids| ids.contains(&p.id))
        })
        .collect()
}
/// Price identity includes the paired selection but excludes money judged by the transaction.
fn price_selection(rows: &[price::Model]) -> BTreeMap<Uuid, (Uuid, Option<Uuid>)> {
    rows.iter()
        .filter(|p| p.state == "draft" || p.state == "pending")
        .map(|p| (p.id, (p.price_book_entry_id, p.paired_price_id)))
        .collect()
}
impl Observations {
    /// Read local policy rows, then capture dependency results outside a transaction. Errors
    /// remain typed observations until submit/apply; non-final votes and rejects need no gate.
    /// # Errors
    /// Local storage failures; dependency failures are retained for the subject's gate.
    pub async fn capture(
        conn: &DbConn<'_>,
        hub: &toolkit::ClientHub,
        ctx: &SecurityContext,
        entries: Vec<price_book_entry::Model>,
        extra_skus: Vec<Uuid>,
        selection: Selection,
    ) -> Result<Self, DoorError> {
        let policies =
            usage_policy_repo::for_entries(conn, ctx.subject_tenant_id(), &entries).await?;
        let mut result = Self {
            local_entries: entries.clone(),
            selection,
            entries: BTreeMap::new(),
            skus: BTreeMap::new(),
            versions: BTreeMap::new(),
        };
        let wanted: Vec<Uuid> = entries
            .iter()
            .map(|e| e.sku_id)
            .chain(extra_skus)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let read = match reference_registry::resolve(hub) {
            Ok(registry) => registry
                .skus_for_write(ctx, ctx.subject_tenant_id(), &wanted)
                .await
                .map_err(|e| {
                    if crate::infra::reference_work::definite_refusal(&e) {
                        e
                    } else {
                        support::registry_unavailable(&e)
                    }
                }),
            Err(e) => Err(support::registry_unavailable(&e)),
        };
        match read {
            Ok(found) => {
                let by_id: BTreeMap<_, _> = found.into_iter().map(|sku| (sku.id, sku)).collect();
                for id in wanted {
                    result.skus.insert(
                        id,
                        match by_id.get(&id).cloned() {
                            Some(sku) => Ok(sku),
                            None => Err(support::missing_what("sku")),
                        },
                    );
                }
            }
            Err(error) => {
                for id in wanted {
                    result.skus.insert(id, Err(error.clone()));
                }
            }
        }
        for entry in entries.into_iter().filter(|e| e.charge_kind == "usage") {
            if let std::collections::btree_map::Entry::Vacant(slot) =
                result.versions.entry(entry.sku_id)
            {
                slot.insert(history(hub, ctx, entry.sku_id).await);
            }
            let observed = async {
                let policy = policies.get(&entry.id).ok_or_else(|| {
                    support::invalid("usage_rating_policy", "MISSING_RATING_POLICY")
                })?;
                let sku = result
                    .skus
                    .get(&entry.sku_id)
                    .ok_or_else(|| support::conflict(support::UNIT_CONTENDED))?
                    .as_ref()
                    .map_err(Clone::clone)?;
                let evidence = resolve(hub, ctx, &policy.content, sku).await?;
                Ok(ObservedEntry {
                    entry: entry.clone(),
                    policy: policy.clone(),
                    evidence,
                })
            }
            .await;
            result.entries.insert(entry.id, observed);
        }
        Ok(result)
    }
    /// Compare the entire local selection and every captured entry before consuming any
    /// provider result. Local drift is a typed rollback signal, never an evidence conflict.
    /// # Errors
    /// Storage failure or a selection that must be captured again outside the transaction.
    pub async fn check_local(&self, conn: &impl DBRunner, tenant: Uuid) -> Result<(), DoorError> {
        if !self.selection.matches(conn, tenant).await? {
            return Err(DoorError::SelectionMoved);
        }
        let current = price_book_entry_repo::find_many(
            conn,
            &AccessScope::for_tenant(tenant),
            tenant,
            &self.local_entries.iter().map(|e| e.id).collect::<Vec<_>>(),
        )
        .await?;
        let captured = self
            .local_entries
            .iter()
            .map(|e| (e.id, e))
            .collect::<BTreeMap<_, _>>();
        let current = current
            .iter()
            .map(|e| (e.id, e))
            .collect::<BTreeMap<_, _>>();
        if current != captured {
            return Err(DoorError::SelectionMoved);
        }
        Ok(())
    }
    /// Recheck local identity before using detached evidence in submit or apply.
    /// # Errors
    /// Missing policy, dependency failure or changed local identity refuses publication.
    pub fn check(&self, entry: &price_book_entry::Model) -> Result<(), CanonicalError> {
        if entry.charge_kind != "usage" {
            return Ok(());
        }
        if entry.usage_policy_id.is_none() {
            return Err(support::invalid(
                "usage_rating_policy",
                "MISSING_RATING_POLICY",
            ));
        }
        let observed = self
            .entries
            .get(&entry.id)
            .ok_or_else(|| support::conflict(support::UNIT_CONTENDED))?
            .as_ref()
            .map_err(Clone::clone)?;
        if &observed.entry != entry {
            return Err(support::conflict(support::UNIT_CONTENDED));
        }
        Ok(())
    }
    /// Fresh caller-authorized SKU observations, consumed without a dependency call.
    /// # Errors
    /// The original dependency refusal, or missing evidence after a local selection change.
    pub fn skus(&self, ids: impl IntoIterator<Item = Uuid>) -> Result<Vec<Sku>, CanonicalError> {
        ids.into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|id| {
                self.skus
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| Err(support::conflict(support::UNIT_CONTENDED)))
            })
            .collect()
    }
    /// Dated metering from immutable history captured before the transaction.
    /// # Errors
    /// Original provider refusal, or missing capture for a changed entry.
    pub fn metering(
        &self,
        sku: Uuid,
        on: time::Date,
    ) -> Result<crate::domain::price::SkuMetering, CanonicalError> {
        let versions = self
            .versions
            .get(&sku)
            .ok_or_else(|| support::conflict(support::UNIT_CONTENDED))?
            .as_ref()
            .map_err(Clone::clone)?;
        let version = versions
            .iter()
            .find(|v| v.effective_from <= on)
            .or_else(|| versions.last());
        Ok(crate::domain::price::SkuMetering {
            unit: version.and_then(|v| v.content.unit.clone()),
            usage_type_ref: version.and_then(|v| v.content.usage_type_ref.clone()),
        })
    }
    /// Provider evidence retained outside the fingerprinted business content.
    #[must_use]
    pub fn audit(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.entries
                .iter()
                .filter_map(|(id, e)| {
                    e.as_ref().ok().map(|e| {
                        serde_json::json!({
                            "price_book_entry_id":id, "policy_id":e.policy.policy_id,
                            "policy_version":e.policy.version, "policy_digest":e.policy.digest,
                            "usage_sku_version": e.entry.usage_sku_version,
                            "rules": e.policy.content,
                            "meter_evidence":e.evidence
                        })
                    })
                })
                .collect(),
        )
    }
}
/// Dated SKU versions for the chain guard's unit. One walk per distinct SKU, not per entry.
///
/// The meter itself is not learned here. Capture reads the head once, in the `skus_for_write`
/// batch, and P-D-232 pins that meter for every revision. The walk remains because
/// `Observations::metering` still supplies the dated unit to the price chain guard.
async fn history(
    hub: &toolkit::ClientHub,
    ctx: &SecurityContext,
    sku: Uuid,
) -> Result<Vec<bss_products_sdk::models::SkuVersion>, CanonicalError> {
    let registry =
        reference_registry::resolve(hub).map_err(|e| support::registry_unavailable(&e))?;
    let mut result = Vec::new();
    let mut on = time::Date::MAX;
    while let Some(version) = registry
        .sku_version_as_of(ctx, ctx.subject_tenant_id(), sku, on)
        .await?
    {
        let previous = version.effective_from.previous_day();
        if version.effective_from > on {
            return Err(CanonicalError::internal("invalid SKU version history").create());
        }
        result.push(version);
        let Some(previous) = previous else {
            break;
        };
        on = previous;
    }
    Ok(result)
}
