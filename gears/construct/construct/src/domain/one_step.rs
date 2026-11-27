//! The one-step plan path: a value a person sets by hand, a reviewer's
//! correction or a tenant administrator's added or edited fact, as a plan
//! with one step and no planner, through the sensitive-data checks,
//! Admission and the profile writer, against the profile's version.

use std::future::Future;
use std::sync::Arc;

use serde_json::Value;
use tokio::time::Instant;
use toolkit_macros::domain_model;

use super::admission::{Admission, AdmissionOutcome};
use super::error::DomainError;
use super::plan::{Change, NodeKey, Origin, Plan, Property, Step};
use super::planner::catalog::{Catalog, Catalogs, KindSpec};
use super::planner::processing::Written;
use super::profile::Category;
use super::profile_writes::{Attempt, ProfileWrites, SubjectFacts, WriteError};
use super::record_intake::DropReason;
use super::sensitive_data::{Checked, SensitiveDataChecks};
use super::subject_settings::SubjectKey;
use construct_sdk::linked_schema::Owner;

/// Why a value set by hand stored nothing.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotStored {
    /// The checks failed, or Admission dropped the plan.
    Dropped(DropReason),
    /// The checks blocked the value.
    Blocked,
}

impl NotStored {
    /// The reason's name in the answer.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dropped(reason) => reason.name(),
            Self::Blocked => "value_blocked",
        }
    }
}

/// The stages a value set by hand passes before it is written: the same
/// checks and Admission as any plan, and the person types' schemas. Built
/// only where the deployment opted in to storing values without
/// sensitive-data checks, as record processing is.
#[domain_model]
pub struct OneStepPath {
    pub checks: SensitiveDataChecks,
    pub admission: Admission,
    /// Each person's catalog: the kinds a value may be stored under are those of the person's own schema.
    pub catalogs: Arc<Catalogs>,
}

/// A fact the one-step path stored: its new key, its category and its
/// property with the value Admission let through.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub struct NewFact {
    pub node_key: NodeKey,
    pub category: Category,
    pub property: Property,
}

/// A replace found the fact to replace gone; nothing was stored.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gone;

/// An add found the value, or a value of its one-valued property, held
/// already as this fact; nothing was stored.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Held(pub NodeKey);

/// What a one-step write did: stored the new fact, stored nothing for the
/// step's own reason `E` ([`Gone`] for a replace, [`Held`] for an add), or
/// stored nothing because the checks or Admission refused it.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OneStepDone<E> {
    Stored(NewFact),
    Skipped(E),
    NotStored(NotStored),
}

/// The step built on a read of the profile, or the step's reason to store
/// nothing.
#[domain_model]
enum Built<E> {
    Skipped(E),
    Step(Category, Change),
}

/// A plan ready to write, or what was done instead.
#[domain_model]
enum Prepared<E> {
    Done(OneStepDone<E>),
    Write(Box<Plan>, NewFact),
}

impl OneStepPath {
    /// The schema of the kind `property` names, when the schema has it and its nodes belong to the person.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] for a property that is no kind of the schema, a kind owned by another node (the
    /// planner adds those, with their owner), or a value its schema does not take. The message never repeats the
    /// caller's property or value.
    pub(crate) fn check<'c>(
        catalog: &'c Catalog,
        property: &str,
        value: &Value,
    ) -> Result<&'c KindSpec, DomainError> {
        let spec = catalog
            .kind(property)
            .ok_or_else(|| DomainError::InvalidValue {
                field: "property",
                rule: "the profile schema has no such kind".to_owned(),
            })?;
        if !matches!(spec.kind().owner, Owner::Root) {
            return Err(DomainError::InvalidValue {
                field: "property",
                rule: "a node of this kind is owned by another node, and only the planner adds it"
                    .to_owned(),
            });
        }
        spec.check(value)
            .map_err(|rule| DomainError::InvalidValue {
                field: "value",
                rule,
            })?;
        Ok(spec)
    }

    /// Add a fact holding `property` with `value` to the subject `key`'s
    /// profile, as `origin`. A subject without a profile gets one.
    ///
    /// NOT cancel-safe, as [`Self::replace`].
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] for a property or value the person types
    /// do not take; otherwise as [`Self::replace`].
    pub(crate) async fn add(
        &self,
        writes: &ProfileWrites,
        key: SubjectKey,
        origin: &Origin,
        property: &str,
        value: &Value,
        deadline: Instant,
    ) -> Result<OneStepDone<Held>, WriteError> {
        let catalog = self.catalogs.of(key).await?;
        let spec = Self::check(&catalog, property, value)?;
        self.write(
            writes,
            key,
            origin,
            "add",
            deadline,
            |current, new_key| {
                let named = value.get(spec.kind().key);
                let held = current.facts.iter().find(|fact| {
                    fact.category == spec.category
                        && (spec.kind().single
                            || named.is_some_and(|named| {
                                fact.value
                                    .get(spec.kind().key)
                                    .is_some_and(|held| same_name(held, named))
                            }))
                });
                Ok(match held {
                    Some(held) => Built::Skipped(Held(held.node_key.clone())),
                    None => Built::Step(
                        spec.category,
                        Change::AddEntity {
                            key: new_key.clone(),
                            owner: None,
                            properties: vec![Property::new(property, value.clone())],
                        },
                    ),
                })
            },
            |_| std::future::ready(Ok(())),
        )
        .await
    }

    /// Store `value` for `property` in place of the subject `key`'s fact
    /// `fact`, as `origin`, under a new node key; `what` names the write in
    /// logs and answers. `pend` is called with the new key before each write.
    ///
    /// NOT cancel-safe: a drop after `pend` and before the write leaves what
    /// `pend` recorded with nothing written, and a drop while the writer runs
    /// leaves a write that may have landed. The caller's own records, such as
    /// a claim's pending outcome, settle either.
    ///
    /// # Errors
    ///
    /// Internal when no person type has `property`, a stored fact's own
    /// property; [`DomainError::InvalidValue`] for a value its schema does
    /// not take; `pend`'s error; unavailable when the profile kept changing;
    /// may have been written when the writer failed.
    #[allow(
        clippy::too_many_arguments,
        reason = "one write and its two callers' hooks"
    )]
    pub(crate) async fn replace<P, PF>(
        &self,
        writes: &ProfileWrites,
        key: SubjectKey,
        origin: &Origin,
        what: &'static str,
        fact: &NodeKey,
        property: &str,
        value: &Value,
        deadline: Instant,
        pend: P,
    ) -> Result<OneStepDone<Gone>, WriteError>
    where
        P: Fn(&NodeKey) -> PF,
        PF: Future<Output = Result<(), DomainError>>,
    {
        let catalog = self.catalogs.of(key).await?;
        let spec = catalog.kind(property).ok_or_else(|| {
            DomainError::internal(format!(
                "stored fact {} has property `{property}`, which is no kind of the schema",
                fact.0
            ))
        })?;
        spec.check(value)
            .map_err(|rule| DomainError::InvalidValue {
                field: "value",
                rule,
            })?;
        self.write(
            writes,
            key,
            origin,
            what,
            deadline,
            |current, _new_key| {
                let Some(old) = current.facts.into_iter().find(|old| &old.node_key == fact) else {
                    return Ok(Built::Skipped(Gone));
                };
                Ok(Built::Step(
                    old.category,
                    Change::ReplaceEntity {
                        // In place: the node keeps its links and the nodes it owns.
                        old: old.node_key.clone(),
                        key: old.node_key,
                        owner: old.owner,
                        properties: vec![Property::new(old.property, value.clone())],
                    },
                ))
            },
            pend,
        )
        .await
    }

    /// Read the profile, `build` the step on it under a new node key, run
    /// the checks and Admission, call `pend` with the new key, and write the
    /// plan against the version read; on a conflict, read and write again as
    /// [`ProfileWrites::with_reruns`] does.
    #[allow(
        clippy::too_many_arguments,
        reason = "the step's builder and the caller's hook"
    )]
    async fn write<E, B, P, PF>(
        &self,
        writes: &ProfileWrites,
        key: SubjectKey,
        origin: &Origin,
        what: &'static str,
        deadline: Instant,
        build: B,
        pend: P,
    ) -> Result<OneStepDone<E>, WriteError>
    where
        B: Fn(SubjectFacts, &NodeKey) -> Result<Built<E>, DomainError>,
        P: Fn(&NodeKey) -> PF,
        PF: Future<Output = Result<(), DomainError>>,
    {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        let (construct, scope) =
            ProfileWrites::before_write(deadline, what, writes.construct(key.tenant_id)).await?;
        let (construct, scope, build, pend) = (&construct, &scope, &build, &pend);
        // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        writes
            .with_reruns(deadline, what, move || async move {
                let prepared = ProfileWrites::before_write(deadline, what, async move {
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                    let current = writes.read(construct, key).await?;
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                    let new_key = NodeKey::new_in(key.tenant_id);
                    let version = current.version;
                    let (category, change) = match build(current, &new_key)? {
                        Built::Skipped(why) => {
                            return Ok(Prepared::Done(OneStepDone::Skipped(why)));
                        }
                        Built::Step(category, change) => (category, change),
                    };
                    let plan = Plan {
                        tenant_id: key.tenant_id,
                        subject_id: key.subject_id,
                        origin: origin.clone(),
                        profile_version: version,
                        steps: vec![Step {
                            entity_kind: category.name().to_owned(),
                            change,
                            confidence: None,
                        }],
                    };
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-admit
                    let not_stored = |why| Ok(Prepared::Done(OneStepDone::NotStored(why)));
                    let Checked::Verdicts(verdicts) = self.checks.run(&plan).await else {
                        return not_stored(NotStored::Dropped(DropReason::CheckFailed));
                    };
                    let admitted = match self
                        .admission
                        .admit(construct, scope, plan, &verdicts)
                        .await?
                    {
                        AdmissionOutcome::Admitted(admitted) => admitted.plan,
                        AdmissionOutcome::Dropped(reason) => {
                            return not_stored(NotStored::Dropped(reason));
                        }
                    };
                    // A blocked value drops the step, and only a stored
                    // step keeps its value.
                    let Some(property) = admitted
                        .steps
                        .first()
                        .and_then(|step| step.change.properties().first())
                        .cloned()
                    else {
                        return not_stored(NotStored::Blocked);
                    };
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-admit
                    let node_key = admitted
                        .steps
                        .first()
                        .and_then(|step| match &step.change {
                            Change::AddEntity { key, .. } | Change::ReplaceEntity { key, .. } => {
                                Some(key.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or(new_key);
                    Ok(Prepared::Write(
                        Box::new(admitted),
                        NewFact {
                            node_key,
                            category,
                            property,
                        },
                    ))
                })
                .await?;
                let (admitted, fact) = match prepared {
                    Prepared::Done(done) => return Ok(Attempt::Done(done)),
                    Prepared::Write(admitted, fact) => (admitted, fact),
                };
                // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-write
                pend(&fact.node_key).await?;
                let written = writes
                    .write(construct, &admitted, |now| {
                        now.facts.iter().any(|stored| {
                            stored.node_key == fact.node_key && stored.value == fact.property.value
                        })
                    })
                    .await?;
                Ok(match written {
                    Written::Stored => Attempt::Done(OneStepDone::Stored(fact)),
                    Written::Conflict => Attempt::Conflict,
                })
                // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-write
            })
            .await
    }
}

/// Whether two values name the same thing: text compared without case and spacing.
fn same_name(held: &Value, named: &Value) -> bool {
    match (held, named) {
        (Value::String(held), Value::String(named)) => {
            let comparable = |text: &str| {
                text.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
            };
            comparable(held) == comparable(named)
        }
        _ => held == named,
    }
}
