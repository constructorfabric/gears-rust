//! Create `bss.pricing_catalog_version_ref` — the **pending vs committed**
//! `CatalogVersion` linkage, one row per publish
//! (`design/01-foundation.md` §3.7).
//!
//! The registry (Product & SKU) is the sole incrementer and batches approved
//! publishes (D-47), so between the publish commit and
//! `CatalogVersionPublished` a publish genuinely has an identity — the
//! registry's **pending handle** — with no version number yet. This table is
//! where that handle lives and where it is later resolved to the committed
//! version, which is what lets `pricingSnapshotRef` be stamped at publish and
//! finalized afterwards.
//!
//! `chk_pricing_catalog_version_ref_commit` keeps the commit atomic
//! in the row: the version and the commit instant are set together or not at
//! all, so no row can claim a version with no record of when it was assigned.
//! It is what stops a half-finalize, and it is untouched.
//!
//! # The version index is **not** unique, and the unique one it replaces made
//! batching unbuildable (amended 2026-08-03, building the projector)
//!
//! This table carried
//! `uq_pricing_catalog_version_ref_version (tenant_id, catalog_version) WHERE
//! catalog_version IS NOT NULL`, on the argument that it "makes the mapping a
//! bijection per tenant — two pending handles resolving to one committed
//! version would make 'which publish produced this version' unanswerable".
//!
//! **Under batching that index refuses the normal case.** §4.2 step 5 and §3.6:
//! the registry is the sole incrementer and **batches approved publishes**
//! before emitting `CatalogVersionPublished`. `../PRD.md` says the consequence
//! without room for reading — "under batching a version bundles many
//! `PlanPublished` events, so there is no per-`PlanPublished` marker" — and
//! §4.4 and D-101 speak throughout of "**every** subject row that version
//! projects", plural, while D-157 has the projector arriving "holding
//! **committed refs**", plural. Two publishes of one tenant landing in one
//! batch is the case the whole D-47 batching model exists to serve, and the
//! unique index made the second finalize fail.
//!
//! The purpose it was defending is already served by something better. D-157
//! put `subject_kind` / `subject_ref` on this row, so "which publish produced
//! this version" is answered by the row itself — and the honest answer is
//! legitimately a **set**, which is exactly what the projected-row-set rule
//! requires it to be. What must stay one-way is the *finalize*, and that is a
//! property of the compare-and-swap in
//! [`catalog_version_ref_repo`](crate::infra::storage::repo::catalog_version_ref_repo)
//! plus the `CHECK` above, not of a uniqueness constraint over versions.
//!
//! `idx_pricing_catalog_version_ref_version` replaces it, non-unique, over the
//! same `(tenant_id, catalog_version)` columns and with no partial predicate:
//! the projector reads every ref of a version and the frontier walk reads the
//! smallest committed version above a watermark, so the index earns its place
//! on reads it is now the only support for.
//!
//! **Reported as a divergence, not resolved by editing the design set.** No
//! section states the bijection; the index asserted it, and the batching rule
//! two sections away contradicts it.
//!
//! # The ref names its subject, and it has to (added 2026-08-03, building the
//! publish commit)
//!
//! `subject_kind` and `subject_ref` are on this table because the projector
//! cannot do its job without them and nothing else can carry them. §4.4 requires
//! the projection at `CatalogVersionPublished` to write "exactly the subjects of
//! the publish units that produced" a version (D-86/D-91) — and the projector
//! arrives holding a batch of **committed refs**. Without a subject on the ref
//! row there is no path at all from a `pending_ref` back to what it published.
//!
//! **Reported as a divergence, not resolved by editing the design set.** §3.7
//! describes this table as "`pending` vs `committed` version linkage per
//! publish" and lists neither column, while the requirement that forces them
//! lives a section away in §4.4's projected-row-set rule.
//!
//! The rejected alternative was the **outbox row**: `pricing_outbox.aggregate_id`
//! is the plan id, its payload carries the pending ref, and nothing deletes
//! outbox rows today. That makes a delivery queue the projector's durable index
//! — one table with two unrelated contracts — and the first compaction of
//! delivered history would silently remove the projector's input. The ref row is
//! the projector's own; nothing drains it.
//!
//! `subject_kind`'s `CHECK` is **the same four tokens** `pricing_read_model`
//! carries, deliberately: the ref names the subject the projector will write,
//! so two vocabularies would be two answers to one question. Both are rendered
//! from `domain::read_model::SubjectKind::as_str`.
//!
//! # The ref names the revision its publish judged (added 2026-08-03)
//!
//! `subject_revision` is here for the same shape of reason `subject_kind` and
//! `subject_ref` are, and it closes a defect that was reachable rather than
//! hypothetical. §4.4 says the projection source is the plan's **current**
//! revision, and the projector arrives up to the D-47 maximum batching delay —
//! **five minutes** — after the commit. A second publish of the same plan
//! inside that window makes its own revision current, so projecting from
//! "current" freezes content the earlier version's publish never judged. The
//! damage is permanent: a delta row is INSERT-only on the seven-year truth
//! horizon, in a store whose entire contract is that a completed version never
//! changes.
//!
//! The revision is a property of the **publish unit**, not of the subject's
//! identity, so carrying it here disturbs nothing about D-86/D-91's subject
//! typing — that keying is `pricing_read_model`'s, and it is untouched.
//!
//! Nullable, because only a revisioned subject kind has one; a `plan` subject
//! arriving without it is refused by the projector rather than defaulted, a
//! default being a guess about which content a frozen version froze.
//!
//! `subject_lifecycle_state` is the same fact one field over, and it closes the
//! residue pinning the revision left. Reading the pinned revision's state as it
//! **now** stands lets a third value into a delta: `superseded`, which
//! `plan_repo::load_current` could never return and which D-128 does not
//! contemplate for a projected subject — its clause names `published` **or**
//! `retired`, and D-90 calls "the published revision" the input to sellability
//! predicate (4), so a consumer coding that predicate as "is published" reads
//! the version as unsellable. Permanently, on an INSERT-only row.
//!
//! So the state the publish **judged** is frozen with the revision it judged,
//! and the `CHECK` admits exactly the two tokens D-128 sanctions. That is
//! coherent with the rest of the design rather than an exception to it:
//! retirement is a publish unit of its own (D-128) that re-projects the plan
//! subject, so a `retired` state arrives carrying **its own** version instead of
//! leaking backwards into an older one. Every unit that re-projects a plan
//! subject knows this value at commit — the publish commit after its own flip,
//! retirement after its own, a D-99 window mutation from the revision it read —
//! so there is no unit for which the column is unknowable.
//!
//! **Reported as a divergence.** §3.7 lists neither this column nor the two
//! D-157 added, and §4.4's "current revision" rule is what it contradicts.
//!
//! # `commit_observed_at` is the instant every post-commit clause was missing
//! (added 2026-08-03, D-166)
//!
//! The set names three signals for a publish that has not become visible, and
//! two of them had no measurable start. `fr-publish-fanout-atomicity` puts
//! `PlanPublishDegraded` **after** the commit and explicitly puts the pre-commit
//! batching wait outside it — and the only instants this table held were
//! `requested_at`, stamped when the handle was asked for, and `committed_at`,
//! stamped by the **finalize**, which is the very step that never runs on the
//! path where the warm is failing. Measured from `requested_at` the 5s rule
//! marks **every** publish degraded, including every one behaving exactly as
//! D-47 budgets, so the clause distinguishing a fault from a budgeted wait was
//! unbuildable and `PlanPublishDegraded` and `commit_overdue` collapsed into one
//! predicate.
//!
//! `commit_observed_at` is when this gear **first saw** the registry's answer for
//! the ref, written by the sweep independently of whether the projection then
//! lands. It has to be recorded rather than derived, and the reason is precise:
//! the projector writes a subject's delta warm in the **same transaction that
//! finalizes its ref**, so "committed but unwarm" is not a state this store can
//! hold. Without the column there is nothing on the row that distinguishes "the
//! registry has not answered" from "the registry answered and the warm keeps
//! failing".
//!
//! It is an **upper bound**: the observation lags the registry's real commit by
//! at most one sweep pass, so a signal derived from it is late rather than
//! false. The accurate form is the registry supplying its own commit instant
//! with the version, which is the registry gear's owed half of D-163's adoption.
//!
//! **No `CHECK` pairs it with anything, deliberately.**
//! `chk_pricing_catalog_version_ref_commit` exists because a version and its
//! commit instant are one fact; this column is the opposite kind of thing — its
//! whole purpose is to be settable while `catalog_version` is still NULL, which
//! is the state it was added to describe.
//!
//! **Amended in place rather than fixed up.** The chain has never been deployed
//! and every environment and every test creates it from scratch, so a trailing
//! `ALTER TABLE` migration would add a step that only exists to describe a
//! history nothing lived through.
//!
//! **Backend differences.** None beyond the systematic type mirror; both
//! backends express the version index and every row `CHECK` the `CREATE TABLE`
//! statements below declare.
//!
//! # The key carries the subject, because one handle covers a unit's subjects
//!
//! `pricing_catalog_version_ref`'s primary key gains the subject — one pending
//! handle, every subject the publish unit that requested it projects (D-234).
//!
//! A key of `(tenant_id, pending_ref)` is one row per handle, and that is not the
//! general shape. A plan publish projects exactly one subject; an **overlay** publish
//! unit projects two — the overlay document and the D-112/D-133 `overlay_index` shard
//! — and three when a revision moves the scope value, since D-133 has a commit
//! rewriting *"exactly one shard (two when a revision moves the scope value)"* beside
//! the document itself.
//!
//! The pair of *columns* was never the obstacle: `subject_revision` and
//! `subject_lifecycle_state` are per **row**, so N subject rows carry N pairs. What
//! could not hold two subjects was the key.
//!
//! # Two handles is not the way out, and the reason is a pin
//!
//! The obvious alternative is one handle per subject — no key change, two registry
//! requests, two rows. It fails on the thing the index exists for. D-112 added the
//! `overlay_index` subject as evaluation's **enumeration access path**, and the
//! registry batches (D-47), so two handles of one act may resolve to two versions.
//! A pin landing between them resolves the overlay document as live while the shard
//! at or below the pin still does not list it: enumeration and resolution
//! disagreeing at one pin, permanently, on INSERT-only rows. One act, one registry
//! request, one handle, N subject rows.
//!
//! # What the widened key still refuses, unchanged
//!
//! `record_pending`'s contract is that a second record of one assignment is refused
//! rather than upserted — "a handle arriving twice means two publish transactions
//! believe they own the same assignment, and silently overwriting the first would
//! hand one publish's subject to the other's version". That protection is about a
//! **subject** claiming a handle twice, and the widened key keeps it at exactly that
//! granularity: `(tenant_id, pending_ref, subject_kind, subject_ref)` still refuses
//! the duplicate and now admits the sibling.
//!
//! # Why a column here rather than a revision table there
//!
//! The plan and overlay planes pin a **revision**, because their content is
//! revision-scoped and immutable once published: the ref names which immutable
//! rows to read and the projector reads them. `pricing_group_membership` has no
//! such table — a membership is minted by `enroll` and thereafter mutated **in
//! place**, and `group_membership_repo`'s only in-place mutation is
//! `end_membership` narrowing `effective_to` (`inst-ms-time`: "ending early =
//! setting `to`"; a *move* is an end plus a new row, never an edit of an
//! existing one). So the immutable half of a membership is the row itself and
//! the moving half is one column, which is precisely the shape
//! `subject_lifecycle_state` already has on the plan plane: the fact that moves
//! is frozen on the ref, the facts that cannot are read from the truth row.
//!
//! Giving the membership plane a revision-scoped content table was the
//! alternative and was rejected as a second store for content that has exactly
//! one mutable field: it would duplicate every membership row per mutation to
//! carry a value the ref row already has a place for, and put the truth and the
//! copy in a position to disagree.
//!
//! # `subject_revision` carries the row version, and that is what makes NULL
//! readable
//!
//! `NULL` in `subject_effective_to` has to mean *"judged open-ended"* — the
//! ordinary state of a live membership — so it cannot also mean "no pin was
//! written". The membership arm therefore pins the row's `row_version` in
//! `subject_revision` (the column's own doc calls it "the revision of the
//! subject the publish unit judged", which is what a row version is for a row
//! that has no revisions), and the projector **refuses** a membership subject
//! arriving without one rather than defaulting — `pinned_revision`'s existing
//! rule, applied to the kind that had been exempt from it. A pin's absence is
//! then a fault this gear can name, and its `NULL` end is a fact.
//!
//! Dependency level 0.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_catalog_version_ref (
            tenant_id               uuid        NOT NULL,
            pending_ref             text        NOT NULL,
            subject_kind            text        NOT NULL,
            subject_ref             text        NOT NULL,
            catalog_version         bigint,
            commit_observed_at      timestamptz,
            committed_at            timestamptz,
            requested_at            timestamptz NOT NULL DEFAULT now(),
            subject_effective_to    timestamptz,
            subject_lifecycle_state text,
            subject_revision        bigint,
            CONSTRAINT chk_pricing_catalog_version_ref_commit CHECK ((catalog_version IS NULL) = (committed_at IS NULL)),
            CONSTRAINT chk_pricing_catalog_version_ref_subject_kind CHECK (subject_kind IN ('plan','price_overlay','overlay_index','group_membership')),
            CONSTRAINT chk_pricing_catalog_version_ref_subject_lifecycle CHECK (subject_lifecycle_state IS NULL OR subject_lifecycle_state IN ('published','retired')),
            CONSTRAINT chk_pricing_catalog_version_ref_subject_revision CHECK (subject_revision IS NULL OR subject_revision >= 0),
            CONSTRAINT chk_pricing_catalog_version_ref_version CHECK (catalog_version IS NULL OR catalog_version >= 0),
            CONSTRAINT pricing_catalog_version_ref_pkey PRIMARY KEY (tenant_id, pending_ref, subject_kind, subject_ref)
        )",
    "CREATE INDEX idx_pricing_catalog_version_ref_version ON bss.pricing_catalog_version_ref USING btree (tenant_id, catalog_version)",
];

const PG_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS bss.pricing_catalog_version_ref"];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_catalog_version_ref (
            tenant_id               text   NOT NULL,
            pending_ref             text   NOT NULL,
            subject_kind            text   NOT NULL,
            subject_ref             text   NOT NULL,
            catalog_version         bigint,
            commit_observed_at      text,
            committed_at            text,
            requested_at            text   NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%S', 'now') || '+00:00'),
            subject_effective_to    text,
            subject_lifecycle_state text,
            subject_revision        bigint,
            PRIMARY KEY (tenant_id, pending_ref, subject_kind, subject_ref),
            CONSTRAINT chk_pricing_catalog_version_ref_commit CHECK ((catalog_version IS NULL) = (committed_at IS NULL)),
            CONSTRAINT chk_pricing_catalog_version_ref_subject_kind CHECK (subject_kind IN ('plan','price_overlay','overlay_index','group_membership')),
            CONSTRAINT chk_pricing_catalog_version_ref_subject_lifecycle CHECK (subject_lifecycle_state IS NULL OR subject_lifecycle_state IN ('published','retired')),
            CONSTRAINT chk_pricing_catalog_version_ref_subject_revision CHECK (subject_revision IS NULL OR subject_revision >= 0),
            CONSTRAINT chk_pricing_catalog_version_ref_version CHECK (catalog_version IS NULL OR catalog_version >= 0)
        )",
    "CREATE INDEX idx_pricing_catalog_version_ref_version ON pricing_catalog_version_ref (tenant_id, catalog_version)",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_catalog_version_ref"];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(self.name(), manager, PG_UP_STATEMENTS, SQLITE_UP_STATEMENTS).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        super::exec_backend(
            self.name(),
            manager,
            PG_DOWN_STATEMENTS,
            SQLITE_DOWN_STATEMENTS,
        )
        .await
    }
}
