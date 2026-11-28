use serde_json::json;

use super::plan::NodeKey;
use super::profile::{Category, Fact, owned_by, ownership};

fn fact(key: &str, owner: Option<&str>) -> Fact {
    Fact {
        node_key: NodeKey(key.to_owned()),
        category: Category::PROJECT,
        property: "project".to_owned(),
        value: json!({ "name": key }),
        owner: owner.map(|owner| NodeKey(owner.to_owned())),
    }
}

fn owned(facts: &[Fact], node: &str) -> Vec<String> {
    let mut keys: Vec<String> = owned_by(&ownership(facts), &NodeKey(node.to_owned()))
        .into_iter()
        .map(|key| key.0)
        .collect();
    keys.sort();
    keys
}

#[test]
fn what_a_node_owns_is_found_however_deep_and_nothing_else() {
    let facts = [
        fact("a", None),
        fact("b", Some("a")),
        fact("c", Some("b")),
        fact("d", Some("c")),
        fact("other", None),
        fact("other-child", Some("other")),
    ];

    assert_eq!(owned(&facts, "a"), ["b", "c", "d"]);
    assert_eq!(owned(&facts, "b"), ["c", "d"]);
    assert!(owned(&facts, "d").is_empty());
}

#[test]
fn an_ownership_cycle_ends_the_walk_and_leaves_the_root_out() {
    let facts = [
        fact("a", Some("c")),
        fact("b", Some("a")),
        fact("c", Some("b")),
    ];

    assert_eq!(owned(&facts, "a"), ["b", "c"]);
}
