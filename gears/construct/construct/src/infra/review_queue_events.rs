use crate::domain::review_queue::{ReviewQueueEvent, ReviewQueueEvents};

/// [`ReviewQueueEvents`] written to the structured log, until the Audit gear
/// ships. An event names the request and the fact, and never a value, a
/// comment or a reason.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-events:p1
#[derive(Debug, Default)]
pub struct LogReviewQueueEvents;

impl ReviewQueueEvents for LogReviewQueueEvents {
    fn record(&self, event: ReviewQueueEvent) {
        // @cpt-begin:cpt-cf-construct-algo-review-queue-log:p1:inst-log-event
        match event {
            ReviewQueueEvent::Requested {
                tenant_id,
                subject_id,
                request_id,
                fact_id,
                category,
            } => tracing::info!(
                target: "construct::audit",
                event = "review_requested",
                %tenant_id,
                %subject_id,
                caller = %subject_id,
                %request_id,
                fact_id = %fact_id.0,
                category = category.name(),
                "subject flagged a fact for review"
            ),
            ReviewQueueEvent::Resolved {
                tenant_id,
                subject_id,
                request_id,
                fact_id,
                outcome,
                by,
            } => tracing::info!(
                target: "construct::audit",
                event = "review_resolved",
                %tenant_id,
                %subject_id,
                caller = %by,
                %request_id,
                fact_id = %fact_id.0,
                outcome = outcome.name(),
                corrected_fact_id = outcome.corrected_fact_id().map(|key| tracing::field::display(&key.0)),
                "review request resolved"
            ),
        }
        // @cpt-end:cpt-cf-construct-algo-review-queue-log:p1:inst-log-event
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::domain::plan::NodeKey;
    use crate::domain::profile::Category;
    use crate::domain::review_queue::ReviewOutcome;

    #[tracing_test::traced_test]
    #[test]
    fn a_flag_names_the_request_the_fact_and_the_subject_as_caller() {
        let (tenant, subject, request) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        LogReviewQueueEvents.record(ReviewQueueEvent::Requested {
            tenant_id: tenant,
            subject_id: subject,
            request_id: request,
            fact_id: NodeKey("construct:t:fact-1".to_owned()),
            category: Category::Skill,
        });

        assert!(logs_contain("construct::audit"));
        assert!(logs_contain("event=\"review_requested\""));
        assert!(logs_contain(&format!("tenant_id={tenant}")));
        assert!(logs_contain(&format!("caller={subject}")));
        assert!(logs_contain(&format!("request_id={request}")));
        assert!(logs_contain("fact_id=construct:t:fact-1"));
        assert!(logs_contain("category=\"skill\""));
    }

    #[tracing_test::traced_test]
    #[test]
    fn a_correction_names_the_reviewer_the_outcome_and_the_new_fact() {
        let (tenant, subject, reviewer) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        LogReviewQueueEvents.record(ReviewQueueEvent::Resolved {
            tenant_id: tenant,
            subject_id: subject,
            request_id: Uuid::new_v4(),
            fact_id: NodeKey("construct:t:old".to_owned()),
            outcome: ReviewOutcome::Corrected(NodeKey("construct:t:new".to_owned())),
            by: reviewer,
        });

        assert!(logs_contain("event=\"review_resolved\""));
        assert!(logs_contain(&format!("subject_id={subject}")));
        assert!(logs_contain(&format!("caller={reviewer}")));
        assert!(logs_contain("outcome=\"corrected\""));
        assert!(logs_contain("corrected_fact_id=construct:t:new"));
    }
}
