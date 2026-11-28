mod process_fixture;

use anyhow::{Context as _, Result, ensure};
#[cfg(feature = "cluster")]
use cluster_sdk::grpc::stubs::profile::{
    self, cluster_profile_api_client::ClusterProfileApiClient,
};
use gts::GtsId;
use process_fixture::wire::authenticate_response::Outcome;
use process_fixture::wire::{AuthenticateResponse, Identity};
use process_fixture::{
    CALL, Core, LOG_RECHECK, STARTUP, VENDOR, capture_outcome, events, fixture_config,
};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};
use tonic::client::Grpc;
use tonic::transport::Endpoint;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

const EVENT: &str = "gts.cf.core.events.event.v1~";
const POLICY: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~cf.e2e.eb_test_policy.plugin.v1";
const SHIPPED_POLICY: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.authz_resolver.plugin.v1~cf.builtin.tr_authz_resolver.plugin.v1";
const STATIC_TR: &str = "gts.cf.toolkit.plugins.plugin.v1~cf.core.tenant_resolver.plugin.v1~cf.builtin.static_tenant_resolver.plugin.v1";
const T0: &str = "00000000-0000-0000-0000-000000000001";
const TA: &str = "00000000-0000-0000-0000-000000000002";
const TB: &str = "00000000-0000-0000-0000-000000000003";
const TC: &str = "00000000-0000-0000-0000-000000000004";
const TX: &str = "00000000-0000-0000-0000-000000000005";
const NIL_ID: &str = "00000000-0000-0000-0000-000000000000";
const A: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const B: &str = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
const C: &str = "cccccccc-cccc-cccc-cccc-cccccccccccc";
const BARRIER: &str = "dddddddd-dddd-dddd-dddd-dddddddddddd";
const B_TOKEN: &str = "planted-user-b-bearer-fixture-policy";
const C_TOKEN: &str = "planted-user-c-bearer-fixture-policy";
const A_TB_TOKEN: &str = "planted-user-a-other-tenant-bearer-fixture-policy";
const A_TYPED_TOKEN: &str = "planted-user-a-other-type-bearer-fixture-policy";
const BARRIER_TOKEN: &str = "planted-barrier-bearer-fixture-policy";
const SUBJECT: &str = "gts.acme.orders.x.customer.v1~";
const NAMED_TOPIC: &str = "gts.cf.core.events.topic.v1~acme.orders.x.orders.v1";
const GROUP: &str = "gts.cf.core.events.consumer_group.v1~";
const REACH: &str = "gts.cf.core.events.request.v1~";
const DERIVED_EVENT: &str = "gts.cf.core.events.event.v1~acme.orders.x.placed.v1~";
const CONSUME_PROPERTIES: [&str; 4] = [
    "owner_tenant_id",
    "owner_id",
    "event_type",
    "event_subject_type",
];
const TENANT_PROPERTY: [&str; 1] = ["owner_tenant_id"];

fn broker_config(home: &std::path::Path, secret: &str) -> Result<Value> {
    let mut config = fixture_config(home, secret, VENDOR);
    config["gears"]["api-gateway"]["config"]["prefix_path"] = json!("/fixture");
    let tokens = config["gears"]["static-authn-plugin"]["config"]["tokens"]
        .as_array_mut()
        .context("fixture static token list")?;
    for (token, actor, tenant, kind) in [
        (B_TOKEN, B, TA, "user"),
        (C_TOKEN, C, TB, "user"),
        (A_TB_TOKEN, A, TB, "user"),
        (A_TYPED_TOKEN, A, TA, "service"),
        (BARRIER_TOKEN, BARRIER, TA, "service"),
    ] {
        tokens.push(json!({"token": token, "identity": {
            "subject_id": actor, "subject_tenant_id": tenant, "subject_type": kind, "token_scopes": []
        }}));
    }
    config["gears"]["tenant-resolver"] = json!({"config": {"vendor": "fixture-tenants"}});
    config["gears"]["static-tr-plugin"] = json!({"config": {
        "vendor": "fixture-tenants",
        "tenants": [
            {"id": T0, "name": "T0"},
            {"id": TA, "name": "TA", "parent_id": T0},
            {"id": TB, "name": "TB", "parent_id": T0},
            {"id": TC, "name": "TC", "parent_id": TA},
            {"id": TX, "name": "TX", "parent_id": TA, "self_managed": true}
        ]
    }});
    config["gears"]["authz-resolver"] = json!({"config": {"vendor": "fixture-shipped"}});
    config["gears"]["tr-authz-plugin"] =
        json!({"config": {"vendor": "fixture-shipped", "priority": 1}});
    config["gears"]["eb-test-policy"] = json!({"config": {"grants": [
        {"subject_id": A, "action": "consume", "resource": "gts.cf.core.events.topic.v1~*",
         "event_types": [EVENT], "subject_types": [SUBJECT]},
        {"subject_id": A, "action": "define", "resource": GROUP},
        {"subject_id": A, "action": "produce", "resource": REACH},
        {"subject_id": B, "action": "produce", "resource": EVENT},
        {"subject_id": BARRIER, "action": "produce_ignore_barrier", "resource": EVENT}
    ]}});
    Ok(config)
}

fn start(configure: impl FnOnce(&mut Value)) -> Result<Core> {
    let home = tempfile::tempdir()?;
    let secret = uuid::Uuid::new_v4().to_string();
    let mut config = broker_config(home.path(), &secret)?;
    configure(&mut config);
    let mut core = Core::start(home, &config, secret)?;
    core.additional_bearers = &[B_TOKEN, C_TOKEN, A_TB_TOKEN, A_TYPED_TOKEN, BARRIER_TOKEN];
    Ok(core)
}

fn select_narrowing_policy(config: &mut Value) {
    config["gears"]["authz-resolver"]["config"]["vendor"] = json!("e2e");
}

fn http_client(core: &Core) -> Result<Client> {
    let mut token = HeaderValue::from_str(&core.secret)
        .map_err(|_| anyhow::anyhow!("fixture workload credential is not an HTTP header"))?;
    token.set_sensitive(true);
    let mut headers = HeaderMap::new();
    headers.insert(INTERNAL_TOKEN_HEADER, token);
    Ok(Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(CALL)
        .default_headers(headers)
        .build()?)
}

async fn gateway_endpoint(core: &mut Core) -> Result<Url> {
    timeout(STARTUP, async {
        loop {
            if let Some(status) = core.child.try_wait()? {
                anyhow::bail!("flight-control exited before publishing its gateway ({status})");
            }
            let log = core.log()?;
            let events = events(&log);
            if let Some(url) = events.iter().find_map(|event| {
                (event["fields"]["message"]
                    == "REST host endpoint published for directory registration")
                    .then(|| event["fields"]["endpoint"].as_str())
                    .flatten()
            }) {
                let url = Url::parse(url)?;
                ensure!(
                    url.scheme() == "http"
                        && url.host_str() == Some("127.0.0.1")
                        && url.port().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "expected the published loopback gateway URL"
                );
                return Ok(url);
            }
            sleep(LOG_RECHECK).await;
        }
    })
    .await
    .context("flight-control did not publish a gateway endpoint")?
}

fn gateway_route(base: &Url, route: &str) -> String {
    format!("{}{route}", base.as_str().trim_end_matches('/'))
}

async fn response(core: &Core, request: RequestBuilder) -> Result<(StatusCode, Value)> {
    let response = request.send().await?;
    let status = response.status();
    let text = response.text().await?;
    core.check_confidential(&text, "HTTP response")?;
    Ok((status, serde_json::from_str(&text)?))
}

async fn read_registry(core: &Core, client: &Client, base: &Url, id: &str) -> Result<Value> {
    let route = format!("/types-registry/v2/platform/entities/{id}?$select=content");
    let (status, body) = response(core, client.get(gateway_route(base, &route))).await?;
    assert_eq!(status, StatusCode::OK, "registry exact read for {id}");
    Ok(body)
}

#[tokio::test]
async fn the_event_broker_profile_admits_the_fixture_and_shipped_plugins() -> Result<()> {
    let mut core = start(|_| {})?;
    let result = capture_outcome(async {
        core.admitted_endpoint().await?;
        let base = gateway_endpoint(&mut core).await?;
        assert_eq!(base.path(), "/fixture");
        let client = http_client(&core)?;
        for (id, vendor, priority) in [
            (POLICY, "e2e", 100),
            (SHIPPED_POLICY, "fixture-shipped", 1),
            (STATIC_TR, "fixture-tenants", 100),
            (process_fixture::INSTANCE, VENDOR, 100),
        ] {
            let entity = read_registry(&core, &client, &base, id).await?;
            assert_eq!(
                entity,
                json!({
                    "gts_id": id, "gts_uuid": GtsId::try_new(id)?.to_uuid(), "kind": "instance",
                    "lifecycle_status": "active", "ownership": "global", "content": {
                        "id": id, "vendor": vendor, "priority": priority, "properties": {}
                    },
                }),
                "{id} plugin document"
            );
            println!("Admitted plugin {id} for vendor {vendor}");
        }
        let log = core.log()?;
        let initialized = events(&log)
            .into_iter()
            .filter(|event| event["fields"]["message"] == "Initialized a gear.")
            .filter_map(|event| event["fields"]["gear"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            initialized.into_iter().collect::<Vec<_>>(),
            [
                "api-gateway",
                "authn-resolver",
                "authz-resolver",
                #[cfg(feature = "cluster")]
                "cluster",
                "eb-test-policy",
                "grpc-hub",
                "service-discovery",
                "static-authn-plugin",
                "static-tr-plugin",
                "tenant-resolver",
                "test-flight-control-authn",
                "tr-authz-plugin",
                "types-registry",
            ]
        );
        Ok(())
    })
    .await;
    core.finish(result).await
}

#[cfg(feature = "cluster")]
#[tokio::test]
async fn the_remote_profile_serves_the_configured_standalone_cluster_descriptor() -> Result<()> {
    let mut core = start(|config| {
        config["gears"]["cluster"]["config"]["profiles"]["orders"]["cache"]["provider"] =
            json!("standalone");
    })?;
    let result = capture_outcome(async {
        let describe = async {
            // The host starts system gears before installing the application routes.
            // This admission marker therefore follows the cluster profile publication.
            let endpoint = core.admitted_endpoint().await?;
            let channel = Endpoint::from_shared(format!("http://{endpoint}"))?
                .connect_timeout(CALL)
                .timeout(CALL)
                .connect()
                .await?;
            let mut client = ClusterProfileApiClient::new(channel);
            let mut request = tonic::Request::new(profile::DescribeProfilesRequest {
                profiles: vec!["orders".into()],
            });
            let mut credential = tonic::metadata::MetadataValue::try_from(core.secret.as_str())
                .map_err(|_| anyhow::anyhow!("invalid fixture workload gRPC metadata"))?;
            credential.set_sensitive(true);
            request
                .metadata_mut()
                .insert(INTERNAL_TOKEN_HEADER, credential);
            let response = client.describe_profiles(request).await.map_err(|status| {
                anyhow::anyhow!(
                    "admitted flight-control returned {:?} for DescribeProfiles",
                    status.code()
                )
            })?;
            Ok::<_, anyhow::Error>(response.into_inner())
        };
        let actual = timeout(STARTUP, describe).await.context(
            "flight-control did not admit and serve DescribeProfiles before the startup deadline",
        )??;
        assert!(
            core.child.try_wait()?.is_none(),
            "flight-control exited after DescribeProfiles"
        );
        core.check_confidential(&format!("{actual:?}"), "DescribeProfiles response")?;
        assert_eq!(
            actual,
            profile::DescribeProfilesResponse {
                generation: 1,
                profiles: vec![profile::ProfileDescriptor {
                    name: "orders".into(),
                    health: i32::from(profile::ProfileHealth::Serving),
                    cache: Some(profile::CacheDescriptor {
                        provider: "standalone".into(),
                        consistency: i32::from(profile::WireCacheConsistency::Linearizable),
                        features: Some(profile::WireCacheFeatures {
                            prefix_watch: true,
                            watch: Some(true),
                        }),
                    }),
                    leader_election: Some(profile::LeaderElectionDescriptor {
                        provider: "standalone".into(),
                        features: Some(profile::WireLeaderElectionFeatures { linearizable: true }),
                    }),
                    lock: Some(profile::LockDescriptor {
                        provider: "standalone".into(),
                        features: Some(profile::WireLockFeatures { linearizable: true }),
                    }),
                }],
            }
        );
        Ok(())
    })
    .await;
    core.finish(result).await
}

async fn authenticate_identities(core: &mut Core) -> Result<[Identity; 7]> {
    let endpoint = core.admitted_endpoint().await?;
    let channel = Endpoint::from_shared(format!("http://{endpoint}"))?
        .connect_timeout(CALL)
        .timeout(CALL)
        .connect()
        .await?;
    let mut grpc = Grpc::new(channel);
    let mut identities = Vec::new();
    for (token, subject, tenant, kind, scopes) in [
        (
            process_fixture::KNOWN,
            A,
            TA,
            Some("user"),
            &["read:data", "write:data"][..],
        ),
        (B_TOKEN, B, TA, Some("user"), &[][..]),
        (C_TOKEN, C, TB, Some("user"), &[][..]),
        (A_TB_TOKEN, A, TB, Some("user"), &[][..]),
        (A_TYPED_TOKEN, A, TA, Some("service"), &[][..]),
        (process_fixture::NIL, NIL_ID, NIL_ID, None, &[][..]),
        (BARRIER_TOKEN, BARRIER, TA, Some("service"), &[][..]),
    ] {
        let actual = process_fixture::authenticate(&mut grpc, core.request(token)).await?;
        core.check_confidential(&format!("{actual:?}"), "private identity response")?;
        assert_eq!(
            actual,
            AuthenticateResponse {
                outcome: Some(Outcome::Identity(Identity {
                    subject_id: subject.into(),
                    subject_type: kind.map(str::to_owned),
                    subject_tenant_id: tenant.into(),
                    token_scopes: scopes.iter().map(|scope| (*scope).to_owned()).collect(),
                })),
            }
        );
        let Some(Outcome::Identity(identity)) = actual.outcome else {
            anyhow::bail!("expected a static identity");
        };
        identities.push(identity);
    }
    identities
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected seven static identities"))
}

struct Gateway<'a> {
    core: &'a Core,
    client: Client,
    base: Url,
}

impl Gateway<'_> {
    async fn evaluate(&self, identity: &Identity, operation: Evaluation<'_>) -> Result<Value> {
        let Evaluation {
            action,
            resource,
            tenant,
            mode,
            barrier,
            properties,
        } = operation;
        let mut subject = json!({
            "id": identity.subject_id,
            "properties": {"tenant_id": identity.subject_tenant_id},
        });
        if let Some(kind) = &identity.subject_type {
            subject["type"] = json!(kind);
        }
        let input = json!({
            "subject": subject,
            "action": {"name": action},
            "resource": {"type": resource},
            "context": {
                "tenant_context": {"root_id": tenant, "mode": mode, "barrier_mode": barrier},
                "require_constraints": action == "consume",
                "supported_properties": properties,
                "token_scopes": identity.token_scopes,
            }
        });
        let request = self
            .client
            .post(gateway_route(&self.base, "/authz-resolver/v1/evaluate"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(input.to_string());
        let (status, mut body) = response(self.core, request).await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "platform evaluate for {action} on {resource}"
        );
        if let Some(branches) = body["context"]["constraints"].as_array_mut() {
            for branch in branches {
                if let Some(predicates) = branch["predicates"].as_array_mut() {
                    for predicate in predicates {
                        if predicate["property"] == "owner_tenant_id"
                            && let Some(values) = predicate["values"].as_array_mut()
                        {
                            values.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
                        }
                    }
                }
            }
        }
        Ok(body)
    }
}

#[derive(Clone, Copy)]
struct Evaluation<'a> {
    action: &'a str,
    resource: &'a str,
    tenant: &'a str,
    mode: &'a str,
    barrier: &'a str,
    properties: &'a [&'a str],
}

#[tokio::test]
async fn the_default_policy_uses_the_shipped_hierarchy_without_fixture_grants() -> Result<()> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../config/event-broker-test-flight-control.yaml"
    ))?;
    let vendor = fixture
        .pointer("/gears/authz-resolver/config/vendor")
        .and_then(Value::as_str)
        .context("default Event Broker AuthZ vendor")?;
    let mut core = start(|config| {
        config["gears"]["authz-resolver"]["config"]["vendor"] = json!(vendor);
    })?;
    let result = capture_outcome(async {
        let [actor, peer, ..] = authenticate_identities(&mut core).await?;
        let base = gateway_endpoint(&mut core).await?;
        let gateway = Gateway {
            core: &core,
            client: http_client(&core)?,
            base,
        };
        let consume = Evaluation {
            action: "consume",
            resource: NAMED_TOPIC,
            tenant: TC,
            mode: "root_only",
            barrier: "respect",
            properties: &CONSUME_PROPERTIES,
        };
        let actor_decision = gateway.evaluate(&actor, consume).await?;
        let peer_decision = gateway.evaluate(&peer, consume).await?;
        let log = core.log()?;
        let entries = events(&log);
        let selections = entries
            .iter()
            .filter_map(|event| {
                let message = event["fields"]["message"].as_str()?;
                let instance = event["fields"]["plugin_gts_id"].as_str()?;
                let vendor = event["span"]["vendor"].as_str()?;
                Some((message, instance, vendor))
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            (
                selections.into_iter().collect::<Vec<_>>(),
                actor_decision,
                peer_decision
            ),
            (
                vec![
                    (
                        "Selected authn_resolver plugin instance",
                        process_fixture::INSTANCE,
                        VENDOR
                    ),
                    (
                        "Selected authz_resolver plugin instance",
                        SHIPPED_POLICY,
                        "fixture-shipped"
                    ),
                    (
                        "Selected tenant resolver plugin instance",
                        STATIC_TR,
                        "fixture-tenants"
                    ),
                ],
                json!({"decision": true, "context": {"constraints": [{"predicates": [
                    {"op": "in", "property": "owner_tenant_id", "values": [TC]}
                ]}]}}),
                json!({"decision": true, "context": {"constraints": [{"predicates": [
                    {"op": "in", "property": "owner_tenant_id", "values": [TC]}
                ]}]}}),
            )
        );
        Ok(())
    })
    .await;
    core.finish(result).await
}

#[tokio::test]
async fn the_selected_policy_preserves_hierarchy_and_narrows_topic_consumption() -> Result<()> {
    let mut core = start(select_narrowing_policy)?;
    let result = capture_outcome(async {
        let [
            actor,
            producer,
            sibling,
            actor_in_sibling,
            service,
            nil,
            barrier,
        ] = authenticate_identities(&mut core).await?;
        let base = gateway_endpoint(&mut core).await?;
        let gateway = Gateway {
            core: &core,
            client: http_client(&core)?,
            base,
        };
        let consume = Evaluation {
            action: "consume",
            resource: NAMED_TOPIC,
            tenant: TC,
            mode: "root_only",
            barrier: "respect",
            properties: &CONSUME_PROPERTIES,
        };
        assert_eq!(
            gateway.evaluate(&actor, consume).await?,
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TC]},
                {"op": "in", "property": "event_type", "values": [EVENT]},
                {"op": "in", "property": "event_subject_type", "values": [SUBJECT]}
            ]}]}})
        );
        assert_eq!(
            gateway
                .evaluate(
                    &service,
                    Evaluation {
                        tenant: TA,
                        mode: "subtree",
                        ..consume
                    }
                )
                .await?,
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TA, TC]},
                {"op": "in", "property": "event_type", "values": [EVENT]},
                {"op": "in", "property": "event_subject_type", "values": [SUBJECT]}
            ]}]}})
        );
        assert_eq!(
            gateway
                .evaluate(
                    &actor_in_sibling,
                    Evaluation {
                        tenant: TB,
                        ..consume
                    }
                )
                .await?,
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TB]},
                {"op": "in", "property": "event_type", "values": [EVENT]},
                {"op": "in", "property": "event_subject_type", "values": [SUBJECT]}
            ]}]}})
        );
        for (label, identity, operation) in [
            (
                "matching grant cannot reach a sibling",
                &actor,
                Evaluation {
                    tenant: TB,
                    ..consume
                },
            ),
            (
                "same subject in a sibling cannot reach the original tenant",
                &actor_in_sibling,
                consume,
            ),
            (
                "sibling has no grant",
                &sibling,
                Evaluation {
                    tenant: TB,
                    ..consume
                },
            ),
            (
                "different subject in the same tenant has no grant",
                &producer,
                consume,
            ),
            ("nil identity receives no grant", &nil, consume),
            (
                "topic grant does not match a consumer group",
                &actor,
                Evaluation {
                    resource: GROUP,
                    ..consume
                },
            ),
            (
                "the caller must advertise the subject type property",
                &actor,
                Evaluation {
                    properties: &["owner_tenant_id", "event_type"],
                    ..consume
                },
            ),
        ] {
            assert_eq!(
                gateway.evaluate(identity, operation).await?,
                json!({"decision": false, "context": {"constraints": []}}),
                "{label}"
            );
        }
        verify_write_grants(&gateway, &actor, &producer, &barrier).await?;
        assert_eq!(
            gateway.evaluate(&actor, consume).await?,
            json!({"decision": true, "context": {"constraints": [{"predicates": [
                {"op": "in", "property": "owner_tenant_id", "values": [TC]},
                {"op": "in", "property": "event_type", "values": [EVENT]},
                {"op": "in", "property": "event_subject_type", "values": [SUBJECT]}
            ]}]}})
        );
        let log = core.log()?;
        let entries = events(&log);
        let selections = entries
            .iter()
            .filter_map(|event| {
                let message = event["fields"]["message"].as_str()?;
                let instance = event["fields"]["plugin_gts_id"].as_str()?;
                let vendor = event["span"]["vendor"].as_str()?;
                Some((message, instance, vendor))
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            selections.into_iter().collect::<Vec<_>>(),
            [
                (
                    "Selected authn_resolver plugin instance",
                    process_fixture::INSTANCE,
                    VENDOR
                ),
                ("Selected authz_resolver plugin instance", POLICY, "e2e"),
                (
                    "Selected tenant resolver plugin instance",
                    STATIC_TR,
                    "fixture-tenants"
                ),
            ]
        );
        println!(
            "Selected the fixture grant policy and static hierarchy through the platform gateway"
        );
        Ok(())
    })
    .await;
    core.finish(result).await
}

async fn verify_write_grants(
    gateway: &Gateway<'_>,
    actor: &Identity,
    producer: &Identity,
    barrier: &Identity,
) -> Result<()> {
    let define = Evaluation {
        action: "define",
        resource: GROUP,
        tenant: TA,
        mode: "root_only",
        barrier: "respect",
        properties: &TENANT_PROPERTY,
    };
    let produce = Evaluation {
        action: "produce",
        resource: DERIVED_EVENT,
        ..define
    };
    let reach = Evaluation {
        action: "produce",
        resource: REACH,
        tenant: TC,
        ..define
    };
    let across_barrier = Evaluation {
        action: "produce_ignore_barrier",
        resource: DERIVED_EVENT,
        tenant: TX,
        barrier: "ignore",
        ..define
    };
    for (label, identity, operation, expected) in [
        (
            "can define a consumer group",
            actor,
            define,
            json!({"decision": true, "context": {"constraints": [{"predicates": [{"op": "in", "property": "owner_tenant_id", "values": [TA]}]}]}}),
        ),
        (
            "producer cannot define a consumer group",
            producer,
            define,
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "producer can produce a derived event",
            producer,
            produce,
            json!({"decision": true, "context": {"constraints": [{"predicates": [{"op": "in", "property": "owner_tenant_id", "values": [TA]}]}]}}),
        ),
        (
            "group definer cannot produce an event",
            actor,
            produce,
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "ordinary produce reach includes a child",
            actor,
            reach,
            json!({"decision": true, "context": {"constraints": [{"predicates": [{"op": "in", "property": "owner_tenant_id", "values": [TC]}]}]}}),
        ),
        (
            "ordinary produce reach respects the barrier",
            actor,
            Evaluation {
                tenant: TX,
                ..reach
            },
            json!({"decision": false, "context": {"constraints": []}}),
        ),
        (
            "barrier principal may cross the barrier",
            barrier,
            across_barrier,
            json!({"decision": true, "context": {"constraints": [{"predicates": [{"op": "in", "property": "owner_tenant_id", "values": [TX]}]}]}}),
        ),
        (
            "ordinary principal has no barrier right",
            actor,
            across_barrier,
            json!({"decision": false, "context": {"constraints": []}}),
        ),
    ] {
        assert_eq!(
            gateway.evaluate(identity, operation).await?,
            expected,
            "{label}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn invalid_policy_configuration_stops_the_host_before_it_serves() -> Result<()> {
    let mut valid = start(select_narrowing_policy)?;
    let result = capture_outcome(async {
        valid.admitted_endpoint().await?;
        let base = gateway_endpoint(&mut valid).await?;
        assert_eq!(
            read_registry(&valid, &http_client(&valid)?, &base, POLICY).await?,
            json!({
                "gts_id": POLICY, "gts_uuid": GtsId::try_new(POLICY)?.to_uuid(), "kind": "instance",
                "lifecycle_status": "active", "ownership": "global", "content": {
                    "id": POLICY, "vendor": "e2e", "priority": 100, "properties": {}
                },
            }),
        );
        Ok(())
    })
    .await;
    valid.finish(result).await?;

    let mut invalid = start(|config| {
        select_narrowing_policy(config);
        config["gears"]["eb-test-policy"]["config"]["grants"][0]["event_types"] =
            json!(["gts.cf.core.events.event.v1~*"]);
    })?;
    let exit = timeout(STARTUP, invalid.child.wait()).await;
    invalid.stop().await?;
    let log = invalid.log()?;
    let exit = exit.context("invalid policy configuration did not stop flight-control")??;
    assert_eq!(exit.code(), Some(1), "invalid policy child status");
    assert_eq!(log.trim(), "Error: invalid eb-test-policy configuration");
    assert!(
        !invalid
            .home
            .path()
            .join("types-registry/types_registry.db")
            .exists()
    );
    println!("Rejected invalid policy before registry initialization ({exit})");
    Ok(())
}
