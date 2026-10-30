//! A binary that links two plugins and two evaluators.

mod common;

use std::sync::Arc;

use license_enforcement_sdk::{LinkError, linked_admission_plugin, linked_evaluator};

license_enforcement_sdk::register_admission_plugin!(|| Arc::new(common::StubPlugin("a")));
license_enforcement_sdk::register_admission_plugin!(|| Arc::new(common::StubPlugin("b")));
license_enforcement_sdk::register_evaluator!(|| Arc::new(common::StubEvaluator));
license_enforcement_sdk::register_evaluator!(|| Arc::new(common::StubEvaluator));

#[test]
fn two_linked_is_an_error() {
    assert_eq!(linked_admission_plugin().err(), Some(LinkError::Many(2)));
    assert_eq!(linked_evaluator().err(), Some(LinkError::Many(2)));
}
