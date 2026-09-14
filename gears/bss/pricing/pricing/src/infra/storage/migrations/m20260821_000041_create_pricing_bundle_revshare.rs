//! Create `bss.pricing_bundle_revshare` — one rev-share **party** row within one
//! group of one bundle revision (`design/08-bundles.md` §6, D-07 + D-92 +
//! D-105), keyed `(bundle_id, plan_revision, vendor_sku_id, party)`.
//!
//! # Two share columns, and the difference between them is the whole of D-07
//!
//! `share_bp` is what the operator **typed**; `effective_share_bp` is what the
//! publish **normalized**. Authoring accepts
//! `|Σ(share_bp) + platform_cut_bp − 10000| ≤ 1 bp`, because percentages arrive
//! from contracts as 33.33% and three of those are 9999 bp; publish then adjusts
//! the group's `residual_absorber_party`'s effective share so the published
//! shares sum to **exactly** 10000 bp per group. The typed values are retained
//! for audit — that is the entire reason there are two columns rather than one
//! rewritten in place — and downstream consumers (Tariffs, Marketplace) read
//! only the effective ones.
//!
//! `effective_share_bp` is therefore **nullable**: an unpublished draft has no
//! normalized answer yet, and defaulting it to the typed value would make "not
//! yet reconciled" indistinguishable from "reconciled to exactly what was
//! typed", which is the common case for every party that is not the absorber.
//!
//! # The group foreign key is what makes an implicit platform cut impossible
//!
//! `(bundle_id, plan_revision, vendor_sku_id)` references
//! `pricing_bundle_revshare_group`, so a party row cannot exist without its
//! group — and the group is where `platform_cut_bp` lives. `inst-rs-sum` requires
//! *"an explicit per-group platform cut"*, and this is the physical half of it:
//! there is no state in which shares are authored against a total whose platform
//! cut nobody stated. `REVSHARE_UNBALANCED` remains the report line for the
//! malformations a foreign key cannot see.
//!
//! There is deliberately **no** foreign key onto `pricing_bundle` here, though
//! `bundle_id` is a column: the group reference already covers it transitively,
//! and a second path to the same parent is a second thing that can be true.
//!
//! # Rev-share is a `sum_of_parts` property (D-55) and the schema does not say so
//!
//! An `own_price` bundle has one bundle amount and no per-vendor-SKU revenue to
//! allocate, so `own_price` + rev-share fails publish
//! (`REVSHARE_BASIS_UNSUPPORTED`, 422). That is a cross-table property —
//! `price_basis` lives on `pricing_bundle` — and a `CHECK` sees one row, so it is
//! `inst-rs-sum`'s and not this table's. Recorded here because the absence looks
//! like an oversight otherwise.
//!
//! **Backend differences and the append-only discipline** are
//! `pricing_bundle_component`'s, verbatim and for the same reasons.
//!
//! # `party` is held to both of `Party::new`'s refusals
//!
//! `chk_pricing_bundle_revshare_party` trims against a named character set —
//! ASCII whitespace entire, `pricing_region_taxonomy`'s set and its argument
//! (D-242) — and applies **both** clauses to the trimmed value: it must be
//! non-blank, and it must not be `PLATFORM_SENTINEL`. That is exactly what
//! `domain::bundle::Party::new` refuses, and each clause needs the trim for its own
//! reason.
//!
//! The second one is the sharper of the two. `party <> 'platform'` compares the
//! **stored** text, so `' platform '` satisfies it while trimming to the sentinel —
//! a party row that forges the very token the absorber column uses for D-07's
//! default, which is the one thing `pricing_bundle_revshare_group`'s doc says the
//! sentinel's safety rests on. A trim on the blankness clause alone leaves that
//! open, so the trim is applied to both.
//!
//! `Party` is a newtype and the trim lives in it, applied at the REST door
//! (`api::rest::bundles`) and again on read: `bundle_repo::load_composition` mints
//! every stored party through `Party::new` and folds a refusal to
//! `RepoError::CorruptRow`, so one unusable party row fails the whole bundle's
//! composition read. The population this `CHECK` guards is therefore the writer that
//! never runs the pipeline.
//!
//! A **padded** party still lands — `' acme '` reads back as `acme` — because that is
//! what `Party::new` does with it. The residue is `pricing_region_taxonomy`'s:
//! non-ASCII whitespace satisfies the predicate and `Party::new` still refuses it.
//!
//! Dependency level 2.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_bundle_revshare (
            tenant_id          uuid    NOT NULL,
            bundle_id          uuid    NOT NULL,
            plan_revision      bigint  NOT NULL,
            vendor_sku_id      uuid    NOT NULL,
            party              text    NOT NULL,
            effective_share_bp integer,
            share_bp           integer NOT NULL,
            CONSTRAINT chk_pricing_bundle_revshare_effective_share_bp CHECK (effective_share_bp IS NULL OR (effective_share_bp >= 0 AND effective_share_bp <= 10000)),
            CONSTRAINT chk_pricing_bundle_revshare_party CHECK (length(btrim(party, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0 AND btrim(party, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32)) <> 'platform'),
            CONSTRAINT chk_pricing_bundle_revshare_share_bp CHECK (share_bp >= 0 AND share_bp <= 10000),
            CONSTRAINT fk_pricing_bundle_revshare_group FOREIGN KEY (bundle_id, plan_revision, vendor_sku_id) REFERENCES bss.pricing_bundle_revshare_group(bundle_id, plan_revision, vendor_sku_id),
            CONSTRAINT pricing_bundle_revshare_pkey PRIMARY KEY (bundle_id, plan_revision, vendor_sku_id, party)
        )",
    "CREATE INDEX idx_pricing_bundle_revshare_revision ON bss.pricing_bundle_revshare USING btree (tenant_id, bundle_id, plan_revision)",
    "CREATE OR REPLACE FUNCTION bss.pricing_bundle_revshare_append_only() RETURNS trigger AS $$
        DECLARE
          parent_state text;
        BEGIN
          IF TG_OP <> 'INSERT' THEN
            SELECT p.lifecycle_state INTO parent_state
              FROM bss.pricing_bundle b
              JOIN bss.pricing_plan p ON p.plan_id = b.plan_id
             WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision;
            IF parent_state IS DISTINCT FROM 'draft' THEN
              RAISE EXCEPTION
                'pricing_bundle_revshare: % of a rev-share party under a non-draft plan revision is not permitted (state %)',
                TG_OP, coalesce(parent_state, 'missing');
            END IF;
          END IF;

          IF TG_OP = 'DELETE' THEN
            RETURN OLD;
          END IF;

          SELECT p.lifecycle_state INTO parent_state
            FROM bss.pricing_bundle b
            JOIN bss.pricing_plan p ON p.plan_id = b.plan_id
           WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision;
          IF parent_state IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION
              'pricing_bundle_revshare: % of a rev-share party under a non-draft plan revision is not permitted (state %)',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_bundle_revshare_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_bundle_revshare FOR EACH ROW EXECUTE FUNCTION bss.pricing_bundle_revshare_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_bundle_revshare",
    "DROP FUNCTION IF EXISTS bss.pricing_bundle_revshare_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_bundle_revshare (
            tenant_id          text   NOT NULL,
            bundle_id          text   NOT NULL,
            plan_revision      bigint NOT NULL,
            vendor_sku_id      text   NOT NULL,
            party              text   NOT NULL,
            effective_share_bp int,
            share_bp           int    NOT NULL,
            PRIMARY KEY (bundle_id, plan_revision, vendor_sku_id, party),
            CONSTRAINT chk_pricing_bundle_revshare_effective_share_bp CHECK (effective_share_bp IS NULL OR (effective_share_bp >= 0 AND effective_share_bp <= 10000)),
            CONSTRAINT chk_pricing_bundle_revshare_party CHECK (length(trim(party, char(9,10,11,12,13,32))) > 0 AND trim(party, char(9,10,11,12,13,32)) <> 'platform'),
            CONSTRAINT chk_pricing_bundle_revshare_share_bp CHECK (share_bp >= 0 AND share_bp <= 10000),
            CONSTRAINT fk_pricing_bundle_revshare_group FOREIGN KEY (bundle_id, plan_revision, vendor_sku_id) REFERENCES pricing_bundle_revshare_group(bundle_id, plan_revision, vendor_sku_id)
        )",
    "CREATE INDEX idx_pricing_bundle_revshare_revision ON pricing_bundle_revshare (tenant_id, bundle_id, plan_revision)",
    "CREATE TRIGGER trg_pricing_bundle_revshare_no_delete BEFORE DELETE ON pricing_bundle_revshare FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_revshare: DELETE of a rev-share party under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision AND p.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_bundle_revshare_no_insert BEFORE INSERT ON pricing_bundle_revshare FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_revshare: INSERT of a rev-share party under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision AND p.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_bundle_revshare_no_update BEFORE UPDATE ON pricing_bundle_revshare FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_revshare: UPDATE of a rev-share party under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision AND p.lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision AND p.lifecycle_state = 'draft'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_bundle_revshare"];

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
