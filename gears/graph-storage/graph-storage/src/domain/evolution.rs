//! Type-schema evolution: what change to a registered type the gear admits.
//!
//! Pure logic over schemas, like [`super::ontology`] — no I/O, so the rule can
//! be read and tested in one place. The relation itself is not implemented
//! here: `gts::schema_evolution` ships it as **OP#8** (spec sec 4.2), and this
//! module wraps that with the gear's policy.
//!
//! The policy is not the gear's invention either. types-registry ADR-0003
//! fixes the direction (`BACKWARD`), the baseline (the entity's *current*
//! revision, never its history) and the posture (an undecidable check is a
//! refusal — the registry fails closed); ADR-0004 says a major-only GTS id
//! names a mutable logical entity and that *"a content update that is backward
//! compatible under ADR-0003 preserves the same GTS ID"*. The gear's
//! `gts_type` table is documented as a cache of that registry, and a cache
//! that refuses what its authority accepts is a bug in the cache.
//!
//! One thing is the gear's own, and is deliberately a *second* ground rather
//! than a relaxation of the first: the gear holds the data. When the schemas
//! cannot prove inclusion, it can still ask whether every stored row of the
//! type validates against the candidate. That admits a change for *these
//! rows*, which is a weaker claim than the schema-proved one and is reported
//! as such ([`Basis::DataBacked`]).
//!
//! **Measured on the Studio domain model (2026-09-10).** With the payload
//! object left open — as the exporter emits it — adding one optional property
//! is `Incompatible` for 188 of 188 node types, because declaring a property
//! at an open level *narrows* the accepted set (gts sec 4.4: the old
//! definition accepted any value under that name). With the payload
//! materialized and closed at the leaf it is `Compatible` for 188 of 188.
//! Nothing came back `Unknown`. That is why the closed-envelope shape matters
//! to the exporter, and why re-validation exists for everything not emitted
//! that way.

use graph_storage_sdk::models::{
    EffectiveTraits, SchemaDiagnostic, TraitChange, TypeChangeState,
};
use gts::schema_evolution::CompatibilityVerdict;
use gts::store::GtsStore;
use serde_json::Value;

use crate::domain::error::DomainError;

/// Both directional verdicts of one comparison, with the evidence for the one
/// that gates admission.
#[derive(Clone, Debug)]
pub struct Comparison {
    /// `Valid(old) ⊆ Valid(new)` — the direction ADR-0003 enforces.
    pub backward: CompatibilityVerdict,
    /// `Valid(new) ⊆ Valid(old)` — computed and reported, never enforced.
    pub forward: CompatibilityVerdict,
    /// Why `backward` does not hold, each entry naming its schema location.
    pub diagnostics: Vec<SchemaDiagnostic>,
    /// Candidate object levels a later definition cannot extend in place.
    pub levels_not_evolvable_in_place: Vec<String>,
}

impl Comparison {
    /// The candidate's standing, in the vocabulary the API reports.
    #[must_use]
    pub fn state(&self) -> TypeChangeState {
        match self.backward {
            CompatibilityVerdict::Compatible => TypeChangeState::Compatible,
            CompatibilityVerdict::Incompatible => TypeChangeState::Incompatible,
            CompatibilityVerdict::Unknown => TypeChangeState::Undecidable,
        }
    }
}

/// What the gear does with a candidate, given the request's options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Admit without reading a row: the schemas prove inclusion.
    Accept,
    /// Admit only if every live row of the type validates against the
    /// candidate.
    Revalidate,
    /// Refuse. The caller gets the diagnostics.
    Refuse,
}

/// Apply the rule to one comparison.
///
/// `update` is `options.on_existing == Update`, `revalidate` is
/// `options.revalidate`. Kept as two booleans rather than the options struct
/// so this stays a function of the decision's actual inputs.
#[must_use]
pub fn decide(state: TypeChangeState, update: bool, revalidate: bool) -> Decision {
    match state {
        TypeChangeState::New | TypeChangeState::Unchanged => Decision::Accept,
        _ if !update => Decision::Refuse,
        TypeChangeState::Compatible => Decision::Accept,
        // ADR-0003 fails closed on both of these from the schemas alone. The
        // gear's own data is the only thing that can say more.
        TypeChangeState::Incompatible | TypeChangeState::Undecidable => {
            if revalidate {
                Decision::Revalidate
            } else {
                Decision::Refuse
            }
        }
    }
}

/// Compare the registered definition against the candidate.
///
/// `chain` carries every ancestor definition, so both documents resolve their
/// `gts://` references. Both sides resolve against the **same** ancestor set —
/// the one in force in this transaction — so the comparison reports this
/// type's own change and nothing else. That is sound rather than convenient:
/// composition is an intersection of accepted sets, and inclusion is monotone
/// under intersection, so per-type verdicts compose (an ancestor's own change
/// is decided on the ancestor's own row).
pub fn compare(
    old: &Value,
    new: &Value,
    chain: impl IntoIterator<Item = (String, Value)>,
) -> Result<Comparison, DomainError> {
    let mut store = GtsStore::new();
    for (type_id, schema) in chain {
        store.register_schema(&type_id, &schema).map_err(|error| {
            DomainError::internal(format!(
                "ancestor `{type_id}` is not a usable schema reference: {error}"
            ))
        })?;
    }
    let comparison = store.compare_documents(old, new).map_err(|error| {
        DomainError::invalid(format!(
            "the two definitions cannot be compared: {error}"
        ))
    })?;

    Ok(Comparison {
        backward: comparison.backward_compatibility(),
        forward: comparison.forward_compatibility(),
        diagnostics: comparison
            .backward_diagnostics
            .iter()
            .map(|diagnostic| SchemaDiagnostic {
                location: diagnostic.path.clone(),
                finding: finding_name(diagnostic.finding),
                message: diagnostic.detail.clone(),
            })
            .collect(),
        levels_not_evolvable_in_place: comparison
            .levels_not_evolvable_in_place()
            .into_iter()
            .map(|level| level.path.clone())
            .collect(),
    })
}

/// `gts`'s finding kind as the snake-case token the API publishes.
///
/// Spelled out rather than derived from `Debug`: this crosses the public
/// boundary, so a rename upstream must fail to compile here rather than
/// silently change a value clients match on.
fn finding_name(finding: gts::schema_evolution::CompatibilityFinding) -> String {
    use gts::schema_evolution::CompatibilityFinding as F;
    match finding {
        F::PropertyAdded => "property_added",
        F::PropertyRemoved => "property_removed",
        F::RequiredChanged => "required_changed",
        F::ContentModelChanged => "content_model_changed",
        F::TypeChanged => "type_changed",
        F::EnumChanged => "enum_changed",
        F::BoundChanged => "bound_changed",
        F::NarrowingConstraintChanged => "narrowing_constraint_changed",
        F::ConstraintChanged => "constraint_changed",
        F::DialectChanged => "dialect_changed",
        F::NotProvable => "not_provable",
    }
    .to_owned()
}

/// One line per trait whose declared paths moved.
///
/// Traits are not schema, so the compatibility relation says nothing about
/// them — but they decide what is filterable, what is searched and what is
/// embedded, so a caller has to be told when they change even if the schemas
/// are identical.
#[must_use]
pub fn traits_diff(old: &EffectiveTraits, new: &EffectiveTraits) -> Vec<TraitChange> {
    let mut changes = Vec::new();
    let mut compare_lists = |name: &str, old: &[String], new: &[String]| {
        let added: Vec<String> = new
            .iter()
            .filter(|path| !old.contains(path))
            .cloned()
            .collect();
        let removed: Vec<String> = old
            .iter()
            .filter(|path| !new.contains(path))
            .cloned()
            .collect();
        if !added.is_empty() || !removed.is_empty() {
            changes.push(TraitChange {
                trait_name: name.to_owned(),
                added,
                removed,
            });
        }
    };
    compare_lists("index", &old.index, &new.index);
    compare_lists(
        "full_text_search",
        &old.full_text_search,
        &new.full_text_search,
    );
    compare_lists("vector_search", &old.vector_search, &new.vector_search);
    compare_lists("src_types", &old.src_types, &new.src_types);
    compare_lists("dst_types", &old.dst_types, &new.dst_types);
    if old.family != new.family {
        changes.push(TraitChange {
            trait_name: "family".to_owned(),
            added: new.family.clone().into_iter().collect(),
            removed: old.family.clone().into_iter().collect(),
        });
    }
    changes
}

/// Which traits, if changed, oblige the store to touch stored rows.
///
/// `full_text_search` decides the lexical text, `vector_search` the embedding
/// input; both are materialized per row, so a changed declaration leaves every
/// existing row describing itself by the old rule until it is recomputed.
/// `index` needs nothing: the projection reads the payload through the
/// declared path at query time.
#[must_use]
pub fn recompute_needed(changes: &[TraitChange]) -> (bool, bool) {
    let touched = |name: &str| changes.iter().any(|change| change.trait_name == name);
    (touched("full_text_search"), touched("vector_search"))
}

/// The one-line reason a refusal carries, naming up to `limit` locations.
///
/// The structured report is what `POST /types/compatibility` is for; a
/// conflict on the write path still has to say *where*, because a caller who
/// only sees "incompatible" has nowhere to go.
#[must_use]
pub fn refusal_reason(
    type_id: &str,
    state: TypeChangeState,
    diagnostics: &[SchemaDiagnostic],
    limit: usize,
) -> String {
    let lead = match state {
        TypeChangeState::Undecidable => format!(
            "type `{type_id}` is registered and the candidate cannot be proven backward \
             compatible with it"
        ),
        _ => format!(
            "type `{type_id}` is registered and the candidate is not backward compatible \
             with it"
        ),
    };
    let mut listed: Vec<String> = diagnostics
        .iter()
        .take(limit)
        .map(|diagnostic| format!("{} {}", diagnostic.location, diagnostic.message))
        .collect();
    if diagnostics.len() > limit {
        listed.push(format!("and {} more", diagnostics.len() - limit));
    }
    if listed.is_empty() {
        return lead;
    }
    format!("{lead}: {}", listed.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A leaf that derives from `parent` and declares `payload` with the given
    /// properties, closed or open.
    fn leaf(properties: &Value, closed: bool, required: &Value) -> Value {
        let mut payload = json!({ "type": "object", "properties": properties.clone() });
        if closed {
            payload["additionalProperties"] = json!(false);
        }
        if required != &json!([]) {
            payload["required"] = required.clone();
        }
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "allOf": [
                { "$ref": "gts://gts.test.gs._.evolution_base.v1~" },
                { "type": "object", "properties": { "payload": payload } }
            ]
        })
    }

    fn chain() -> Vec<(String, Value)> {
        vec![(
            "gts.test.gs._.evolution_base.v1~".to_owned(),
            json!({
                "$id": "gts://gts.test.gs._.evolution_base.v1~",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "node_key": { "type": "string" }, "payload": { "type": "object" } }
            }),
        )]
    }

    fn state_of(old: &Value, new: &Value) -> TypeChangeState {
        compare(old, new, chain())
            .expect("the chain resolves")
            .state()
    }

    #[test]
    fn an_added_optional_property_is_compatible_at_a_closed_level() {
        let old = leaf(&json!({ "key": { "type": "string" } }), true, &json!(["key"]));
        let new = leaf(
            &json!({ "key": { "type": "string" }, "owner": { "type": "string" } }),
            true,
            &json!(["key"]),
        );
        assert_eq!(state_of(&old, &new), TypeChangeState::Compatible);
    }

    /// The finding that decides the exporter's shape: at an open level the old
    /// definition already accepted *any* value under the new name, so
    /// declaring it narrows the accepted set. Measured across the whole Studio
    /// model, this is 188 of 188 node types (see the module docs).
    #[test]
    fn the_same_property_added_at_an_open_level_is_incompatible() {
        let old = leaf(&json!({ "key": { "type": "string" } }), false, &json!(["key"]));
        let new = leaf(
            &json!({ "key": { "type": "string" }, "owner": { "type": "string" } }),
            false,
            &json!(["key"]),
        );
        let comparison = compare(&old, &new, chain()).expect("the chain resolves");
        assert_eq!(comparison.state(), TypeChangeState::Incompatible);
        let diagnostic = comparison
            .diagnostics
            .first()
            .expect("an incompatible verdict must carry its evidence");
        assert_eq!(diagnostic.location, "$.payload");
        assert_eq!(diagnostic.finding, "property_added");
    }

    #[test]
    fn a_widened_enum_is_compatible_and_a_narrowed_one_is_not() {
        let with = |values: Value| {
            leaf(
                &json!({ "status": { "type": "string", "enum": values } }),
                true,
                &json!([]),
            )
        };
        let narrow = with(json!(["proposed", "approved"]));
        let wide = with(json!(["proposed", "approved", "blocked"]));
        assert_eq!(state_of(&narrow, &wide), TypeChangeState::Compatible);
        assert_eq!(state_of(&wide, &narrow), TypeChangeState::Incompatible);
    }

    #[test]
    fn a_renamed_property_is_incompatible_in_both_shapes() {
        for closed in [true, false] {
            let old = leaf(&json!({ "priority": { "type": "string" } }), closed, &json!([]));
            let new = leaf(&json!({ "urgency": { "type": "string" } }), closed, &json!([]));
            assert_eq!(
                state_of(&old, &new),
                TypeChangeState::Incompatible,
                "closed={closed}"
            );
        }
    }

    #[test]
    fn a_new_required_property_is_incompatible() {
        let old = leaf(&json!({ "key": { "type": "string" } }), true, &json!(["key"]));
        let new = leaf(
            &json!({ "key": { "type": "string" }, "owner": { "type": "string" } }),
            true,
            &json!(["key", "owner"]),
        );
        let comparison = compare(&old, &new, chain()).expect("the chain resolves");
        assert_eq!(comparison.state(), TypeChangeState::Incompatible);
        assert!(
            comparison
                .diagnostics
                .iter()
                .any(|d| d.finding == "required_changed"),
            "{:?}",
            comparison.diagnostics
        );
    }

    /// Forward compatibility is reported, not enforced: adding an optional
    /// property is exactly the case where the two directions disagree, and a
    /// producer needs to hear it.
    #[test]
    fn forward_is_reported_separately() {
        let old = leaf(&json!({ "key": { "type": "string" } }), true, &json!([]));
        let new = leaf(
            &json!({ "key": { "type": "string" }, "owner": { "type": "string" } }),
            true,
            &json!([]),
        );
        let comparison = compare(&old, &new, chain()).expect("the chain resolves");
        assert!(comparison.backward.is_compatible());
        assert!(comparison.forward.is_incompatible());
    }

    #[test]
    fn the_rule_refuses_everything_it_cannot_prove_unless_asked_to_revalidate() {
        for state in [TypeChangeState::Incompatible, TypeChangeState::Undecidable] {
            assert_eq!(decide(state, false, false), Decision::Refuse);
            assert_eq!(decide(state, true, false), Decision::Refuse);
            assert_eq!(decide(state, true, true), Decision::Revalidate);
        }
        assert_eq!(
            decide(TypeChangeState::Compatible, true, false),
            Decision::Accept
        );
        // Reject mode is today's behaviour: even a provably compatible change
        // is a conflict, so no existing caller changes.
        assert_eq!(
            decide(TypeChangeState::Compatible, false, false),
            Decision::Refuse
        );
        assert_eq!(
            decide(TypeChangeState::Unchanged, false, false),
            Decision::Accept
        );
    }

    #[test]
    fn a_trait_diff_names_the_paths_that_moved() {
        let old = EffectiveTraits {
            index: vec!["/payload/priority".to_owned()],
            full_text_search: vec!["/name".to_owned()],
            ..EffectiveTraits::default()
        };
        let new = EffectiveTraits {
            index: vec!["/payload/urgency".to_owned()],
            full_text_search: vec!["/name".to_owned()],
            ..EffectiveTraits::default()
        };
        let changes = traits_diff(&old, &new);
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert_eq!(changes[0].trait_name, "index");
        assert_eq!(changes[0].added, vec!["/payload/urgency".to_owned()]);
        assert_eq!(changes[0].removed, vec!["/payload/priority".to_owned()]);
        assert_eq!(recompute_needed(&changes), (false, false));
    }

    #[test]
    fn a_changed_search_trait_asks_for_a_recompute() {
        let old = EffectiveTraits::default();
        let new = EffectiveTraits {
            full_text_search: vec!["/payload/statement".to_owned()],
            vector_search: vec!["/payload/statement".to_owned()],
            ..EffectiveTraits::default()
        };
        assert_eq!(recompute_needed(&traits_diff(&old, &new)), (true, true));
    }

    #[test]
    fn a_refusal_names_the_offending_locations() {
        let reason = refusal_reason(
            "gts.test.gs._.evolution_base.v1~thing.v1~",
            TypeChangeState::Incompatible,
            &[
                SchemaDiagnostic {
                    location: "$.payload".to_owned(),
                    finding: "required_changed".to_owned(),
                    message: "adds required properties: [\"owner\"]".to_owned(),
                },
                SchemaDiagnostic {
                    location: "$.payload.status".to_owned(),
                    finding: "enum_changed".to_owned(),
                    message: "removes enum values".to_owned(),
                },
            ],
            1,
        );
        assert!(reason.contains("$.payload adds required"), "{reason}");
        assert!(reason.contains("and 1 more"), "{reason}");
    }
}
