use event_broker_sdk::gts::EventV1Traits;
use toolkit_gts::gts_id;

fn main() {
    let _ = EventV1Traits::builder().build();

    let _ = EventV1Traits::builder()
        .with_topic(gts_id!("cf.core.events.topic.v1~example.eb.generic.topic.v1"))
        .build();

    let _ = EventV1Traits::builder()
        .with_allowed_subject_types([gts_id!("*")])
        .build();
}
