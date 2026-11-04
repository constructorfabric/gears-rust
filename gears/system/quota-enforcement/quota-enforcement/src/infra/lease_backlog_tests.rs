use quota_enforcement_sdk::MetricId;

use super::*;
use crate::domain::ports::metrics::MetricLabel;

fn label(metric: &str) -> MetricLabel {
    MetricLabel::admitted(&MetricId::parse(metric).expect("metric id"))
}

#[test]
fn a_published_backlog_is_read_back_and_a_withdrawal_clears_it() {
    let cell = LeaseBacklogCell::default();
    assert!(cell.load().is_none(), "nothing before the first cycle");

    let tokens = label("gts.cf.core.qe.metric_type.v1~cf.genai.llm_gateway.ai_tokens_input.v1");
    cell.publish(Some(vec![(tokens.clone(), 3)]));
    assert_eq!(cell.load().as_deref(), Some(&vec![(tokens, 3)]));

    cell.publish(None);
    assert!(
        cell.load().is_none(),
        "leadership loss withdraws the sample"
    );
}
