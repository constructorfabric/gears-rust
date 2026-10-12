//! P-D-262: the actor ids every read shows, and their `*_name` siblings.
//!
//! A read builds its answer from its statements, then names the actors of the whole answer with
//! one `ActorNames::fill`: no statement is added, and no transaction waits on Account Management.
//! A write answer may be stored as its key's receipt, and a name is never stored, so no write
//! answer names anyone: its `*_name` fields stay null.
use super::dto::{
    DecisionDto, ProductsCategoryDto, ProductsCategoryItem, ProductsDerivedUsageType,
    ProductsDerivedUsageTypeItem, ProductsDerivedUsageTypeVersion, ProductsDerivedVersionHeader,
    ProductsSkuHistoryEntry, SkuCard, SkuDto, SkuListItem, UnitDto, UnitList,
};
use bss_rest::actor_names::{ActorFields, Names, label};
use serde_json::Value;
use uuid::Uuid;

bss_rest::actor_fields!(SkuListItem {}[sku]);
bss_rest::actor_fields!(SkuCard {}[sku]);
bss_rest::actor_fields!(ProductsSkuHistoryEntry { actor => actor_name } []);
bss_rest::actor_fields!(DecisionDto { actor => actor_name } []);
bss_rest::actor_fields!(UnitList {}[items]);
bss_rest::actor_fields!(ProductsDerivedUsageTypeVersion { created_by => created_by_name } []);
bss_rest::actor_fields!(ProductsDerivedVersionHeader { created_by => created_by_name } []);
bss_rest::actor_fields!(ProductsDerivedUsageType { created_by => created_by_name } [versions]);
bss_rest::actor_fields!(ProductsDerivedUsageTypeItem { created_by => created_by_name } [latest]);

/// A SKU names its creator, and the actor who archived it while it is archived (P-D-263).
impl ActorFields for SkuDto {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.push(self.created_by);
        ids.extend(self.archived_by);
    }
    fn fill_names(&mut self, names: &Names) {
        self.created_by_name = label(names, self.created_by);
        self.archived_by_name = self.archived_by.and_then(|id| label(names, id));
    }
}

/// A category names the actor who archived it while it is archived (P-D-263).
impl ActorFields for ProductsCategoryDto {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.extend(self.archived_by);
    }
    fn fill_names(&mut self, names: &Names) {
        self.archived_by_name = self.archived_by.and_then(|id| label(names, id));
    }
}
bss_rest::actor_fields!(ProductsCategoryItem {}[category]);

/// A unit names its submitter and its voters, and the creator of the live SKU its card carries
/// (`impact_live`, a [`SkuDto`] as JSON).
impl ActorFields for UnitDto {
    fn actor_ids(&self, ids: &mut Vec<Uuid>) {
        ids.push(self.submitted_by);
        self.decisions.actor_ids(ids);
        ids.extend(live_actor(self.impact_live.as_ref(), "created_by"));
        ids.extend(live_actor(self.impact_live.as_ref(), "archived_by"));
    }
    fn fill_names(&mut self, names: &Names) {
        self.submitted_by_name = label(names, self.submitted_by);
        self.decisions.fill_names(names);
        for (field, name) in [
            ("created_by", "created_by_name"),
            ("archived_by", "archived_by_name"),
        ] {
            if let Some(id) = live_actor(self.impact_live.as_ref(), field)
                && let Some(Value::Object(live)) = self.impact_live.as_mut()
            {
                live.insert(
                    name.to_owned(),
                    label(names, id).map_or(Value::Null, Value::String),
                );
            }
        }
    }
}

/// An actor id (`created_by`, or `archived_by` while archived, P-D-263) of the live SKU a card
/// carries as JSON.
fn live_actor(live: Option<&Value>, field: &str) -> Option<Uuid> {
    live?.get(field)?.as_str()?.parse().ok()
}
