#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

const WIDGET: &str = "gts.cf.core.example.widget.v1~";
const DERIVED: &str = "gts.cf.core.example.widget.v1~cf.core.example.special.v1~";
const OTHER: &str = "gts.cf.core.example.gadget.v1~";

fn patterns(list: &[&str]) -> Vec<GtsIdPattern> {
    list.iter()
        .map(|p| GtsIdPattern::try_new(p).unwrap())
        .collect()
}

fn applies(types: &[&str], actions: &[&str], type_id: &str, action: &str) -> bool {
    let actions: Vec<String> = actions.iter().map(|a| (*a).to_owned()).collect();
    document_applies(
        &patterns(types),
        &actions,
        &GtsId::try_new(type_id).unwrap(),
        action,
    )
}

#[test]
fn resource_types_match_concrete_ids_and_wildcards() {
    assert!(applies(&[WIDGET], &[], WIDGET, "create"));
    assert!(applies(&[WIDGET], &[], DERIVED, "create"), "derived type");
    assert!(applies(&["gts.cf.core.example.*"], &[], OTHER, "create"));
    assert!(applies(&[OTHER, WIDGET], &[], WIDGET, "create"));
    assert!(!applies(&[OTHER], &[], WIDGET, "create"));
    assert!(!applies(&[], &[], WIDGET, "create"), "no resource types");
}

#[test]
fn empty_actions_mean_every_action() {
    assert!(applies(&[WIDGET], &[], WIDGET, "delete"));
    assert!(applies(&[WIDGET], &["create", "delete"], WIDGET, "delete"));
    assert!(!applies(&[WIDGET], &["create"], WIDGET, "delete"));
}
