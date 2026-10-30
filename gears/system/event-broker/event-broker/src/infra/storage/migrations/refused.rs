//! A migration that fails with the reason it stands in for: what the
//! registry returns in place of migrations that could not be built, so the
//! platform's migration phase stops the process with that reason rather than
//! letting it start without the tables.

use sea_orm_migration::prelude::*;

pub struct Refused {
    pub reason: String,
}

impl MigrationName for Refused {
    fn name(&self) -> &'static str {
        "m00000000_000000_event_broker_refused"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Refused {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Custom(self.reason.clone()))
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
