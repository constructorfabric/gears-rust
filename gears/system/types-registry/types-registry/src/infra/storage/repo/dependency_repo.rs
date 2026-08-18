//! The `dependency` repository: one entity's outgoing edges, and the transitive
//! closure the transient `gts-rust` store is built from.

use std::collections::{HashMap, HashSet};

use gts::GtsId;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{
    AccessScope, DBRunner, ScopeError, SecureDeleteExt, SecureEntityExt, SecureInsertManyExt,
};

use super::IN_CHUNK;
use super::entity_repo::EntityRepo;
use crate::domain::enums::DependencyKind;
use crate::domain::ports::{DependencyClosure, EntityRow};
use crate::infra::storage::entity::dependency;

/// Maximum size of one dependency closure.
///
/// **Its own bound, not `limits.activation_write_set`** — it took that key's value
/// (512) as a starting point and nothing more. The two count different things: this
/// bounds the entities one store build **reads**, SPEC §8.1 step 4.6's write set
/// bounds the dependents an admission **refreshes**. The earlier wording here said
/// this "mirrors" the key, which read as though an operator could move it; they
/// cannot, and the key's own documentation now says so (`config::Limits`).
///
/// A private constant is the honest shape while the number has no operator meaning:
/// the closure a single admission unit needs is bounded by its dependency graph
/// rather than by the entity count, and the measured max fan-out in-repo is well
/// under this. T14 is where a configured bound reaches this layer, and where the two
/// bounds should be told apart by name. Upgrade path if it is hit: the
/// generation/staging protocol in DESIGN §4.
const CLOSURE_BOUND: usize = 512;

pub struct DependencyRepo;

impl DependencyRepo {
    /// Replace one entity's outgoing edges.
    ///
    /// Admission replaces only the admitted entity's outgoing rows, never anyone
    /// else's, so the delete is keyed on `from_entity_id` alone. Delete-then-insert
    /// rather than a diff: the edge set is small, and a diff would need to read the
    /// current rows to compute it.
    ///
    /// `edges` is treated as a **set**, because that is what the table is:
    /// `(from_entity_id, kind, to_entity_id)` is the primary key. Two references to
    /// the same base from one schema — a `$ref` used twice — are one edge, and
    /// leaving the duplicate in would turn an ordinary document into a primary-key
    /// violation partway through admission.
    pub async fn replace_outgoing(
        runner: &impl DBRunner,
        scope: &AccessScope,
        from_entity_id: i64,
        edges: &[(DependencyKind, i64)],
    ) -> Result<(), ScopeError> {
        dependency::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(dependency::Column::FromEntityId.eq(from_entity_id)))
            .exec(runner)
            .await?;

        let mut seen: HashSet<(DependencyKind, i64)> = HashSet::with_capacity(edges.len());
        let unique: Vec<(DependencyKind, i64)> =
            edges.iter().filter(|e| seen.insert(**e)).copied().collect();
        if unique.is_empty() {
            return Ok(());
        }
        let rows = unique.iter().map(|(kind, to)| dependency::ActiveModel {
            from_entity_id: Set(from_entity_id),
            kind: Set((*kind).into()),
            to_entity_id: Set(*to),
        });
        for chunk in rows.collect::<Vec<_>>().chunks(IN_CHUNK) {
            dependency::Entity::insert_many(chunk.to_vec())
                .secure()
                // `dependency` carries no security dimension of its own: an edge is
                // reachable exactly when both endpoints are, and those rows are
                // scoped. There is nothing per-row to validate.
                .scope_unchecked(scope)?
                .exec(runner)
                .await?;
        }
        Ok(())
    }

    /// The entities directly consumed by any of `from_entity_ids`, chunked.
    pub async fn direct_dependencies(
        runner: &impl DBRunner,
        scope: &AccessScope,
        from_entity_ids: &[i64],
    ) -> Result<Vec<i64>, ScopeError> {
        let mut out = Vec::new();
        for chunk in from_entity_ids.chunks(IN_CHUNK) {
            let rows = dependency::Entity::find()
                .filter(dependency::Column::FromEntityId.is_in(chunk.iter().copied()))
                .secure()
                .scope_with(scope)
                .all(runner)
                .await?;
            out.extend(rows.into_iter().map(|r| r.to_entity_id));
        }
        Ok(out)
    }

    /// Candidate identifiers plus the transitive closure of what they consume,
    /// `gts_id`-sorted. This is what the transient `gts-rust` store is built from.
    ///
    /// An iterative worklist over direct edges, not a recursive CTE — see the
    /// module header. The `seen` set is not an optimization: cycles are valid
    /// (ADR-0012), so without it the walk would not terminate.
    ///
    /// # The worklist is seeded from the identifier, not only from the edge table
    ///
    /// Each root contributes its whole `GtsId::chain_ids()` — every prefix of its
    /// `~`-chain — before the edge walk starts (T10). This is not a shortcut for
    /// T13's edges; it is a **different relation** that needs no table at all.
    /// A derivation base is a pure function of the identifier: `base~derived~`
    /// consumes `base~` by being named that way, and an Instance `base~thing.v1`
    /// conforms to `base~` the same way. Nothing writes a `dependency` row for
    /// either, and nothing should — the identifier already carries it, and a stored
    /// edge could disagree with the name.
    ///
    /// Without this seed, `validate_schema` on a derived candidate and
    /// `validate_instance` on any Instance would both fail with a missing base,
    /// because the edge table is empty until T13. T13 adds what is *genuinely*
    /// edge-derived: `$ref` and `x-gts-ref` targets, which no identifier implies.
    ///
    /// Candidates with no entity row are reported in
    /// [`DependencyClosure::missing_roots`] rather than failing the read, because a
    /// first admission's own candidate is exactly that case. **`missing_roots` is
    /// computed over the original roots only** — a chain member the seed added is
    /// not something the caller asked for, so its absence is the store builder's
    /// problem to name, not a missing root.
    pub async fn closure(
        runner: &impl DBRunner,
        scope: &AccessScope,
        roots: &[String],
    ) -> Result<DependencyClosure, ScopeError> {
        let mut seeds: Vec<String> = Vec::with_capacity(roots.len());
        for root in roots {
            match GtsId::try_new(root) {
                // Every prefix, which includes the root itself.
                Ok(id) => seeds.extend(id.chain_ids()),
                // An unparsable root is passed through unchanged so it lands in
                // `missing_roots` below, exactly as before. Refusing here would turn
                // a caller's bad identifier into a storage error.
                Err(_) => seeds.push(root.clone()),
            }
        }
        seeds.sort();
        seeds.dedup();

        let resolved = EntityRepo::find_by_gts_ids(runner, scope, &seeds).await?;
        let found: HashSet<&str> = resolved.iter().map(|r| r.gts_id.as_str()).collect();
        let mut missing_roots: Vec<String> = roots
            .iter()
            .filter(|r| !found.contains(r.as_str()))
            .cloned()
            .collect();
        missing_roots.sort();
        missing_roots.dedup();

        let mut seen: HashSet<i64> = resolved.iter().map(|r| r.id).collect();
        let mut frontier: Vec<i64> = seen.iter().copied().collect();
        let mut by_id: HashMap<i64, EntityRow> = resolved.into_iter().map(|r| (r.id, r)).collect();

        while !frontier.is_empty() {
            let discovered = Self::direct_dependencies(runner, scope, &frontier).await?;
            let fresh: Vec<i64> = discovered
                .into_iter()
                .filter(|id| seen.insert(*id))
                .collect();
            if fresh.is_empty() {
                break;
            }
            if by_id.len() + fresh.len() > CLOSURE_BOUND {
                tracing::warn!(
                    roots = ?roots,
                    closure_bound = CLOSURE_BOUND,
                    resolved_entities = by_id.len(),
                    newly_discovered = fresh.len(),
                    "types_registry dependency closure exceeded its safety bound"
                );
                return Err(ScopeError::Invalid(
                    "dependency closure exceeds the 512-entity store-build bound; see the \
                     structured warning for roots and reached size",
                ));
            }
            for row in EntityRepo::find_by_ids(runner, scope, &fresh).await? {
                by_id.insert(row.id, row);
            }
            frontier = fresh;
        }

        let mut entities: Vec<EntityRow> = by_id.into_values().collect();
        entities.sort_by(|a, b| a.gts_id.cmp(&b.gts_id));
        Ok(DependencyClosure {
            entities,
            missing_roots,
        })
    }
}
