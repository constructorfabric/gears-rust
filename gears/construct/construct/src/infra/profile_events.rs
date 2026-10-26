use crate::domain::profile_read::{ProfileEvents, ProfileReadEvent};

/// [`ProfileEvents`] written to the structured log, until the Audit gear
/// ships. The event names the categories read and never a value.
///
/// @cpt-dod:cpt-cf-construct-dod-profile-reader-event:p1
#[derive(Debug, Default)]
pub struct LogProfileEvents;

impl ProfileEvents for LogProfileEvents {
    fn read(&self, event: ProfileReadEvent) {
        // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-log
        let categories = event
            .categories
            .iter()
            .map(|category| category.name())
            .collect::<Vec<_>>()
            .join(",");
        tracing::info!(
            target: "construct::audit",
            event = "profile_read",
            tenant_id = %event.tenant_id,
            subject_id = %event.subject_id,
            caller = %event.caller,
            categories = %categories,
            "profile read"
        );
        // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-log
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::domain::profile::Category;

    #[tracing_test::traced_test]
    #[test]
    fn a_read_is_logged_with_who_read_whose_categories_and_no_value() {
        let event = ProfileReadEvent {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
            caller: Uuid::new_v4(),
            categories: vec![Category::Identity, Category::Skill],
        };

        LogProfileEvents.read(event.clone());

        assert!(logs_contain("construct::audit"));
        assert!(logs_contain("event=\"profile_read\""));
        assert!(logs_contain(&format!("tenant_id={}", event.tenant_id)));
        assert!(logs_contain(&format!("subject_id={}", event.subject_id)));
        assert!(logs_contain(&format!("caller={}", event.caller)));
        assert!(logs_contain("categories=identity,skill"));
    }
}
