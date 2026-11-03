use construct_sdk::person_types::Cardinality;
use serde_json::Value;
use toolkit_macros::domain_model;

use super::catalog::PropertyCatalog;
use super::tools::{
    ADD, AddArguments, REMOVE, REPLACE, RemoveArguments, ReplaceArguments, ToolRefusal, echoed,
    parse_arguments, tool_specs,
};
use crate::domain::model_client::{ToolCall, ToolSpec};
use crate::domain::profile::{Category, Fact, Profile};

#[domain_model]
#[derive(Debug)]
pub(super) enum DraftStep {
    Add {
        category: Category,
        property: String,
        value: Value,
        confidence: f32,
    },
    Replace {
        old: Fact,
        value: Value,
        confidence: f32,
    },
    Remove {
        old: Fact,
        confidence: f32,
    },
}

/// A plan being built from the model's tool calls. Each step takes its profile value out of the draft, so a number
/// names one step at most.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-numbered-profile:p1
#[domain_model]
#[derive(Debug)]
pub struct PlanDraft<'a> {
    catalog: &'a PropertyCatalog,
    facts: Vec<Option<Fact>>,
    pub(super) version: Option<u64>,
    pub(super) steps: Vec<DraftStep>,
}

impl<'a> PlanDraft<'a> {
    /// A draft over `profile`, with no steps yet.
    #[must_use]
    pub fn new(catalog: &'a PropertyCatalog, profile: Profile) -> Self {
        Self {
            catalog,
            facts: profile.facts.into_iter().map(Some).collect(),
            version: profile.version,
            steps: Vec::new(),
        }
    }

    /// The profile as the model sees it: one line per value, `n. property (category): value`, with every value as
    /// JSON, so a line break in a value cannot start another line.
    #[must_use]
    pub fn numbered_profile(&self) -> String {
        self.untouched()
            .map(|(number, fact)| {
                format!(
                    "{number}. {} ({}): {}",
                    fact.property,
                    fact.category.name(),
                    fact.value
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The tools the model may call on this draft.
    #[must_use]
    pub fn tools(&self) -> Vec<ToolSpec> {
        tool_specs(self.catalog)
    }

    /// Applies one tool call.
    ///
    /// # Errors
    ///
    /// [`ToolRefusal`] when the call breaks a rule; the draft does not change.
    ///
    /// @cpt-dod:cpt-cf-construct-dod-planner-tools:p1
    pub fn apply(&mut self, call: &ToolCall) -> Result<String, ToolRefusal> {
        match call.name.as_str() {
            ADD => self.add(parse_arguments(&call.arguments)?),
            REPLACE => self.replace(parse_arguments(&call.arguments)?),
            REMOVE => self.remove(parse_arguments(&call.arguments)?),
            other => Err(ToolRefusal::UnknownTool(echoed(other))),
        }
    }

    fn add(&mut self, arguments: AddArguments) -> Result<String, ToolRefusal> {
        let AddArguments {
            property,
            value,
            confidence,
        } = arguments;
        check_confidence(confidence)?;
        let spec = self
            .catalog
            .get(&property)
            .ok_or_else(|| self.unknown(&property))?;
        spec.check(&value)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit {
                property: property.clone(),
                reason,
            })?;
        if let Some(number) = self.held_number(&property, &value, None) {
            return Err(ToolRefusal::AlreadyHeld { number });
        }
        if self.plan_writes(&property, Some(&value)) {
            return Err(ToolRefusal::AlreadyAdded { property });
        }
        if spec.cardinality == Cardinality::One {
            if let Some(number) = self.untouched_number(&property) {
                return Err(ToolRefusal::SecondValue { property, number });
            }
            if self.plan_writes(&property, None) {
                return Err(ToolRefusal::SecondValueInPlan { property });
            }
        }
        let added = format!("added {property}");
        self.steps.push(DraftStep::Add {
            category: spec.category,
            property,
            value,
            confidence,
        });
        Ok(added)
    }

    fn replace(&mut self, arguments: ReplaceArguments) -> Result<String, ToolRefusal> {
        let ReplaceArguments {
            number,
            value,
            confidence,
        } = arguments;
        check_confidence(confidence)?;
        let fact = self.free_fact(number)?;
        let property = fact.property.clone();
        let spec = self
            .catalog
            .get(&property)
            .ok_or_else(|| self.unknown(&property))?;
        spec.check(&value)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit {
                property: property.clone(),
                reason,
            })?;
        if fact.value == value {
            return Err(ToolRefusal::AlreadyHeld { number });
        }
        if let Some(held) = self.held_number(&property, &value, Some(number)) {
            return Err(ToolRefusal::AlreadyHeld { number: held });
        }
        if self.plan_writes(&property, Some(&value)) {
            return Err(ToolRefusal::AlreadyAdded { property });
        }
        let old = self.take(number)?;
        self.steps.push(DraftStep::Replace {
            old,
            value,
            confidence,
        });
        Ok(format!("replaced {number}"))
    }

    fn remove(&mut self, arguments: RemoveArguments) -> Result<String, ToolRefusal> {
        let RemoveArguments { number, confidence } = arguments;
        check_confidence(confidence)?;
        let old = self.take(number)?;
        self.steps.push(DraftStep::Remove { old, confidence });
        Ok(format!("removed {number}"))
    }

    fn unknown(&self, property: &str) -> ToolRefusal {
        ToolRefusal::UnknownProperty {
            property: echoed(property),
            known: self.catalog.names().collect::<Vec<_>>().join(", "),
        }
    }

    fn free_fact(&self, number: usize) -> Result<&Fact, ToolRefusal> {
        let slot = number
            .checked_sub(1)
            .and_then(|index| self.facts.get(index))
            .ok_or(ToolRefusal::UnknownNumber(number))?;
        slot.as_ref().ok_or(ToolRefusal::NumberUsed(number))
    }

    fn take(&mut self, number: usize) -> Result<Fact, ToolRefusal> {
        self.free_fact(number)?;
        number
            .checked_sub(1)
            .and_then(|index| self.facts.get_mut(index))
            .and_then(Option::take)
            .ok_or(ToolRefusal::NumberUsed(number))
    }

    fn untouched(&self) -> impl Iterator<Item = (usize, &Fact)> {
        self.facts
            .iter()
            .enumerate()
            .filter_map(|(index, fact)| fact.as_ref().map(|fact| (index + 1, fact)))
    }

    fn untouched_number(&self, property: &str) -> Option<usize> {
        self.untouched()
            .find(|(_, fact)| fact.property == property)
            .map(|(number, _)| number)
    }

    fn held_number(&self, property: &str, value: &Value, besides: Option<usize>) -> Option<usize> {
        self.untouched()
            .find(|(number, fact)| {
                Some(*number) != besides
                    && fact.property == property
                    && same_value(&fact.value, value)
            })
            .map(|(number, _)| number)
    }

    fn plan_writes(&self, property: &str, value: Option<&Value>) -> bool {
        self.steps.iter().any(|step| {
            let (written, written_value) = match step {
                DraftStep::Add {
                    property: written,
                    value,
                    ..
                } => (written, value),
                DraftStep::Replace { old, value, .. } => (&old.property, value),
                DraftStep::Remove { .. } => return false,
            };
            written == property && value.is_none_or(|value| same_value(written_value, value))
        })
    }
}

fn check_confidence(confidence: f32) -> Result<(), ToolRefusal> {
    if (0.0..=1.0).contains(&confidence) {
        Ok(())
    } else {
        Err(ToolRefusal::BadConfidence)
    }
}

/// Whether two values say the same, comparing text, also the text fields of an entry, without case and spacing.
fn same_value(stored: &Value, new: &Value) -> bool {
    match (stored, new) {
        (Value::String(stored), Value::String(new)) => comparable(stored) == comparable(new),
        (Value::Object(stored), Value::Object(new)) => {
            stored.len() == new.len()
                && stored.iter().all(|(name, field)| {
                    new.get(name).is_some_and(|other| same_value(field, other))
                })
        }
        _ => stored == new,
    }
}

fn comparable(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
