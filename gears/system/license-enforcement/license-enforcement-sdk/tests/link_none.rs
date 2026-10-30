//! A binary that links no plugin and no evaluator.

use license_enforcement_sdk::{LinkError, linked_admission_plugin, linked_evaluator};

#[test]
fn nothing_linked_is_an_error() {
    assert_eq!(linked_admission_plugin().err(), Some(LinkError::None));
    assert_eq!(linked_evaluator().err(), Some(LinkError::None));
}
