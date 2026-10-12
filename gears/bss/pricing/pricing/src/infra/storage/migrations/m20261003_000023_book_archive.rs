//! D-522: a finished price book can be archived, and archiving it releases its entries' SKU
//! references.
//!
//! A forward migration: the shipped chain is frozen. It makes three changes.
//!
//! - `pricing_price_book` gains `archived_at` and `archived_by`, both null until the book is
//!   archived, and set and cleared together: `(archived_at IS NULL) = (archived_by IS NULL)`.
//!   Both engines add them in place; on `SQLite` the CHECK rides on `archived_by`'s column, which
//!   `down` therefore drops first.
//! - The entry's `reference_state` CHECK gains `released`: the reference of an archived book's
//!   entry, released in Products.
//! - The reference op's `kind` CHECK gains `release`: the op that releases that reference.
//!
//! Postgres drops each CHECK and adds it again widened. `SQLite` cannot alter a CHECK, and the
//! toolkit runner runs every `up()` inside a transaction, where `PRAGMA foreign_keys=OFF` has no
//! effect, so dropping a parent that still has child rows fails. The `SQLite` arm therefore
//! rebuilds the entry's family as `m20260930_000018` does, without a PRAGMA: the entry, and the two
//! tables that reference it, `pricing_price` (its shape of `m20261003_000022`) and
//! `pricing_plan_item` (its shape of `m20260930_000018`), with 000022's two pairing CHECKs. Nothing
//! references a price but another price. A change's `target_price_id` is copied with its row, so
//! the pairing holds on insert: `SQLite` checks an immediate foreign key at the end of the
//! statement, so a change copied before the price it names is no violation. The pair references
//! (`paired_price_id`, `return_of_price_id`) are copied after every row exists, as 000022 copies
//! them. The children go
//! before the parent, the new tables are renamed (`ALTER TABLE … RENAME` rewrites the references
//! to the final names), and the entry's key index, its two `usage_sku_version` triggers
//! (`m20261002_000021`) and the price's two indexes are created again with their text. The plan
//! item table has no index beside its inline key. `pricing_reference_op` has no foreign key in or
//! out, so it is rebuilt alone, with its `due` index.
//!
//! `down()` restores the previous shape. It fails while an entry is `released` or an op is a
//! `release` (the narrower CHECKs refuse the copy), and an archive mark is lost with its columns.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const PG_UP: &[&str] = &[
    "ALTER TABLE bss.pricing_price_book \
     ADD COLUMN IF NOT EXISTS archived_at timestamptz, \
     ADD COLUMN IF NOT EXISTS archived_by uuid",
    "ALTER TABLE bss.pricing_price_book ADD CONSTRAINT pricing_price_book_archive_mark_check \
     CHECK ((archived_at IS NULL) = (archived_by IS NULL))",
    "ALTER TABLE bss.pricing_price_book_entry \
     DROP CONSTRAINT IF EXISTS pricing_price_book_entry_reference_state_check",
    "ALTER TABLE bss.pricing_price_book_entry \
     ADD CONSTRAINT pricing_price_book_entry_reference_state_check \
     CHECK (reference_state IN ('confirmation_pending','confirmed','lost','released'))",
    "ALTER TABLE bss.pricing_reference_op DROP CONSTRAINT IF EXISTS pricing_reference_op_kind_check",
    "ALTER TABLE bss.pricing_reference_op ADD CONSTRAINT pricing_reference_op_kind_check \
     CHECK (kind IN ('create','delete','rereserve','attach','release'))",
];

const PG_DOWN: &[&str] = &[
    "ALTER TABLE bss.pricing_reference_op DROP CONSTRAINT IF EXISTS pricing_reference_op_kind_check",
    "ALTER TABLE bss.pricing_reference_op ADD CONSTRAINT pricing_reference_op_kind_check \
     CHECK (kind IN ('create','delete','rereserve','attach'))",
    "ALTER TABLE bss.pricing_price_book_entry \
     DROP CONSTRAINT IF EXISTS pricing_price_book_entry_reference_state_check",
    "ALTER TABLE bss.pricing_price_book_entry \
     ADD CONSTRAINT pricing_price_book_entry_reference_state_check \
     CHECK (reference_state IN ('confirmation_pending','confirmed','lost'))",
    "ALTER TABLE bss.pricing_price_book \
     DROP CONSTRAINT IF EXISTS pricing_price_book_archive_mark_check",
    "ALTER TABLE bss.pricing_price_book DROP COLUMN IF EXISTS archived_at, \
     DROP COLUMN IF EXISTS archived_by",
];

/// The entry's reference state CHECK before this migration and after it; each is spelled once
/// more in the Postgres statements (`tests/pure_model.rs` pins the widened one on both).
const STATES_BEFORE: &str =
    "CHECK (reference_state IN ('confirmation_pending','confirmed','lost'))";
const STATES_AFTER: &str =
    "CHECK (reference_state IN ('confirmation_pending','confirmed','lost','released'))";
/// The op kind CHECK before this migration and after it, spelled as the states are.
const KINDS_BEFORE: &str = "CHECK (kind IN ('create','delete','rereserve','attach'))";
const KINDS_AFTER: &str = "CHECK (kind IN ('create','delete','rereserve','attach','release'))";

/// The entry table of `m20260930_000018`, with `usage_sku_version` (`m20261002_000021`) and the
/// reference state CHECK `states`, named `table`.
fn entry_table(table: &str, states: &str) -> String {
    format!(
        r"CREATE TABLE {table} (
  id text PRIMARY KEY, tenant_id text NOT NULL, book_id text NOT NULL REFERENCES pricing_price_book(id),
  sku_id text NOT NULL, charge_kind text NOT NULL CHECK (charge_kind IN ('recurring','usage','one_time')),
  period text, dimension_key text, invoice_line_override text,
  reservation_id text NOT NULL,
  reference_state text NOT NULL {states},
  version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  model text NOT NULL DEFAULT 'flat' CHECK (model IN ('flat','per_unit','graduated','volume','package')),
  usage_policy_id text, usage_policy_version integer, usage_policy_digest text,
  usage_sku_version integer,
  FOREIGN KEY (tenant_id, dimension_key) REFERENCES pricing_dimension_key(tenant_id, key),
  CHECK ((charge_kind = 'recurring' AND period IS NOT NULL AND period IN ('month','year'))
    OR (charge_kind IN ('usage','one_time') AND period IS NULL))
,
  CONSTRAINT pricing_entry_policy_complete CHECK (
    (usage_policy_id IS NULL AND usage_policy_version IS NULL AND usage_policy_digest IS NULL) OR
    (usage_policy_id IS NOT NULL AND usage_policy_version IS NOT NULL AND usage_policy_digest IS NOT NULL AND charge_kind = 'usage')),
  FOREIGN KEY (tenant_id, usage_policy_id, usage_policy_version, usage_policy_digest)
    REFERENCES pricing_usage_rating_policy (tenant_id, policy_id, version, digest)
)"
    )
}

const ENTRY_COLUMNS: &str = "id, tenant_id, book_id, sku_id, charge_kind, period, dimension_key, \
invoice_line_override, reservation_id, reference_state, version, created_at, updated_at, model, \
usage_policy_id, usage_policy_version, usage_policy_digest, usage_sku_version";

/// The price table of `m20261003_000022`, named `table`, its entry `entry`.
fn price_table(table: &str, entry: &str) -> String {
    format!(
        "CREATE TABLE {table} (
  id text PRIMARY KEY, tenant_id text NOT NULL, price_book_entry_id text NOT NULL REFERENCES {entry}(id),
  version_no integer NOT NULL, dim_value text,
  price_json text NOT NULL,
  min_fee text CHECK (min_fee GLOB '[0-9]*' AND min_fee NOT GLOB '*[^0-9.]*' AND min_fee NOT GLOB '*.*.*' AND min_fee NOT GLOB '*.'), eligibility text NOT NULL CHECK (eligibility IN ('all','new')),
  effective_from text NOT NULL, effective_to text, keep_for_bound integer NOT NULL DEFAULT 0,
  closed_explicitly integer NOT NULL DEFAULT 0,
  temporary_until text, paired_price_id text REFERENCES {table}(id),
  return_of_price_id text REFERENCES {table}(id),
  state text NOT NULL CHECK (state IN ('draft','pending','approved','rejected','cancelled')),
  change_kind text NOT NULL DEFAULT 'set' CHECK (change_kind IN ('set','cancel','end')),
  target_price_id text REFERENCES {table}(id),
  cancelled_by_unit_id text REFERENCES pricing_approval_unit(id),
  pending_unit_id text REFERENCES pricing_approval_unit(id),
  approved_by_unit_id text REFERENCES pricing_approval_unit(id), note text, created_by text NOT NULL,
  approved_at text, version integer NOT NULL DEFAULT 1,
  created_at text NOT NULL, updated_at text NOT NULL,
  UNIQUE (price_book_entry_id, version_no), CHECK (dim_value IS NULL OR dim_value <> ''),
  CHECK (effective_to IS NULL OR effective_from < effective_to),
  CHECK ((change_kind = 'set') = (target_price_id IS NULL)),
  CHECK ((state = 'cancelled') = (cancelled_by_unit_id IS NOT NULL))
)"
    )
}

/// The columns copied with the rows, a change's `target_price_id` among them (its pairing CHECK
/// holds on insert); the two pair references follow once every row exists.
const PRICE_COLUMNS: &str = "id, tenant_id, price_book_entry_id, version_no, dim_value, price_json, \
min_fee, eligibility, effective_from, effective_to, keep_for_bound, closed_explicitly, \
temporary_until, state, change_kind, target_price_id, cancelled_by_unit_id, pending_unit_id, \
approved_by_unit_id, note, created_by, approved_at, version, created_at, updated_at";

/// The plan item table of `m20260930_000018`, named `table`, its entry `entry`.
fn item_table(table: &str, entry: &str) -> String {
    format!(
        "CREATE TABLE {table} (
  id text PRIMARY KEY, tenant_id text NOT NULL, revision_id text NOT NULL REFERENCES pricing_plan_revision(id),
  sku_id text NOT NULL, price_book_entry_id text REFERENCES {entry}(id),
  treatment text NOT NULL, included_qty text, qty_min integer, reservation_id text, reference_state text NOT NULL,
  version integer NOT NULL DEFAULT 1, created_by text NOT NULL,
  created_at text NOT NULL, updated_at text NOT NULL,
  CONSTRAINT pricing_plan_item_sku UNIQUE (revision_id, sku_id),
  CONSTRAINT chk_pricing_plan_item_treatment CHECK (treatment IN ('paid','optional','included')),
  CONSTRAINT chk_pricing_plan_item_entry CHECK (treatment = 'included' OR price_book_entry_id IS NOT NULL),
  CONSTRAINT chk_pricing_plan_item_included_qty CHECK (included_qty GLOB '[0-9]*' AND included_qty NOT GLOB '*[^0-9.]*' AND included_qty NOT GLOB '*.*.*' AND included_qty NOT GLOB '*.'),
  CONSTRAINT chk_pricing_plan_item_qty_min CHECK (qty_min >= 0),
  CONSTRAINT chk_pricing_plan_item_reference_state CHECK (reference_state IN ('unreserved','confirmation_pending','confirmed','lost'))
)"
    )
}

const ITEM_COLUMNS: &str = "id, tenant_id, revision_id, sku_id, price_book_entry_id, treatment, \
included_qty, qty_min, reservation_id, reference_state, version, created_by, created_at, updated_at";

/// The reference op table of `m20260926_000006`, named `table`, with the op kind CHECK `kinds`.
fn op_table(table: &str, kinds: &str) -> String {
    format!(
        r"CREATE TABLE {table} (
  op_id text PRIMARY KEY, tenant_id text NOT NULL,
  kind text NOT NULL {kinds},
  ref_kind text NOT NULL CHECK (ref_kind IN ('price_book_entry','plan_item')),
  ref_id text NOT NULL, sku_id text NOT NULL, reservation_id text,
  idempotency_key text,
  state text NOT NULL CHECK (state IN ('reserving','written','cancelling','releasing','done')),
  outcome text, attempts integer NOT NULL DEFAULT 0, next_attempt_at text NOT NULL,
  last_error text, created_by text NOT NULL, created_at text NOT NULL, updated_at text NOT NULL
)"
    )
}

const OP_COLUMNS: &str = "op_id, tenant_id, kind, ref_kind, ref_id, sku_id, reservation_id, \
idempotency_key, state, outcome, attempts, next_attempt_at, last_error, created_by, created_at, \
updated_at";

/// The SQLite rebuild of the entry's family and of the op table with `states` and `kinds`, under
/// the transient suffix `suffix`.
fn sqlite_rebuild(states: &str, kinds: &str, suffix: &str) -> Vec<String> {
    let entry = format!("pricing_price_book_entry__{suffix}");
    let price = format!("pricing_price__{suffix}");
    let item = format!("pricing_plan_item__{suffix}");
    let op = format!("pricing_reference_op__{suffix}");
    vec![
        entry_table(&entry, states),
        format!(
            "INSERT INTO {entry} ({ENTRY_COLUMNS}) SELECT {ENTRY_COLUMNS} FROM pricing_price_book_entry"
        ),
        price_table(&price, &entry),
        format!("INSERT INTO {price} ({PRICE_COLUMNS}) SELECT {PRICE_COLUMNS} FROM pricing_price"),
        format!(
            "UPDATE {price} SET \
             paired_price_id = (SELECT p.paired_price_id FROM pricing_price p WHERE p.id = {price}.id), \
             return_of_price_id = (SELECT p.return_of_price_id FROM pricing_price p WHERE p.id = {price}.id)"
        ),
        item_table(&item, &entry),
        format!(
            "INSERT INTO {item} ({ITEM_COLUMNS}) SELECT {ITEM_COLUMNS} FROM pricing_plan_item"
        ),
        "DROP TABLE pricing_plan_item".to_owned(),
        "DROP TABLE pricing_price".to_owned(),
        "DROP TABLE pricing_price_book_entry".to_owned(),
        format!("ALTER TABLE {entry} RENAME TO pricing_price_book_entry"),
        format!("ALTER TABLE {price} RENAME TO pricing_price"),
        format!("ALTER TABLE {item} RENAME TO pricing_plan_item"),
        "CREATE UNIQUE INDEX pricing_price_book_entry_key ON pricing_price_book_entry (book_id, sku_id, charge_kind, coalesce(period, ''), model, coalesce(usage_policy_digest, ''))".to_owned(),
        "CREATE TRIGGER pricing_entry_sku_version_insert BEFORE INSERT ON pricing_price_book_entry WHEN NEW.usage_sku_version IS NOT NULL AND (NEW.usage_policy_id IS NULL OR NEW.usage_sku_version < 1) BEGIN SELECT RAISE(ABORT, 'pricing_entry_sku_version'); END".to_owned(),
        "CREATE TRIGGER pricing_entry_sku_version_update BEFORE UPDATE ON pricing_price_book_entry WHEN NEW.usage_sku_version IS NOT NULL AND (NEW.usage_policy_id IS NULL OR NEW.usage_sku_version < 1) BEGIN SELECT RAISE(ABORT, 'pricing_entry_sku_version'); END".to_owned(),
        "CREATE UNIQUE INDEX pricing_price_approved_start\n  ON pricing_price (price_book_entry_id, coalesce(dim_value, ''), effective_from) WHERE state = 'approved' AND change_kind = 'set'".to_owned(),
        "CREATE INDEX pricing_price_chain\n  ON pricing_price (price_book_entry_id, dim_value, effective_from) WHERE state = 'approved'".to_owned(),
        op_table(&op, kinds),
        format!("INSERT INTO {op} ({OP_COLUMNS}) SELECT {OP_COLUMNS} FROM pricing_reference_op"),
        "DROP TABLE pricing_reference_op".to_owned(),
        format!("ALTER TABLE {op} RENAME TO pricing_reference_op"),
        "CREATE INDEX pricing_reference_op_due ON pricing_reference_op (state, next_attempt_at) WHERE state <> 'done'".to_owned(),
    ]
}

fn sqlite_up() -> Vec<String> {
    let mut statements = vec![
        "ALTER TABLE pricing_price_book ADD COLUMN archived_at text".to_owned(),
        "ALTER TABLE pricing_price_book ADD COLUMN archived_by text \
         CHECK ((archived_at IS NULL) = (archived_by IS NULL))"
            .to_owned(),
    ];
    statements.extend(sqlite_rebuild(STATES_AFTER, KINDS_AFTER, "d522"));
    statements
}

fn sqlite_down() -> Vec<String> {
    let mut statements = sqlite_rebuild(STATES_BEFORE, KINDS_BEFORE, "d522_old");
    // `archived_by` first: it carries the CHECK that names `archived_at`.
    statements.extend([
        "ALTER TABLE pricing_price_book DROP COLUMN archived_by".to_owned(),
        "ALTER TABLE pricing_price_book DROP COLUMN archived_at".to_owned(),
    ]);
    statements
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sqlite = sqlite_up();
        let sqlite: Vec<&str> = sqlite.iter().map(String::as_str).collect();
        super::exec_backend(self.name(), manager, PG_UP, &sqlite).await
    }

    /// The previous shape. It fails while a row needs the wider sets; the archive marks go.
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let sqlite = sqlite_down();
        let sqlite: Vec<&str> = sqlite.iter().map(String::as_str).collect();
        super::exec_backend(self.name(), manager, PG_DOWN, &sqlite).await
    }
}

#[cfg(test)]
#[path = "m20261003_000023_book_archive_tests.rs"]
mod tests;
