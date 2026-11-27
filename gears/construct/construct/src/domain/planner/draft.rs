use std::collections::HashSet;

use construct_sdk::linked_schema::Owner;
use serde_json::{Value, json};
use toolkit_macros::domain_model;

use super::catalog::Catalog;
use super::tools::{
    ADD, AddArguments, LINK, LinkArguments, REMOVE, REPLACE, RemoveArguments, ReplaceArguments,
    ToolRefusal, echoed, parse_arguments, tool_specs,
};
use crate::domain::model_client::{ToolCall, ToolSpec};
use crate::domain::plan::NodeKey;
use crate::domain::profile::{Category, Fact, Link, Mark, Profile, owned_by, ownership};

/// A node a step names: one the profile holds, or one this plan adds, by its place among the added nodes.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NodeRef {
    Stored(NodeKey),
    New(usize),
}

#[domain_model]
#[derive(Debug)]
pub(super) enum DraftStep {
    Add {
        category: Category,
        value: Value,
        owner: Option<NodeRef>,
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
    Link {
        kind: String,
        from: NodeRef,
        to: NodeRef,
        fields: Value,
        confidence: f32,
    },
    Unlink {
        link: Link,
        confidence: f32,
    },
}

/// A node as a number names it: its category, its key and what names it.
struct Held {
    category: Category,
    key: NodeRef,
}

/// A plan being built from the model's tool calls. The profile's lines are the nodes, then the links, each
/// numbered from 1; the nodes this plan adds come after them. A step takes its line out of the draft, so a number
/// names one step at most.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-numbered-profile:p1
#[domain_model]
#[derive(Debug)]
pub struct PlanDraft<'a> {
    catalog: &'a Catalog,
    /// Every node of the profile, whether a step took it or not, by line number minus one.
    nodes: Vec<Fact>,
    taken: HashSet<usize>,
    /// The nodes that go with a node a step removes: they are taken, and no step names them, as the writer removes
    /// what a removed node owns.
    cascaded: HashSet<NodeKey>,
    links: Vec<Link>,
    taken_links: HashSet<usize>,
    /// What the person forgot, and the tool and message this plan comes from, if it comes from one.
    marks: Vec<Mark>,
    source: Option<(String, String)>,
    pub(super) version: Option<u64>,
    pub(super) steps: Vec<DraftStep>,
}

impl<'a> PlanDraft<'a> {
    /// A draft over `profile`, with no steps yet.
    #[must_use]
    pub fn new(catalog: &'a Catalog, profile: Profile) -> Self {
        Self {
            catalog,
            nodes: profile.facts,
            taken: HashSet::new(),
            cascaded: HashSet::new(),
            links: profile.links,
            taken_links: HashSet::new(),
            marks: profile.marks,
            source: None,
            version: profile.version,
            steps: Vec::new(),
        }
    }

    /// The draft of a plan that comes from the message `message` of the tool `tool`: a node the person forgot
    /// from that message is not added again.
    #[must_use]
    pub fn for_message(mut self, tool: &str, message: &str) -> Self {
        self.source = Some((tool.to_owned(), message.chars().take(500).collect()));
        self
    }

    fn added(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| matches!(step, DraftStep::Add { .. }))
            .count()
    }

    fn line_of_link(&self, index: usize) -> usize {
        self.nodes.len() + index + 1
    }

    fn line_of_new(&self, index: usize) -> usize {
        self.nodes.len() + self.links.len() + index + 1
    }

    /// The line number of the stored node under `key`.
    fn number_of(&self, key: &NodeKey) -> Option<usize> {
        self.nodes
            .iter()
            .position(|fact| &fact.node_key == key)
            .map(|index| index + 1)
    }

    /// The profile as the model sees it: one line per node and per link, with every value as JSON, so a line break
    /// in a value cannot start another line. A node reads `n. kind: {fields}`, an owned one
    /// `n. kind (owned by m): {fields}`, and a link `n. link kind from a to b: {fields}`.
    #[must_use]
    pub fn numbered_profile(&self) -> String {
        let nodes = self.nodes.iter().enumerate().filter_map(|(index, fact)| {
            if self.taken.contains(&index) {
                return None;
            }
            let owned = fact
                .owner
                .as_ref()
                .and_then(|owner| self.number_of(owner))
                .map(|owner| format!(" (owned by {owner})"))
                .unwrap_or_default();
            Some(format!(
                "{}. {}{owned}: {}",
                index + 1,
                fact.category.name(),
                fact.value
            ))
        });
        let links = self.links.iter().enumerate().filter_map(|(index, link)| {
            if self.taken_links.contains(&index) {
                return None;
            }
            let from = self.number_of(&link.from)?;
            let to = self.number_of(&link.to)?;
            if self.removed_key(&link.from) || self.removed_key(&link.to) {
                return None;
            }
            Some(format!(
                "{}. link {} from {from} to {to}: {}",
                self.line_of_link(index),
                link.kind,
                link.fields
            ))
        });
        nodes.chain(links).collect::<Vec<_>>().join("\n")
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
            LINK => self.link(parse_arguments(&call.arguments)?),
            REPLACE => self.replace(parse_arguments(&call.arguments)?),
            REMOVE => self.remove(parse_arguments(&call.arguments)?),
            other => Err(ToolRefusal::UnknownTool(echoed(other))),
        }
    }

    fn add(&mut self, arguments: AddArguments) -> Result<String, ToolRefusal> {
        let AddArguments {
            kind,
            value,
            owner,
            confidence,
        } = arguments;
        check_confidence(confidence)?;
        let spec = self
            .catalog
            .kind(&kind)
            .ok_or_else(|| self.unknown_kind(&kind))?;
        spec.check(&value)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit {
                kind: kind.clone(),
                reason,
            })?;
        let category = spec.category;
        let owner = self.owner_for(category, owner)?;
        if let Some(number) = self.held_number(category, &value, owner.as_ref(), None) {
            return Err(ToolRefusal::AlreadyExists { kind, number });
        }
        if let Some(number) = self.plan_adds(category, &value, owner.as_ref()) {
            return Err(ToolRefusal::AlreadyAdded { kind, number });
        }
        if self.forgotten(category, &value) {
            return Err(ToolRefusal::Forgotten { kind });
        }
        if self.catalog.node(category).single {
            if let Some(number) = self.held_single(category) {
                return Err(ToolRefusal::SecondNode { kind, number });
            }
            if self.steps.iter().any(
                |step| matches!(step, DraftStep::Add { category: added, .. } if *added == category),
            ) {
                return Err(ToolRefusal::SecondNodeInPlan { kind });
            }
        }
        let number = self.line_of_new(self.added());
        self.steps.push(DraftStep::Add {
            category,
            value,
            owner,
            confidence,
        });
        Ok(format!("added {kind} as {number}"))
    }

    /// The node an added node of `category` is owned by, as the call named it.
    fn owner_for(
        &self,
        category: Category,
        owner: Option<usize>,
    ) -> Result<Option<NodeRef>, ToolRefusal> {
        let kind = self.catalog.node(category);
        match (&kind.owner, owner) {
            (Owner::Root, None) => Ok(None),
            (Owner::Root, Some(_)) => Err(ToolRefusal::OwnerNotAllowed {
                kind: kind.name.to_owned(),
            }),
            (Owner::Kinds(owners), None) => Err(ToolRefusal::OwnerRequired {
                kind: kind.name.to_owned(),
                owners: owners.join(", "),
            }),
            (Owner::Kinds(owners), Some(number)) => {
                let held = self.node_by_number(number)?;
                if !owners.contains(&held.category.name()) {
                    return Err(ToolRefusal::WrongOwner {
                        kind: kind.name.to_owned(),
                        number,
                        found: held.category.name().to_owned(),
                        owners: owners.join(", "),
                    });
                }
                Ok(Some(held.key))
            }
        }
    }

    /// The node line `number` names: a stored node no step removes, or one this plan adds.
    fn node_by_number(&self, number: usize) -> Result<Held, ToolRefusal> {
        if let Some(index) = number
            .checked_sub(1)
            .filter(|index| *index < self.nodes.len())
        {
            let fact = &self.nodes[index];
            if self.removed_key(&fact.node_key) {
                return Err(ToolRefusal::NumberUsed(number));
            }
            return Ok(Held {
                category: fact.category,
                key: NodeRef::Stored(fact.node_key.clone()),
            });
        }
        let first_new = self.nodes.len() + self.links.len() + 1;
        if number > self.nodes.len() && number < first_new {
            return Err(ToolRefusal::NotANode(number));
        }
        let index = number
            .checked_sub(first_new)
            .ok_or(ToolRefusal::UnknownNumber(number))?;
        let step = self
            .steps
            .iter()
            .filter_map(|step| match step {
                DraftStep::Add { category, .. } => Some(*category),
                _ => None,
            })
            .nth(index)
            .ok_or(ToolRefusal::UnknownNumber(number))?;
        Ok(Held {
            category: step,
            key: NodeRef::New(index),
        })
    }

    fn removed_key(&self, key: &NodeKey) -> bool {
        self.cascaded.contains(key)
            || self.steps.iter().any(|step| match step {
                DraftStep::Remove { old, .. } => &old.node_key == key,
                _ => false,
            })
    }

    fn link(&mut self, arguments: LinkArguments) -> Result<String, ToolRefusal> {
        let LinkArguments {
            kind,
            from,
            to,
            fields,
            confidence,
        } = arguments;
        check_confidence(confidence)?;
        let spec = self
            .catalog
            .link(&kind)
            .ok_or_else(|| self.unknown_link_kind(&kind))?;
        let fields = fields.unwrap_or_else(|| json!({}));
        spec.check(&fields)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit {
                kind: kind.clone(),
                reason,
            })?;
        let (start, end) = (self.node_by_number(from)?, self.node_by_number(to)?);
        let link_kind = spec.kind();
        let ends_fit = link_kind.from.contains(&start.category.name())
            && link_kind.to.contains(&end.category.name());
        if !ends_fit {
            let (number, found) = if link_kind.from.contains(&start.category.name()) {
                (to, end.category)
            } else {
                (from, start.category)
            };
            return Err(ToolRefusal::LinkEnd {
                link: kind,
                from: link_kind.from.join(", "),
                to: link_kind.to.join(", "),
                number,
                found: found.name().to_owned(),
            });
        }
        let (start, end) = (start.key, end.key);
        if self.has_link(&kind, &start, &end) {
            return Err(ToolRefusal::LinkExists { kind, from, to });
        }
        self.steps.push(DraftStep::Link {
            kind: kind.clone(),
            from: start,
            to: end,
            fields,
            confidence,
        });
        Ok(format!("linked {from} -> {to} by {kind}"))
    }

    fn has_link(&self, kind: &str, from: &NodeRef, to: &NodeRef) -> bool {
        let stored = self.links.iter().enumerate().any(|(index, link)| {
            !self.taken_links.contains(&index)
                && link.kind == kind
                && NodeRef::Stored(link.from.clone()) == *from
                && NodeRef::Stored(link.to.clone()) == *to
        });
        stored
            || self.steps.iter().any(|step| {
                matches!(step, DraftStep::Link { kind: k, from: f, to: t, .. } if k == kind && f == from && t == to)
            })
    }

    fn replace(&mut self, arguments: ReplaceArguments) -> Result<String, ToolRefusal> {
        let ReplaceArguments {
            number,
            value,
            confidence,
        } = arguments;
        check_confidence(confidence)?;
        let index = self.free_node(number)?;
        let fact = &self.nodes[index];
        let category = fact.category;
        let spec = self
            .catalog
            .kind(category.name())
            .ok_or_else(|| self.unknown_kind(category.name()))?;
        spec.check(&value)
            .map_err(|reason| ToolRefusal::ValueDoesNotFit {
                kind: category.name().to_owned(),
                reason,
            })?;
        if fact.value == value {
            return Err(ToolRefusal::AlreadyHeld { number });
        }
        let owner = fact.owner.clone().map(NodeRef::Stored);
        if let Some(held) = self.held_number(category, &value, owner.as_ref(), Some(number)) {
            return Err(ToolRefusal::AlreadyHeld { number: held });
        }
        if let Some(held) = self.plan_adds(category, &value, owner.as_ref()) {
            return Err(ToolRefusal::AlreadyHeld { number: held });
        }
        let old = fact.clone();
        self.taken.insert(index);
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
        if number > self.nodes.len() && number <= self.nodes.len() + self.links.len() {
            let index = number - self.nodes.len() - 1;
            if self.taken_links.contains(&index) {
                return Err(ToolRefusal::NumberUsed(number));
            }
            let link = self.links[index].clone();
            self.taken_links.insert(index);
            self.steps.push(DraftStep::Unlink { link, confidence });
            return Ok(format!("removed {number}"));
        }
        let index = self.free_node(number)?;
        // A node goes with the nodes it owns, however deep: the writer removes them with it. One already changed in
        // this plan stops the removal: a number names one step.
        let mut gone = vec![index];
        let owners = ownership(&self.nodes);
        for key in owned_by(&owners, &self.nodes[index].node_key) {
            // The walk names only keys of `self.nodes`, so each has a place.
            let Some(owned) = self.nodes.iter().position(|fact| fact.node_key == key) else {
                continue;
            };
            if self.taken.contains(&owned) {
                return Err(ToolRefusal::NumberUsed(owned + 1));
            }
            gone.push(owned);
        }
        for step in &self.steps {
            if let DraftStep::Add {
                owner: Some(NodeRef::Stored(owner)),
                ..
            } = step
                && let Some(at) = gone.iter().find(|at| &self.nodes[**at].node_key == owner)
            {
                return Err(ToolRefusal::OwnsNewNodes(*at + 1));
            }
        }
        for index in gone.iter().skip(1) {
            self.taken.insert(*index);
            self.cascaded.insert(self.nodes[*index].node_key.clone());
        }
        let old = self.nodes[index].clone();
        self.taken.insert(index);
        self.steps.push(DraftStep::Remove { old, confidence });
        Ok(format!("removed {number}"))
    }

    fn unknown_kind(&self, kind: &str) -> ToolRefusal {
        ToolRefusal::UnknownKind {
            kind: echoed(kind),
            known: self.catalog.kind_names().collect::<Vec<_>>().join(", "),
        }
    }

    fn unknown_link_kind(&self, kind: &str) -> ToolRefusal {
        ToolRefusal::UnknownLinkKind {
            kind: echoed(kind),
            known: self.catalog.link_names().collect::<Vec<_>>().join(", "),
        }
    }

    /// The index of the node line `number` if no step took it.
    fn free_node(&self, number: usize) -> Result<usize, ToolRefusal> {
        let index = number
            .checked_sub(1)
            .ok_or(ToolRefusal::UnknownNumber(number))?;
        if index >= self.nodes.len() {
            return if index < self.nodes.len() + self.links.len() {
                Err(ToolRefusal::NotANode(number))
            } else {
                Err(ToolRefusal::UnknownNumber(number))
            };
        }
        if self.taken.contains(&index) {
            return Err(ToolRefusal::NumberUsed(number));
        }
        Ok(index)
    }

    fn untouched(&self) -> impl Iterator<Item = (usize, &Fact)> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(index, _)| !self.taken.contains(index))
            .map(|(index, fact)| (index + 1, fact))
    }

    /// The line of the node of a single kind the profile holds after this plan: one changed in place counts, one
    /// removed does not.
    fn held_single(&self, category: Category) -> Option<usize> {
        self.nodes
            .iter()
            .position(|fact| fact.category == category && !self.removed_key(&fact.node_key))
            .map(|index| index + 1)
    }

    /// The line of a node of `category` and `owner` that names the same thing as `value`: the same value of the kind's
    /// key property, without case and spacing. A node of an entry kind is the same only under the same owner.
    fn held_number(
        &self,
        category: Category,
        value: &Value,
        owner: Option<&NodeRef>,
        besides: Option<usize>,
    ) -> Option<usize> {
        let key = self.catalog.node(category).key;
        let named = value.get(key)?;
        self.untouched()
            .find(|(number, fact)| {
                Some(*number) != besides
                    && fact.category == category
                    && fact
                        .value
                        .get(key)
                        .is_some_and(|held| same_value(held, named))
                    && fact.owner.clone().map(NodeRef::Stored).as_ref() == owner
            })
            .map(|(number, _)| number)
    }

    /// Whether the person forgot a node of this kind and name that this plan's message taught.
    fn forgotten(&self, category: Category, value: &Value) -> bool {
        let (Some((tool, message)), Some(key)) = (
            &self.source,
            Mark::identity(self.catalog.node(category).key, value),
        ) else {
            return false;
        };
        self.marks.iter().any(|mark| {
            mark.kind == category.name()
                && mark.key == key
                && &mark.tool == tool
                && &mark.message == message
        })
    }

    /// The line of a node this plan adds, or changes in place, that already has this kind, owner and name.
    fn plan_adds(
        &self,
        category: Category,
        value: &Value,
        owner: Option<&NodeRef>,
    ) -> Option<usize> {
        let key = self.catalog.node(category).key;
        let named = value.get(key)?;
        let same = |held: &Value| held.get(key).is_some_and(|held| same_value(held, named));
        let mut added = 0;
        for step in &self.steps {
            match step {
                DraftStep::Add {
                    category: other,
                    value: other_value,
                    owner: other_owner,
                    ..
                } => {
                    if *other == category && other_owner.as_ref() == owner && same(other_value) {
                        return Some(self.line_of_new(added));
                    }
                    added += 1;
                }
                DraftStep::Replace {
                    old,
                    value: other_value,
                    ..
                } if old.category == category
                    && old.owner.clone().map(NodeRef::Stored).as_ref() == owner
                    && same(other_value) =>
                {
                    return self.number_of(&old.node_key);
                }
                _ => {}
            }
        }
        None
    }
}

fn check_confidence(confidence: f32) -> Result<(), ToolRefusal> {
    if (0.0..=1.0).contains(&confidence) {
        Ok(())
    } else {
        Err(ToolRefusal::BadConfidence)
    }
}

/// Whether two values say the same, comparing text without case and spacing.
fn same_value(stored: &Value, new: &Value) -> bool {
    match (stored, new) {
        (Value::String(stored), Value::String(new)) => comparable(stored) == comparable(new),
        _ => stored == new,
    }
}

fn comparable(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
