//! Create `bss.pricing_bundle_revshare_group` — one rev-share group per
//! **included vendor SKU** within one bundle revision (`design/08-bundles.md`
//! §6, D-07 + D-55 + D-92 + D-105), keyed
//! `(bundle_id, plan_revision, vendor_sku_id)`.
//!
//! This table lands **before** `pricing_bundle_revshare`, and the order is a
//! foreign key: a party row belongs to a group, and the group is what carries the
//! platform cut and the absorber. Under the reverse order there is no referent.
//!
//! # Why the group exists at all
//!
//! D-55's 2026-07-28 correction. The tolerance and exact-sum rule of D-07 is
//! **per `(bundle, vendor SKU)` group** — *"sum to 100% per included vendor
//! SKU"* — so the values it ranges over have to live on a group row rather than
//! be smeared across the party rows or hoisted to the bundle. Before the
//! correction `platform_cut_bp` was a per-party column used once per group (so
//! nothing stopped two parties disagreeing about one group's cut) and the
//! absorber was a bundle-level column typed as *"a `vendor_sku_id`"*, which names
//! a **group** and not a resolvable party — matching the PRD's *"nominated
//! primary party"* only for the platform case.
//!
//! # `residual_absorber_party` is `text`, and so is a party everywhere else
//!
//! The column holds **either** a party of this group **or** the literal
//! `platform` sentinel (D-07: the default, so an "unnominated" state cannot
//! exist). One column, two inhabitants, so the type has to admit both — and a
//! `uuid` party beside a text sentinel would need a cast at every comparison,
//! in a comparison the reconciler makes for every group it normalizes.
//! `pricing_bundle_revshare.party` is therefore `text` as well: the two are
//! compared to each other, and a column pair that is compared should not have
//! two types.
//!
//! **`platform` is safe as a sentinel only because a party may not *be* named
//! `platform`** — the same argument `pricing_price`'s scope key makes for the
//! empty string as the `meter` sentinel. `domain::bundle::Party::new` is where that
//! is enforced for a party row, `chk_pricing_bundle_revshare_party` is its floor in
//! the store, and this table's own predicate carries the other half: the sentinel is
//! this column's default and its padded copies are refused, so a value here is the
//! default or a nomination and never something between the two.
//!
//! # What the schema does **not** enforce, and why
//!
//! That the absorber is a party **of this group** is a membership property of a
//! set the table cannot see one row at a time, so it is the reconciler's
//! (`RESIDUAL_ABSORBER_UNKNOWN` is not a code the design set declares — the
//! refusal renders under `REVSHARE_UNBALANCED`, and the gap is in the owed
//! register). A foreign key would not do it either: it would have to point at
//! `pricing_bundle_revshare`, which points **here**, and the `platform` sentinel
//! has no party row to point at by construction.
//!
//! That `SUM(share_bp) + platform_cut_bp` lands within 1 bp of 10000 is likewise
//! a set property and is `inst-rs-residual`'s. A `CHECK` sees one row.
//!
//! **And that `vendor_sku_id` names a SKU that exists** — raised as review A1-6
//! beside `component_plan_id`, and it is the different half of that pair rather
//! than a second instance of it. `component_plan_id` names a `pricing_plan` row
//! and is dereferenced at publish in the caller's scope, so a foreign one is
//! refused indistinguishably (see `pricing_bundle_component`'s module doc).
//! `vendor_sku_id` is dereferenced against **nothing, anywhere in this crate**:
//! it is a group key and an arithmetic subject, its only readers being the
//! reconciler and the two composite foreign keys on this table's own children.
//! Declaring a SKU is the registry's act and not this gear's (D-32), and this gear
//! holds no registry client, so there is no row to check it against and no scope
//! in which to check it — which is why an ownership check here would have to be
//! invented rather than restored. It carries no isolation consequence either: the
//! key is rooted in a server-minted `bundle_id`, so no value a caller supplies
//! here occupies anything another tenant can want.
//!
//! **Backend differences and the append-only discipline** are
//! `pricing_bundle_component`'s, verbatim and for the same reasons; see that
//! module's doc. `uuid` becomes `text`, `bss.` is dropped, and the single PL/pgSQL
//! function becomes three fixed-message `SQLite` triggers.
//!
//! # The absorber predicate is `Absorber::parse`'s two arms
//!
//! `chk_pricing_bundle_revshare_group_absorber` spells the same disjunction the
//! reader does: the sentinel **exactly**, or a party — non-blank after a trim
//! against ASCII whitespace entire (`pricing_region_taxonomy`'s set and its
//! argument, D-242) and not the sentinel after that trim.
//!
//! The sentinel arm is why the trim cannot simply be wrapped around the whole
//! column: `'platform'` is this column's default and a legitimate inhabitant, so a
//! predicate that refused it would refuse every unnominated group. The third clause
//! is why the trim is not only about blankness: `' platform '` is neither the default
//! nor a nomination — `Absorber::parse` reads the sentinel by equality before it
//! tries `Party::new`, so a padded copy falls through to `Party::new`, which trims
//! and then refuses it for spelling the sentinel. Such a row is the ambiguity the
//! section above says the sentinel's safety rests on not existing.
//!
//! The trim lives in the domain — `Absorber::parse` at the REST door
//! (`api::rest::bundles`) and again in `bundle_repo::load_composition`, which folds a
//! refusal to `RepoError::CorruptRow` and fails the whole bundle's composition read.
//! The residue is `pricing_region_taxonomy`'s: non-ASCII whitespace satisfies the
//! predicate and the domain still refuses it.
//!
//! Dependency level 1.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE bss.pricing_bundle_revshare_group (
            tenant_id               uuid    NOT NULL,
            bundle_id               uuid    NOT NULL,
            plan_revision           bigint  NOT NULL,
            vendor_sku_id           uuid    NOT NULL,
            platform_cut_bp         integer NOT NULL,
            residual_absorber_party text    NOT NULL DEFAULT 'platform'::text,
            CONSTRAINT chk_pricing_bundle_revshare_group_absorber CHECK (residual_absorber_party = 'platform' OR (length(btrim(residual_absorber_party, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32))) > 0 AND btrim(residual_absorber_party, chr(9) || chr(10) || chr(11) || chr(12) || chr(13) || chr(32)) <> 'platform')),
            CONSTRAINT chk_pricing_bundle_revshare_group_platform_cut_bp CHECK (platform_cut_bp >= 0 AND platform_cut_bp <= 10000),
            CONSTRAINT fk_pricing_bundle_revshare_group_bundle FOREIGN KEY (bundle_id) REFERENCES bss.pricing_bundle(bundle_id),
            CONSTRAINT pricing_bundle_revshare_group_pkey PRIMARY KEY (bundle_id, plan_revision, vendor_sku_id)
        )",
    "CREATE INDEX idx_pricing_bundle_revshare_group_revision ON bss.pricing_bundle_revshare_group USING btree (tenant_id, bundle_id, plan_revision)",
    "CREATE OR REPLACE FUNCTION bss.pricing_bundle_revshare_group_append_only() RETURNS trigger AS $$
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
                'pricing_bundle_revshare_group: % of a rev-share group under a non-draft plan revision is not permitted (state %)',
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
              'pricing_bundle_revshare_group: % of a rev-share group under a non-draft plan revision is not permitted (state %)',
              TG_OP, coalesce(parent_state, 'missing');
          END IF;

          RETURN NEW;
        END;
     $$ LANGUAGE plpgsql",
    "CREATE TRIGGER trg_pricing_bundle_revshare_group_append_only BEFORE INSERT OR DELETE OR UPDATE ON bss.pricing_bundle_revshare_group FOR EACH ROW EXECUTE FUNCTION bss.pricing_bundle_revshare_group_append_only()",
];

const PG_DOWN_STATEMENTS: &[&str] = &[
    "DROP TABLE IF EXISTS bss.pricing_bundle_revshare_group",
    "DROP FUNCTION IF EXISTS bss.pricing_bundle_revshare_group_append_only()",
];

const SQLITE_UP_STATEMENTS: &[&str] = &[
    "CREATE TABLE pricing_bundle_revshare_group (
            tenant_id               text   NOT NULL,
            bundle_id               text   NOT NULL,
            plan_revision           bigint NOT NULL,
            vendor_sku_id           text   NOT NULL,
            platform_cut_bp         int    NOT NULL,
            residual_absorber_party text   NOT NULL DEFAULT 'platform',
            PRIMARY KEY (bundle_id, plan_revision, vendor_sku_id),
            CONSTRAINT chk_pricing_bundle_revshare_group_absorber CHECK (residual_absorber_party = 'platform' OR (length(trim(residual_absorber_party, char(9,10,11,12,13,32))) > 0 AND trim(residual_absorber_party, char(9,10,11,12,13,32)) <> 'platform')),
            CONSTRAINT chk_pricing_bundle_revshare_group_platform_cut_bp CHECK (platform_cut_bp >= 0 AND platform_cut_bp <= 10000),
            CONSTRAINT fk_pricing_bundle_revshare_group_bundle FOREIGN KEY (bundle_id) REFERENCES pricing_bundle(bundle_id)
        )",
    "CREATE INDEX idx_pricing_bundle_revshare_group_revision ON pricing_bundle_revshare_group (tenant_id, bundle_id, plan_revision)",
    "CREATE TRIGGER trg_pricing_bundle_revshare_group_no_delete BEFORE DELETE ON pricing_bundle_revshare_group FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_revshare_group: DELETE of a rev-share group under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision AND p.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_bundle_revshare_group_no_insert BEFORE INSERT ON pricing_bundle_revshare_group FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_revshare_group: INSERT of a rev-share group under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision AND p.lifecycle_state = 'draft'); END",
    "CREATE TRIGGER trg_pricing_bundle_revshare_group_no_update BEFORE UPDATE ON pricing_bundle_revshare_group FOR EACH ROW BEGIN SELECT RAISE(ABORT, 'pricing_bundle_revshare_group: UPDATE of a rev-share group under a non-draft plan revision is not permitted') WHERE NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = OLD.bundle_id AND p.revision = OLD.plan_revision AND p.lifecycle_state = 'draft') OR NOT EXISTS (SELECT 1 FROM pricing_bundle b JOIN pricing_plan p ON p.plan_id = b.plan_id WHERE b.bundle_id = NEW.bundle_id AND p.revision = NEW.plan_revision AND p.lifecycle_state = 'draft'); END",
];

const SQLITE_DOWN_STATEMENTS: &[&str] = &["DROP TABLE IF EXISTS pricing_bundle_revshare_group"];

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
