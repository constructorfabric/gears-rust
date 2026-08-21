use sea_orm_migration::prelude::*;

pub mod comments_005;
pub mod commits_004;
pub mod initial_001;
pub mod issues_002;
pub mod pull_requests_003;
pub mod review_comments_006;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(initial_001::Migration),
            Box::new(issues_002::Migration),
            Box::new(pull_requests_003::Migration),
            Box::new(commits_004::Migration),
            Box::new(comments_005::Migration),
            Box::new(review_comments_006::Migration),
        ]
    }
}
