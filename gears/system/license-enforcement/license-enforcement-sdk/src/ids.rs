//! Identifiers of the license-enforcement entities.
//!
//! Distinct newtypes over [`Uuid`], so a pack id cannot be passed where a grant
//! id is expected. On the wire each one is a plain UUID string.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
            utoipa::ToSchema,
        )]
        #[serde(transparent)]
        #[schema(value_type = String, format = Uuid)]
        pub struct $name(pub Uuid);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::from_str(s).map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }
    };
}

uuid_id!(
    /// Id of a grant. The admission plugin gives the id of a root; the gear
    /// gives the id of a sub-grant.
    GrantId
);
uuid_id!(
    /// Id of a pack.
    PackId
);
uuid_id!(
    /// Id shared by the sub-grants that one `delegate` created.
    DelegationId
);
uuid_id!(
    /// Id of admitted evidence.
    EvidenceId
);
