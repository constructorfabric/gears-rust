//! Dry runs of the publish operations: [`ManagementService::check_publish`]
//! runs an operation's plan - every check it makes, in its order, reading
//! only - and stops before the write. The operation runs the same plan, so a
//! dry run answers exactly what the operation would on the same state,
//! errors and their masking included. It reserves nothing: the operation
//! checks again, and its transaction still guards against a concurrent
//! writer.

use policy_engine_sdk::management::{ManagementError, PublishOperation};
use toolkit_security::SecurityContext;

use super::ContentStore;
use super::service::ManagementService;

impl<S: ContentStore> ManagementService<S> {
    /// Dry-runs `operation`. Capability: publish, as the operation.
    ///
    /// # Errors
    ///
    /// The error the operation would return.
    pub async fn check_publish(
        &self,
        ctx: &SecurityContext,
        operation: PublishOperation,
    ) -> Result<(), ManagementError> {
        match operation {
            PublishOperation::Activate {
                bundle_id,
                version_id,
            } => self
                .plan_activation(ctx, bundle_id, version_id)
                .await
                .map(drop),
            PublishOperation::Assign(spec) => self.plan_assign(ctx, &spec).await.map(drop),
            PublishOperation::UpdateAssignment { assignment_id } => {
                self.plan_update(ctx, assignment_id).await.map(drop)
            }
            PublishOperation::Unassign { assignment_id } => {
                self.plan_unassign(ctx, assignment_id).await.map(drop)
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dry_run_tests.rs"]
mod dry_run_tests;
