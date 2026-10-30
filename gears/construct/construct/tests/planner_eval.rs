#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write as _;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use construct::domain::plan::{Change, NodeKey, Plan};
use construct::domain::planner::agent::{Planner, PlannerCaps};
use construct::domain::planner::catalog::PropertyCatalog;
use construct::domain::profile::{Fact, Profile};
use construct::domain::record_intake::{DropEvent, Envelope, IntakeEvents, ReceivedRecord};
use construct::infra::model::chat_completions::{ChatCompletionsModel, ChatCompletionsSettings};
use construct_sdk::gts::{CHAT_MESSAGE_TYPE, ENRICHMENT_FACT_CANDIDATE_TYPE, record_type_schema};
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use oagw_sdk::{Body, ServiceGatewayClientV1};
use serde::Deserialize;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

const TENANT: Uuid = Uuid::from_u128(0x3f7a_1c5e_8b2d_4e6f_9a0b_1c2d_3e4f_5a6b);
const SUBJECT: Uuid = Uuid::from_u128(0x7e2b_9d4c_1a3f_4b5e_8c6d_0f1a_2b3c_4d5e);
const UPSTREAM: &str = "eval";
const DECISION_TARGET: f64 = 0.95;
const MODEL_TIMEOUT: Duration = Duration::from_mins(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    profile: Vec<Held>,
    record: CaseRecord,
    #[serde(default)]
    expect: BTreeMap<String, Decision>,
    #[serde(default)]
    gone: Vec<Held>,
    #[serde(default)]
    holds: Vec<Held>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct Held {
    property: String,
    value: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum CaseRecord {
    UserSays(String),
    FactCandidate(Value),
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Add,
    Replace,
    Remove,
}

impl Decision {
    fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Replace => "replace",
            Self::Remove => "remove",
        }
    }
}

fn shown(decisions: &BTreeMap<String, Decision>) -> String {
    if decisions.is_empty() {
        return "no change".to_owned();
    }
    decisions
        .iter()
        .map(|(property, decision)| format!("{} {property}", decision.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

struct Outcome {
    decided: BTreeMap<String, Decision>,
    held: Vec<(String, String)>,
    dropped: bool,
}

fn cases() -> Vec<Case> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/eval");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the eval directory")
        .map(|entry| entry.expect("an eval file").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).expect("a readable eval file");
            serde_json::from_str::<Vec<Case>>(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        })
        .collect()
}

fn received(case: &Case) -> ReceivedRecord {
    let (connector, type_id, payload) = match &case.record {
        CaseRecord::UserSays(text) => ("chat_engine", CHAT_MESSAGE_TYPE, chat("user", text)),
        CaseRecord::FactCandidate(payload) => (
            "enrichment",
            ENRICHMENT_FACT_CANDIDATE_TYPE,
            payload.clone(),
        ),
    };
    let provenance = format!("eval/{}", case.name);
    let now = OffsetDateTime::now_utc();
    let observed_at = now.format(&Rfc3339).expect("a time");
    ReceivedRecord {
        tenant_id: TENANT,
        connector: connector.to_owned(),
        envelope: Envelope {
            type_id: type_id.to_owned(),
            provenance: provenance.clone(),
            version: "v1".to_owned(),
            subject_id: Some(SUBJECT),
            observed_at: now,
        },
        record: json!({
            "type": type_id,
            "provenance": provenance,
            "version": "v1",
            "observed_at": observed_at,
            "subject_id": SUBJECT,
            "payload": payload,
        }),
    }
}

fn chat(role: &str, text: &str) -> Value {
    json!({
        "source": "chat_engine",
        "message_id": "message-1",
        "thread_id": "thread-1",
        "role": role,
        "text": text,
    })
}

fn profile(case: &Case, catalog: &PropertyCatalog) -> Profile {
    Profile {
        version: Some(1),
        facts: case
            .profile
            .iter()
            .map(|held| Fact {
                node_key: NodeKey(format!("construct:{TENANT}:{}", Uuid::new_v4())),
                category: catalog
                    .get(&held.property)
                    .expect("a listed property")
                    .category,
                property: held.property.clone(),
                value: held.value.clone(),
            })
            .collect(),
    }
}

fn outcome(profile: &Profile, plan: Option<&Plan>) -> Outcome {
    let property_of = |key: &NodeKey| {
        profile
            .facts
            .iter()
            .find(|fact| fact.node_key == *key)
            .map(|fact| fact.property.clone())
            .expect("a step on a key of the profile")
    };
    let mut adds = BTreeSet::new();
    let mut replaces = BTreeSet::new();
    let mut removes = BTreeSet::new();
    let mut gone = HashSet::new();
    let mut held: Vec<(String, String)> = Vec::new();
    for step in plan.map(|plan| plan.steps.as_slice()).unwrap_or_default() {
        match &step.change {
            Change::AddEntity { properties, .. } => {
                for property in properties {
                    adds.insert(property.name.clone());
                    held.push((property.name.clone(), stored(&property.value)));
                }
            }
            Change::ReplaceEntity {
                old, properties, ..
            } => {
                replaces.insert(property_of(old));
                gone.insert(old.clone());
                for property in properties {
                    held.push((property.name.clone(), stored(&property.value)));
                }
            }
            Change::RemoveEntity { entity } => {
                removes.insert(property_of(entity));
                gone.insert(entity.clone());
            }
            _ => panic!("the planner changes whole entities only"),
        }
    }
    held.extend(
        profile
            .facts
            .iter()
            .filter(|fact| !gone.contains(&fact.node_key))
            .map(|fact| (fact.property.clone(), stored(&fact.value))),
    );
    let decided = adds
        .iter()
        .chain(&replaces)
        .chain(&removes)
        .map(|property| {
            let decision = if replaces.contains(property)
                || (removes.contains(property) && adds.contains(property))
            {
                Decision::Replace
            } else if adds.contains(property) {
                Decision::Add
            } else {
                Decision::Remove
            };
            (property.clone(), decision)
        })
        .collect();
    Outcome {
        decided,
        held,
        dropped: plan.is_none(),
    }
}

fn comparable(text: &str) -> String {
    text.trim().to_lowercase()
}

fn stored(value: &Value) -> String {
    match value {
        Value::String(text) => comparable(text),
        other => comparable(&other.to_string()),
    }
}

fn same_value(left: &Value, right: &Value) -> bool {
    stored(left) == stored(right)
}

fn contradictions(case: &Case, outcome: &Outcome) -> Vec<String> {
    case.gone
        .iter()
        .filter(|gone| {
            outcome.held.iter().any(|(property, value)| {
                *property == gone.property && *value == stored(&gone.value)
            })
        })
        .map(|gone| format!("{}: {} is still held", gone.property, gone.value))
        .collect()
}

fn missing(case: &Case, outcome: &Outcome) -> Vec<String> {
    case.holds
        .iter()
        .filter(|kept| {
            !outcome.held.iter().any(|(property, value)| {
                *property == kept.property && *value == stored(&kept.value)
            })
        })
        .map(|kept| format!("{}: {} is not held", kept.property, kept.value))
        .collect()
}

struct NoDrops;

impl IntakeEvents for NoDrops {
    fn dropped(&self, _: DropEvent) {}
}

struct DirectGateway {
    http: reqwest::Client,
    url: String,
    key: Option<String>,
}

#[async_trait]
impl ServiceGatewayClientV1 for DirectGateway {
    async fn create_upstream(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn get_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn list_upstreams(
        &self,
        _: SecurityContext,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Upstream>, CanonicalError> {
        unimplemented!()
    }
    async fn update_upstream(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateUpstreamRequest,
    ) -> Result<oagw_sdk::Upstream, CanonicalError> {
        unimplemented!()
    }
    async fn delete_upstream(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn create_route(
        &self,
        _: SecurityContext,
        _: oagw_sdk::CreateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn get_route(
        &self,
        _: SecurityContext,
        _: Uuid,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn list_routes(
        &self,
        _: SecurityContext,
        _: Option<Uuid>,
        _: &oagw_sdk::ListQuery,
    ) -> Result<Vec<oagw_sdk::Route>, CanonicalError> {
        unimplemented!()
    }
    async fn update_route(
        &self,
        _: SecurityContext,
        _: Uuid,
        _: oagw_sdk::UpdateRouteRequest,
    ) -> Result<oagw_sdk::Route, CanonicalError> {
        unimplemented!()
    }
    async fn delete_route(&self, _: SecurityContext, _: Uuid) -> Result<(), CanonicalError> {
        unimplemented!()
    }
    async fn resolve_proxy_target(
        &self,
        _: SecurityContext,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(oagw_sdk::Upstream, oagw_sdk::Route), CanonicalError> {
        unimplemented!()
    }
    async fn proxy_request(
        &self,
        _: SecurityContext,
        request: http::Request<Body>,
    ) -> Result<http::Response<Body>, CanonicalError> {
        let body = request
            .into_body()
            .into_bytes()
            .await
            .map_err(|error| CanonicalError::internal(error.to_string()).create())?;
        let mut post = self
            .http
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        if let Some(key) = &self.key {
            post = post.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        let response = post
            .send()
            .await
            .map_err(|error| CanonicalError::internal(error.to_string()).create())?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|error| CanonicalError::internal(error.to_string()).create())?;
        Ok(http::Response::builder()
            .status(status.as_u16())
            .body(Body::Bytes(Bytes::from(bytes.to_vec())))
            .expect("a response"))
    }
}

fn planner(catalog: PropertyCatalog) -> Planner {
    let required = |name: &str| {
        std::env::var(name).unwrap_or_else(|_| panic!("set {name} to run the evaluation"))
    };
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn ServiceGatewayClientV1>(Arc::new(DirectGateway {
        http: reqwest::Client::new(),
        url: required("CONSTRUCT_EVAL_URL"),
        key: std::env::var("CONSTRUCT_EVAL_API_KEY").ok(),
    }));
    let model = ChatCompletionsModel::new(
        hub,
        ChatCompletionsSettings {
            upstream_alias: UPSTREAM.to_owned(),
            model: required("CONSTRUCT_EVAL_MODEL"),
            timeout: MODEL_TIMEOUT,
        },
    )
    .expect("a valid upstream alias");
    Planner::new(
        Arc::new(model),
        Arc::new(catalog),
        Arc::new(NoDrops),
        PlannerCaps {
            max_rounds: NonZeroU32::new(10).expect("rounds"),
            max_tokens: NonZeroU64::new(50_000).expect("tokens"),
        },
    )
}

fn context() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::new_v4())
        .subject_tenant_id(TENANT)
        .build()
        .expect("a security context")
}

fn payload_schema(type_id: &str) -> Value {
    record_type_schema(type_id)
        .expect("a record type")
        .pointer("/allOf/1/properties/payload")
        .cloned()
        .expect("a payload schema")
}

#[test]
fn every_case_fits_the_person_types_and_its_record_type() {
    let catalog = PropertyCatalog::load().expect("the catalog");
    let cases = cases();
    let mut names = BTreeSet::new();
    for case in &cases {
        assert!(
            names.insert(case.name.clone()),
            "{}: a second case of this name",
            case.name
        );
        for held in case.profile.iter().chain(&case.gone).chain(&case.holds) {
            let spec = catalog
                .get(&held.property)
                .unwrap_or_else(|| panic!("{}: no property {}", case.name, held.property));
            spec.check(&held.value)
                .unwrap_or_else(|error| panic!("{}: {}: {error}", case.name, held.property));
        }
        for gone in &case.gone {
            assert!(
                case.profile
                    .iter()
                    .any(|held| held.property == gone.property
                        && same_value(&held.value, &gone.value)),
                "{}: {} is gone but not in the profile",
                case.name,
                gone.property
            );
        }
        for kept in &case.holds {
            assert!(
                !case
                    .gone
                    .iter()
                    .any(|gone| gone.property == kept.property
                        && same_value(&gone.value, &kept.value)),
                "{}: {} must be both gone and held",
                case.name,
                kept.property
            );
        }
        for property in case.expect.keys() {
            assert!(
                catalog.get(property).is_some(),
                "{}: no property {property}",
                case.name
            );
        }
        let record = received(case);
        let schema = payload_schema(&record.envelope.type_id);
        let validator = jsonschema::validator_for(&schema).expect("a valid schema");
        assert!(
            validator.is_valid(&record.record["payload"]),
            "{}: the payload does not fit its record type",
            case.name
        );
    }
}

// @cpt-dod:cpt-cf-construct-dod-planner-eval:p1
#[tokio::test]
#[ignore = "calls a model: set CONSTRUCT_EVAL_URL, CONSTRUCT_EVAL_MODEL and CONSTRUCT_EVAL_API_KEY"]
async fn the_planner_meets_the_decision_target() {
    let catalog = PropertyCatalog::load().expect("the catalog");
    let cases = cases();
    let planner = planner(PropertyCatalog::load().expect("the catalog"));
    let ctx = context();
    let mut report = String::new();
    let mut right = 0_usize;
    let mut pairs = 0_usize;
    for case in &cases {
        let profile = profile(case, &catalog);
        let plan = planner.plan(&ctx, &received(case), profile.clone()).await;
        let outcome = outcome(&profile, plan.as_ref());
        let contradicting = contradictions(case, &outcome);
        let not_held = missing(case, &outcome);
        pairs += contradicting.len();
        if outcome.dropped {
            writeln!(report, "MISS {}: dropped", case.name).expect("a line");
        } else if outcome.decided != case.expect {
            writeln!(
                report,
                "MISS {}: expected {}, decided {}",
                case.name,
                shown(&case.expect),
                shown(&outcome.decided)
            )
            .expect("a line");
        } else if !not_held.is_empty() {
            writeln!(report, "MISS {}: {}", case.name, not_held.join(", ")).expect("a line");
        } else {
            right += 1;
        }
        for pair in contradicting {
            writeln!(report, "CONTRADICTION {}: {pair}", case.name).expect("a line");
        }
    }
    #[allow(clippy::cast_precision_loss)]
    let score = right as f64 / cases.len() as f64;
    writeln!(
        report,
        "decisions: {right}/{} ({:.1} %), contradicting pairs: {pairs}",
        cases.len(),
        score * 100.0
    )
    .expect("a line");
    println!("{report}");
    assert!(score >= DECISION_TARGET && pairs == 0, "{report}");
}
