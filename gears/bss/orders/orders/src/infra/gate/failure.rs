//! The one failure log of the submit and commit paths (PR #5536 review n7-n11, n116, n117).
//!
//! An integration failure names the order, the attempt and the line it concerns (every one that
//! is known where it is raised), a static description and the cause's `Display`, never stored
//! content or a secret (D-44, D-218): the `Display` of a cause here is a static member name
//! (`MalformedRequests`), a codec category (`CodecError`) or a component name at most
//! (`AdmittedLineError`, `TotalsError`). The row read's `line_integration` (`infra/read.rs`) is
//! the precedent.
use std::fmt::Display;
use uuid::Uuid;

/// The identities known where a failure is raised.
#[derive(Clone, Copy, Default)]
#[expect(
    clippy::struct_field_names,
    reason = "the field names are the structured log fields operators query by"
)]
pub(super) struct Ids {
    pub order_id: Option<Uuid>,
    pub attempt_id: Option<Uuid>,
    pub line_id: Option<Uuid>,
}

impl Ids {
    /// Only the order is known.
    pub(super) fn order(order_id: Uuid) -> Self {
        Self {
            order_id: Some(order_id),
            ..Self::default()
        }
    }

    /// The order and the attempt are known.
    pub(super) fn attempt(order_id: Uuid, attempt_id: Uuid) -> Self {
        Self {
            order_id: Some(order_id),
            attempt_id: Some(attempt_id),
            line_id: None,
        }
    }

    /// The same identities narrowed to one line.
    pub(super) fn line(self, line_id: Uuid) -> Self {
        Self {
            line_id: Some(line_id),
            ..self
        }
    }
}

/// Log one submit/commit integration failure.
pub(super) fn submit_integration(what: &'static str, ids: Ids, cause: Option<&dyn Display>) {
    tracing::error!(
        target: "orders.gate.submit",
        what,
        order_id = ?ids.order_id,
        attempt_id = ?ids.attempt_id,
        line_id = ?ids.line_id,
        cause = cause.map(tracing::field::display),
        "bss-orders: submit integration failure"
    );
}
