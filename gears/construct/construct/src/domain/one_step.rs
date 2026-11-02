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
use super::planner::catalog::{PropertyCatalog, PropertySpec};
use super::planner::processing::Written;
use super::profile::Category;
use super::profile_writes::{Attempt, ProfileWrites, WriteError};
use super::record_intake::DropReason;
use super::sensitive_data::{Checked, SensitiveDataChecks};
use super::subject_settings::SubjectKey;
use construct_sdk::person_types::Cardinality;

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
    pub catalog: Arc<PropertyCatalog>,
}

/// The one step to write.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub(crate) enum OneStep<'a> {
    /// A new fact holding `property` with `value`.
    Add { property: &'a str, value: &'a Value },
    /// A new fact in place of `fact`, holding its property with `value`.
    Replace { fact: &'a NodeKey, value: &'a Value },
}

impl OneStep<'_> {
    const fn what(self) -> &'static str {
        match self {
            Self::Add { .. } => "add",
            Self::Replace { .. } => "edit",
        }
    }
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

/// What a one-step write did.
#[domain_model]
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum OneStepDone {
    Stored(NewFact),
    /// The fact to replace is gone; nothing was stored.
    Gone,
    /// The subject already holds this value, or a value of this one-valued
    /// property, as the named fact; nothing was stored.
    Held(NodeKey),
    NotStored(NotStored),
}

/// A plan ready to write, or why there is none.
#[domain_model]
enum Prepared {
    Done(OneStepDone),
    Write(Box<Plan>, NewFact),
}

impl OneStepPath {
    /// The schema of `property`, when a person type has it.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] for a property no person type has, or
    /// a value its schema does not take.
    pub(crate) fn check(
        &self,
        property: &str,
        value: &Value,
    ) -> Result<&PropertySpec, DomainError> {
        let spec = self
            .catalog
            .get(property)
            .ok_or_else(|| DomainError::InvalidValue {
                field: "property",
                rule: format!("no person type has the property `{property}`"),
            })?;
        spec.check(value)
            .map_err(|rule| DomainError::InvalidValue {
                field: "value",
                rule,
            })?;
        Ok(spec)
    }

    /// Write `step` for the subject `key` as `origin`: read the profile,
    /// build the plan with a new node key, run the checks and Admission, call
    /// `pend` with the new key, and write it against the version read; on a
    /// conflict, read and write again as [`ProfileWrites::with_reruns`] does.
    ///
    /// # Errors
    ///
    /// [`DomainError::InvalidValue`] for a property or value the person types
    /// do not take; `pend`'s error; unavailable when the profile kept
    /// changing; may have been written when the writer failed.
    pub(crate) async fn write<P, PF>(
        &self,
        writes: &ProfileWrites,
        key: SubjectKey,
        origin: &Origin,
        step: OneStep<'_>,
        deadline: Instant,
        pend: P,
    ) -> Result<OneStepDone, WriteError>
    where
        P: Fn(&NodeKey) -> PF,
        PF: Future<Output = Result<(), DomainError>>,
    {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        let what = step.what();
        let (construct, scope) = writes.construct(key.tenant_id).await?;
        let (construct, scope, pend) = (&construct, &scope, &pend);
        // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-start
        writes
            .with_reruns(deadline, what, move || async move {
                let prepared = ProfileWrites::before_write(deadline, what, async move {
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                    let current = writes.read(construct, key).await?;
                    // @cpt-end:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-read
                    // @cpt-begin:cpt-cf-construct-algo-review-queue-correct:p1:inst-correct-plan
                    let new_key = NodeKey::new_in(key.tenant_id);
                    let (category, change) = match step {
                        OneStep::Add { property, value } => {
                            let spec = self.check(property, value)?;
                            let held = current.facts.iter().find(|fact| {
                                fact.property == property
                                    && (spec.cardinality == Cardinality::One
                                        || &fact.value == value)
                            });
                            if let Some(held) = held {
                                return Ok(Prepared::Done(OneStepDone::Held(
                                    held.node_key.clone(),
                                )));
                            }
                            (
                                spec.category,
                                Change::AddEntity {
                                    key: new_key.clone(),
                                    properties: vec![Property::new(property, value.clone())],
                                },
                            )
                        }
                        OneStep::Replace { fact, value } => {
                            let Some(old) =
                                current.facts.into_iter().find(|old| &old.node_key == fact)
                            else {
                                return Ok(Prepared::Done(OneStepDone::Gone));
                            };
                            self.check(&old.property, value)?;
                            (
                                old.category,
                                Change::ReplaceEntity {
                                    old: old.node_key,
                                    key: new_key.clone(),
                                    properties: vec![Property::new(old.property, value.clone())],
                                },
                            )
                        }
                    };
                    let plan = Plan {
                        tenant_id: key.tenant_id,
                        subject_id: key.subject_id,
                        origin: origin.clone(),
                        profile_version: current.version,
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
                    Ok(Prepared::Write(
                        Box::new(admitted),
                        NewFact {
                            node_key: new_key,
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
                        now.facts
                            .iter()
                            .any(|stored| stored.node_key == fact.node_key)
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
