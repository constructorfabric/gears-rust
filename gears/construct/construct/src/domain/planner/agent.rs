use std::sync::Arc;

use serde_json::Value;
use toolkit_security::SecurityContext;

use super::catalog::PropertyCatalog;
use super::draft::PlanDraft;
use crate::domain::model_client::{AnswerKind, Message, ModelClient, ModelOutput, ModelRequest};
use crate::domain::plan::{Plan, PlanOrigin, RecordOrigin};
use crate::domain::profile::Profile;
use crate::domain::record_intake::{DropCause, DropEvent, IntakeEvents, ReceivedRecord};

pub const INSTRUCTIONS: &str = "Decide how the record changes the person's profile. Use the tools add, replace \
and remove, and name profile values by their numbers. Answer without a tool call when the plan is complete.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerCaps {
    pub max_rounds: u32,
    pub max_tokens: u64,
}

pub struct Planner {
    model: Arc<dyn ModelClient>,
    catalog: Arc<PropertyCatalog>,
    events: Arc<dyn IntakeEvents>,
    caps: PlannerCaps,
}

impl Planner {
    #[must_use]
    pub fn new(
        model: Arc<dyn ModelClient>,
        catalog: Arc<PropertyCatalog>,
        events: Arc<dyn IntakeEvents>,
        caps: PlannerCaps,
    ) -> Self {
        Self {
            model,
            catalog,
            events,
            caps,
        }
    }

    /// @cpt-dod:cpt-cf-construct-dod-planner-loop:p1
    pub async fn plan(
        &self,
        ctx: &SecurityContext,
        record: &ReceivedRecord,
        profile: Profile,
    ) -> Option<Plan> {
        match self.run(ctx, record, profile).await {
            Ok(plan) => Some(plan),
            Err(cause) => {
                self.events.dropped(DropEvent::for_record(record, cause));
                None
            }
        }
    }

    async fn run(
        &self,
        ctx: &SecurityContext,
        record: &ReceivedRecord,
        profile: Profile,
    ) -> Result<Plan, DropCause> {
        let subject_id = record
            .envelope
            .subject_id
            .ok_or(DropCause::ProcessingFailed)?;
        let origin = RecordOrigin::of(record).map_err(|_| DropCause::ProcessingFailed)?;
        let mut draft = PlanDraft::new(&self.catalog, profile);
        let tools = draft.tools();
        let mut messages = vec![
            Message::System(INSTRUCTIONS.to_owned()),
            Message::User(input(record, &draft)),
        ];
        let mut tokens: u64 = 0;
        for _ in 0..self.caps.max_rounds {
            let response = self
                .model
                .complete(
                    ctx,
                    ModelRequest {
                        messages: messages.clone(),
                        tools: tools.clone(),
                        answer: AnswerKind::Text,
                        max_output_tokens: None,
                    },
                )
                .await
                .map_err(|error| {
                    tracing::warn!(event = "planner_model_failed", %error, "model call failed");
                    DropCause::ModelFailed
                })?;
            tokens = tokens
                .saturating_add(response.usage.input_tokens)
                .saturating_add(response.usage.output_tokens);
            if tokens > self.caps.max_tokens {
                return Err(DropCause::TokenCap);
            }
            let ModelOutput::ToolCalls(calls) = response.output else {
                return Ok(draft.finish(record.tenant_id, subject_id, PlanOrigin::Record(origin)));
            };
            messages.push(Message::Assistant {
                text: None,
                tool_calls: calls.clone(),
            });
            for call in calls {
                let content = match draft.apply(&call) {
                    Ok(result) => result,
                    Err(refusal) => refusal.to_string(),
                };
                messages.push(Message::ToolResult {
                    call_id: call.id,
                    content,
                });
            }
        }
        Err(DropCause::RoundCap)
    }
}

fn input(record: &ReceivedRecord, draft: &PlanDraft<'_>) -> String {
    let payload = record
        .record
        .get("payload")
        .map_or_else(String::new, Value::to_string);
    let profile = draft.numbered_profile();
    let profile = if profile.is_empty() {
        "(empty)".to_owned()
    } else {
        profile
    };
    format!("Record:\n{payload}\n\nProfile:\n{profile}")
}
