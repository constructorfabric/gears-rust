use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// One durable run row: the CAS-guarded journal header and its progress projection.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "durable_execution__runs")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    owner_col = "owner_id",
    no_type
)]
pub struct Model {
    /// Run identifier, also the secure resource id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// Tenant that owns the run.
    pub tenant_id: Uuid,
    /// Subject that started the run.
    pub owner_id: Uuid,
    /// Registered definition name the run executes.
    pub definition: String,
    /// Contract fingerprint captured at start.
    pub fingerprint: String,
    /// Compare-and-swap token; incremented by every committed transition.
    pub revision: i64,
    /// Definition registration generation that admitted the run.
    pub registration_generation: i64,
    /// Persisted run status label, such as `queued` or `running`.
    pub status: String,
    /// Serialized journal metadata; activities, attempts and epochs live in normalized tables.
    pub journal: Vec<u8>,
    /// Versioned, payload-free progress projection committed with each revision.
    pub progress: Vec<u8>,
    /// Public progress status label used for filtered listing.
    pub progress_status: String,
    /// Row layout version; only `1` is readable.
    pub storage_version: i32,
    /// Number of normalized activity rows expected; a mismatch signals a torn read.
    pub activity_count: i32,
    /// Claim lease expiry; `None` when no worker holds the run.
    pub lease_until: Option<DateTimeUtc>,
    /// Next dispatch time; `None` when nothing is scheduled.
    pub due_at: Option<DateTimeUtc>,
    /// Creation time of the run.
    pub created_at: DateTimeUtc,
    /// Time of the last committed transition.
    pub updated_at: DateTimeUtc,
}
/// The run entity declares no `SeaORM` relations.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
