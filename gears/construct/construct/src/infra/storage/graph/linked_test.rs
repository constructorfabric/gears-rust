//! The linked profile over graph storage: nodes owned by nodes, links between them, a node changed where it
//! stands, and what goes with a removed node.

use serde_json::{Value, json};
use uuid::Uuid;

use super::graph_test::Profiles;
use crate::domain::plan::{Change, NodeKey, Property, Step};
use crate::domain::planner::processing::{PlanWriter, ProfileSource, Written};
use crate::domain::profile::Link;

fn key() -> NodeKey {
    NodeKey(format!("construct:test:{}", Uuid::new_v4()))
}

fn node(kind: &str, key: &NodeKey, owner: Option<&NodeKey>, value: Value) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::AddEntity {
            key: key.clone(),
            owner: owner.cloned(),
            properties: vec![Property::new(kind, value)],
        },
        confidence: Some(0.9),
    }
}

fn link(kind: &str, from: &NodeKey, to: &NodeKey, fields: Value) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::AddLink {
            kind: kind.to_owned(),
            from: from.clone(),
            to: to.clone(),
            properties: vec![Property::new(kind, fields)],
        },
        confidence: Some(0.9),
    }
}

fn unlink(kind: &str, from: &NodeKey, to: &NodeKey) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::RemoveLink {
            kind: kind.to_owned(),
            from: from.clone(),
            to: to.clone(),
        },
        confidence: Some(0.9),
    }
}

fn remove(key: &NodeKey) -> Step {
    Step {
        entity_kind: "project".to_owned(),
        change: Change::RemoveEntity {
            entity: key.clone(),
        },
        confidence: Some(0.9),
    }
}

fn replace(key: &NodeKey, kind: &str, value: Value) -> Step {
    Step {
        entity_kind: kind.to_owned(),
        change: Change::ReplaceEntity {
            old: key.clone(),
            key: key.clone(),
            owner: None,
            properties: vec![Property::new(kind, value)],
        },
        confidence: Some(0.9),
    }
}

/// Priya, the project she works on, the commitment the project owns and its two links.
struct Priya {
    person: NodeKey,
    project: NodeKey,
    promise: NodeKey,
}

impl Priya {
    fn new() -> Self {
        Self {
            person: key(),
            project: key(),
            promise: key(),
        }
    }

    fn steps(&self) -> Vec<Step> {
        vec![
            node(
                "person",
                &self.person,
                None,
                json!({ "name": "Priya Shah", "space": "work" }),
            ),
            node(
                "project",
                &self.project,
                None,
                json!({ "name": "Construct Browser onboarding" }),
            ),
            link(
                "works_on",
                &self.person,
                &self.project,
                json!({ "role": "lead" }),
            ),
            node(
                "commitment",
                &self.promise,
                Some(&self.project),
                json!({ "kind": "promise", "text": "send the interview script", "due": "2026-11-27" }),
            ),
            link("owner", &self.promise, &self.person, json!({})),
        ]
    }
}

#[tokio::test]
async fn nodes_and_links_are_written_in_one_plan_and_read_back_with_their_owner() {
    let profiles = Profiles::new();
    let priya = Priya::new();

    profiles.given(None, priya.steps()).await;

    let profile = profiles.read().await;
    assert_eq!(profile.facts.len(), 3);
    let by_key = |key: &NodeKey| {
        profile
            .facts
            .iter()
            .find(|fact| &fact.node_key == key)
            .unwrap()
    };
    assert_eq!(by_key(&priya.project).category.name(), "project");
    assert_eq!(by_key(&priya.promise).owner.as_ref(), Some(&priya.project));
    assert_eq!(by_key(&priya.person).owner, None);
    assert_eq!(by_key(&priya.promise).value["kind"], "promise");
    let mut links = profile.links.clone();
    links.sort_by(|a, b| a.kind.cmp(&b.kind));
    assert_eq!(
        links,
        [
            Link {
                kind: "owner".to_owned(),
                from: priya.promise.clone(),
                to: priya.person.clone(),
                fields: json!({}),
            },
            Link {
                kind: "works_on".to_owned(),
                from: priya.person.clone(),
                to: priya.project.clone(),
                fields: json!({ "role": "lead" }),
            },
        ]
    );
}

#[tokio::test]
async fn each_node_and_link_records_the_tool_and_the_message_it_came_from() {
    let profiles = Profiles::new();
    let priya = Priya::new();

    profiles.given(None, priya.steps()).await;

    for fact in profiles.stored().await {
        let [learned] = fact.learned.as_slice() else {
            panic!("one place: {fact:?}");
        };
        assert_eq!(learned.tool, "connector-a");
        assert_eq!(
            learned.message.as_deref(),
            Some("chat_engine/thread-42/msg-7")
        );
    }
    let stored = profiles
        .client
        .get_node(&profiles.ctx, &priya.person.0, None)
        .await
        .unwrap();
    assert_eq!(
        stored.payload.unwrap()["record"]["provenance"],
        "chat_engine/thread-42/msg-7"
    );
}

#[tokio::test]
async fn a_node_changed_where_it_stands_keeps_its_links_and_what_it_owns_and_adds_its_source() {
    let profiles = Profiles::new();
    let priya = Priya::new();
    profiles.given(None, priya.steps()).await;

    profiles
        .given(
            Some(1),
            vec![replace(
                &priya.project,
                "project",
                json!({ "name": "Construct Browser onboarding", "status": "active" }),
            )],
        )
        .await;

    let profile = profiles.read().await;
    assert_eq!(profile.facts.len(), 3);
    assert_eq!(profile.links.len(), 2, "the links stay");
    let project = profile
        .facts
        .iter()
        .find(|fact| fact.node_key == priya.project)
        .unwrap();
    assert_eq!(project.value["status"], "active");
    let promise = profile
        .facts
        .iter()
        .find(|fact| fact.node_key == priya.promise)
        .unwrap();
    assert_eq!(
        promise.owner.as_ref(),
        Some(&priya.project),
        "what it owns stays"
    );
    let stored = profiles
        .stored()
        .await
        .into_iter()
        .find(|fact| fact.node_key == priya.project)
        .unwrap();
    assert_eq!(stored.learned.len(), 2, "learned twice: {stored:?}");
}

#[tokio::test]
async fn a_removed_node_goes_with_the_nodes_it_owns_and_all_its_links_and_the_others_stay() {
    let profiles = Profiles::new();
    let priya = Priya::new();
    profiles.given(None, priya.steps()).await;

    profiles.given(Some(1), vec![remove(&priya.project)]).await;

    let profile = profiles.read().await;
    let keys: Vec<&NodeKey> = profile.facts.iter().map(|fact| &fact.node_key).collect();
    assert_eq!(keys, [&priya.person], "Priya stays");
    assert!(
        profile.links.is_empty(),
        "no link is left: {:?}",
        profile.links
    );
    // Soft deletes have run: the nodes are not live in graph storage.
    assert!(!profiles.is_live(&priya.project.0).await);
    assert!(!profiles.is_live(&priya.promise.0).await);
    assert!(profiles.is_live(&priya.person.0).await);
}

#[tokio::test]
async fn a_link_is_removed_alone() {
    let profiles = Profiles::new();
    let priya = Priya::new();
    profiles.given(None, priya.steps()).await;

    profiles
        .given(
            Some(1),
            vec![unlink("works_on", &priya.person, &priya.project)],
        )
        .await;

    let profile = profiles.read().await;
    assert_eq!(profile.facts.len(), 3);
    let kinds: Vec<&str> = profile
        .links
        .iter()
        .map(|link| link.kind.as_str())
        .collect();
    assert_eq!(kinds, ["owner"]);
}

#[tokio::test]
async fn the_kinds_of_two_people_in_one_tenant_do_not_collide() {
    let profiles = Profiles::new();
    let neighbour = profiles.neighbour();
    let (mine, theirs) = (Priya::new(), Priya::new());

    profiles.given(None, mine.steps()).await;
    neighbour.given(None, theirs.steps()).await;

    assert_eq!(profiles.read().await.facts.len(), 3);
    let other = neighbour.read().await;
    assert_eq!(other.facts.len(), 3);
    assert!(other.facts.iter().all(|fact| fact.node_key != mine.project));
    assert_eq!(other.links.len(), 2);
}

#[tokio::test]
async fn a_link_to_a_node_of_the_wrong_kind_is_refused_by_graph_storage() {
    let profiles = Profiles::new();
    let priya = Priya::new();
    // works_on goes from a person to a project, not from a project to a person.
    let mut steps = priya.steps();
    steps.push(link("works_on", &priya.project, &priya.person, json!({})));

    let written = profiles
        .writer()
        .write(&profiles.ctx, &profiles.plan(None, steps))
        .await;

    assert!(written.is_err(), "{written:?}");
    assert!(
        profiles
            .reader()
            .read(&profiles.ctx, profiles.tenant, profiles.subject)
            .await
            .unwrap()
            .facts
            .is_empty(),
        "nothing was stored"
    );
    let _ = Written::Stored;
}

#[tokio::test]
async fn a_forgotten_project_is_marked_with_its_message_and_that_message_does_not_create_it_again()
{
    use crate::domain::plan::Origin;
    use crate::domain::planner::catalog::Catalog;
    use crate::domain::planner::draft::PlanDraft;
    use crate::domain::planner::tools::ToolRefusal;

    let profiles = Profiles::new();
    let priya = Priya::new();
    profiles.given(None, priya.steps()).await;
    // The person's own forget, as Subject Control plans it.
    let mut forget = profiles.plan(Some(1), vec![remove(&priya.project)]);
    forget.origin = Origin::Subject {
        subject: profiles.subject,
    };

    let written = profiles
        .writer()
        .write(&profiles.ctx, &forget)
        .await
        .unwrap();

    assert_eq!(written, Written::Stored);
    let profile = profiles.read().await;
    // The project, and the commitment that went with it.
    let [mark, owned] = profile.marks.as_slice() else {
        panic!("two marks: {:?}", profile.marks);
    };
    assert_eq!(owned.kind, "commitment");
    assert_eq!(
        (
            mark.kind.as_str(),
            mark.key.as_str(),
            mark.tool.as_str(),
            mark.message.as_str()
        ),
        (
            "project",
            "construct browser onboarding",
            "connector-a",
            "chat_engine/thread-42/msg-7"
        )
    );
    let catalog = Catalog::load().unwrap();
    let add = crate::domain::model_client::ToolCall {
        id: "c".to_owned(),
        name: "add".to_owned(),
        arguments: json!({
            "kind": "project",
            "value": { "name": "Construct  Browser Onboarding" },
            "confidence": 0.9
        }),
    };
    // The same message, sent again, does not create it.
    let mut same = PlanDraft::new(&catalog, profile.clone())
        .for_message("connector-a", "chat_engine/thread-42/msg-7");
    assert_eq!(
        same.apply(&add),
        Err(ToolRefusal::Forgotten {
            kind: "project".to_owned()
        })
    );
    // Another message of the person's may.
    let mut other =
        PlanDraft::new(&catalog, profile).for_message("connector-a", "chat_engine/thread-42/msg-9");
    assert!(other.apply(&add).is_ok());
}

#[tokio::test]
async fn a_link_removed_and_linked_again_in_one_plan_stays() {
    let profiles = Profiles::new();
    let priya = Priya::new();
    profiles.given(None, priya.steps()).await;

    profiles
        .given(
            Some(1),
            vec![
                unlink("works_on", &priya.person, &priya.project),
                link(
                    "works_on",
                    &priya.person,
                    &priya.project,
                    json!({ "role": "advisor" }),
                ),
            ],
        )
        .await;

    let profile = profiles.read().await;
    let works_on: Vec<&Link> = profile
        .links
        .iter()
        .filter(|link| link.kind == "works_on")
        .collect();
    assert_eq!(works_on.len(), 1, "{:?}", profile.links);
    assert_eq!(works_on[0].fields, json!({ "role": "advisor" }));
}

#[tokio::test]
async fn a_record_with_a_long_provenance_is_stored_with_its_message_clipped() {
    use crate::domain::plan::Origin;
    let profiles = Profiles::new();
    let mut plan = profiles.plan(
        None,
        vec![node("project", &key(), None, json!({ "name": "Atlas" }))],
    );
    plan.origin = Origin::Connector {
        connector: "connector-a".to_owned(),
        provenance: "p".repeat(900),
        version: "v1".to_owned(),
        type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
    };

    let written = profiles.writer().write(&profiles.ctx, &plan).await.unwrap();

    assert_eq!(written, Written::Stored);
    let stored = profiles.stored().await;
    assert_eq!(
        stored[0].learned[0].message.as_deref().map(str::len),
        Some(500)
    );
}

#[test]
fn the_record_keeps_a_long_provenance_whole() {
    use crate::domain::plan::Origin;
    let profiles = Profiles::new();
    let mut plan = profiles.plan(None, Vec::new());
    plan.origin = Origin::Connector {
        connector: "connector-a".to_owned(),
        provenance: "p".repeat(1024),
        version: "v1".to_owned(),
        type_id: construct_sdk::gts::CHAT_MESSAGE_TYPE.to_owned(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH,
    };

    let record = super::fact::record_of(&plan).expect("a connector record");

    assert_eq!(record["provenance"].as_str().map(str::len), Some(1024));
}
