//! The publish unit's vocabulary: what is published, on whose authority, and
//! what the commit hands back.
//!
//! `design/01-foundation.md` §4.2 is one path with five steps, and this module
//! names the three values that path carries. It runs no rule and touches no
//! store — those are [`crate::domain::plan_rules`] and
//! [`crate::infra::publish`] respectively — because the words have to be
//! agreed before either can be written against them.
//!
//! ## Two publish units, one type, and the criterion that decided it
//!
//! [`PlanPublishUnit`] carries a [`PublishUnitKind`] discriminator rather than a
//! type per unit, and the second unit is what decided it: D-99 makes every window
//! *mutation* — a schedule, a future-`effectiveTo` adjustment, a cancellation — a
//! publish unit through this same engine path.
//!
//! **The criterion is the subject, and it is the design set's own, not one
//! invented here.** `01-foundation.md` §3.7 keys `pricing_read_model` on
//! `subject_kind ∈ {plan, price_overlay, overlay_index, group_membership}` and
//! D-157 puts that same pair on `pricing_catalog_version_ref`, so **which subject
//! a unit projects is a declared, enumerated fact about it**. Hence:
//!
//! > A publish unit that projects a **different subject** gets its own type. A
//! > publish unit that projects the **same subject** and differs only in which of
//! > its facts moved gets a variant.
//!
//! Applied to the four kinds §4.2 names, the criterion says something different
//! about each rather than "add a variant every time":
//!
//! - **window mutations** (D-99) — *"windows are plan facts, so they need no
//!   subject kind of their own"*, verbatim. Same `plan` subject, same
//!   `(plan_id, revision)`, same re-projection. **A variant**, and it is here.
//! - **plan retirement** (D-128) — the `published → retired` flip re-projects the
//!   plan subject and pins that same revision number. Same subject: **a variant**,
//!   owed by the group that builds retirement.
//! - **the `PriceOverlay` unit** and **customer-group membership** (D-06) — each
//!   projects `price_overlay` / `group_membership`, and the overlay unit projects
//!   a *second* subject with it (the D-112/D-133 `overlay_index` shard). Different
//!   subjects, and one of them plural: **their own types**, with their own subject
//!   refs and their own revisions, when Slice 9 lands them.
//!
//! **What the rejected option would have cost.** A sibling `WindowPublishUnit`
//! would restate the plan identity, the revision, and every field
//! [`PublishService::commit`](crate::infra::publish::PublishService::commit) reads
//! off the unit — `unit.plan_id` for the assembly, `unit.revision` for the
//! shape-equality check that refuses a moved draft, both again for the pending
//! ref's `subject_revision` and for the registry request id. Two carriers of one
//! subject are two things free to disagree about **which revision a version
//! froze**, on the path whose whole discipline is that the commit flips exactly
//! the row set its re-validation judged (D-155). The discriminator makes that
//! disagreement unrepresentable: there is one subject, and the kind says only what
//! moved.
//!
//! **What the discriminator is deliberately not.** It is not a `subject_kind`.
//! Nothing here or downstream writes `window` into `pricing_read_model` or into
//! `pricing_catalog_version_ref` — D-99 forbids it in as many words — and nothing
//! here extends `AuditSubjectKind`, which D-158 binds to
//! `chk_pricing_approval_subject_kind` as one enumeration extended only beside its
//! writer.
//!
//! **What is not built.** The commit's own handling of a window mutation is not in
//! this group: [`PublishService::commit`](crate::infra::publish::PublishService::commit)
//! still assembles the plan's **open draft revision** and refuses a unit whose
//! revision is not the draft's, while a window mutation is a unit over the plan's
//! *current published* revision. So today the discriminator changes exactly one
//! observable thing — the registry `request_id`, so the two units of one revision
//! cannot dedup onto each other — and the group that builds `WindowService` owes
//! the commit path that reads the kind. **Reported**, rather than left for that
//! group to discover from a signature that already admits their unit.
//!
//! ## The approval gate is a value, not a call
//!
//! [`PublishAuthorization`] has no `Default` and no third arm, so
//! `PublishService::commit` cannot be called without a decision having been
//! made. That is the whole of what the commit genuinely needs from Slice 5:
//! **that it cannot proceed undecided**, and that whatever was decided reaches
//! the audit trail (`inst-au-complete`, `inst-tp-record`).
//!
//! What this deliberately is **not** is the approval workflow. Slice 5's
//! surface is the `MaterialityEvaluator` over a registered-trigger set owned by
//! eight slices (`inst-mat-registered` names D-10, D-13, D-50, D-62, D-104,
//! D-109 and D-115 in one paragraph), the `pricing_approval` store — which has
//! no migration and no entity in this crate — the content pin and its
//! `APPROVAL_CONTENT_MISMATCH`, the withdraw path and the approver scope check.
//! None of it is reachable from anywhere today, because this gear has no
//! authoring REST surface at all. Building it here would put that workflow in
//! front of the five moves the publish commit exists to make, and would shape it
//! with no caller to shape it against.
//!
//! The consequence is stated rather than hidden: **nothing in this crate can
//! mint a [`PublishAuthorization`] today except a test.** That costs exactly
//! nothing while no surface can reach the commit, and it becomes the approval
//! group's first obligation the moment one can.
//!
//! ## What is deliberately absent
//!
//! The two-person rule's `submitter != approver` check (`inst-tp-distinct`) is
//! **not** asserted by [`PublishAuthorization::approved`], even though the
//! comparison is one line. `inst-tp-selfaudit` binds the refusal to an audit
//! record of the *attempted violation*, written against the approval's own
//! subject — and there is no approval record and no approval `chain_id` for
//! that record to extend. Enforcing half the rule here would give it two
//! owners, free to disagree the day the other half lands, which is the defect
//! this crate's single-owner discipline exists to prevent. So the type
//! **carries** both principals, which is what makes the trail complete, and the
//! check plus its audited refusal stay whole in Slice 5's group.
//! `SELF_APPROVAL_FORBIDDEN` therefore gets no constant here: a code declared
//! where nothing raises it reads as enforcement to everyone who greps for it.

pub mod rules;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::evaluation_policy::EVALUATION_POLICY_GENERATION;
use crate::domain::scope_key::PlanId;
use crate::domain::snapshot::{PricingSnapshotRef, VersionRef};

/// Which of the plan subject's facts a publish unit moved.
///
/// The discriminator, and **only** a discriminator: it carries no second copy of
/// the subject, no revision and no state. See the module doc for the criterion
/// that made this a variant set rather than a second type.
#[domain_model]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PublishUnitKind {
    /// The plan's own content — the revision, its Slice-2 children and its price
    /// rows (§4.2 step 1's authored draft).
    ///
    /// The default because it is the unit the engine path was written for and the
    /// only one any surface can reach today.
    #[default]
    PlanContent,
    /// One window mutation: a schedule, a future-`effectiveTo` adjustment or a
    /// cancellation (D-99, `inst-ws-publishunit`).
    ///
    /// It names the window because two mutations of one plan at one revision are
    /// **two units** — a schedule on one key and a cancellation on another are
    /// separate change sets with separate materiality — and the id is what tells
    /// them apart in the registry request that makes each addressable.
    ///
    /// **Activation and expiry are not here, and their absence is the rule.**
    /// D-99: the read model carries window *intervals*, so a time-driven flip
    /// changes nothing projected and needs no version. A variant for them would be
    /// a shape saying the opposite.
    WindowMutation {
        /// The window whose interval or state the mutation moved.
        window_id: Uuid,
    },
}

impl PublishUnitKind {
    /// The token that discriminates one unit's registry request from another's.
    ///
    /// A **stable string**, and the plan-content token is `plan-publish` — the
    /// prefix `publish_request_id` already used before there was a kind at all.
    /// Preserved deliberately: `request_id` is the registry's own idempotency
    /// handle, so re-spelling it would make every outstanding pending ref
    /// unreachable by the retry that is supposed to find it. This is
    /// `PIN_DOMAIN_SEP`'s question one plane over, and the answer is the same —
    /// an encoding is re-frozen only when the re-freeze is the intent.
    #[must_use]
    pub fn request_token(self) -> String {
        match self {
            Self::PlanContent => "plan-publish".to_owned(),
            Self::WindowMutation { window_id } => format!("window-mutation/{window_id}"),
        }
    }
}

/// The subject of a publish, and which of its facts moved.
///
/// `(plan_id, revision)` is the durable name D-145 made permanent — the number
/// is minted once and never re-minted — so naming the unit by it is naming
/// exactly one row for the life of the plan. Every unit of this type names that
/// same subject; [`PlanPublishUnit::kind`] says what about it changed.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanPublishUnit {
    /// The plan being published.
    pub plan_id: PlanId,
    /// Which of its revisions.
    pub revision: u64,
    /// Which of the subject's facts this unit moved.
    pub kind: PublishUnitKind,
}

impl PlanPublishUnit {
    /// Name a plan-content publish unit — §4.2's ordinary path.
    ///
    /// **There is deliberately no `new`.** Both constructors are named for their
    /// kind, so every construction in the crate states which unit it is rather
    /// than inheriting a default from whichever arm happens to be first. That is
    /// the whole benefit a discriminator has over a second type: it is only worth
    /// having if the choice is visible where the unit is built.
    #[must_use]
    pub const fn plan_content(plan_id: PlanId, revision: u64) -> Self {
        Self {
            plan_id,
            revision,
            kind: PublishUnitKind::PlanContent,
        }
    }

    /// Name a window-mutation publish unit (D-99).
    ///
    /// `revision` is the plan's **current** revision — the one the mutation's
    /// re-projection will freeze — and not an open draft's: a window mutation
    /// authors no plan content and opens no revision.
    #[must_use]
    pub const fn window_mutation(plan_id: PlanId, revision: u64, window_id: Uuid) -> Self {
        Self {
            plan_id,
            revision,
            kind: PublishUnitKind::WindowMutation { window_id },
        }
    }
}

/// The approval decision a publish commit runs under (§4.2 step 3).
///
/// Two arms and no `Default`, so there is no way to reach the commit with the
/// question unanswered. The fail-safe direction is the design set's: the
/// two-person rule applies **unless** an explicit threshold is configured, the
/// change is below it, and it is not a first publish — so
/// [`PublishAuthorization::auto_publishable`] is the narrow case and
/// [`PublishAuthorization::approved`] is the ordinary one.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishAuthorization {
    /// A material change that cleared the two-person rule.
    Approved {
        /// The `pricing_approval` record this publish runs under.
        approval_ref: Uuid,
        /// The principal that submitted the change.
        submitter_principal: Uuid,
        /// The independent principal that approved it.
        approver_principal: Uuid,
        /// **The content that approval was over** — the record's pinned
        /// `content_hash` (`inst-ap-pin`).
        ///
        /// It is a member of the authorization rather than an argument beside
        /// it because it is what the authorization *is about*: a decision names
        /// a change set, and an `Approved` that named a record and not its
        /// content would be a permission to publish whatever the plan happens
        /// to hold at commit time.
        ///
        /// The window this closes is the one `inst-ap-pin` deliberately leaves
        /// open. That instruction scopes the TOCTOU void to a **`submitted`**
        /// record, in as many words, so a mutation after the approve does not
        /// void anything — correct as specified, and it leaves the approve and
        /// the commit with a gap between them. The commit's own
        /// compare-and-swap does not cover it either: it swaps on the
        /// *revision's* row version, and a price row edited inside the window
        /// moves the row's version and not the revision's. So the digest
        /// travels with the decision and
        /// [`PublishService::commit`](crate::infra::publish::PublishService::commit)
        /// re-derives it **inside its own transaction**, where nothing can move
        /// between the check and the freeze.
        content_hash: [u8; 32],
    },
    /// A below-threshold, non-first publish that needs no approver
    /// (`inst-ap-materiality`).
    AutoPublishable,
}

impl PublishAuthorization {
    /// A publish running under a closed approval record.
    ///
    /// **What the caller must already have established**, none of which this
    /// constructor re-checks: that `approval_ref` names a `pricing_approval`
    /// row in `approved` state whose `subject_ref` is the unit being published;
    /// that the approver holds the scope `inst-rb-approve` requires; and that
    /// the two principals are distinct (`inst-tp-distinct` — see the module doc
    /// for why that one is not asserted here).
    ///
    /// **One item is not on that list**, and it is the one that used to
    /// read "that its pinned `content_hash` still matches the submitted
    /// content". A precondition a caller establishes *before* calling is a
    /// precondition that stops holding the moment it returns, and this one has a
    /// live window on the other side of it. It is now carried instead —
    /// `content_hash` is a member — and re-derived inside the commit's
    /// transaction. See the field's own doc.
    #[must_use]
    pub const fn approved(
        approval_ref: Uuid,
        submitter_principal: Uuid,
        approver_principal: Uuid,
        content_hash: [u8; 32],
    ) -> Self {
        Self::Approved {
            approval_ref,
            submitter_principal,
            approver_principal,
            content_hash,
        }
    }

    /// The digest this authorization is over, when there is a decision behind
    /// it.
    ///
    /// `None` on the auto-publishable arm, and that is not a hole: nobody
    /// reviewed anything, so there is no content a second person was shown and
    /// nothing for a re-derivation to disagree with.
    #[must_use]
    pub const fn pinned_content_hash(&self) -> Option<[u8; 32]> {
        match self {
            Self::Approved { content_hash, .. } => Some(*content_hash),
            Self::AutoPublishable => None,
        }
    }

    /// A publish that needs no approver.
    ///
    /// **What the caller must already have established**: that the tenant has
    /// an explicitly configured approval threshold, that the
    /// `MaterialityEvaluator` scored this change below it, and that it is not a
    /// first publish. The fail-safe reading is load-bearing — a tenant with no
    /// configured threshold makes *everything* material (S5 G1), so an
    /// evaluator that cannot answer must not reach for this arm.
    #[must_use]
    pub const fn auto_publishable() -> Self {
        Self::AutoPublishable
    }

    /// The approval record backing this publish, when there is one.
    ///
    /// It is what lands in `pricing_audit_log.approval_ref`, so an auditor can
    /// walk from the mutation to the decision that permitted it.
    #[must_use]
    pub const fn approval_ref(&self) -> Option<Uuid> {
        match self {
            Self::Approved { approval_ref, .. } => Some(*approval_ref),
            Self::AutoPublishable => None,
        }
    }

    /// The `(submitter, approver)` pair, when the publish was approved.
    ///
    /// `inst-au-complete` requires the approval trail on the audit record and
    /// `inst-tp-record` requires both identities on it. An auto-publishable
    /// change has no second principal to record, and `None` says exactly that
    /// rather than repeating the actor as its own approver.
    #[must_use]
    pub const fn principals(&self) -> Option<(Uuid, Uuid)> {
        match self {
            Self::Approved {
                submitter_principal,
                approver_principal,
                ..
            } => Some((*submitter_principal, *approver_principal)),
            Self::AutoPublishable => None,
        }
    }
}

/// What a successful publish commit hands back.
///
/// Enough for a surface to answer §4.2's 202 and for a test to assert the
/// commit's five artifacts, and no more — it is deliberately not a second copy
/// of the row set, which the caller can read from the store it just wrote.
///
/// The version ref is **structurally pending**: [`PublishReceipt::new`] takes
/// the registry's handle and builds the [`VersionRef`] itself, so a receipt
/// carrying a committed version is not expressible. That is the G5/G6 seam as a
/// type — the commit holds a handle, and only `CatalogVersionPublished` turns a
/// handle into a version.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishReceipt {
    plan_id: PlanId,
    revision: u64,
    version_ref: VersionRef,
    published_price_ids: Vec<Uuid>,
    audit_seq: u64,
}

impl PublishReceipt {
    /// Stamp a receipt from what the commit produced.
    #[must_use]
    pub fn new(
        unit: PlanPublishUnit,
        pending_ref: String,
        published_price_ids: Vec<Uuid>,
        audit_seq: u64,
    ) -> Self {
        Self {
            plan_id: unit.plan_id,
            revision: unit.revision,
            version_ref: VersionRef::Pending(pending_ref),
            published_price_ids,
            audit_seq,
        }
    }

    /// The plan that was published.
    #[must_use]
    pub const fn plan_id(&self) -> PlanId {
        self.plan_id
    }

    /// The revision that became current.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The pending version ref the publish is addressable through until
    /// `CatalogVersionPublished` resolves it.
    #[must_use]
    pub const fn version_ref(&self) -> &VersionRef {
        &self.version_ref
    }

    /// The price rows this commit moved into `published`.
    #[must_use]
    pub fn published_price_ids(&self) -> &[Uuid] {
        &self.published_price_ids
    }

    /// The `seq` the plan's audit chain segment reached.
    #[must_use]
    pub const fn audit_seq(&self) -> u64 {
        self.audit_seq
    }

    /// The catalog-side `pricingSnapshotRef` segments this commit produced — all
    /// three (D-162), composed as one value.
    ///
    /// The evaluation-policy generation is taken from the constant rather than
    /// carried in the receipt: it is the publishing gear's, not the publish's,
    /// so there is no call site that could stamp a period with a semantics its
    /// rows were never frozen under. The version ref is structurally pending.
    ///
    /// **What finalizes that pending ref is not this type.** The live one-way
    /// pin is a row compare-and-swap on `pricing_catalog_version_ref` —
    /// [`catalog_version_ref_repo::finalize`], whose predicate is
    /// `catalog_version IS NULL` so two sweeps resolving one handle cannot both
    /// win — and its own doc calls
    /// [`VersionRef::finalize`](crate::domain::snapshot::VersionRef::finalize)
    /// "the storage-side sibling … not one mechanism and cannot be". It is **not**
    /// `CatalogVersionPublished` finalizing through
    /// [`PricingSnapshotRef::finalize`]: that method has no caller outside
    /// `domain::snapshot_tests`, so following it sends a reader looking for a
    /// finalizer on the live path that is not there.
    ///
    /// This constructor likewise has no consumer beyond `publish_tests` — the
    /// segments reach the wire through `outbox_repo`'s own composition. See
    /// [`crate::domain::snapshot`] for why the type is kept rather than built
    /// out or deleted.
    ///
    /// [`catalog_version_ref_repo::finalize`]: crate::infra::storage::repo::catalog_version_ref_repo::finalize
    #[must_use]
    pub fn snapshot_ref(&self) -> PricingSnapshotRef {
        PricingSnapshotRef::new(
            self.version_ref.clone(),
            self.published_price_ids.clone(),
            EVALUATION_POLICY_GENERATION.to_owned(),
        )
    }
}

/// One overlay publish unit — the overlay plane's analogue of
/// [`PlanPublishUnit`], and **deliberately not a value of it** (D-234).
///
/// That type is plan-shaped in every field: a plan id, one of its revisions, and
/// a kind discriminator whose variants are plan acts. An overlay has no plan, so
/// an overlay publish unit cannot be a value of it, and `inst-pl-commit`'s second
/// half is a new pipeline rather than a third constructor. Giving the overlay
/// plane its own unit type is what makes that a statement in the type system
/// instead of a paragraph somebody has to find.
///
/// It carries no kind discriminator. `PlanPublishUnit` needs one because a plan
/// revision can be published by several different acts — content, a D-99 window
/// mutation — which would otherwise present one registry request id; an overlay
/// revision publishes exactly once, through one act.
#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayPublishUnit {
    /// The overlay being published.
    pub price_overlay_id: Uuid,
    /// The revision this act freezes.
    pub revision: u64,
}

impl OverlayPublishUnit {
    /// The unit over one overlay revision.
    #[must_use]
    pub const fn new(price_overlay_id: Uuid, revision: u64) -> Self {
        Self {
            price_overlay_id,
            revision,
        }
    }
}

#[cfg(test)]
#[path = "publish_tests.rs"]
mod publish_tests;
