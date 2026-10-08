use std::collections::BTreeSet;

use construct_sdk::person_types::Cardinality;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use super::catalog::PropertyCatalog;
use crate::domain::model_client::{ToolCall, ToolSpec};
use crate::domain::plan::{NewFact, Plan, PlanOrigin, Step};
use crate::domain::profile::{Category, Fact, Profile, ProfileVersion};

pub const ADD: &str = "add";
pub const REPLACE: &str = "replace";
pub const REMOVE: &str = "remove";

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolRefusal {
    #[error("there is no tool `{0}`; use add, replace or remove")]
    UnknownTool(String),
    #[error("the arguments do not fit the tool: {0}")]
    BadArguments(String),
    #[error("confidence must be a number from 0 to 1")]
    BadConfidence,
    #[error("there is no value {0} in the profile")]
    UnknownNumber(usize),
    #[error("value {0} already has a step in this plan")]
    NumberUsed(usize),
    #[error("there is no property `{property}`; use one of: {known}")]
    UnknownProperty { property: String, known: String },
    #[error("the value does not fit `{property}`: {reason}")]
    ValueDoesNotFit { property: String, reason: String },
    #[error("`{property}` holds one value, and value {number} is it; use replace {number}")]
    SecondValue { property: String, number: usize },
    #[error("`{property}` holds one value, and this plan already adds one")]
    SecondValueInPlan { property: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddArguments {
    property: String,
    value: Value,
    confidence: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceArguments {
    number: usize,
    value: Value,
    confidence: f64,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveArguments {
    number: usize,
    confidence: f64,
}

enum DraftStep {
    Add {
        category: Category,
        property: String,
        value: Value,
        confidence: f64,
    },
    Replace {
        number: usize,
        value: Value,
        confidence: f64,
    },
    Remove {
        number: usize,
        confidence: f64,
    },
}

/// @cpt-dod:cpt-cf-construct-dod-planner-numbered-profile:p1
pub struct PlanDraft<'a> {
    catalog: &'a PropertyCatalog,
    facts: Vec<Fact>,
    version: ProfileVersion,
    used: BTreeSet<usize>,
    steps: Vec<DraftStep>,
}

impl<'a> PlanDraft<'a> {
    #[must_use]
    pub fn new(catalog: &'a PropertyCatalog, profile: Profile) -> Self {
        Self {
            catalog,
            facts: profile.facts,
            version: profile.version,
            used: BTreeSet::new(),
            steps: Vec::new(),
        }
    }

    #[must_use]
    pub fn numbered_profile(&self) -> String {
        self.facts
            .iter()
            .enumerate()
            .map(|(index, fact)| {
                format!(
                    "{}. {} ({}): {}",
                    index + 1,
                    fact.property,
                    fact.category.name(),
                    shown(&fact.value)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[must_use]
    pub fn tools(&self) -> Vec<ToolSpec> {
        let confidence = json!({ "type": "number", "minimum": 0, "maximum": 1 });
        let number = json!({ "type": "integer", "minimum": 1 });
        let value = json!({ "description": "The value, in the form the property takes." });
        vec![
            ToolSpec {
                name: ADD.to_owned(),
                description: "Add a new value of a property to the profile.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "property": { "type": "string", "enum": self.catalog.names().collect::<Vec<_>>() },
                        "value": value,
                        "confidence": confidence,
                    },
                    "required": ["property", "value", "confidence"],
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: REPLACE.to_owned(),
                description: "Replace the profile value with this number by a new value."
                    .to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": { "number": number, "value": value, "confidence": confidence },
                    "required": ["number", "value", "confidence"],
                    "additionalProperties": false,
                }),
            },
            ToolSpec {
                name: REMOVE.to_owned(),
                description: "Remove the profile value with this number.".to_owned(),
                parameters: json!({
                    "type": "object",
                    "properties": { "number": number, "confidence": confidence },
                    "required": ["number", "confidence"],
                    "additionalProperties": false,
                }),
            },
        ]
    }

    /// @cpt-dod:cpt-cf-construct-dod-planner-tools:p1
    pub fn apply(&mut self, call: &ToolCall) -> Result<String, ToolRefusal> {
        match call.name.as_str() {
            ADD => {
                let arguments: AddArguments = arguments(&call.arguments)?;
                self.add(arguments)
            }
            REPLACE => {
                let arguments: ReplaceArguments = arguments(&call.arguments)?;
                self.replace(arguments)
            }
            REMOVE => {
                let arguments: RemoveArguments = arguments(&call.arguments)?;
                self.remove(arguments)
            }
            other => Err(ToolRefusal::UnknownTool(other.to_owned())),
        }
    }

    fn add(&mut self, arguments: AddArguments) -> Result<String, ToolRefusal> {
        let AddArguments {
            property,
            value,
            confidence,
        } = arguments;
        check_confidence(confidence)?;
        let Some(spec) = self.catalog.get(&property) else {
            return Err(ToolRefusal::UnknownProperty {
                property,
                known: self.catalog.names().collect::<Vec<_>>().join(", "),
            });
        };
        spec.check(&value)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit {
                property: property.clone(),
                reason,
            })?;
        if spec.cardinality == Cardinality::One {
            if let Some(number) = self.untouched_number(&property) {
                return Err(ToolRefusal::SecondValue { property, number });
            }
            if self.adds(&property) {
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
            .ok_or_else(|| ToolRefusal::UnknownProperty {
                property: property.clone(),
                known: self.catalog.names().collect::<Vec<_>>().join(", "),
            })?;
        spec.check(&value)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit { property, reason })?;
        self.used.insert(number);
        self.steps.push(DraftStep::Replace {
            number,
            value,
            confidence,
        });
        Ok(format!("replaced {number}"))
    }

    fn remove(&mut self, arguments: RemoveArguments) -> Result<String, ToolRefusal> {
        let RemoveArguments { number, confidence } = arguments;
        check_confidence(confidence)?;
        self.free_fact(number)?;
        self.used.insert(number);
        self.steps.push(DraftStep::Remove { number, confidence });
        Ok(format!("removed {number}"))
    }

    fn free_fact(&self, number: usize) -> Result<&Fact, ToolRefusal> {
        let fact = number
            .checked_sub(1)
            .and_then(|index| self.facts.get(index))
            .ok_or(ToolRefusal::UnknownNumber(number))?;
        if self.used.contains(&number) {
            return Err(ToolRefusal::NumberUsed(number));
        }
        Ok(fact)
    }

    fn untouched_number(&self, property: &str) -> Option<usize> {
        self.facts
            .iter()
            .enumerate()
            .map(|(index, fact)| (index + 1, fact))
            .find(|(number, fact)| fact.property == property && !self.used.contains(number))
            .map(|(number, _)| number)
    }

    fn adds(&self, property: &str) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, DraftStep::Add { property: added, .. } if added == property))
    }

    /// @cpt-dod:cpt-cf-construct-dod-planner-plan:p1
    #[must_use]
    pub fn finish(self, tenant_id: Uuid, subject_id: Uuid, origin: PlanOrigin) -> Plan {
        let new_key = || format!("construct:{tenant_id}:{}", Uuid::new_v4());
        let fact = |number: usize| &self.facts[number - 1];
        let steps = self
            .steps
            .iter()
            .map(|step| match step {
                DraftStep::Add {
                    category,
                    property,
                    value,
                    confidence,
                } => Step::Add {
                    node_key: new_key(),
                    fact: NewFact {
                        category: *category,
                        property: property.clone(),
                        value: value.clone(),
                    },
                    confidence: *confidence,
                },
                DraftStep::Replace {
                    number,
                    value,
                    confidence,
                } => {
                    let old = fact(*number);
                    Step::Replace {
                        old_key: old.node_key.clone(),
                        node_key: new_key(),
                        fact: NewFact {
                            category: old.category,
                            property: old.property.clone(),
                            value: value.clone(),
                        },
                        confidence: *confidence,
                    }
                }
                DraftStep::Remove { number, confidence } => Step::Remove {
                    node_key: fact(*number).node_key.clone(),
                    confidence: *confidence,
                },
            })
            .collect();
        Plan {
            tenant_id,
            subject_id,
            origin,
            profile_version: self.version,
            steps,
        }
    }
}

fn arguments<T: DeserializeOwned>(arguments: &Value) -> Result<T, ToolRefusal> {
    T::deserialize(arguments).map_err(|error| ToolRefusal::BadArguments(error.to_string()))
}

fn check_confidence(confidence: f64) -> Result<(), ToolRefusal> {
    if (0.0..=1.0).contains(&confidence) {
        Ok(())
    } else {
        Err(ToolRefusal::BadConfidence)
    }
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
