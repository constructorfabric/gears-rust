//! The declarative order state table (Foundation §4.3 *Order State Machine*; DESIGN §3.2 *State
//! table*; ADR-0004).
//!
//! The 29 rows are data, declared with the registered state/trigger/event tokens and compiled at
//! startup. Compilation resolves every token against the closed SDK registries and rejects
//! duplicate expanded `(from-state, trigger)` keys, terminal exits, the normative exclusions and
//! any drift between a row's event declaration and the event's declared source rows. An edge
//! that is not a row cannot be taken; the engine never infers one.
use std::collections::{BTreeMap, BTreeSet, HashMap};

use bss_orders_lifecycle_sdk::catalog::{EventKind, OrderState, Trigger};
use serde::de::DeserializeOwned;

use super::idempotency::WORKFLOW_CLASS;

/// Versioning behaviour of a row (§4.3): `append` rows create a commercial version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Versioning {
    Append,
    StateOnly,
}

/// Declared actor class of a row. `Authorized` rows are governed by the permission matrix
/// alone; the actor class grants nothing (08 §4.3) but a declared class must match the
/// configured identity of the caller (rows 6 and 24 `system`, rows 28 and 29 `user`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActorRule {
    Authorized,
    System,
    User,
}

/// Target of a row: a fixed state, the outgoing state (self-loop), or the stored pre-hold state
/// (row 22 only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    State(OrderState),
    Same,
    PreHold,
}

/// Source-row number, 1..=29.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RowId(u8);
impl RowId {
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
    /// A declared row number (validated against the compiled table where it matters).
    #[must_use]
    pub const fn of(number: u8) -> Self {
        Self(number)
    }
}
impl std::fmt::Display for RowId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "row {}", self.0)
    }
}

/// One declared row, in registered tokens. `from` is empty only for create (from nothing).
#[derive(Debug, Clone, Copy)]
pub struct RowDecl {
    pub row: u8,
    pub from: &'static [&'static str],
    pub to: &'static str,
    pub trigger: &'static str,
    pub versioning: Versioning,
    pub event: Option<&'static str>,
    pub actor: ActorRule,
}

/// Target tokens: a state token, `$same` (self-loop) or `$pre_hold_state` (row 22).
pub const SAME: &str = "$same";
pub const PRE_HOLD: &str = "$pre_hold_state";

const SUBMITTED_TO_APPROVED: &[&str] = &["submitted", "pending_approval", "approved"];
const NON_TERMINAL: &[&str] = &[
    "draft",
    "submitted",
    "pending_approval",
    "approved",
    "in_fulfillment",
    "on_hold",
];

const fn row(
    row: u8,
    from: &'static [&'static str],
    to: &'static str,
    trigger: &'static str,
    versioning: Versioning,
    event: Option<&'static str>,
) -> RowDecl {
    RowDecl {
        row,
        from,
        to,
        trigger,
        versioning,
        event,
        actor: ActorRule::Authorized,
    }
}
const fn with_actor(mut decl: RowDecl, actor: ActorRule) -> RowDecl {
    decl.actor = actor;
    decl
}

use Versioning::{Append, StateOnly};

/// The 29 normative rows of `cpt-cf-bss-orders-lifecycle-state-order-lifecycle`, verbatim.
pub const ROWS: [RowDecl; 29] = [
    row(1, &[], "draft", "create", Append, None),
    row(2, &["draft"], "draft", "draft-mutate", StateOnly, None),
    row(
        3,
        NON_TERMINAL,
        SAME,
        "administrative-edit",
        StateOnly,
        None,
    ),
    row(
        4,
        &["draft"],
        "submitted",
        "submit",
        Append,
        Some("OrderSubmitted"),
    ),
    row(
        5,
        &["draft"],
        "cancelled",
        "cancel",
        StateOnly,
        Some("OrderCancelled"),
    ),
    with_actor(
        row(
            6,
            &["draft"],
            "expired",
            "auto-void",
            StateOnly,
            Some("OrderExpired"),
        ),
        ActorRule::System,
    ),
    row(
        7,
        &["submitted"],
        "pending_approval",
        "reflect-approval-required",
        StateOnly,
        None,
    ),
    row(
        8,
        &["submitted"],
        "approved",
        "reflect-approval-not-required",
        StateOnly,
        Some("OrderApproved"),
    ),
    row(
        9,
        &["pending_approval"],
        "approved",
        "reflect-approval-granted",
        StateOnly,
        Some("OrderApproved"),
    ),
    row(
        10,
        &["pending_approval"],
        "rejected",
        "reflect-approval-denied",
        StateOnly,
        Some("OrderRejected"),
    ),
    row(
        11,
        &["approved"],
        "in_fulfillment",
        "begin-fulfillment",
        StateOnly,
        None,
    ),
    row(
        12,
        &["in_fulfillment"],
        "in_fulfillment",
        "report-spawn-signal",
        StateOnly,
        None,
    ),
    row(
        13,
        &["in_fulfillment"],
        "completed",
        "acknowledge-completed",
        StateOnly,
        Some("OrderCompleted"),
    ),
    row(
        14,
        &["in_fulfillment"],
        "fulfillment_failed",
        "acknowledge-failed",
        StateOnly,
        Some("OrderFulfillmentFailed"),
    ),
    row(
        15,
        &["in_fulfillment"],
        "cancelled",
        "cancel",
        StateOnly,
        Some("OrderCancelled"),
    ),
    row(
        16,
        &["in_fulfillment"],
        "cancelled",
        "cancel-workflow-mediated",
        StateOnly,
        Some("OrderCancelled"),
    ),
    row(
        17,
        SUBMITTED_TO_APPROVED,
        "cancelled",
        "cancel",
        StateOnly,
        Some("OrderCancelled"),
    ),
    row(
        18,
        &["submitted"],
        SAME,
        "amendment",
        Append,
        Some("OrderAmended"),
    ),
    row(
        19,
        &["pending_approval"],
        "submitted",
        "amendment",
        Append,
        Some("OrderAmended"),
    ),
    row(
        20,
        &["approved"],
        "submitted",
        "amendment",
        Append,
        Some("OrderAmended"),
    ),
    row(
        21,
        &[
            "submitted",
            "pending_approval",
            "approved",
            "in_fulfillment",
        ],
        "on_hold",
        "hold",
        StateOnly,
        Some("OrderHeld"),
    ),
    row(
        22,
        &["on_hold"],
        PRE_HOLD,
        "resume",
        StateOnly,
        Some("OrderResumed"),
    ),
    row(
        23,
        &["on_hold"],
        "cancelled",
        "cancel",
        StateOnly,
        Some("OrderCancelled"),
    ),
    with_actor(
        row(
            24,
            &["submitted", "pending_approval", "approved", "on_hold"],
            "expired",
            "expire",
            StateOnly,
            Some("OrderExpired"),
        ),
        ActorRule::System,
    ),
    row(
        25,
        &[
            "submitted",
            "pending_approval",
            "approved",
            "in_fulfillment",
            "on_hold",
        ],
        SAME,
        "record-acceptance",
        StateOnly,
        Some("OrderAcceptanceRecorded"),
    ),
    row(
        26,
        &["on_hold"],
        "fulfillment_failed",
        "acknowledge-failed",
        StateOnly,
        Some("OrderFulfillmentFailed"),
    ),
    row(
        27,
        &["on_hold"],
        "cancelled",
        "cancel-workflow-mediated",
        StateOnly,
        Some("OrderCancelled"),
    ),
    with_actor(
        row(
            28,
            &["in_fulfillment"],
            "fulfillment_failed",
            "force-fail-unreconciled",
            StateOnly,
            Some("OrderFulfillmentFailed"),
        ),
        ActorRule::User,
    ),
    with_actor(
        row(
            29,
            &["on_hold"],
            "fulfillment_failed",
            "force-fail-unreconciled",
            StateOnly,
            Some("OrderFulfillmentFailed"),
        ),
        ActorRule::User,
    ),
];

/// Declared source rows of each event (DESIGN §4.4 *The event set*; catalog `events`).
pub const EVENT_ROWS: [(&str, &[u8]); 11] = [
    ("OrderSubmitted", &[4]),
    ("OrderApproved", &[8, 9]),
    ("OrderRejected", &[10]),
    ("OrderAmended", &[18, 19, 20]),
    ("OrderHeld", &[21]),
    ("OrderResumed", &[22]),
    ("OrderCancelled", &[5, 15, 16, 17, 23, 27]),
    ("OrderExpired", &[6, 24]),
    ("OrderCompleted", &[13]),
    ("OrderFulfillmentFailed", &[14, 26, 28, 29]),
    ("OrderAcceptanceRecorded", &[25]),
];

/// Edges the design explicitly forbids (§4.3 *Normative exclusions*, D-109). Validation proves
/// no declaration introduces one.
pub const FORBIDDEN_EDGES: [(&str, &str); 4] = [
    ("in_fulfillment", "expire"),
    ("in_fulfillment", "amendment"),
    ("on_hold", "acknowledge-completed"),
    ("on_hold", "amendment"),
];

/// A compiled, validated row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: RowId,
    /// `None` for create only.
    pub from: Vec<Option<OrderState>>,
    pub target: Target,
    pub trigger: Trigger,
    pub versioning: Versioning,
    pub event: Option<EventKind>,
    pub actor: ActorRule,
}
impl Row {
    /// The effective target for an outgoing state (§3.6 step 14). Row 22's stored pre-hold
    /// state is supplied by the caller; `None` there means the stored target is missing.
    #[must_use]
    pub fn effective_target(
        &self,
        outgoing: OrderState,
        pre_hold: Option<OrderState>,
    ) -> Option<OrderState> {
        match self.target {
            Target::State(state) => Some(state),
            Target::Same => Some(outgoing),
            Target::PreHold => pre_hold,
        }
    }
    #[must_use]
    pub fn is_versioning(&self) -> bool {
        self.versioning == Versioning::Append
    }
}

/// Startup rejection of an invalid declaration set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TableError {
    #[error("row numbers must be exactly 1..=29 in order (found {0})")]
    RowNumbering(u8),
    #[error("{0}: unregistered token `{1}`")]
    UnknownToken(RowId, &'static str),
    #[error("duplicate expanded key ({from:?}, {trigger:?}) in {first} and {second}")]
    DuplicateKey {
        from: Option<OrderState>,
        trigger: Trigger,
        first: RowId,
        second: RowId,
    },
    #[error("{0}: only create starts from nothing, and create only from nothing")]
    CreateShape(RowId),
    #[error("{0}: a terminal state has no exit")]
    TerminalExit(RowId),
    #[error("{0}: only resume targets the stored pre-hold state, from on_hold")]
    PreHoldShape(RowId),
    #[error("trigger {0:?} has no row")]
    UnusedTrigger(Trigger),
    #[error("event {0:?} declared rows differ from the rows declaring it")]
    EventMismatch(EventKind),
    #[error("{0}: forbidden edge from {1:?}")]
    ForbiddenEdge(RowId, OrderState),
    #[error("{0}: versioning rows are exactly create, submit and amendment")]
    VersioningShape(RowId),
}

/// The compiled state table and its expanded lookup keys.
#[derive(Debug, Clone)]
pub struct StateTable {
    rows: Vec<Row>,
    keys: HashMap<(Option<OrderState>, Trigger), RowId>,
}

/// The terminal set (§4.3).
pub const TERMINAL: [OrderState; 5] = [
    OrderState::Completed,
    OrderState::Rejected,
    OrderState::Cancelled,
    OrderState::FulfillmentFailed,
    OrderState::Expired,
];

#[must_use]
pub fn is_terminal(state: OrderState) -> bool {
    TERMINAL.contains(&state)
}

/// The workflow-trigger class (DESIGN §4.1, D-110): version before admissibility.
#[must_use]
pub fn is_workflow_class(trigger: Trigger) -> bool {
    WORKFLOW_CLASS.contains(&trigger)
}

fn token<T: DeserializeOwned>(row: RowId, token: &'static str) -> Result<T, TableError> {
    serde_json::from_value(serde_json::Value::String(token.to_owned()))
        .map_err(|_| TableError::UnknownToken(row, token))
}

impl StateTable {
    /// Compile the normative declarations.
    ///
    /// # Errors
    /// Never for the shipped declarations (a unit test proves it); see [`Self::compile`].
    pub fn registered() -> Result<Self, TableError> {
        Self::compile(&ROWS, &EVENT_ROWS)
    }

    /// Compile and validate a declaration set.
    ///
    /// # Errors
    /// Any [`TableError`]; the gear refuses to start.
    pub fn compile(
        decls: &[RowDecl],
        event_rows: &[(&'static str, &'static [u8])],
    ) -> Result<Self, TableError> {
        let mut rows = Vec::with_capacity(decls.len());
        let mut keys = HashMap::new();
        for (index, decl) in decls.iter().enumerate() {
            let expected =
                u8::try_from(index + 1).map_err(|_| TableError::RowNumbering(decl.row))?;
            if decl.row != expected || decl.row > 29 {
                return Err(TableError::RowNumbering(decl.row));
            }
            let id = RowId(decl.row);
            let trigger: Trigger = token(id, decl.trigger)?;
            let from = if decl.from.is_empty() {
                vec![None]
            } else {
                decl.from
                    .iter()
                    .map(|t| token::<OrderState>(id, t).map(Some))
                    .collect::<Result<Vec<_>, _>>()?
            };
            let target = match decl.to {
                SAME => Target::Same,
                PRE_HOLD => Target::PreHold,
                state => Target::State(token(id, state)?),
            };
            let event = decl.event.map(|e| token::<EventKind>(id, e)).transpose()?;
            let is_create = trigger == Trigger::Create;
            if is_create != (from == [None]) {
                return Err(TableError::CreateShape(id));
            }
            if (target == Target::PreHold)
                != (trigger == Trigger::Resume && from == [Some(OrderState::OnHold)])
            {
                return Err(TableError::PreHoldShape(id));
            }
            let versioning_trigger = matches!(
                trigger,
                Trigger::Create | Trigger::Submit | Trigger::Amendment
            );
            if (decl.versioning == Versioning::Append) != versioning_trigger {
                return Err(TableError::VersioningShape(id));
            }
            for state in from.iter().flatten() {
                if is_terminal(*state) {
                    return Err(TableError::TerminalExit(id));
                }
                if FORBIDDEN_EDGES.iter().any(|(s, t)| {
                    token::<OrderState>(id, s).ok() == Some(*state)
                        && token::<Trigger>(id, t).ok() == Some(trigger)
                }) {
                    return Err(TableError::ForbiddenEdge(id, *state));
                }
            }
            for state in &from {
                if let Some(first) = keys.insert((*state, trigger), id) {
                    return Err(TableError::DuplicateKey {
                        from: *state,
                        trigger,
                        first,
                        second: id,
                    });
                }
            }
            rows.push(Row {
                id,
                from,
                target,
                trigger,
                versioning: decl.versioning,
                event,
                actor: decl.actor,
            });
        }
        if rows.len() != 29 {
            return Err(TableError::RowNumbering(
                u8::try_from(rows.len()).unwrap_or(u8::MAX),
            ));
        }
        let used: BTreeSet<_> = rows.iter().map(|r| trigger_index(r.trigger)).collect();
        if let Some(missing) = Trigger::ALL
            .iter()
            .find(|t| !used.contains(&trigger_index(**t)))
        {
            return Err(TableError::UnusedTrigger(*missing));
        }
        let mut declared: BTreeMap<usize, (EventKind, BTreeSet<u8>)> = BTreeMap::new();
        for (name, event_rows) in event_rows {
            let kind: EventKind = token(RowId(0), name)?;
            declared.insert(
                event_index(kind),
                (kind, event_rows.iter().copied().collect()),
            );
        }
        for kind in EventKind::ALL {
            let from_rows: BTreeSet<u8> = rows
                .iter()
                .filter(|r| r.event == Some(*kind))
                .map(|r| r.id.0)
                .collect();
            match declared.get(&event_index(*kind)) {
                Some((_, rows)) if *rows == from_rows && !rows.is_empty() => {}
                _ => return Err(TableError::EventMismatch(*kind)),
            }
        }
        Ok(Self { rows, keys })
    }

    /// The create row (dedicated create branch, D-105).
    #[must_use]
    pub fn create_row(&self) -> &Row {
        self.lookup_key(None, Trigger::Create)
            .unwrap_or_else(|| unreachable!("compile requires the create row"))
    }

    /// Admissibility (§3.6 step 10/11): the row for `(state, trigger)` or none.
    #[must_use]
    pub fn lookup(&self, state: OrderState, trigger: Trigger) -> Option<&Row> {
        self.lookup_key(Some(state), trigger)
    }

    fn lookup_key(&self, from: Option<OrderState>, trigger: Trigger) -> Option<&Row> {
        self.keys.get(&(from, trigger)).and_then(|id| self.row(*id))
    }

    #[must_use]
    pub fn row(&self, id: RowId) -> Option<&Row> {
        self.rows.get(usize::from(id.0).checked_sub(1)?)
    }

    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Every expanded `(from, trigger) -> row` key.
    pub fn expanded(&self) -> impl Iterator<Item = ((Option<OrderState>, Trigger), RowId)> + '_ {
        self.keys.iter().map(|(k, v)| (*k, *v))
    }
}

fn trigger_index(trigger: Trigger) -> usize {
    Trigger::ALL
        .iter()
        .position(|t| *t == trigger)
        .unwrap_or(usize::MAX)
}
fn event_index(kind: EventKind) -> usize {
    EventKind::ALL
        .iter()
        .position(|k| *k == kind)
        .unwrap_or(usize::MAX)
}

#[cfg(test)]
#[path = "state_table_tests.rs"]
mod tests;
