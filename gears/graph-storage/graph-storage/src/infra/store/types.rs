//! Ontology storage: registration, update, lookup and pattern resolution.
//!
//! Registration is idempotent for a byte-identical schema. A *different*
//! schema under a registered identifier is a conflict by default and, when the
//! caller asks for `on_existing: update`, an evolution question instead —
//! decided by `domain::evolution` (the `BACKWARD` direction of types-registry
//! ADR-0003, computed by `gts` OP#8) and, only where the schemas cannot decide
//! it, by re-validating the type's own rows.
//!
//! `ON CONFLICT DO NOTHING` skipping every row is **convergence, not
//! failure** — reporting it as an error is the trap a re-registration hit in
//! the prototype (ADR-0005 § Confirmation).

use graph_storage_sdk::models::{
    AdmissionBasis, EffectiveTraits, GtsTypeId, OnExisting, Page, RegisteredType, TraitChange,
    TypeChange, TypeChangeState, TypeIdSet, TypeKind, TypeOutcome, TypeQuery, TypeRecord,
    TypeRegistration, TypeRegistrationOptions,
};
use graph_storage_sdk::plugin_api::{GraphStoreError, StoreCtx};
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{SecureEntityExt, SecureInsertExt, SecureUpdateExt};

use crate::domain::{evolution, ontology};
use crate::infra::storage::entity::gts_type;
use crate::infra::store::{PgGraphStore, TxStoreError, map_db_error, map_scope_err};

fn kind_to_str(kind: TypeKind) -> &'static str {
    kind.as_str()
}

fn kind_from_str(value: &str) -> Result<TypeKind, GraphStoreError> {
    match value {
        "node" => Ok(TypeKind::Node),
        "edge" => Ok(TypeKind::Edge),
        "attribute" => Ok(TypeKind::Attribute),
        other => Err(GraphStoreError::Corrupt {
            reason: format!("gts_type.kind holds `{other}`"),
        }),
    }
}

/// Read the stored trait resolution back. Hand-written, like the write side:
/// the SDK models carry no serde by contract, so the JSON shape is owned here,
/// beside the column that holds it.
pub(crate) fn traits_from_json(value: &serde_json::Value) -> EffectiveTraits {
    let strings = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    EffectiveTraits {
        family: value
            .get("family")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        scope_managed: value
            .get("scope_managed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        emit_events: value
            .get("emit_events")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        index: strings("index"),
        full_text_search: strings("full_text_search"),
        vector_search: strings("vector_search"),
        src_types: strings("src_types"),
        dst_types: strings("dst_types"),
    }
}

/// The resolved `index` kinds stored beside the traits (`index_kinds`), as the
/// projection needs them: pointer -> scalar kind.
pub(crate) fn index_kinds_from_json(
    value: &serde_json::Value,
) -> std::collections::BTreeMap<String, ontology::ScalarKind> {
    value
        .get("index_kinds")
        .and_then(serde_json::Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(pointer, kind)| {
            kind.as_str()
                .and_then(ontology::ScalarKind::parse)
                .map(|kind| (pointer.clone(), kind))
        })
        .collect()
}

fn to_record(model: gts_type::Model) -> Result<TypeRecord, GraphStoreError> {
    let effective_traits = traits_from_json(&model.effective_traits);
    let is_abstract = model
        .type_schema
        .get("x-gts-abstract")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    Ok(TypeRecord {
        type_id: model.gts_type_id,
        type_uuid: model.gts_type_uuid,
        kind: kind_from_str(&model.kind)?,
        is_abstract,
        schema: model.type_schema,
        effective_traits,
        created_at: model.created_at,
        revision: model.revision,
    })
}

/// Traits are stored as their resolved JSON so the shape the SDK reads and
/// the shape the column holds cannot drift.
fn traits_to_json(descriptor: &ontology::TypeDescriptor) -> serde_json::Value {
    let traits = &descriptor.effective_traits;
    let index_kinds: serde_json::Map<String, serde_json::Value> = descriptor
        .index_paths
        .iter()
        .map(|p| {
            (
                p.pointer.clone(),
                serde_json::Value::String(p.kind.as_str().to_owned()),
            )
        })
        .collect();
    serde_json::json!({
        "index_kinds": index_kinds,
        "family": traits.family,
        "scope_managed": traits.scope_managed,
        "emit_events": traits.emit_events,
        "index": traits.index,
        "full_text_search": traits.full_text_search,
        "vector_search": traits.vector_search,
        "src_types": traits.src_types,
        "dst_types": traits.dst_types,
    })
}

/// The bounds a re-validating update runs inside, read from configuration
/// once per call so the batch loop cannot see a changed value halfway.
#[derive(Clone, Copy)]
struct UpdateLimits {
    max_rows: u64,
    max_migration_rows: u64,
    batch: u64,
    max_reported: usize,
    /// The call's absolute deadline, so a re-validating scan stops waiting
    /// rather than outliving the request that asked for it.
    budget: graph_storage_sdk::models::RemainingBudget,
}

/// Who is registering, and under what authority.
///
/// One struct rather than three threaded parameters because a migration writes
/// element rows, and `fr-audit-envelope` requires the acting subject to be
/// stamped on every one of them: the subject has to travel with the tenant and
/// the scope from here to the row.
struct Actor<'a> {
    tenant: uuid::Uuid,
    scope: &'a toolkit_security::AccessScope,
    subject: graph_storage_sdk::models::Subject,
}

pub async fn register_types(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    batch: Vec<TypeRegistration>,
    options: TypeRegistrationOptions,
) -> Result<Vec<RegisteredType>, GraphStoreError> {
    // The batch commits atomically: a partway failure leaves nothing.
    let tenant = ctx.tenant;
    let scope = ctx.scope.clone();
    let subject = ctx.subject.clone();
    let config = store.config();
    let max_chain_depth = usize::from(config.ontology_max_chain_depth);
    let limits = UpdateLimits {
        max_rows: u64::from(config.type_update_max_rows),
        max_migration_rows: u64::from(config.type_migration_max_rows),
        batch: u64::from(config.type_update_batch),
        max_reported: config.type_update_max_reported_rows as usize,
        budget: ctx.budget,
    };
    store
        .db()
        .transaction_ref_mapped::<_, Vec<RegisteredType>, TxStoreError>(move |tx| {
            let batch = batch.clone();
            let scope = scope.clone();
            let options = options.clone();
            let subject = subject.clone();
            Box::pin(async move {
                register_in_tx(
                    Actor {
                        tenant,
                        scope: &scope,
                        subject,
                    },
                    tx,
                    batch,
                    max_chain_depth,
                    &options,
                    limits,
                )
                .await
                .map_err(TxStoreError::from)
            })
        })
        .await
        .map_err(|error| error.0)
}

/// One type's ancestors, resolved from what is registered in this
/// transaction, outermost base first.
/// `in_batch` carries what this batch already analyzed, consulted before the
/// table: a batch may register a family and its producer type together, and a
/// dry run writes nothing at all, so the ancestor of the second entry has to
/// be findable without a row.
async fn ancestor_definitions(
    scope: &toolkit_security::AccessScope,
    tx: &impl toolkit_db::secure::DBRunner,
    type_id: &str,
    in_batch: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Result<Vec<(String, serde_json::Value)>, GraphStoreError> {
    let chain = ontology::ancestors(type_id);
    let mut out = Vec::new();
    for ancestor in &chain[..chain.len().saturating_sub(1)] {
        if let Some(schema) = in_batch.get(ancestor) {
            out.push((ancestor.clone(), schema.clone()));
            continue;
        }
        let existing = gts_type::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(gts_type::Column::GtsTypeId.eq(ancestor.clone())))
            .one(tx)
            .await
            .map_err(map_scope_err)?;
        match existing {
            Some(model) => out.push((model.gts_type_id, model.type_schema)),
            None => {
                return Err(GraphStoreError::Validation {
                    items: vec![graph_storage_sdk::models::ItemError {
                        index: 0,
                        family: graph_storage_sdk::models::ItemFamily::Node,
                        gts_type: Some(type_id.to_owned()),
                        pointer: None,
                        message: format!("ancestor `{ancestor}` is not registered"),
                    }],
                });
            }
        }
    }
    Ok(out)
}

fn invalid_candidate(type_id: &str, message: String) -> GraphStoreError {
    GraphStoreError::Validation {
        items: vec![graph_storage_sdk::models::ItemError {
            index: 0,
            family: graph_storage_sdk::models::ItemFamily::Node,
            gts_type: Some(type_id.to_owned()),
            pointer: None,
            message,
        }],
    }
}

/// The conflict a `reject`-mode caller gets — the gear's historical answer,
/// word for word, plus where to look for the reason.
fn rejected(type_id: &str) -> GraphStoreError {
    GraphStoreError::Conflict {
        reason: format!(
            "type `{type_id}` is already registered with a different schema; \
             POST /types/compatibility reports what the change would cost, and \
             `options.on_existing: \"update\"` admits it when it is admissible"
        ),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one type's admission is one sequence — resolve, compare, decide, \
              re-validate, write — and splitting it across helpers would hide \
              the order the decision depends on"
)]
async fn register_in_tx(
    actor: Actor<'_>,
    tx: &impl toolkit_db::secure::DBRunner,
    batch: Vec<TypeRegistration>,
    max_chain_depth: usize,
    options: &TypeRegistrationOptions,
    limits: UpdateLimits,
) -> Result<Vec<RegisteredType>, GraphStoreError> {
    let Actor {
        tenant,
        scope,
        ref subject,
    } = actor;
    let update = options.on_existing == OnExisting::Update;
    // A migration naming a type this batch does not carry would silently do
    // nothing, which is the worst possible answer to a typo.
    for spec in &options.migrations {
        if !batch.iter().any(|r| r.type_id == spec.type_id) {
            return Err(GraphStoreError::InvalidQuery {
                what: format!(
                    "a migration names `{}`, which this batch does not register",
                    spec.type_id
                ),
            });
        }
    }
    let mut out = Vec::with_capacity(batch.len());
    let mut in_batch: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for registration in batch {
        // Resolve the chain from what is already registered plus what this
        // batch carries.
        let ancestors = ancestor_definitions(scope, tx, &registration.type_id, &in_batch).await?;
        let ancestor_refs: Vec<&serde_json::Value> =
            ancestors.iter().map(|(_, schema)| schema).collect();
        let descriptor = ontology::analyze(
            &registration.type_id,
            &registration.schema,
            &ancestor_refs,
            max_chain_depth,
        )
        .map_err(|error| invalid_candidate(&registration.type_id, error.to_string()))?;
        let traits_json = traits_to_json(&descriptor);
        in_batch.insert(descriptor.type_id.clone(), descriptor.schema.clone());
        let migration = options.migration_for(&descriptor.type_id);

        let existing = gts_type::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all().add(gts_type::Column::GtsTypeId.eq(descriptor.type_id.clone())),
            )
            .one(tx)
            .await
            .map_err(map_scope_err)?;

        let Some(model) = existing else {
            if migration.is_some() {
                return Err(GraphStoreError::InvalidQuery {
                    what: format!(
                        "the migration for `{}` has nothing to migrate: the type is not \
                         registered yet, so it holds no rows",
                        descriptor.type_id
                    ),
                });
            }
            if options.dry_run {
                out.push(RegisteredType {
                    record: dry_record(&descriptor, &traits_json),
                    outcome: TypeOutcome::Created,
                    basis: None,
                    change: Some(new_type_change(&descriptor.type_id)),
                });
                continue;
            }
            let active = gts_type::ActiveModel {
                tenant_id: ActiveValue::Set(tenant),
                id: ActiveValue::NotSet,
                gts_type_uuid: ActiveValue::Set(descriptor.type_uuid),
                gts_type_id: ActiveValue::Set(descriptor.type_id.clone()),
                kind: ActiveValue::Set(kind_to_str(descriptor.kind).to_owned()),
                type_schema: ActiveValue::Set(descriptor.schema.clone()),
                effective_traits: ActiveValue::Set(traits_json),
                created_at: ActiveValue::Set(time::OffsetDateTime::now_utc()),
                revision: ActiveValue::Set(1),
                updated_at: ActiveValue::Set(time::OffsetDateTime::now_utc()),
            };
            // scope_unchecked: an INSERT cannot subtree-clamp a row
            // that does not exist yet.
            let model = gts_type::Entity::insert(active)
                .secure()
                .scope_unchecked(scope)
                .map_err(map_scope_err)?
                .exec_with_returning(tx)
                .await
                .map_err(map_scope_err)?;
            out.push(RegisteredType {
                record: to_record(model)?,
                outcome: TypeOutcome::Created,
                basis: None,
                change: Some(new_type_change(&descriptor.type_id)),
            });
            continue;
        };

        let stored_traits = traits_from_json(&model.effective_traits);
        let trait_changes = evolution::traits_diff(&stored_traits, &descriptor.effective_traits);
        let schema_changed = model.type_schema != descriptor.schema;
        let stored_resolution_is_stale = model.effective_traits != traits_json;

        if !schema_changed && migration.is_some() {
            // A migration rewrites payloads, and it is admitted as part of
            // moving a type to a new definition. Offered against an unchanged
            // schema it would be a data-editing endpoint wearing a type
            // registration's clothes, which is a different feature with a
            // different authorization story.
            return Err(GraphStoreError::InvalidQuery {
                what: format!(
                    "the migration for `{}` has nothing to migrate towards: the candidate \
                     schema is byte-identical to the registered one",
                    descriptor.type_id
                ),
            });
        }

        if !schema_changed {
            // Byte-identical re-registration converges. It is *not* nothing
            // when the stored trait resolution differs from what this gear
            // resolves: a type registered by an older build carries a stale
            // `effective_traits` (no `index_kinds`, for one), and the
            // prototype's workaround was to recreate the database. An
            // updating caller refreshes it; a rejecting one keeps converging,
            // so the default path is unchanged.
            if stored_resolution_is_stale && update && !options.dry_run {
                let revision = model.revision.saturating_add(1);
                gts_type::Entity::update_many()
                    .col_expr(
                        gts_type::Column::EffectiveTraits,
                        Expr::value(traits_json.clone()),
                    )
                    .col_expr(gts_type::Column::Revision, Expr::value(revision))
                    .col_expr(
                        gts_type::Column::UpdatedAt,
                        Expr::value(time::OffsetDateTime::now_utc()),
                    )
                    .filter(Condition::all().add(gts_type::Column::Id.eq(model.id)))
                    .secure()
                    .scope_with(scope)
                    .exec(tx)
                    .await
                    .map_err(map_scope_err)?;
                super::ingest::bump_revision(tenant, scope, tx).await?;
                out.push(RegisteredType {
                    record: written_record(&model, &descriptor, revision),
                    outcome: TypeOutcome::Updated,
                    basis: Some(AdmissionBasis::SchemaProved),
                    change: Some(unchanged_type_change(&descriptor.type_id, trait_changes)),
                });
                continue;
            }
            out.push(RegisteredType {
                record: to_record(model)?,
                outcome: TypeOutcome::Unchanged,
                basis: None,
                change: Some(unchanged_type_change(&descriptor.type_id, trait_changes)),
            });
            continue;
        }

        // The schema moved. Which of the two definitions accepts more is a
        // question about their accepted instance sets, and `gts` OP#8 answers
        // it — over documents whose `$ref`s are resolved, both sides against
        // the same ancestor set (see `domain::evolution::compare`).
        let comparison = evolution::compare(
            &model.type_schema,
            &descriptor.schema,
            ancestors.iter().cloned(),
        )
        .map_err(|error| invalid_candidate(&descriptor.type_id, error.to_string()))?;
        let state = comparison.state();
        let mut change = TypeChange {
            type_id: descriptor.type_id.clone(),
            state,
            backward: comparison.backward.as_str().to_owned(),
            forward: comparison.forward.as_str().to_owned(),
            diagnostics: comparison.diagnostics.clone(),
            traits_changed: trait_changes,
            rows: None,
            rows_rewritten: None,
            levels_not_evolvable_in_place: comparison.levels_not_evolvable_in_place.clone(),
            migration_required: !matches!(state, TypeChangeState::Compatible),
            admissible: false,
        };

        let decision = evolution::decide(
            state,
            evolution::Asked {
                update,
                offered: evolution::offered(migration.is_some(), options.revalidate),
            },
        );
        if decision == evolution::Decision::Refuse {
            if options.dry_run {
                out.push(RegisteredType {
                    record: to_record(model)?,
                    outcome: TypeOutcome::Unchanged,
                    basis: None,
                    change: Some(change),
                });
                continue;
            }
            if !update {
                return Err(rejected(&descriptor.type_id));
            }
            return Err(GraphStoreError::Conflict {
                reason: evolution::refusal_reason(
                    &descriptor.type_id,
                    state,
                    &change.diagnostics,
                    limits.max_reported.min(5),
                ),
            });
        }

        let candidate = Candidate {
            descriptor: &descriptor,
            ancestors: &ancestors,
            interned: model.id,
            migration,
        };
        let admitted = admit(
            super::evolution::Migrator {
                scope,
                subject,
                dry_run: options.dry_run,
            },
            tx,
            &candidate,
            decision,
            limits,
            &mut change,
        )
        .await?;
        let Some(basis) = admitted else {
            // A dry run reports a refusal instead of raising it; the change
            // now carries why.
            out.push(RegisteredType {
                record: to_record(model)?,
                outcome: TypeOutcome::Unchanged,
                basis: None,
                change: Some(change),
            });
            continue;
        };

        if options.dry_run {
            out.push(RegisteredType {
                record: to_record(model)?,
                outcome: TypeOutcome::Updated,
                basis: Some(basis),
                change: Some(change),
            });
            continue;
        }

        let revision = model.revision.saturating_add(1);
        gts_type::Entity::update_many()
            .col_expr(
                gts_type::Column::TypeSchema,
                Expr::value(descriptor.schema.clone()),
            )
            .col_expr(
                gts_type::Column::EffectiveTraits,
                Expr::value(traits_json.clone()),
            )
            .col_expr(gts_type::Column::Revision, Expr::value(revision))
            .col_expr(
                gts_type::Column::UpdatedAt,
                Expr::value(time::OffsetDateTime::now_utc()),
            )
            .filter(Condition::all().add(gts_type::Column::Id.eq(model.id)))
            .secure()
            .scope_with(scope)
            .exec(tx)
            .await
            .map_err(map_scope_err)?;
        // A read at the previous revision could refuse a filter this definition
        // admits, or accept a payload it now rejects. That is exactly what the
        // revision exists to fence, so an accepted update advances it — once
        // per updated type, inside the same transaction. A `created` type
        // changes no existing read and leaves the counter alone, as
        // registration always has.
        super::ingest::bump_revision(tenant, scope, tx).await?;
        out.push(RegisteredType {
            record: written_record(&model, &descriptor, revision),
            outcome: TypeOutcome::Updated,
            basis: Some(basis),
            change: Some(change),
        });
    }
    Ok(out)
}

/// The candidate being admitted, and what the caller offered with it.
struct Candidate<'a> {
    descriptor: &'a ontology::TypeDescriptor,
    ancestors: &'a [(String, serde_json::Value)],
    /// The interned id of the registered type this candidate replaces.
    interned: i32,
    migration: Option<&'a graph_storage_sdk::models::MigrationSpec>,
}

/// Carry out the decision, and say on what ground the change is admitted.
///
/// `Ok(None)` is a dry run's refusal: `change` carries the reason and the
/// caller reports it. A write refuses by returning `Err`, which rolls the
/// transaction back — the two row-reading grounds both validate before they
/// write, and neither leaves a half-applied type behind.
async fn admit(
    who: super::evolution::Migrator<'_>,
    tx: &impl toolkit_db::secure::DBRunner,
    candidate: &Candidate<'_>,
    decision: evolution::Decision,
    limits: UpdateLimits,
    change: &mut TypeChange,
) -> Result<Option<AdmissionBasis>, GraphStoreError> {
    let descriptor = candidate.descriptor;
    match decision {
        evolution::Decision::Refuse => Ok(None),
        evolution::Decision::Accept => {
            change.admissible = true;
            Ok(Some(AdmissionBasis::SchemaProved))
        }
        evolution::Decision::Revalidate | evolution::Decision::Migrate => {
            let rows =
                super::evolution::count_live(who.scope, tx, descriptor.kind, candidate.interned)
                    .await?;
            change.rows = Some(rows);
            if let Some(refusal) = row_ceiling(&descriptor.type_id, rows, limits.bound(decision)) {
                if who.dry_run {
                    change.diagnostics.push(refusal.diagnostic);
                    return Ok(None);
                }
                return Err(refusal.error);
            }
            let validator = chain_validator(candidate.ancestors, descriptor)?;
            let bounds = super::evolution::ScanBounds {
                batch: limits.batch,
                max_reported: limits.max_reported,
                budget: limits.budget,
            };

            if decision == evolution::Decision::Revalidate {
                // What the schemas could not prove, the rows may still
                // satisfy. A claim about *these* rows, reported as its own
                // basis and never cached as a verdict about the type.
                let failures = super::evolution::revalidate(
                    who.scope,
                    tx,
                    &descriptor.type_id,
                    descriptor.kind,
                    candidate.interned,
                    &validator,
                    bounds,
                )
                .await?;
                if failures.is_empty() {
                    change.admissible = true;
                    return Ok(Some(AdmissionBasis::DataBacked {
                        rows_validated: rows,
                    }));
                }
                if !who.dry_run {
                    return Err(GraphStoreError::Validation { items: failures });
                }
                report_rows(change, &failures, "stored_row_invalid");
                return Ok(None);
            }

            // A migration: the caller stated what to do with the data, so the
            // question becomes whether the rows fit *once the steps have run*
            // — answered the only honest way, by running them and validating
            // the result before writing.
            let Some(spec) = candidate.migration else {
                return Err(GraphStoreError::Internal(
                    "the rule asked for a migration where none was declared".to_owned(),
                ));
            };
            let plan = crate::domain::migration::compile(spec).map_err(|error| {
                GraphStoreError::InvalidQuery {
                    what: error.to_string(),
                }
            })?;
            let outcome = super::evolution::migrate(
                who,
                tx,
                super::evolution::Migrating {
                    type_id: &descriptor.type_id,
                    kind: descriptor.kind,
                    interned: candidate.interned,
                    plan: &plan,
                    validator: &validator,
                    full_text_search: &descriptor.effective_traits.full_text_search,
                    vectorized: !descriptor.effective_traits.vector_search.is_empty(),
                },
                bounds,
            )
            .await?;
            change.rows_rewritten = Some(outcome.rows_rewritten);
            if outcome.failures.is_empty() {
                change.admissible = true;
                return Ok(Some(AdmissionBasis::Migrated {
                    rows_scanned: outcome.rows_scanned,
                    rows_rewritten: outcome.rows_rewritten,
                }));
            }
            if !who.dry_run {
                return Err(GraphStoreError::Validation {
                    items: outcome.failures,
                });
            }
            report_rows(change, &outcome.failures, "row_invalid_after_migration");
            Ok(None)
        }
    }
}

/// Fold offending rows into the dry run's diagnostics.
fn report_rows(
    change: &mut TypeChange,
    failures: &[graph_storage_sdk::models::ItemError],
    finding: &str,
) {
    for failure in failures {
        change
            .diagnostics
            .push(graph_storage_sdk::models::SchemaDiagnostic {
                location: failure.pointer.clone().unwrap_or_default(),
                finding: finding.to_owned(),
                message: failure.message.clone(),
            });
    }
}

/// A refusal that a write raises and a dry run reports.
struct Ceiling {
    error: GraphStoreError,
    diagnostic: graph_storage_sdk::models::SchemaDiagnostic,
}

impl UpdateLimits {
    /// The bound this pass runs under, and the key that sets it.
    ///
    /// Two bounds rather than one because the passes run at different rates
    /// and only one of them writes: re-validation reads ~19 000 rows/s, a
    /// migration rewrites ~1 900 (one statement per changed row, measured on
    /// a stand). Under a gateway that kills a synchronous request at 30 s, a
    /// shared ceiling sized for the first admits a migration that does all of
    /// its work and is then killed — the work rolls back, and the caller hears
    /// about a timeout rather than about a bound.
    fn bound(self, decision: evolution::Decision) -> (u64, &'static str) {
        if decision == evolution::Decision::Migrate {
            (self.max_migration_rows, "type_migration_max_rows")
        } else {
            (self.max_rows, "type_update_max_rows")
        }
    }
}

/// `None` while the type fits inside the synchronous bound for this pass.
///
/// The bound is not the gear's own deadline: `api-gateway` kills a synchronous
/// request at 30 s whatever this gear is configured with, so the ceiling is
/// what keeps a row-reading update inside a request that can actually answer.
fn row_ceiling(type_id: &str, rows: u64, bound: (u64, &str)) -> Option<Ceiling> {
    let (max_rows, key) = bound;
    if rows <= max_rows {
        return None;
    }
    let what = format!(
        "type `{type_id}` has {rows} live rows; one synchronous pass handles at most \
         {max_rows} (`{key}`)"
    );
    Some(Ceiling {
        error: GraphStoreError::LimitExceeded { what: what.clone() },
        diagnostic: graph_storage_sdk::models::SchemaDiagnostic {
            location: "$".to_owned(),
            finding: "row_ceiling_exceeded".to_owned(),
            message: what,
        },
    })
}

/// A validator for the candidate, with its ancestors resolvable.
///
/// The same validator ingest would compile for this type once the candidate is
/// registered — which is the point: a row admitted here must be a row the next
/// ingest of the same content would also admit.
fn chain_validator(
    ancestors: &[(String, serde_json::Value)],
    descriptor: &ontology::TypeDescriptor,
) -> Result<ontology::ChainValidator, GraphStoreError> {
    let mut chain: Vec<(String, serde_json::Value)> = ancestors.to_vec();
    chain.push((descriptor.type_id.clone(), descriptor.schema.clone()));
    ontology::ChainValidator::compile(&descriptor.schema, chain)
        .map_err(|error| invalid_candidate(&descriptor.type_id, error.to_string()))
}

/// The record a dry run reports for a type it did not write.
fn dry_record(
    descriptor: &ontology::TypeDescriptor,
    traits_json: &serde_json::Value,
) -> TypeRecord {
    TypeRecord {
        type_id: descriptor.type_id.clone(),
        type_uuid: descriptor.type_uuid,
        kind: descriptor.kind,
        is_abstract: descriptor.is_abstract,
        schema: descriptor.schema.clone(),
        effective_traits: traits_from_json(traits_json),
        created_at: time::OffsetDateTime::now_utc(),
        revision: 0,
    }
}

fn new_type_change(type_id: &str) -> TypeChange {
    TypeChange {
        type_id: type_id.to_owned(),
        state: TypeChangeState::New,
        backward: "compatible".to_owned(),
        forward: "compatible".to_owned(),
        diagnostics: Vec::new(),
        traits_changed: Vec::new(),
        rows: None,
        rows_rewritten: None,
        levels_not_evolvable_in_place: Vec::new(),
        migration_required: false,
        admissible: true,
    }
}

/// The row as it stands after an accepted update.
///
/// Built from what was just written rather than read back: the values are in
/// hand, and a second SELECT inside the transaction would report the same row
/// at the cost of a round trip per updated type in a 415-type batch.
fn written_record(
    stored: &gts_type::Model,
    descriptor: &ontology::TypeDescriptor,
    revision: i32,
) -> TypeRecord {
    TypeRecord {
        type_id: descriptor.type_id.clone(),
        type_uuid: stored.gts_type_uuid,
        kind: descriptor.kind,
        is_abstract: descriptor.is_abstract,
        schema: descriptor.schema.clone(),
        effective_traits: descriptor.effective_traits.clone(),
        created_at: stored.created_at,
        revision,
    }
}

fn unchanged_type_change(type_id: &str, traits_changed: Vec<TraitChange>) -> TypeChange {
    TypeChange {
        type_id: type_id.to_owned(),
        state: TypeChangeState::Unchanged,
        backward: "compatible".to_owned(),
        forward: "compatible".to_owned(),
        diagnostics: Vec::new(),
        traits_changed,
        rows: None,
        rows_rewritten: None,
        levels_not_evolvable_in_place: Vec::new(),
        migration_required: false,
        admissible: true,
    }
}

pub async fn get_type(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    id: &GtsTypeId,
) -> Result<TypeRecord, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let model = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .filter(Condition::all().add(gts_type::Column::GtsTypeId.eq(id.clone())))
        .one(&conn)
        .await
        .map_err(map_scope_err)?
        .ok_or(GraphStoreError::NotFound)?;
    to_record(model)
}

pub async fn list_types(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    query: TypeQuery,
) -> Result<Page<TypeRecord>, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let mut select = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .order_by(gts_type::Column::GtsTypeId, sea_orm::Order::Asc);
    if let Some(kind) = query.kind {
        select = select.filter(Condition::all().add(gts_type::Column::Kind.eq(kind_to_str(kind))));
    }
    let limit = query.top.unwrap_or(store.config().projection_max_page);
    let models = select
        .limit(u64::from(limit))
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    // The pattern is resolved through the platform GTS implementation, never
    // compiled into SQL: no identifier ever reaches a LIKE pattern.
    let mut items = Vec::new();
    for model in models {
        if let Some(pattern) = &query.pattern {
            let patterns = vec![pattern.clone()];
            let matches =
                ontology::matches_any_pattern(&model.gts_type_id, &patterns).map_err(|error| {
                    GraphStoreError::LimitExceeded {
                        what: error.to_string(),
                    }
                })?;
            if !matches {
                continue;
            }
        }
        items.push(to_record(model)?);
    }

    let revision = crate::infra::store::reads::revision(store, ctx).await?;
    Ok(Page {
        items,
        next_cursor: None,
        revision,
    })
}

pub async fn resolve_type_set(
    store: &PgGraphStore,
    ctx: &StoreCtx<'_>,
    patterns: &[String],
) -> Result<TypeIdSet, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    let models = gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .all(&conn)
        .await
        .map_err(map_scope_err)?;

    let mut set = std::collections::BTreeSet::new();
    for model in models {
        let matches =
            ontology::matches_any_pattern(&model.gts_type_id, patterns).map_err(|error| {
                GraphStoreError::LimitExceeded {
                    what: error.to_string(),
                }
            })?;
        if matches {
            set.insert(model.gts_type_id);
        }
    }
    Ok(TypeIdSet(set))
}

/// Interned ids for the given GTS identifiers, for the write path.
pub async fn interned_ids(
    scope: &toolkit_security::AccessScope,
    runner: &impl toolkit_db::secure::DBRunner,
    type_ids: &[String],
) -> Result<std::collections::BTreeMap<String, (i32, uuid::Uuid, serde_json::Value)>, GraphStoreError>
{
    let models = gts_type::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(gts_type::Column::GtsTypeId.is_in(type_ids.to_vec())))
        .all(runner)
        .await
        .map_err(map_scope_err)?;
    Ok(models
        .into_iter()
        .map(|m| (m.gts_type_id, (m.id, m.gts_type_uuid, m.effective_traits)))
        .collect())
}

/// Count of registered types, used by the readiness surface.
pub async fn count(store: &PgGraphStore, ctx: &StoreCtx<'_>) -> Result<u64, GraphStoreError> {
    let conn = store.db().conn().map_err(|error| map_db_error(&error))?;
    gts_type::Entity::find()
        .secure()
        .scope_with(ctx.scope)
        .count(&conn)
        .await
        .map_err(map_scope_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> UpdateLimits {
        UpdateLimits {
            max_rows: 100_000,
            max_migration_rows: 25_000,
            batch: 2_000,
            max_reported: 50,
            budget: graph_storage_sdk::models::RemainingBudget::starting_now(
                std::time::Duration::from_secs(10),
            ),
        }
    }

    /// The two passes run at different rates under one 30 s gateway cap, so
    /// they cannot share a ceiling: at the measured ~1 900 rows/s a migration
    /// of 100 000 rows would do 52 s of work and be killed. The refusal has to
    /// name the key that actually applies, or an operator raises the wrong one.
    #[test]
    fn each_pass_is_bounded_by_its_own_ceiling_and_names_it() {
        let migration = row_ceiling(
            "gts.test.gs._.thing.v1~",
            40_000,
            limits().bound(evolution::Decision::Migrate),
        )
        .expect("40 000 rows is past the migration ceiling");
        assert!(
            migration
                .diagnostic
                .message
                .contains("type_migration_max_rows"),
            "{}",
            migration.diagnostic.message
        );
        assert!(migration.diagnostic.message.contains("25000"));

        // The same size is well inside the read-only pass.
        assert!(
            row_ceiling(
                "gts.test.gs._.thing.v1~",
                40_000,
                limits().bound(evolution::Decision::Revalidate),
            )
            .is_none(),
            "40 000 rows is ~2 s of re-validation"
        );

        let revalidation = row_ceiling(
            "gts.test.gs._.thing.v1~",
            250_000,
            limits().bound(evolution::Decision::Revalidate),
        )
        .expect("250 000 rows is past the read ceiling too");
        assert!(
            revalidation
                .diagnostic
                .message
                .contains("type_update_max_rows"),
            "{}",
            revalidation.diagnostic.message
        );
    }
}
