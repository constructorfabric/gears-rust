use crate::domain::deletion::{DeletionEvent, DeletionEvents};

/// [`DeletionEvents`] written to the structured log, until the Audit gear
/// ships. An event names the subject and counts; never a value.
///
/// @cpt-dod:cpt-cf-construct-dod-erasure-events:p1
#[derive(Debug, Default)]
pub struct LogDeletionEvents;

impl DeletionEvents for LogDeletionEvents {
    #[expect(
        clippy::cognitive_complexity,
        reason = "four log lines; the tracing macros count toward the complexity"
    )]
    fn record(&self, event: DeletionEvent) {
        // @cpt-begin:cpt-cf-construct-algo-erasure-log:p1:inst-log-event
        match event {
            DeletionEvent::ErasureRequested {
                tenant_id,
                subject_id,
            } => tracing::info!(
                target: "construct::audit",
                event = "subject_erasure_requested",
                %tenant_id,
                %subject_id,
                caller = %subject_id,
                "subject asked to erase everything"
            ),
            DeletionEvent::Erased {
                tenant_id,
                subject_id,
            } => tracing::info!(
                target: "construct::audit",
                event = "subject_erased",
                %tenant_id,
                %subject_id,
                "subject erased"
            ),
            DeletionEvent::FactsExpired {
                tenant_id,
                subject_id,
                facts,
            } => tracing::info!(
                target: "construct::audit",
                event = "facts_expired",
                %tenant_id,
                %subject_id,
                facts,
                "facts past the retention period removed"
            ),
            DeletionEvent::RowsExpired {
                tenant_id,
                review_requests,
                record_ids,
            } => tracing::info!(
                target: "construct::audit",
                event = "rows_expired",
                %tenant_id,
                review_requests,
                record_ids,
                "review requests and record identities past the retention period removed"
            ),
        }
        // @cpt-end:cpt-cf-construct-algo-erasure-log:p1:inst-log-event
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    /// The one logged line of `event`'s name, which must name the audit
    /// target and each of `fields`.
    fn assert_line(lines: &[&str], event: &str, fields: &[String]) -> Result<(), String> {
        let name = format!("event=\"{event}\"");
        let [line] = lines
            .iter()
            .filter(|line| line.contains(&name))
            .collect::<Vec<_>>()[..]
        else {
            return Err(format!("one line for {event}"));
        };
        if !line.contains("construct::audit") {
            return Err(format!("{event} is not on the audit target: {line}"));
        }
        match fields.iter().find(|field| !line.contains(field.as_str())) {
            Some(field) => Err(format!("{event} lacks {field}: {line}")),
            None => Ok(()),
        }
    }

    #[tracing_test::traced_test]
    #[test]
    fn each_event_is_logged_to_the_audit_target_with_its_name_ids_and_counts() {
        let (tenant_id, subject_id) = (Uuid::new_v4(), Uuid::new_v4());
        let events = LogDeletionEvents;
        events.record(DeletionEvent::ErasureRequested {
            tenant_id,
            subject_id,
        });
        events.record(DeletionEvent::Erased {
            tenant_id,
            subject_id,
        });
        events.record(DeletionEvent::FactsExpired {
            tenant_id,
            subject_id,
            facts: 7,
        });
        events.record(DeletionEvent::RowsExpired {
            tenant_id,
            review_requests: 2,
            record_ids: 3,
        });

        let (tenant, subject) = (
            format!("tenant_id={tenant_id}"),
            format!("subject_id={subject_id}"),
        );
        logs_assert(|lines: &[&str]| {
            assert_line(
                lines,
                "subject_erasure_requested",
                &[
                    tenant.clone(),
                    subject.clone(),
                    format!("caller={subject_id}"),
                ],
            )?;
            assert_line(lines, "subject_erased", &[tenant.clone(), subject.clone()])?;
            assert_line(
                lines,
                "facts_expired",
                &[tenant.clone(), subject.clone(), "facts=7".to_owned()],
            )?;
            assert_line(
                lines,
                "rows_expired",
                &[
                    tenant.clone(),
                    "review_requests=2".to_owned(),
                    "record_ids=3".to_owned(),
                ],
            )
        });
    }
}
