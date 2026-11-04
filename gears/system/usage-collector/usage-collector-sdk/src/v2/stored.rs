//! The Plugin SPI's meter reference and storage-shaped ledger entry.
//!
//! These two types are what [`crate::v2::UsageCollectorPluginV2`] speaks.
//! Neither crosses a wire: `UsageRecord` is the SDK/domain model a REST
//! response deserializes into, and `UsageRecordDto` is the wire shape.

use std::collections::BTreeMap;

use thiserror::Error;
use uuid::Uuid;

use crate::v2::models::{
    EntryType, IdempotencyKey, Invalidation, MetadataKey, MeterTypeId, RecordOrigin, ResourceRef,
    SubjectRef, UsageRecord,
};
use crate::v2::quantity::UsageQuantity;

/// How a meter crosses the Plugin SPI inward.
///
/// # What a plugin may do with each field
///
/// [`Self::uuid`] **is** the meter's identity: the value a plugin persists,
/// keys on, indexes and partitions by. It is the `types-registry` *Registry
/// Reference* (`cpt-cf-types-registry-adr-storage-identity-query-model`),
/// which guarantees one identifier maps to one reference for the life of
/// the installation.
///
/// [`Self::id`] is **diagnostic**. A plugin MAY put it in a log line or an
/// error detail, so an operator reading a backend's own output sees a meter
/// name rather than an opaque UUID. A plugin MUST NOT persist it, index it,
/// key on it, or derive either value from the other.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MeterRef {
    /// The registry reference — the identity.
    pub uuid: Uuid,
    /// The GTS identifier — diagnostic only. See the type's docs.
    pub id: MeterTypeId,
}

impl MeterRef {
    /// Pairs a reference with the identifier it was issued for.
    ///
    /// Neither value is checked against the other: a caller supplying a
    /// mismatched pair gets a mismatched pair.
    #[must_use]
    pub const fn new(uuid: Uuid, id: MeterTypeId) -> Self {
        Self { uuid, id }
    }
}

/// A ledger entry as storage holds it.
///
/// Field-for-field [`UsageRecord`], with `gts_type_id: MeterTypeId`
/// replaced by [`Self::gts_type_uuid`]. It is the same type in **both**
/// directions across the SPI, and the meter rides inward as a call parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredUsageRecord {
    /// Deterministic gateway-derived entry identity.
    ///
    /// **Unrelated to [`Self::gts_type_uuid`].** This is `UUIDv5` over the
    /// six-tuple dedup identity, whose meter component is the GTS
    /// *identifier*
    /// (`cpt-cf-usage-collector-adr-record-identity-derivation`,
    /// [`crate::v2::derive_usage_record_id`]). The reference is not an input to
    /// it.
    pub id: Uuid,
    /// The registry reference of the meter this entry is metered against —
    /// the value storage keys on. See [`MeterRef`].
    pub gts_type_uuid: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// Resource attribution composite (mandatory).
    pub resource_ref: ResourceRef,
    /// Optional subject attribution composite.
    pub subject_ref: Option<SubjectRef>,
    /// Caller-supplied metadata, validated gateway-side.
    pub metadata: BTreeMap<MetadataKey, String>,
    /// The measured quantity, in the meter's canonical unit.
    pub quantity: UsageQuantity,
    /// Caller-supplied dedup key.
    pub idempotency_key: IdempotencyKey,
    /// Gear-assigned instant of acceptance.
    pub accepted_at: time::OffsetDateTime,
    /// Which ingestion route admitted this entry.
    pub origin: RecordOrigin,
    /// The withdrawal this entry carries, absent on an ordinary measurement.
    pub invalidation: Option<Invalidation>,
    /// Inclusive start of the covered period.
    pub window_start: time::OffsetDateTime,
    /// Exclusive end of the covered period — the bound every read selects on.
    pub window_end: time::OffsetDateTime,
}

/// A batch element on its way **inward** across the SPI: a meter paired with
/// the entry metered against it, whose two *references* cannot disagree.
///
/// The meter identity rides inward twice — as [`MeterRef::uuid`] and as
/// [`StoredUsageRecord::gts_type_uuid`] — and a plugin may key on either
/// ([`crate::v2::UsageCollectorPluginV2::create_usage_records`]). That is only
/// safe while the two agree: a plugin binding `meter.uuid` on insert while
/// resolving a dedup collision through [`StoredUsageRecord::caller_supplied_eq`],
/// which reads the record's own `gts_type_uuid`, would store one value and
/// deduplicate against the other. Constructing this type is the check.
///
/// The guarantee covers the reference and nothing else: a
/// [`StoredUsageRecord`] carries no identifier to compare.
#[derive(Debug, Clone, PartialEq)]
pub struct MeteredRecord {
    meter: MeterRef,
    record: StoredUsageRecord,
}

/// The two halves of a [`MeteredRecord`] named different meters.
#[derive(Debug, Error, PartialEq, Eq)]
#[error(
    "a batch element's two meter identities must agree: the meter reference is \
     {meter_uuid} and the entry's gts_type_uuid is {record_gts_type_uuid}"
)]
pub struct MeterRecordMismatch {
    /// The reference the meter carried.
    pub meter_uuid: Uuid,
    /// The reference the entry carried.
    pub record_gts_type_uuid: Uuid,
}

impl MeteredRecord {
    /// Pairs a meter with an entry already in storage shape.
    ///
    /// # Errors
    ///
    /// [`MeterRecordMismatch`] when the entry's
    /// [`StoredUsageRecord::gts_type_uuid`] is not the meter's
    /// [`MeterRef::uuid`].
    pub fn new(meter: MeterRef, record: StoredUsageRecord) -> Result<Self, MeterRecordMismatch> {
        if record.gts_type_uuid == meter.uuid {
            Ok(Self { meter, record })
        } else {
            Err(MeterRecordMismatch {
                meter_uuid: meter.uuid,
                record_gts_type_uuid: record.gts_type_uuid,
            })
        }
    }

    /// Pairs a meter with a domain record, taking the reference from the
    /// meter. Infallible because the reference is *taken* rather than
    /// compared; it is the path the gateway uses.
    ///
    /// **`record.gts_type_id` is discarded, not checked.** Pass a meter the
    /// record was resolved under: pairing one with a foreign meter files it
    /// under that meter's reference while its `id` stays derived over its own
    /// identifier, leaving the entry unreproducible offline
    /// ([`crate::v2::derive_usage_record_id`]).
    #[must_use]
    pub fn from_usage_record(meter: MeterRef, record: UsageRecord) -> Self {
        let record = record.into_stored(meter.uuid);
        Self { meter, record }
    }

    /// The meter this entry is metered against.
    #[must_use]
    pub const fn meter(&self) -> &MeterRef {
        &self.meter
    }

    /// The entry, in storage shape.
    #[must_use]
    pub const fn record(&self) -> &StoredUsageRecord {
        &self.record
    }

    /// Splits the pair back into its halves.
    #[must_use]
    pub fn into_parts(self) -> (MeterRef, StoredUsageRecord) {
        (self.meter, self.record)
    }
}

impl StoredUsageRecord {
    /// This entry's kind, derived from the withdrawal it carries.
    #[must_use]
    pub const fn entry_type(&self) -> EntryType {
        if self.invalidation.is_some() {
            EntryType::Invalidation
        } else {
            EntryType::Record
        }
    }

    /// Whether `other` carries the same caller-supplied fields as `self`.
    ///
    /// The reference-keyed counterpart of [`UsageRecord::caller_supplied_eq`],
    /// and equivalent to it, because one identifier maps to one reference for
    /// the life of the installation. See that method for why `invalidation` is
    /// compared and the three server-assigned fields are not.
    #[must_use]
    pub fn caller_supplied_eq(&self, other: &Self) -> bool {
        let Self {
            id: _,
            gts_type_uuid,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at: _,
            origin: _,
            invalidation,
            window_start,
            window_end,
        } = self;
        let Self {
            id: _,
            gts_type_uuid: other_gts_type_uuid,
            tenant_id: other_tenant_id,
            resource_ref: other_resource_ref,
            subject_ref: other_subject_ref,
            metadata: other_metadata,
            quantity: other_quantity,
            idempotency_key: other_idempotency_key,
            accepted_at: _,
            origin: _,
            invalidation: other_invalidation,
            window_start: other_window_start,
            window_end: other_window_end,
        } = other;
        gts_type_uuid == other_gts_type_uuid
            && tenant_id == other_tenant_id
            && resource_ref == other_resource_ref
            && subject_ref == other_subject_ref
            && metadata == other_metadata
            && quantity == other_quantity
            && idempotency_key == other_idempotency_key
            && invalidation == other_invalidation
            && window_start == other_window_start
            && window_end == other_window_end
    }

    /// Re-attaches `gts_type_id`, producing the domain model.
    ///
    /// The identifier is supplied by the caller rather than derived here:
    /// nothing in this crate can derive one from the other.
    ///
    /// **This method does not verify that `gts_type_id` corresponds to
    /// [`Self::gts_type_uuid`]**, and cannot. A caller re-attaching an
    /// identifier to a record returned by a plugin must check the reference
    /// it expected against the one it got *before* calling this, or it will
    /// relabel a foreign record rather than detect one.
    #[must_use]
    pub fn into_usage_record(self, gts_type_id: MeterTypeId) -> UsageRecord {
        let Self {
            id,
            gts_type_uuid: _,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at,
            origin,
            invalidation,
            window_start,
            window_end,
        } = self;
        UsageRecord {
            id,
            gts_type_id,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at,
            origin,
            invalidation,
            window_start,
            window_end,
        }
    }
}

impl UsageRecord {
    /// Replaces this record's identifier with the reference it was issued
    /// for, producing the shape the Plugin SPI speaks.
    ///
    /// The reference is supplied rather than derived, for
    /// [`StoredUsageRecord::into_usage_record`]'s reason.
    #[must_use]
    pub fn into_stored(self, gts_type_uuid: Uuid) -> StoredUsageRecord {
        let Self {
            id,
            gts_type_id: _,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at,
            origin,
            invalidation,
            window_start,
            window_end,
        } = self;
        StoredUsageRecord {
            id,
            gts_type_uuid,
            tenant_id,
            resource_ref,
            subject_ref,
            metadata,
            quantity,
            idempotency_key,
            accepted_at,
            origin,
            invalidation,
            window_start,
            window_end,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "stored_tests.rs"]
mod stored_tests;
