//! The effective catalog (DESIGN §3.6 `cpt-cf-model-registry-seq-effective-catalog`).
//!
//! One algorithm serves resolution, the available list, and the management
//! reads: per slug the nearest candidate wins; the allow-list is the enabled
//! winners; a model is visible when its provider won and the model is own or
//! shared. Nothing here applies approval or PDP gates.

use std::collections::HashMap;

use model_registry_sdk::{
    ApprovalState, ModelEntryV1, ModelV1, ProviderEntryV1, ProviderV1, SharingMode,
};
use toolkit_macros::domain_model;
use uuid::Uuid;

/// The narrow provider projection the resolution paths read.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCandidate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub enabled: bool,
    pub sharing_mode: SharingMode,
}

impl From<&ProviderV1> for ProviderCandidate {
    fn from(p: &ProviderV1) -> Self {
        Self {
            id: p.id,
            tenant_id: p.tenant_id,
            slug: p.slug.clone(),
            enabled: p.enabled,
            sharing_mode: p.sharing_mode,
        }
    }
}

struct Ranked {
    candidate: ProviderCandidate,
    distance: usize,
}

/// The effective catalog of one tenant, computed per request.
#[domain_model]
pub struct Catalog {
    t0: Uuid,
    candidates: HashMap<Uuid, Ranked>,
    winners: HashMap<String, Uuid>,
}

impl Catalog {
    /// `chain` is `[T0, a1, …, ak]`. Rows outside the chain, and ancestor rows
    /// in `tenant` mode, are not candidates even if the caller passed them.
    #[must_use]
    pub fn build(chain: &[Uuid], rows: impl IntoIterator<Item = ProviderCandidate>) -> Self {
        let t0 = chain.first().copied().unwrap_or_default();
        let distance: HashMap<Uuid, usize> =
            chain
                .iter()
                .enumerate()
                .fold(HashMap::new(), |mut acc, (i, t)| {
                    acc.entry(*t).or_insert(i);
                    acc
                });
        let mut candidates = HashMap::new();
        let mut winners: HashMap<String, (Uuid, usize)> = HashMap::new();
        for candidate in rows {
            let Some(&d) = distance.get(&candidate.tenant_id) else {
                continue;
            };
            if d > 0 && candidate.sharing_mode != SharingMode::Shared {
                continue;
            }
            match winners.get(&candidate.slug) {
                Some((_, best)) if *best <= d => {}
                _ => {
                    winners.insert(candidate.slug.clone(), (candidate.id, d));
                }
            }
            candidates.insert(
                candidate.id,
                Ranked {
                    candidate,
                    distance: d,
                },
            );
        }
        Self {
            t0,
            candidates,
            winners: winners.into_iter().map(|(s, (id, _))| (s, id)).collect(),
        }
    }

    #[must_use]
    pub fn tenant(&self) -> Uuid {
        self.t0
    }

    #[must_use]
    pub fn winner(&self, slug: &str) -> Option<&ProviderCandidate> {
        self.winners
            .get(slug)
            .and_then(|id| self.candidates.get(id))
            .map(|r| &r.candidate)
    }

    #[must_use]
    pub fn candidate(&self, provider_id: Uuid) -> Option<&ProviderCandidate> {
        self.candidates.get(&provider_id).map(|r| &r.candidate)
    }

    #[must_use]
    pub fn is_winner(&self, provider_id: Uuid) -> bool {
        self.candidates
            .get(&provider_id)
            .is_some_and(|r| self.winners.get(&r.candidate.slug) == Some(&provider_id))
    }

    /// Enabled winners; the providers whose models can be evaluated.
    #[must_use]
    pub fn allow_list(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .winners
            .values()
            .copied()
            .filter(|id| self.candidates.get(id).is_some_and(|r| r.candidate.enabled))
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Ancestor candidates (shared), shadowed ones included.
    #[must_use]
    pub fn inherited_candidate_ids(&self) -> Vec<Uuid> {
        let mut ids: Vec<Uuid> = self
            .candidates
            .values()
            .filter(|r| r.distance > 0)
            .map(|r| r.candidate.id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Whether T0 sees the model at all: its provider is a winner and the
    /// model is own or shared.
    #[must_use]
    pub fn model_visible(&self, model: &ModelV1) -> bool {
        self.is_winner(model.provider_id)
            && (model.tenant_id == self.t0 || model.sharing_mode == SharingMode::Shared)
    }

    /// Whether the management reads return the model: own, or an inherited
    /// shared model of a (possibly shadowed) shared candidate provider.
    #[must_use]
    pub fn model_is_candidate(&self, model: &ModelV1) -> bool {
        if model.tenant_id == self.t0 {
            return true;
        }
        model.sharing_mode == SharingMode::Shared
            && self
                .candidates
                .get(&model.provider_id)
                .is_some_and(|r| r.distance > 0 && r.candidate.tenant_id == model.tenant_id)
    }

    /// Whether the management reads return the provider: own, or an inherited
    /// shared candidate.
    #[must_use]
    pub fn provider_is_candidate(&self, provider: &ProviderV1) -> bool {
        provider.tenant_id == self.t0 || self.candidates.contains_key(&provider.id)
    }

    #[must_use]
    pub fn provider_entry(&self, provider: ProviderV1) -> ProviderEntryV1 {
        let inherited = provider.tenant_id != self.t0;
        let winner = self.is_winner(provider.id);
        let available_for_eval = winner && provider.enabled;
        let mut entry = ProviderEntryV1::new(provider);
        entry.inherited = inherited;
        entry.shadowed = inherited && !winner;
        entry.available_for_eval = available_for_eval;
        entry
    }

    /// `None` when the model's provider is not a candidate, which the
    /// repository predicates rule out.
    #[must_use]
    pub fn model_entry(&self, model: ModelV1) -> Option<ModelEntryV1> {
        let provider = self.candidate(model.provider_id)?;
        let shadowed = !self.is_winner(model.provider_id);
        let approved = model.approval.state == ApprovalState::Approved;
        let inherited = model.tenant_id != self.t0;
        let mut entry = ModelEntryV1::new(model);
        entry.inherited = inherited;
        entry.shadowed = shadowed;
        entry.provider_enabled = provider.enabled;
        entry.available_for_eval = !shadowed && provider.enabled && approved;
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_gts::gts_id;

    fn candidate(tenant: Uuid, slug: &str, enabled: bool, mode: SharingMode) -> ProviderCandidate {
        ProviderCandidate {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            slug: slug.to_owned(),
            enabled,
            sharing_mode: mode,
        }
    }

    /// The hierarchy of DESIGN §3.6: Root → Partner → Customer → Team, and
    /// Root → Other partner. Returns (tenants, providers).
    struct Hierarchy {
        root: Uuid,
        partner: Uuid,
        other: Uuid,
        customer: Uuid,
        team: Uuid,
        root_openai: ProviderCandidate,
        partner_openai: ProviderCandidate,
        customer_openai: ProviderCandidate,
    }

    fn hierarchy(customer_enabled: bool) -> Hierarchy {
        let root = Uuid::new_v4();
        let partner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let customer = Uuid::new_v4();
        let team = Uuid::new_v4();
        Hierarchy {
            root,
            partner,
            other,
            customer,
            team,
            root_openai: candidate(root, "openai", true, SharingMode::Shared),
            partner_openai: candidate(partner, "openai", true, SharingMode::Tenant),
            customer_openai: candidate(customer, "openai", customer_enabled, SharingMode::Shared),
        }
    }

    impl Hierarchy {
        fn providers(&self) -> Vec<ProviderCandidate> {
            vec![
                self.root_openai.clone(),
                self.partner_openai.clone(),
                self.customer_openai.clone(),
            ]
        }
        fn catalog(&self, chain: &[Uuid]) -> Catalog {
            Catalog::build(chain, self.providers())
        }
    }

    #[test]
    fn winners_follow_the_design_table() {
        let h = hierarchy(true);
        let root = h.catalog(&[h.root]);
        assert_eq!(root.winner("openai").unwrap().id, h.root_openai.id);

        let partner = h.catalog(&[h.partner, h.root]);
        assert_eq!(partner.winner("openai").unwrap().id, h.partner_openai.id);
        assert!(!partner.is_winner(h.root_openai.id));

        let other = h.catalog(&[h.other, h.root]);
        assert_eq!(other.winner("openai").unwrap().id, h.root_openai.id);

        let customer = h.catalog(&[h.customer, h.partner, h.root]);
        assert_eq!(customer.winner("openai").unwrap().id, h.customer_openai.id);
        assert!(
            customer.candidate(h.partner_openai.id).is_none(),
            "tenant-mode ancestor provider is not a candidate"
        );

        let team = h.catalog(&[h.team, h.customer, h.partner, h.root]);
        assert_eq!(team.winner("openai").unwrap().id, h.customer_openai.id);
        assert_eq!(team.allow_list(), vec![h.customer_openai.id]);
        assert_eq!(team.inherited_candidate_ids().len(), 2);
    }

    #[test]
    fn disabled_shared_shadow_blocks_the_slug_in_the_subtree() {
        let h = hierarchy(false);
        for chain in [
            vec![h.customer, h.partner, h.root],
            vec![h.team, h.customer, h.partner, h.root],
        ] {
            let catalog = h.catalog(&chain);
            assert_eq!(catalog.winner("openai").unwrap().id, h.customer_openai.id);
            assert!(catalog.allow_list().is_empty());
            assert!(!catalog.is_winner(h.root_openai.id));
        }
    }

    #[test]
    fn own_provider_wins_in_tenant_mode_and_rows_outside_the_chain_are_ignored() {
        let t0 = Uuid::new_v4();
        let parent = Uuid::new_v4();
        let sibling = Uuid::new_v4();
        let own = candidate(t0, "x", true, SharingMode::Tenant);
        let parents = candidate(parent, "x", true, SharingMode::Shared);
        let siblings = candidate(sibling, "y", true, SharingMode::Shared);
        let catalog = Catalog::build(&[t0, parent], vec![own.clone(), parents.clone(), siblings]);
        assert_eq!(catalog.winner("x").unwrap().id, own.id);
        assert!(catalog.winner("y").is_none());
        assert_eq!(catalog.inherited_candidate_ids(), vec![parents.id]);
    }

    fn model(
        tenant: Uuid,
        provider: Uuid,
        canonical: &str,
        mode: SharingMode,
        state: ApprovalState,
    ) -> ModelV1 {
        use model_registry_sdk::{ApprovalV1, ContextWindow, ModelInfoV1};
        let now = chrono::Utc::now();
        let name = canonical.split("::").nth(1).unwrap_or("m");
        let info = ModelInfoV1::new(
            gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")),
            name,
            "m",
            ContextWindow::default(),
        );
        let mut m = ModelV1::new(
            Uuid::new_v4(),
            tenant,
            provider,
            name,
            canonical,
            info,
            Uuid::nil(),
            now,
        );
        m.sharing_mode = mode;
        m.approval = ApprovalV1::new(state, Uuid::nil(), now);
        m
    }

    /// Root's `gpt-4o-mini` must stay invisible to Customer although Customer
    /// owns no model with that name: filtering is by provider id, never by
    /// canonical id dedupe.
    #[test]
    fn model_visibility_is_keyed_on_provider_id() {
        let h = hierarchy(true);
        let customer = h.catalog(&[h.customer, h.partner, h.root]);
        let root_mini = model(
            h.root,
            h.root_openai.id,
            "openai::gpt-4o-mini",
            SharingMode::Shared,
            ApprovalState::Approved,
        );
        assert!(!customer.model_visible(&root_mini));
        let entry = customer.model_entry(root_mini).unwrap();
        assert!(entry.inherited && entry.shadowed && !entry.available_for_eval);

        let own = model(
            h.customer,
            h.customer_openai.id,
            "openai::gpt-4o",
            SharingMode::Tenant,
            ApprovalState::Approved,
        );
        assert!(customer.model_visible(&own));
        let entry = customer.model_entry(own).unwrap();
        assert!(
            !entry.inherited
                && !entry.shadowed
                && entry.provider_enabled
                && entry.available_for_eval
        );

        let other = h.catalog(&[h.other, h.root]);
        let root_private = model(
            h.root,
            h.root_openai.id,
            "openai::o1-preview",
            SharingMode::Tenant,
            ApprovalState::Approved,
        );
        assert!(!other.model_visible(&root_private));
        assert!(!other.model_is_candidate(&root_private));
        let root_shared_pending = model(
            h.root,
            h.root_openai.id,
            "openai::gpt-4o",
            SharingMode::Shared,
            ApprovalState::Pending,
        );
        assert!(other.model_visible(&root_shared_pending));
        assert!(other.model_is_candidate(&root_shared_pending));
        let entry = other.model_entry(root_shared_pending).unwrap();
        assert!(entry.inherited && !entry.shadowed && !entry.available_for_eval);
    }

    #[test]
    fn provider_entry_flags() {
        let h = hierarchy(false);
        let team = h.catalog(&[h.team, h.customer, h.partner, h.root]);
        let now = chrono::Utc::now();
        let mk = |c: &ProviderCandidate| {
            let mut p = ProviderV1::new(
                c.id,
                c.tenant_id,
                c.slug.clone(),
                c.slug.clone(),
                gts::GtsTypeId::new(gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~")),
                Uuid::nil(),
                now,
            );
            p.enabled = c.enabled;
            p.sharing_mode = c.sharing_mode;
            p
        };
        let e = team.provider_entry(mk(&h.customer_openai));
        assert!(e.inherited && !e.shadowed && !e.available_for_eval);
        let e = team.provider_entry(mk(&h.root_openai));
        assert!(e.inherited && e.shadowed && !e.available_for_eval);
        assert!(!team.provider_is_candidate(&mk(&h.partner_openai)));
    }
}
