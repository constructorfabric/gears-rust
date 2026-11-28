#![cfg(not(feature = "event-broker"))]

mod process_fixture;

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use prost::Message as _;
use serde_json::{Value, json};
use tokio::time::{sleep, timeout};
use tonic::client::Grpc;
use tonic::metadata::{MetadataKey, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Request, Status};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_transport_grpc::SECCTX_METADATA_KEY;

use process_fixture::wire::authenticate_response::Outcome;
use process_fixture::wire::{AuthenticateResponse, Failure, Identity};
use process_fixture::{BIN, fixture_command};
use process_fixture::{
    CALL, Core, INSTANCE, KNOWN, LOG_RECHECK, METHOD, NIL, STARTUP, UNKNOWN, VENDOR,
    WRONG_WORKLOAD, authenticate, capture_outcome, events, fixture_config, message,
};

#[derive(Clone, Copy)]
enum StatusExpectation {
    Exact(Code, &'static str),
    CodeOnly(Code),
}

struct TraceCase {
    vendor: &'static str,
    traceparents: &'static [&'static str],
    expected_trace_id: Option<&'static str>,
}

impl Core {
    fn check_status(
        &self,
        result: Result<AuthenticateResponse, Status>,
        expectation: StatusExpectation,
    ) -> Result<()> {
        let status = result.err().context("expected a gRPC refusal")?;
        self.check_confidential(&format!("{status:?}"), "gRPC status")?;
        let expected_code = match expectation {
            StatusExpectation::Exact(code, _) | StatusExpectation::CodeOnly(code) => code,
        };
        assert_eq!(status.code(), expected_code);
        if let StatusExpectation::Exact(_, message) = expectation {
            assert_eq!(status.message(), message);
        }
        assert_eq!(status.details(), &[] as &[u8]);
        Ok(())
    }

    async fn verify(&mut self, case: &TraceCase) -> Result<()> {
        let endpoint = self.admitted_endpoint().await?;
        ensure!(
            self.home
                .path()
                .join("types-registry/types_registry.db")
                .is_file(),
            "core must open SQLite in its requested fixture home"
        );
        ensure!(
            !self.home.path().join("alternative").exists(),
            "alternative home was used"
        );
        let channel = Endpoint::from_shared(format!("http://{endpoint}"))?
            .connect_timeout(CALL)
            .timeout(CALL)
            .connect()
            .await?;
        if let Some(status) = self.child.try_wait()? {
            anyhow::bail!("core exited after admission ({status})");
        }
        let mut grpc = Grpc::new(channel);

        let mut first = self.request(KNOWN);
        for traceparent in case.traceparents {
            first
                .metadata_mut()
                .append("traceparent", MetadataValue::from_static(traceparent));
        }
        let known = authenticate(&mut grpc, first).await?;
        self.check_confidential(&format!("{known:?}"), "known-user response")?;
        assert_eq!(
            known,
            AuthenticateResponse {
                outcome: Some(Outcome::Identity(Identity {
                    subject_id: "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into(),
                    subject_type: Some("user".into()),
                    subject_tenant_id: "00000000-0000-0000-0000-000000000002".into(),
                    token_scopes: vec!["read:data".into(), "write:data".into()],
                })),
            },
        );

        let nil = authenticate(&mut grpc, self.request(NIL)).await?;
        self.check_confidential(&format!("{nil:?}"), "nil-user response")?;
        assert_eq!(
            nil,
            AuthenticateResponse {
                outcome: Some(Outcome::Identity(Identity {
                    subject_id: "00000000-0000-0000-0000-000000000000".into(),
                    subject_type: None,
                    subject_tenant_id: "00000000-0000-0000-0000-000000000000".into(),
                    token_scopes: vec![],
                })),
            },
        );

        let unknown = authenticate(&mut grpc, self.request(UNKNOWN)).await?;
        self.check_confidential(&format!("{unknown:?}"), "unknown-user response")?;
        assert_eq!(
            unknown,
            AuthenticateResponse {
                outcome: Some(Outcome::Failure(Failure::InvalidToken.into())),
            },
        );
        self.verify_refusals(&mut grpc).await?;
        self.verify_decoding_cap(&mut grpc).await?;
        assert_eq!(authenticate(&mut grpc, self.request(KNOWN)).await?, known);
        self.verify_log(case).await
    }

    async fn verify_refusals(&self, grpc: &mut Grpc<Channel>) -> Result<()> {
        self.check_status(
            authenticate(grpc, Request::new(message(KNOWN))).await,
            StatusExpectation::Exact(Code::Unauthenticated, "missing internal token"),
        )?;

        let mut wrong_peer = self.request(KNOWN);
        wrong_peer.metadata_mut().insert(
            INTERNAL_TOKEN_HEADER,
            MetadataValue::from_static(WRONG_WORKLOAD),
        );
        self.check_status(
            authenticate(grpc, wrong_peer).await,
            StatusExpectation::Exact(Code::Unauthenticated, "invalid internal token"),
        )?;

        let mut with_auth = self.request(KNOWN);
        let mut authorization = MetadataValue::try_from(format!("Bearer {KNOWN}"))?;
        authorization.set_sensitive(true);
        with_auth
            .metadata_mut()
            .insert("authorization", authorization);
        self.check_status(
            authenticate(grpc, with_auth).await,
            StatusExpectation::Exact(Code::InvalidArgument, "mixed authentication metadata"),
        )?;

        let mut with_context = self.request(KNOWN);
        let mut context = MetadataValue::from_bytes(KNOWN.as_bytes());
        context.set_sensitive(true);
        with_context
            .metadata_mut()
            .insert_bin(MetadataKey::from_static(SECCTX_METADATA_KEY), context);
        self.check_status(
            authenticate(grpc, with_context).await,
            StatusExpectation::Exact(Code::InvalidArgument, "mixed authentication metadata"),
        )
    }

    async fn verify_decoding_cap(&self, grpc: &mut Grpc<Channel>) -> Result<()> {
        let padded = |size: usize| format!("{UNKNOWN}{}", "a".repeat(size - 4 - UNKNOWN.len()));
        let at_limit = self.request(&padded(131_072));
        assert_eq!(at_limit.get_ref().encoded_len(), 131_072);
        assert_eq!(
            authenticate(grpc, at_limit).await?,
            AuthenticateResponse {
                outcome: Some(Outcome::Failure(Failure::InvalidToken.into())),
            },
        );

        let above_limit = self.request(&padded(131_073));
        assert_eq!(above_limit.get_ref().encoded_len(), 131_073);
        self.check_status(
            authenticate(grpc, above_limit).await,
            StatusExpectation::CodeOnly(Code::OutOfRange),
        )
    }

    async fn verify_log(&self, case: &TraceCase) -> Result<()> {
        let vendor = case.vendor;
        timeout(CALL, async {
            loop {
                let log = self.log()?;
                let events = events(&log);
                let successful_peer_checks = events
                    .iter()
                    .filter(|event| {
                        event["fields"]["message"] == "platform-plane gRPC call authenticated"
                            && event["fields"]["method"] == METHOD
                    })
                    .count();
                if successful_peer_checks == 8 {
                    Self::verify_admission(&events, vendor);
                    let selected: Vec<_> = events.iter().filter(|event| {
                        event["fields"]["message"] == "Selected authn_resolver plugin instance"
                            && event["span"]["vendor"] == vendor
                    }).collect();
                    let [selection] = selected.as_slice() else {
                        anyhow::bail!("expected the first call to select its own fixture vendor");
                    };
                    let spans = selection["spans"].as_array().context("selection span stack")?;
                    let handler = spans.iter().find(|s| s["name"] == "fixture_bearer_validation").context("authentication handler span")?;
                    let expected = case.expected_trace_id.map_or_else(
                        || json!({"name": "fixture_bearer_validation"}),
                        |id| json!({"name": "fixture_bearer_validation", "trace_id": id, "parent.trace_id": id}),
                    );
                    assert_eq!(handler, &expected, "{vendor} handler span");

                    println!(
                        "Core selected {INSTANCE}, vendor {vendor}, with {successful_peer_checks} authenticated calls"
                    );
                    return Ok(());
                }
                sleep(LOG_RECHECK).await;
            }
        })
        .await
        .context("actual authenticated call was absent from the core log")?
    }

    fn verify_admission(events: &[Value], vendor: &str) {
        let initialized = events
            .iter()
            .filter(|event| event["fields"]["message"] == "Initialized a gear.")
            .filter_map(|event| event["fields"]["gear"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            initialized,
            BTreeSet::from([
                "api-gateway",
                "authn-resolver",
                "grpc-hub",
                "service-discovery",
                "static-authn-plugin",
                "test-flight-control-authn",
                "types-registry",
            ]),
        );
        assert!(events.iter().any(|event| {
            event["fields"]["message"] == "Loaded plugin configuration"
                && event["fields"]["mode"] == "StaticTokens"
                && event["fields"]["vendor"] == vendor
                && event["fields"]["token_count"] == 2
        }));
        assert!(events.iter().any(|event| {
            event["fields"]["message"] == "Selected authn_resolver plugin instance"
                && event["fields"]["plugin_gts_id"] == INSTANCE
        }));
        assert!(events.iter().any(|event| {
            event["fields"]["message"] == "inbound gRPC platform-plane enforcement enabled"
                && event["fields"]["provider"] == "shared_secret"
                && event["fields"]["enforcement"] == "Required"
        }));
    }
}

#[test]
fn the_fixture_command_keeps_only_unrelated_inherited_settings() {
    let inputs = [
        ("  App__gears__example", "unused"),
        ("APP__", "unused"),
        ("RUST_LOG", "unused"),
        ("RUST_BACKTRACE", "full"),
        ("RUST_LIB_BACKTRACE", "1"),
        ("APP_single", "preserved"),
        ("TMPDIR", "preserved"),
    ];
    let inherited = inputs.map(|(key, value)| (OsString::from(key), OsString::from(value)));
    let command = fixture_command(PathBuf::from(BIN), inherited.into_iter());
    let kept: Vec<_> = command
        .as_std()
        .get_envs()
        .filter_map(|(name, value)| value.map(|value| (name.to_owned(), value.to_owned())))
        .collect();
    assert_eq!(
        kept,
        [
            (OsString::from("APP_single"), OsString::from("preserved")),
            (OsString::from("TMPDIR"), OsString::from("preserved")),
        ]
    );
}

#[cfg(unix)]
#[test]
fn the_fixture_command_filters_non_unicode_names_with_the_figment_prefix() {
    use std::os::unix::ffi::OsStringExt as _;

    let keep = OsString::from_vec(vec![0xff, b'A']);
    let skip = OsString::from_vec(vec![b'a', b'p', b'p', b'_', b'_', 0xff]);
    let value = OsString::from_vec(vec![0xff]);
    let inputs = [(skip, value.clone()), (keep.clone(), value.clone())];
    let command = fixture_command(PathBuf::from(BIN), inputs.into_iter());
    assert_eq!(
        command.as_std().get_envs().collect::<Vec<_>>(),
        [(keep.as_os_str(), Some(value.as_os_str()))]
    );
}

#[tokio::test]
async fn core_authenticates_through_the_admitted_static_plugin() -> Result<()> {
    const VALID: &str = "00-11111111111111111111111111111111-2222222222222222-01";
    const MALFORMED: &str = "00-00000000000000000000000000000000-2222222222222222-01";
    for case in [
        TraceCase {
            vendor: VENDOR,
            traceparents: &[VALID],
            expected_trace_id: Some("11111111111111111111111111111111"),
        },
        TraceCase {
            vendor: "fixture-malformed",
            traceparents: &[MALFORMED],
            expected_trace_id: None,
        },
        TraceCase {
            vendor: "fixture-duplicate",
            traceparents: &[VALID, VALID],
            expected_trace_id: None,
        },
    ] {
        verify_core(&case).await?;
    }
    Ok(())
}

async fn verify_core(case: &TraceCase) -> Result<()> {
    let home = tempfile::tempdir()?;
    let secret = uuid::Uuid::new_v4().to_string();
    let config = fixture_config(home.path(), &secret, case.vendor);
    let mut core = Core::start(home, &config, secret)?;
    let result = capture_outcome(core.verify(case)).await;
    core.finish(result).await
}

#[tokio::test]
async fn run_rejects_an_invalid_static_configuration_before_opening_the_registry() -> Result<()> {
    let home = tempfile::tempdir()?;
    let secret = uuid::Uuid::new_v4().to_string();
    let mut config = fixture_config(home.path(), &secret, VENDOR);
    config["gears"]["static-authn-plugin"]["config"]["mode"] = json!("invalid-test-mode");
    config["gears"]["types-registry"]["database"]["server"] = json!("absent-test-server");
    let mut core = Core::start(home, &config, secret)?;
    let exit = timeout(STARTUP, core.child.wait()).await;
    core.stop().await?;
    assert_eq!(exit??.code(), Some(1));
    let log = core.log()?;
    assert!(
        log.trim() == "Error: invalid static-authn-plugin configuration",
        "Run must report only the fixed static configuration error"
    );
    assert!(
        !core
            .home
            .path()
            .join("types-registry/types_registry.db")
            .exists()
    );
    Ok(())
}
