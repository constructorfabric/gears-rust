//! Stage 1: copy every `active` value into the new store.
//!
//! Runs on the shipped schema, before `m0002`.

use std::collections::HashMap;
use std::path::Path;

use credstore_sdk::{CredStorePluginClientV2, SecretValue, StoreKey, TenantId};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::db::{self, Schema, ShippedRow};
use crate::error::MigrationError;
use crate::fence::{self, CURRENT_FENCE_KEY_ID, FENCE_KEY_REF};
use crate::legacy::LegacyValueStore;
use crate::results::{Entry, Outcome, ResultsWriter};
use crate::{Mode, tool_context};

const STATUS_PROVISIONING: i16 = 1;
const STATUS_ACTIVE: i16 = 2;
const STATUS_DEPROVISIONING: i16 = 3;
const SHARING_PRIVATE: i16 = 1;

/// Identifies a row in a report. Never carries a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowRef {
    /// Row id.
    pub id: Uuid,
    /// Tenant of the row.
    pub tenant_id: Uuid,
    /// Reference of the row.
    pub reference: String,
}

/// A private row whose type differs from the non-private row of the same
/// `(tenant_id, reference)`. Both keep working; a NEW private override with a
/// differing type is rejected after the cutover (`TYPE_MISMATCH_WITH_INHERITED`).
/// Divergence across tenants cannot be computed here: it needs the tenant
/// hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDivergence {
    /// Tenant of both rows.
    pub tenant_id: Uuid,
    /// Their reference.
    pub reference: String,
    /// The private row.
    pub private_row: Uuid,
    /// Its secret type (UUID of the GTS type id).
    pub private_type: Uuid,
    /// The non-private row.
    pub nonprivate_row: Uuid,
    /// Its secret type.
    pub nonprivate_type: Uuid,
}

/// Result of [`copy`].
///
/// In [`Mode::DryRun`] the `copied` / `unverified` counts are what an apply
/// run would copy; nothing was written.
#[derive(Debug, Clone, Default)]
pub struct CopyReport {
    /// Rows in `credstore_secrets`.
    pub total_rows: usize,
    /// Rows that already had an entry in the results file (resumed run); they
    /// are also counted in the buckets below by their recorded outcome.
    pub resumed: usize,
    /// Whether the legacy store held the fence key.
    pub fence_key_present: bool,
    /// Rows copied after a verified fingerprint.
    pub copied: usize,
    /// Rows copied without a fingerprint to verify (served on trust by the
    /// shipped gear).
    pub unverified: usize,
    /// `active` rows with no value in the legacy store.
    pub missing: Vec<RowRef>,
    /// Rows whose value failed the fingerprint check; not copied.
    pub fp_mismatch: Vec<RowRef>,
    /// Rows with an `fp_key_id` other than the shipped one; not copied.
    pub unknown_fence_key: Vec<RowRef>,
    /// Rows in status `1`/`3`; not copied, listed for cleanup.
    pub unfinished: Vec<RowRef>,
    /// Private rows whose type differs from the tenant's non-private row.
    pub type_divergent: Vec<TypeDivergence>,
}

impl CopyReport {
    /// Whether any row ends up without a value (`missing`, `fp_mismatch`,
    /// `unknown_fence_key`).
    #[must_use]
    pub fn has_losses(&self) -> bool {
        !(self.missing.is_empty()
            && self.fp_mismatch.is_empty()
            && self.unknown_fence_key.is_empty())
    }

    fn record(&mut self, entry: &Entry) {
        let row = RowRef {
            id: entry.id,
            tenant_id: entry.tenant_id,
            reference: entry.reference.clone(),
        };
        match entry.outcome {
            Outcome::Copied => self.copied += 1,
            Outcome::Unverified => self.unverified += 1,
            Outcome::Missing => self.missing.push(row),
            Outcome::FpMismatch => self.fp_mismatch.push(row),
            Outcome::UnknownFenceKey => self.unknown_fence_key.push(row),
            Outcome::Unfinished => self.unfinished.push(row),
        }
    }
}

fn legacy_owner(row: &ShippedRow) -> Option<Uuid> {
    (row.sharing == SHARING_PRIVATE).then_some(row.owner_id)
}

fn validate(rows: &[ShippedRow]) -> Result<(), MigrationError> {
    for row in rows {
        if !matches!(
            row.status,
            STATUS_PROVISIONING | STATUS_ACTIVE | STATUS_DEPROVISIONING
        ) {
            return Err(MigrationError::UnexpectedStatus {
                id: row.id,
                status: row.status,
            });
        }
        if !(1..=3).contains(&row.sharing) {
            return Err(MigrationError::BadRow {
                id: row.id,
                reason: format!("unknown sharing code {}", row.sharing),
            });
        }
    }
    Ok(())
}

fn type_divergence(rows: &[ShippedRow]) -> Vec<TypeDivergence> {
    let mut nonprivate: HashMap<(Uuid, &str), &ShippedRow> = HashMap::new();
    for row in rows.iter().filter(|r| r.sharing != SHARING_PRIVATE) {
        nonprivate.insert((row.tenant_id, row.reference.as_str()), row);
    }
    rows.iter()
        .filter(|r| r.sharing == SHARING_PRIVATE)
        .filter_map(|private| {
            let other = nonprivate.get(&(private.tenant_id, private.reference.as_str()))?;
            (other.secret_type_uuid != private.secret_type_uuid).then(|| TypeDivergence {
                tenant_id: private.tenant_id,
                reference: private.reference.clone(),
                private_row: private.id,
                private_type: private.secret_type_uuid,
                nonprivate_row: other.id,
                nonprivate_type: other.secret_type_uuid,
            })
        })
        .collect()
}

fn entry_for(row: &ShippedRow, outcome: Outcome, version: Option<String>) -> Entry {
    Entry {
        id: row.id,
        tenant_id: row.tenant_id,
        reference: row.reference.clone(),
        sharing: row.sharing,
        owner_id: legacy_owner(row),
        status_before: row.status,
        outcome,
        value_version: version,
    }
}

/// Reads the fence key; aborts when it is absent but rows carry fingerprints.
async fn load_fence_key(
    rows: &[ShippedRow],
    legacy: &dyn LegacyValueStore,
) -> Result<Option<Vec<u8>>, MigrationError> {
    let key = legacy.get(Uuid::nil(), FENCE_KEY_REF, None).await?;
    if key.is_none() {
        let rows = rows.iter().filter(|r| r.value_fp.is_some()).count();
        if rows > 0 {
            return Err(MigrationError::FenceKeyAbsent { rows });
        }
    }
    Ok(key)
}

struct Stores<'a> {
    legacy: &'a dyn LegacyValueStore,
    target: &'a dyn CredStorePluginClientV2,
    fence_key: Option<&'a [u8]>,
    mode: Mode,
}

/// Decides one `active` row and, in apply mode, copies it.
async fn process_active(
    row: &ShippedRow,
    stores: &Stores<'_>,
) -> Result<(Outcome, Option<String>), MigrationError> {
    let Some(value) = stores
        .legacy
        .get(row.tenant_id, &row.reference, legacy_owner(row))
        .await?
    else {
        return Ok((Outcome::Missing, None));
    };
    let outcome = match &row.value_fp {
        None => Outcome::Unverified,
        Some(fp) => {
            if row.fp_key_id != Some(CURRENT_FENCE_KEY_ID) {
                return Ok((Outcome::UnknownFenceKey, None));
            }
            let key = stores
                .fence_key
                .ok_or(MigrationError::FenceKeyAbsent { rows: 1 })?;
            if !fence::verify_fp(key, &value, fp) {
                return Ok((Outcome::FpMismatch, None));
            }
            Outcome::Copied
        }
    };
    if stores.mode == Mode::DryRun {
        return Ok((outcome, None));
    }
    let ctx = tool_context()?;
    let key = StoreKey::new(TenantId(row.tenant_id), row.id);
    let version = stores
        .target
        .put(&ctx, &key, SecretValue::new(value.clone()))
        .await?;
    let back = stores.target.get(&ctx, &key, &version).await?;
    match back {
        Some(b) if b.as_bytes() == value.as_slice() => {}
        Some(_) => {
            return Err(MigrationError::ReadBack {
                id: row.id,
                reason: "the new store returned different bytes".to_owned(),
            });
        }
        None => {
            return Err(MigrationError::ReadBack {
                id: row.id,
                reason: "the new store holds no value at the returned version".to_owned(),
            });
        }
    }
    Ok((outcome, Some(version.0)))
}

/// Copies every `active` value of the shipped schema into the new store.
///
/// Per row, in `tenant_id, reference` order: reads the legacy value, verifies
/// the fingerprint with the legacy fence key, `put`s the value under
/// `StoreKey(tenant_id, row id)`, reads it back by the returned version and
/// appends the outcome to the results file. Legacy entries are left in place.
/// A row whose fingerprint is absent (`value_fp IS NULL`) is copied and
/// recorded as `unverified`: the shipped gear served such rows on trust.
///
/// The results file is the progress file: ids already in it are skipped. A
/// `put` whose entry was not recorded (interrupt) leaves an unreferenced
/// version, which is harmless: nothing points at it and the next write to the
/// key reclaims it.
///
/// In [`Mode::DryRun`] the legacy store and the database are only read.
///
/// # Errors
///
/// Any [`MigrationError`] aborts the stage; entries already recorded stay, and
/// a re-run resumes.
pub async fn copy(
    db: &DatabaseConnection,
    legacy: &dyn LegacyValueStore,
    target: &dyn CredStorePluginClientV2,
    results_path: &Path,
    mode: Mode,
) -> Result<CopyReport, MigrationError> {
    let backend = db::backend(db)?;
    if db::detect_schema(db, backend).await? != Schema::Shipped {
        return Err(MigrationError::WrongSchema(
            "copy runs before m0002: value_fp is missing or value_version already exists"
                .to_owned(),
        ));
    }
    let rows = db::load_shipped_rows(db, backend).await?;
    validate(&rows)?;
    let fence_key = load_fence_key(&rows, legacy).await?;

    let mut report = CopyReport {
        total_rows: rows.len(),
        fence_key_present: fence_key.is_some(),
        type_divergent: type_divergence(&rows),
        ..CopyReport::default()
    };
    let mut writer = None;
    let mut done: HashMap<Uuid, Entry> = HashMap::new();
    match mode {
        Mode::Apply => {
            let (w, entries) = ResultsWriter::open(results_path)?;
            writer = Some(w);
            done.extend(entries.into_iter().map(|e| (e.id, e)));
        }
        Mode::DryRun => {
            let entries = crate::results::read(results_path)?.unwrap_or_default();
            done.extend(entries.into_iter().map(|e| (e.id, e)));
        }
    }
    let stores = Stores {
        legacy,
        target,
        fence_key: fence_key.as_deref(),
        mode,
    };
    for row in &rows {
        if let Some(entry) = done.get(&row.id) {
            report.resumed += 1;
            report.record(entry);
            continue;
        }
        let entry = if row.status == STATUS_ACTIVE {
            let (outcome, version) = process_active(row, &stores).await?;
            entry_for(row, outcome, version)
        } else {
            entry_for(row, Outcome::Unfinished, None)
        };
        tracing::info!(
            id = %entry.id,
            tenant = %entry.tenant_id,
            reference = %entry.reference,
            outcome = entry.outcome.as_str(),
            dry_run = mode == Mode::DryRun,
            "credstore value migration: row processed"
        );
        if let Some(w) = writer.as_mut() {
            w.append(&entry)?;
        }
        report.record(&entry);
    }
    Ok(report)
}
