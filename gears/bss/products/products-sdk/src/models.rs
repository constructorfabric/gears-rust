//! Wire types of the SKU registry. Spec §4 and §2.2 (`SkuVersion`).
use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkuType {
    Recurring,
    Usage,
    OneTime,
    Bundle,
}
impl SkuType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recurring => "recurring",
            Self::Usage => "usage",
            Self::OneTime => "one_time",
            Self::Bundle => "bundle",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "recurring" => Some(Self::Recurring),
            "usage" => Some(Self::Usage),
            "one_time" => Some(Self::OneTime),
            "bundle" => Some(Self::Bundle),
            _ => None,
        }
    }
}

/// A SKU's lifecycle. `retiring` is not one (P-D-248): a retire under review keeps the lifecycle
/// the SKU had and sets `retire_pending`. A stored `retiring` is a legacy audit token, mapped
/// when the history is read, never a value of this set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Draft,
    Published,
    Deprecated,
    Retired,
}
impl Lifecycle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Deprecated => "deprecated",
            Self::Retired => "retired",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "published" => Some(Self::Published),
            "deprecated" => Some(Self::Deprecated),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }
}

/// A lifecycle change whose date has not arrived (P-D-249). The head's `lifecycle` stays as it
/// was until `from`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LifecycleNext {
    pub lifecycle: Lifecycle,
    #[serde(with = "iso_date")]
    pub from: Date,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingTiming {
    Advance,
    Arrears,
}
impl BillingTiming {
    /// The token stored and carried on the wire (RS-48, as `SkuType` and `Lifecycle` have).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Advance => "advance",
            Self::Arrears => "arrears",
        }
    }
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "advance" => Some(Self::Advance),
            "arrears" => Some(Self::Arrears),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Category {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    pub is_default: bool,
    pub sort_order: i32,
    pub status: String,
    pub version: i64,
    /// When the retired category was archived (P-D-263); `None` while it is not. A mark, not a
    /// status: nothing that reads the status changes.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
    /// Who archived it; set with [`Self::archived_at`].
    #[serde(default)]
    pub archived_by: Option<Uuid>,
}

/// The registry's SKU as the doors return it. Field names are `snake_case` on the wire (the
/// `api_dto` macro's rule, conv §4); consumers read them as such. The instants are RFC 3339
/// strings, as `SkuDto` writes them (P-D-226): the derive's default would write `time`'s
/// tuples, which no door sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "sellable, type_change_pending and retire_pending are three independent flags (P-D-248)"
)]
pub struct Sku {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub code: String,
    pub name: String,
    pub r#type: SkuType,
    /// `None`: the SKU has no category (P-D-196); the wire carries `null`.
    pub category_id: Option<Uuid>,
    pub description: String,
    pub sellable: bool,
    pub lifecycle: Lifecycle,
    /// Set while a `sku_retire` unit is in review (P-D-248). The lifecycle stays.
    #[serde(default)]
    pub retire_pending: bool,
    /// A dated lifecycle change that has not arrived (P-D-249). Null when none is pending.
    #[serde(default)]
    pub lifecycle_next: Option<LifecycleNext>,
    pub revision: i64,
    pub published_version: i64,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<BillingTiming>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
    pub type_change_pending: bool,
    pub pending_unit_id: Option<Uuid>,
    pub approved_by_unit_id: Option<Uuid>,
    pub created_by: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// When the retired SKU was archived (P-D-263); `None` while it is not. A mark, not a
    /// lifecycle: a read by id, the browse, the consumer reads and the pinned facts ignore it.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
    /// Who archived it; set with [`Self::archived_at`].
    #[serde(default)]
    pub archived_by: Option<Uuid>,
}

/// The business content of a SKU — what an approval unit fingerprints (spec §6: never lock,
/// version, revision or lifecycle fields).
///
/// **A storage format, too** (RS-22): this derive writes the append-only `content` of every stored
/// version and the proposal of every unit, and reads them back. So its serde is compatible
/// forever: a field added is an `Option` (a stored row without it reads `None`) or carries
/// `#[serde(default)]`; a field is never renamed without `#[serde(alias)]` of the old name; and
/// no `deny_unknown_fields`, so a row a later build wrote reads here too. The gear's stored-row
/// fixtures pin it (`sku_repo_tests::stored_content_fixtures_keep_reading`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkuContent {
    pub code: String,
    pub name: String,
    pub r#type: SkuType,
    /// `None`: the SKU has no category (P-D-196).
    pub category_id: Option<Uuid>,
    pub description: String,
    pub sellable: bool,
    pub gl_code: Option<String>,
    pub tax_category: Option<String>,
    pub invoice_line_template: Option<String>,
    pub billing_timing: Option<BillingTiming>,
    pub usage_type_ref: Option<String>,
    pub unit: Option<String>,
}
impl From<&Sku> for SkuContent {
    fn from(s: &Sku) -> Self {
        Self {
            code: s.code.clone(),
            name: s.name.clone(),
            r#type: s.r#type,
            category_id: s.category_id,
            description: s.description.clone(),
            sellable: s.sellable,
            gl_code: s.gl_code.clone(),
            tax_category: s.tax_category.clone(),
            invoice_line_template: s.invoice_line_template.clone(),
            billing_timing: s.billing_timing,
            usage_type_ref: s.usage_type_ref.clone(),
            unit: s.unit.clone(),
        }
    }
}

/// One published version of a SKU, appended on publish and on every applied change (spec §2.2).
/// On the wire as `SkuVersionDto` writes it (P-D-226): the date `YYYY-MM-DD`, the instant RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SkuVersion {
    pub sku_id: Uuid,
    pub published_version: i64,
    #[serde(with = "iso_date")]
    pub effective_from: Date,
    pub content: SkuContent,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Payload of the `SkuChanged` event, as the registry emits it (P-D-226): `camelCase`, the date
/// `YYYY-MM-DD`, and the actor whose approval made the change (the PRD's shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkuChangedPayload {
    pub tenant_id: Uuid,
    pub sku_id: Uuid,
    pub changed: Vec<String>,
    #[serde(with = "iso_date")]
    pub effective_from: Date,
    pub published_version: i64,
    pub actor_ref: Uuid,
}

/// A civil date as `YYYY-MM-DD` on the wire, the doors' and the events' form.
mod iso_date {
    use serde::{Deserialize, Deserializer, Serializer};
    use time::Date;

    const FORMAT: &str = "[year]-[month]-[day]";

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires a borrowed field serializer"
    )]
    pub fn serialize<S: Serializer>(date: &Date, serializer: S) -> Result<S::Ok, S::Error> {
        let format = time::format_description::parse_borrowed::<1>(FORMAT)
            .map_err(serde::ser::Error::custom)?;
        let text = date.format(&format).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(&text)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Date, D::Error> {
        let text = String::deserialize(deserializer)?;
        let format = time::format_description::parse_borrowed::<1>(FORMAT)
            .map_err(serde::de::Error::custom)?;
        Date::parse(&text, &format).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod registry_tests {
    #[test]
    fn sku_type_uses_the_pricebook_vocabulary() {
        for token in ["recurring", "usage", "one_time", "bundle"] {
            assert_eq!(
                super::SkuType::parse(token).map(super::SkuType::as_str),
                Some(token)
            );
        }
        for token in ["offer", "component", "product", "service", "", "Usage"] {
            assert_eq!(super::SkuType::parse(token), None);
        }
    }
}

/// Kind of owner object protected by a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    PriceBookEntry,
    PlanItem,
    SoldAs,
}
/// Released attempts remain tombstones and no longer block a fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceState {
    Reserved,
    Confirmed,
    Released,
}
/// The attempt handle used for confirmation, release and reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ReservationReceipt {
    pub reservation_id: Uuid,
    pub state: ReferenceState,
}
