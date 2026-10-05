//! Typed gear configuration.

use serde::Deserialize;

/// The retention floor: a replay key is kept at least 24 hours (P-D-198).
///
/// A **floor**, not a default: a window shorter than this expires a key while
/// the client that owns it is still retrying, and the next request on that
/// key takes it over and **re-executes the guarded mutation** — at-most-once
/// silently off. `ProductsConfig::default` happens to supply this same value,
/// which is why an unconfigured boot needs no clamp; a *configured* one does.
pub const IDEMPOTENCY_RETENTION_FLOOR_HOURS: u32 = 24;

/// The longest retention window this gear will stamp: ten years, in hours.
///
/// The field is a `u32` of hours, and its largest value is roughly 490 000
/// years — far past what `time` can add to an instant, so
/// `OffsetDateTime::checked_add` returns `None` and the stamp has no
/// representable answer at all. A ceiling is what keeps the resolution
/// **total**: every `u32` an operator can write maps to a window that is
/// neither below the floor nor unrepresentable, so no caller downstream has
/// to invent one. Ten years is chosen because the value being resolved is how
/// long a *client's retry key* is remembered; anything past a decade is a
/// mis-entered unit (seconds or minutes pasted into an hours field), not a
/// retention policy anyone wrote on purpose.
pub const IDEMPOTENCY_RETENTION_CEILING_HOURS: u32 = 24 * 365 * 10;

/// [`ProductsConfig::usage_type_resolver_timeout_ms`]'s default: two seconds
/// (P-D-203).
pub const USAGE_TYPE_RESOLVER_TIMEOUT_MS_DEFAULT: u32 = 2_000;

/// The usage-type catalogs a deployment may name as a **fallback**.
///
/// Only reached when `ClientHub` carries neither a `UsageTypeCatalog` nor a
/// `UsageCollectorClientV1`.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum UsageTypeCatalogSource {
    /// No catalog to ask. The default, and the honest one: the pick-list
    /// answers **501** and every usage-SKU publish keeps failing closed.
    #[default]
    Unconfigured,
    /// Serve a **fabricated** set from this process. Named at length so it
    /// cannot be selected without saying so: a deployment carrying this value
    /// is showing operators usage types no collector issued, and a meter
    /// declared against one names a stream nothing will ever report. See
    /// [`crate::infra::usage_types::LocalDevStaticUsageTypes`].
    LocalDevStaticUsageTypes,
}

/// The gear's boot configuration.
///
/// Every field has a default, so a boot that configures the gear at all gets a
/// working one; `deny_unknown_fields` is what turns a typo in the operator's
/// file into a boot failure rather than a silently ignored setting.
///
/// # `client_wiring.product_catalog_client_v1`
///
/// Products provides pricing's `ProductCatalogClientV1` through
/// `#[toolkit::provides]`. `toolkit::wiring::read_wiring` reads
/// `gears.bss-products.config.client_wiring.product_catalog_client_v1`.
/// **An absent `client_wiring` section, or an absent key, is
/// `ClientWiring::Local`** — a same-process `core-server` needs no config
/// and gets the in-process catalog provider. Only a split deployment
/// declares the REST arm:
///
/// ```yaml
/// gears:
///   bss-products:
///     config:
///       client_wiring:
///         product_catalog_client_v1:
///           transport: rest
///           endpoint: "http://bss-products.virtuozzo.svc:8080"
/// ```
///
/// `ClientWiring` is `#[serde(tag = "transport")]`. The tagged object
/// above is what `toolkit::wiring::read_wiring` accepts. The plan's
/// nested spelling (`rest: { endpoint: ... }`) **will not parse** — it
/// is not a `transport` tag. The key still lives in this typed config so
/// `deny_unknown_fields` does not refuse a split deployment.
///
/// A typo in a *value* has no such spelling, which is why
/// [`Self::resolved_idempotency_retention_hours`] exists: `deny_unknown_fields`
/// catches `idempotency_retention_hous`, and nothing in serde catches a `0`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct ProductsConfig {
    /// Age of an abandoned SKU fence before recovery.
    pub fence_ttl_minutes: u32,
    /// Authenticated service subject ids bound to owner gear names. Empty denies owner operations.
    pub reference_principals: std::collections::BTreeMap<uuid::Uuid, String>,
    /// How long an idempotency key is retained, in hours, **as the operator
    /// wrote it**.
    ///
    /// Read this field only to report what was configured;
    /// [`Self::resolved_idempotency_retention_hours`] is what anything
    /// stamping an expiry must use.
    pub idempotency_retention_hours: u32,

    /// Whether a boot without a reachable event-broker is a **failure**
    /// (P-D-199).
    ///
    /// `Gear::init` binds the broker SDK's producer when `ClientHub` carries an
    /// `EventBrokerApi` and falls back to a holding processor when it does not,
    /// so a deployment with no broker still boots and accumulates its events
    /// undelivered. That fallback has one dangerous property: it is
    /// **indistinguishable from a broker the gear failed to reach**, and the
    /// only signal is one `warn!` line.
    ///
    /// This is the operator's switch for that. `true` turns the fallback into a
    /// boot failure, so a deployment that is supposed to publish cannot
    /// silently stop publishing.
    ///
    /// **Default `false`, and that default is a measurement rather than a
    /// preference**: as of 2026-08-30 no gear in this workspace registers a
    /// `dyn EventBrokerApi` in any `ClientHub`, so defaulting to `true` would
    /// make this gear un-bootable everywhere today. The default is expected to
    /// invert the moment a provider exists.
    pub require_broker: bool,

    /// How long one usage-type resolve through the collector adapter may take,
    /// in milliseconds (P-D-203; default 2000). Submit and apply resolve before
    /// their transaction opens, so the bound costs latency and never a held
    /// lock; a call that outlives it is `Unavailable`.
    pub usage_type_resolver_timeout_ms: u32,

    /// Which usage-type catalog to fall back to when **nothing is registered**
    /// (P-D-184).
    ///
    /// A registered `UsageTypeCatalog`, or the usage collector's own client,
    /// wins over this: the value is the last step of `gear.rs`'s four, not the
    /// first. Default [`UsageTypeCatalogSource::Unconfigured`].
    pub usage_type_catalog_mode: UsageTypeCatalogSource,

    /// `#[toolkit::provides]` wiring for `product_catalog_client_v1`.
    ///
    /// Absent (the default) is in-process `ClientWiring::Local`. Present so
    /// a split-deployment `client_wiring` key is not refused by
    /// `deny_unknown_fields`; `toolkit::wiring::read_wiring` is what
    /// interprets the value.
    #[serde(default)]
    pub client_wiring: serde_json::Value,
}

impl Default for ProductsConfig {
    fn default() -> Self {
        Self {
            fence_ttl_minutes: 30,
            reference_principals: std::collections::BTreeMap::new(),
            idempotency_retention_hours: IDEMPOTENCY_RETENTION_FLOOR_HOURS,
            require_broker: false,
            usage_type_resolver_timeout_ms: USAGE_TYPE_RESOLVER_TIMEOUT_MS_DEFAULT,
            usage_type_catalog_mode: UsageTypeCatalogSource::Unconfigured,
            client_wiring: serde_json::Value::Null,
        }
    }
}

impl ProductsConfig {
    /// The configured window, clamped into
    /// `[IDEMPOTENCY_RETENTION_FLOOR_HOURS, IDEMPOTENCY_RETENTION_CEILING_HOURS]`
    /// — the value every expiry stamp is taken from.
    ///
    /// # Clamped, not refused, and why
    ///
    /// Refusing the boot would take a whole registry offline over a value
    /// whose resolution is plain arithmetic, and would do it on the restart
    /// of a deployment that had been serving happily.
    ///
    /// The operator's mistake does not become invisible in exchange: the
    /// gear's `init` compares this answer with the configured field and logs
    /// the raise at `WARN`, naming both numbers. What must never happen is
    /// the third option — carrying a `0` through to
    /// `crate::api::rest`'s `idempotency_expiry`, which stamps
    /// `expires_at == now`, so the very next request reads the key as expired,
    /// takes it over, and runs the guarded mutation a second time under one
    /// key. That is at-most-once off with no boot failure and no log at all,
    /// and it is the outcome both other options exist to rule out.
    #[must_use]
    pub fn resolved_idempotency_retention_hours(&self) -> u32 {
        self.idempotency_retention_hours.clamp(
            IDEMPOTENCY_RETENTION_FLOOR_HOURS,
            IDEMPOTENCY_RETENTION_CEILING_HOURS,
        )
    }

    /// `usage_type_resolver_timeout_ms` as a `Duration` — the bound on one
    /// collector call (P-D-203).
    #[must_use]
    pub fn usage_type_resolver_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(u64::from(self.usage_type_resolver_timeout_ms))
    }

    /// Boot-time validation: a zero resolver timeout would answer every
    /// usage-type resolve `Unavailable`, and a zero fence TTL would make every
    /// fence no pending unit holds expirable the moment it exists (RS-62), so
    /// both are refused before anything runs.
    ///
    /// # Errors
    ///
    /// A sentence naming the field and why its value admits nothing.
    pub fn validate(&self) -> Result<(), String> {
        if self.usage_type_resolver_timeout_ms == 0 {
            return Err(
                "usage_type_resolver_timeout_ms = 0 admits nothing at all: every usage-type \
                 resolve would time out before it is asked"
                    .to_owned(),
            );
        }
        if self.fence_ttl_minutes == 0 {
            return Err(
                "fence_ttl_minutes = 0 admits nothing at all: every read's orphan-fence recovery \
                 would lift a fence the moment it exists"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod config_tests;
