use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use construct_sdk::gts::PROFILE_ROOT_TYPE;
use graph_storage_sdk::GraphStorageClientV1;
use toolkit_odata::{CursorV1, ODataQuery};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::root::{Root, root_generation};
use super::{DEFAULT_CALL_TIMEOUT, bounded};
use crate::domain::deletion::{ProfileDirectory, SubjectsPage};
use crate::domain::error::DomainError;
use crate::domain::plan::NodeKey;
use crate::domain::subject_settings::SubjectKey;

/// How many profile roots one page lists.
const PAGE: u64 = 100;

/// [`ProfileDirectory`] over graph storage: the live profile roots of the
/// context's tenant, one per subject, in pages. A root an erasure removed is
/// not listed.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-job:p2
pub struct GraphProfileDirectory {
    client: Arc<dyn GraphStorageClientV1>,
    timeout: Duration,
}

impl GraphProfileDirectory {
    #[must_use]
    pub fn new(client: Arc<dyn GraphStorageClientV1>) -> Self {
        Self {
            client,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }
}

#[async_trait]
impl ProfileDirectory for GraphProfileDirectory {
    async fn subjects(
        &self,
        ctx: &SecurityContext,
        cursor: Option<String>,
    ) -> Result<SubjectsPage, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-expire:p2:inst-expire-subjects
        let mut query = ODataQuery::new().with_limit(PAGE);
        if let Some(cursor) = cursor {
            let cursor = CursorV1::decode(&cursor).map_err(|err| {
                DomainError::internal(format!("a profile root cursor cannot be read: {err}"))
            })?;
            query = query.with_cursor(cursor);
        }
        let page = bounded(
            self.timeout,
            "profile root listing",
            self.client
                .project_nodes(ctx, &[PROFILE_ROOT_TYPE.to_owned()], query),
        )
        .await?;
        // A root that cannot be read, or whose key is not its subject's in
        // this tenant, is left out and logged, so it does not stop retention
        // for the rest of the tenant.
        let tenant_id = ctx.subject_tenant_id();
        let subjects = page
            .items
            .iter()
            .filter_map(|row| {
                let key = NodeKey(row.node_key.clone());
                let subject = Root::subject_of(&key, row.payload.as_ref())
                    .and_then(|subject_id| own_root(&key, tenant_id, subject_id));
                subject
                    .inspect_err(|err| {
                        tracing::error!(
                            root = %key.0,
                            error = %err,
                            "construct: a profile root is left out of retention"
                        );
                    })
                    .ok()
            })
            .collect();
        Ok(SubjectsPage {
            subjects,
            next: page.page_info.next_cursor,
        })
        // @cpt-end:cpt-cf-construct-algo-retention-expire:p2:inst-expire-subjects
    }
}

/// `subject_id` when `key` is a root key of that subject in `tenant_id`, of
/// any generation.
fn own_root(key: &NodeKey, tenant_id: Uuid, subject_id: Uuid) -> Result<Uuid, DomainError> {
    let subject = SubjectKey {
        tenant_id,
        subject_id,
    };
    if root_generation(&subject, key).is_some() {
        Ok(subject_id)
    } else {
        Err(DomainError::internal(format!(
            "profile root {} is not the root of subject {subject_id} in tenant {tenant_id}",
            key.0
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_root_key_of_the_subject_in_the_tenant_is_its_root() {
        let (tenant_id, subject_id) = (Uuid::new_v4(), Uuid::new_v4());
        let first = format!("construct:{tenant_id}:root:{subject_id}");
        for own in [first.clone(), format!("{first}:3")] {
            assert_eq!(
                own_root(&NodeKey(own.clone()), tenant_id, subject_id).ok(),
                Some(subject_id),
                "{own}"
            );
        }
        for foreign in [
            format!("construct:{}:root:{subject_id}", Uuid::new_v4()),
            format!("construct:{tenant_id}:root:{}", Uuid::new_v4()),
            format!("{first}:x"),
            format!("{first}3"),
        ] {
            assert!(
                own_root(&NodeKey(foreign.clone()), tenant_id, subject_id).is_err(),
                "{foreign}"
            );
        }
    }
}
