//! Store an empty edge discriminator as none.
//!
//! The edge key hashes an absent and an empty discriminator alike, so both
//! name one edge, but the `discriminator` column kept whichever form the
//! first write carried: one identity read back as `""` or as `NULL`, and an
//! `edge_discriminators` filter matched some of its rows and not others.
//! Ingest now normalises `""` to none before it writes; this brings the rows
//! written before it to the same form. No key changes, so nothing else moves.
//! `down` does not restore the empty strings: which rows had one is not
//! recorded, and none of them means anything `NULL` does not.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("UPDATE edge SET discriminator = NULL WHERE discriminator = ''")
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
