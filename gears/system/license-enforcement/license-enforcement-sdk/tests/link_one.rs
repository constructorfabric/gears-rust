//! A binary that links exactly one plugin and one evaluator.

mod common;

use std::sync::Arc;

use license_enforcement_sdk::{linked_admission_plugin, linked_evaluator};

license_enforcement_sdk::register_admission_plugin!(|| Arc::new(common::StubPlugin("one")));
license_enforcement_sdk::register_evaluator!(|| Arc::new(common::StubEvaluator));

#[test]
fn exactly_one_linked_is_returned() {
    assert_eq!(linked_admission_plugin().unwrap().info().id, "one");
    assert!(linked_evaluator().is_ok());
}
