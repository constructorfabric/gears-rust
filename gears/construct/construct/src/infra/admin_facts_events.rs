use crate::domain::admin_facts::{AdminFactsEvent, AdminFactsEvents};

/// [`AdminFactsEvents`] written to the structured log, until the Audit gear
/// ships. An event names the administrator, the subject and the fact, and
/// never a value.
///
/// @cpt-dod:cpt-cf-construct-dod-admin-facts-events:p2
#[derive(Debug, Default)]
pub struct LogAdminFactsEvents;

impl AdminFactsEvents for LogAdminFactsEvents {
    #[expect(
        clippy::cognitive_complexity,
        reason = "flat match on the event enum; the tracing macros count toward the complexity"
    )]
    fn record(&self, event: AdminFactsEvent) {
        // @cpt-begin:cpt-cf-construct-algo-admin-facts-log:p2:inst-log-event
        match event {
            AdminFactsEvent::Read {
                tenant_id,
                subject_id,
                by,
                categories,
            } => tracing::info!(
                target: "construct::audit",
                event = "admin_facts_read",
                %tenant_id,
                %subject_id,
                caller = %by,
                categories = ?categories.iter().map(|category| category.name()).collect::<Vec<_>>(),
                "administrator read a subject's facts"
            ),
            AdminFactsEvent::Added {
                tenant_id,
                subject_id,
                by,
                fact_id,
                category,
            } => tracing::info!(
                target: "construct::audit",
                event = "admin_fact_added",
                %tenant_id,
                %subject_id,
                caller = %by,
                fact_id = %fact_id.0,
                category = category.name(),
                "administrator added a fact"
            ),
            AdminFactsEvent::Edited {
                tenant_id,
                subject_id,
                by,
                fact_id,
                new_fact_id,
                category,
            } => tracing::info!(
                target: "construct::audit",
                event = "admin_fact_edited",
                %tenant_id,
                %subject_id,
                caller = %by,
                fact_id = %fact_id.0,
                new_fact_id = %new_fact_id.0,
                category = category.name(),
                "administrator edited a fact"
            ),
            AdminFactsEvent::Deleted {
                tenant_id,
                subject_id,
                by,
                fact_id,
                category,
            } => tracing::info!(
                target: "construct::audit",
                event = "admin_fact_deleted",
                %tenant_id,
                %subject_id,
                caller = %by,
                fact_id = %fact_id.0,
                category = category.name(),
                "administrator deleted a fact"
            ),
            AdminFactsEvent::Refused {
                tenant_id,
                subject_id,
                by,
                fact_id,
                reason,
            } => tracing::info!(
                target: "construct::audit",
                event = "admin_fact_refused",
                %tenant_id,
                %subject_id,
                caller = %by,
                fact_id = fact_id.as_ref().map(|key| tracing::field::display(&key.0)),
                reason = reason.name(),
                "administrator's value refused; nothing was stored"
            ),
        }
        // @cpt-end:cpt-cf-construct-algo-admin-facts-log:p2:inst-log-event
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use uuid::Uuid;

    use super::*;
    use crate::domain::one_step::NotStored;
    use crate::domain::plan::NodeKey;
    use crate::domain::profile::Category;
    use crate::domain::record_intake::DropReason;

    #[tracing_test::traced_test]
    #[test]
    fn an_edit_names_the_administrator_the_old_and_the_new_fact() {
        let (tenant, subject, admin) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        LogAdminFactsEvents.record(AdminFactsEvent::Edited {
            tenant_id: tenant,
            subject_id: subject,
            by: admin,
            fact_id: NodeKey("construct:t:old".to_owned()),
            new_fact_id: NodeKey("construct:t:new".to_owned()),
            category: Category::Skill,
        });

        assert!(logs_contain("construct::audit"));
        assert!(logs_contain("event=\"admin_fact_edited\""));
        assert!(logs_contain(&format!("tenant_id={tenant}")));
        assert!(logs_contain(&format!("subject_id={subject}")));
        assert!(logs_contain(&format!("caller={admin}")));
        assert!(logs_contain("fact_id=construct:t:old"));
        assert!(logs_contain("new_fact_id=construct:t:new"));
        assert!(logs_contain("category=\"skill\""));
    }

    #[tracing_test::traced_test]
    #[test]
    fn a_read_names_each_category_it_served() {
        let (tenant, subject, admin) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        LogAdminFactsEvents.record(AdminFactsEvent::Read {
            tenant_id: tenant,
            subject_id: subject,
            by: admin,
            categories: BTreeSet::from([Category::Role, Category::Skill]),
        });

        assert!(logs_contain("event=\"admin_facts_read\""));
        assert!(logs_contain(&format!("caller={admin}")));
        assert!(logs_contain("\"role\""));
        assert!(logs_contain("\"skill\""));
    }

    #[tracing_test::traced_test]
    #[test]
    fn an_add_a_delete_and_a_refused_edit_name_their_fact() {
        let (tenant, subject, admin) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        LogAdminFactsEvents.record(AdminFactsEvent::Added {
            tenant_id: tenant,
            subject_id: subject,
            by: admin,
            fact_id: NodeKey("construct:t:added".to_owned()),
            category: Category::Skill,
        });
        LogAdminFactsEvents.record(AdminFactsEvent::Deleted {
            tenant_id: tenant,
            subject_id: subject,
            by: admin,
            fact_id: NodeKey("construct:t:deleted".to_owned()),
            category: Category::Role,
        });
        LogAdminFactsEvents.record(AdminFactsEvent::Refused {
            tenant_id: tenant,
            subject_id: subject,
            by: admin,
            fact_id: Some(NodeKey("construct:t:edited".to_owned())),
            reason: NotStored::Dropped(DropReason::PersonalizationOff),
        });

        assert!(logs_contain("event=\"admin_fact_added\""));
        assert!(logs_contain("fact_id=construct:t:added"));
        assert!(logs_contain("event=\"admin_fact_deleted\""));
        assert!(logs_contain("fact_id=construct:t:deleted"));
        assert!(logs_contain("event=\"admin_fact_refused\""));
        assert!(logs_contain("fact_id=construct:t:edited"));
        assert!(logs_contain(&format!(
            "reason=\"{}\"",
            DropReason::PersonalizationOff.name()
        )));
    }
}
