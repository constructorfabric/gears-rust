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
                category,
            } => tracing::info!(
                target: "construct::audit",
                event = "subject_fact_deleted",
                %tenant_id,
                %subject_id,
                caller = %subject_id,
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

    #[tracing_test::traced_test]
    #[test]
    fn each_event_names_its_kind_ids_and_categories_and_no_value() {
        let (tenant_id, subject_id, caller) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let events = LogSubjectControlEvents;

        events.record(SubjectControlEvent::Viewed {
            tenant_id,
            subject_id,
            categories: BTreeSet::from([Category::Skill, Category::Role]),
        });
        events.record(SubjectControlEvent::Deleted {
            tenant_id,
            subject_id,
            category: Category::Skill,
        });
        events.record(SubjectControlEvent::Exported {
            tenant_id,
            subject_id,
            caller,
            categories: BTreeSet::new(),
        });

        for event in [
            "subject_facts_viewed",
            "subject_fact_deleted",
            "subject_data_exported",
        ] {
            assert!(logs_contain(event), "{event}");
        }
        assert!(logs_contain(&format!("subject_id={subject_id}")));
        assert!(logs_contain(&format!("caller={caller}")));
        assert!(logs_contain("categories=role,skill") || logs_contain("categories=skill,role"));
    }
}
