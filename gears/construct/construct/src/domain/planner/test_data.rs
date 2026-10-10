use serde_json::json;
use uuid::Uuid;

use super::catalog::PropertyCatalog;
use crate::domain::profile::{Category, Fact, Profile, ProfileVersion};

pub(super) const TENANT: Uuid = Uuid::from_u128(0x6b1f_0c2a_93d4_4e8f_a1b2_c3d4_e5f6_0718);
pub(super) const SUBJECT: Uuid = Uuid::from_u128(0x2c9e_7a41_5b3f_4d6e_8f90_1a2b_3c4d_5e6f);

pub(super) fn catalog() -> PropertyCatalog {
    PropertyCatalog::load().expect("the catalog")
}

pub(super) fn profile() -> Profile {
    Profile {
        version: ProfileVersion(7),
        facts: vec![
            Fact {
                node_key: format!("construct:{TENANT}:5d2c8a3e-0f41-4b7a-9c6d-2e8f1a3b4c5d"),
                category: Category::Roles,
                property: "role".to_owned(),
                value: json!("teacher"),
            },
            Fact {
                node_key: format!("construct:{TENANT}:a7e3f9b1-2c4d-4e6f-8a0b-1c3d5e7f9a2b"),
                category: Category::Skills,
                property: "skills".to_owned(),
                value: json!({ "name": "Python" }),
            },
            Fact {
                node_key: format!("construct:{TENANT}:c4b6d8e0-1f3a-4c5e-9b7d-0e2f4a6c8e1b"),
                category: Category::Identity,
                property: "full_name".to_owned(),
                value: json!("Jane Doe"),
            },
        ],
    }
}
