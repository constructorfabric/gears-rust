use std::collections::BTreeSet;

use crate::domain::profile::Category;
use crate::domain::subject_control::{SubjectControlEvent, SubjectControlEvents};

/// [`SubjectControlEvents`] written to the structured log, until the Audit
/// gear ships. An event names the categories it touched and never a value.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-control-events:p1
#[derive(Debug, Default)]
pub struct LogSubjectControlEvents;

fn names(categories: &BTreeSet<Category>) -> String {
    categories
        .iter()
        .map(|category| category.name())
        .collect::<Vec<_>>()
        .join(",")
}

impl SubjectControlEvents for LogSubjectControlEvents {
    #[expect(
        clippy::cognitive_complexity,
        reason = "three log lines; the tracing macros count toward the complexity"
    )]
    fn record(&self, event: SubjectControlEvent) {
        // @cpt-begin:cpt-cf-construct-algo-subject-control-log:p1:inst-log-event
        match event {
            SubjectControlEvent::Viewed {
                tenant_id,
                subject_id,
                categories,
            } => tracing::info!(
                target: "construct::audit",
                event = "subject_facts_viewed",
                %tenant_id,
                %subject_id,
                caller = %subject_id,
                categories = %names(&categories),
                "subject viewed its facts"
            ),
            SubjectControlEvent::Deleted {
                tenant_id,
                subject_id,
                fact_id,
                category,
            } => tracing::info!(
                target: "construct::audit",
                event = "subject_fact_deleted",
                %tenant_id,
                %subject_id,
                caller = %subject_id,
                fact_id = %fact_id.0,
                category = category.name(),
                "subject deleted a fact"
            ),
            SubjectControlEvent::Exported {
                tenant_id,
                subject_id,
                caller,
                categories,
            } => tracing::info!(
                target: "construct::audit",
                event = "subject_data_exported",
                %tenant_id,
                %subject_id,
                %caller,
                categories = %names(&categories),
                "subject data exported"
            ),
        }
        // @cpt-end:cpt-cf-construct-algo-subject-control-log:p1:inst-log-event
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::domain::plan::NodeKey;

    struct Ids {
        tenant: Uuid,
        subject: Uuid,
    }

    impl Ids {
        fn new() -> Self {
            Self {
                tenant: Uuid::new_v4(),
                subject: Uuid::new_v4(),
            }
        }

        /// The fields every event carries; `logs_contain` is the test's own.
        fn assert_logged_with_caller(&self, caller: Uuid, logs_contain: impl Fn(&str) -> bool) {
            assert!(logs_contain("construct::audit"), "the audit target");
            assert!(logs_contain(&format!("tenant_id={}", self.tenant)));
            assert!(logs_contain(&format!("subject_id={}", self.subject)));
            assert!(logs_contain(&format!("caller={caller}")));
        }
    }

    #[tracing_test::traced_test]
    #[test]
    fn a_view_names_its_tenant_subject_caller_and_categories() {
        let ids = Ids::new();

        LogSubjectControlEvents.record(SubjectControlEvent::Viewed {
            tenant_id: ids.tenant,
            subject_id: ids.subject,
            categories: BTreeSet::from([Category::Skill, Category::Role]),
        });

        assert!(logs_contain("event=\"subject_facts_viewed\""));
        ids.assert_logged_with_caller(ids.subject, logs_contain);
        assert!(logs_contain("categories=role,skill"));
    }

    #[tracing_test::traced_test]
    #[test]
    fn a_delete_names_the_fact_and_its_category_and_no_value() {
        let ids = Ids::new();

        LogSubjectControlEvents.record(SubjectControlEvent::Deleted {
            tenant_id: ids.tenant,
            subject_id: ids.subject,
            fact_id: NodeKey("construct:t:fact-1".to_owned()),
            category: Category::Skill,
        });

        assert!(logs_contain("event=\"subject_fact_deleted\""));
        ids.assert_logged_with_caller(ids.subject, logs_contain);
        assert!(logs_contain("fact_id=construct:t:fact-1"));
        assert!(logs_contain("category=\"skill\""));
    }

    #[tracing_test::traced_test]
    #[test]
    fn an_export_names_the_officer_as_caller() {
        let ids = Ids::new();
        let officer = Uuid::new_v4();

        LogSubjectControlEvents.record(SubjectControlEvent::Exported {
            tenant_id: ids.tenant,
            subject_id: ids.subject,
            caller: officer,
            categories: BTreeSet::from([Category::Preference]),
        });

        assert!(logs_contain("event=\"subject_data_exported\""));
        ids.assert_logged_with_caller(officer, logs_contain);
        assert!(logs_contain("categories=preference"));
    }
}
