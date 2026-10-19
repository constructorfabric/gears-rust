//! `SeaORM` entities of the tables the foundation owns (DECOMPOSITION §1 table ownership rule). Every
//! table is `bss_rating__<name>` (T-D-60, database ADR-0001) and every entity is `Scopable` on its
//! `tenant_id`.

pub mod exception;
pub mod inbox;
pub mod operation;
pub mod operator_audit;
