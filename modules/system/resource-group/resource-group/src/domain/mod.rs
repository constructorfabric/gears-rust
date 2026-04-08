// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-module-scaffold:p1
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

pub mod error;
pub mod group_service;
pub mod membership_service;
pub mod read_service;
pub mod repo;
pub mod rg_service;
pub mod seeding;
pub mod type_service;
pub mod validation;

/// Type alias for the database provider used by domain services.
pub(crate) type DbProvider = modkit_db::DBProvider<modkit_db::DbError>;
