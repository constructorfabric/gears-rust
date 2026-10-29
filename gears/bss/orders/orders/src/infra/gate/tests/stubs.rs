//! Task 8b development stubs: a build without `gate-dev-stubs` rejects `gate.dev_stubs`; with
//! the feature every stub answers only what the fixture states, the composition root binds
//! them for the undelivered ports only, and the assessment stub's rows cover exactly what
//! `assess` expects. In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once"
)]

/// The example fixture shipped beside the gear config.
#[cfg(feature = "gate-dev-stubs")]
const EXAMPLE_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config/gate-dev-stubs.example.json"
);

#[cfg(not(feature = "gate-dev-stubs"))]
mod without_feature {
    use crate::config::GateConfig;

    #[test]
    fn validate_rejects_dev_stubs_without_the_feature() {
        let config: GateConfig = serde_json::from_value(serde_json::json!({
            "dev_stubs": true,
            "dev_stub_fixture": "gears/bss/orders/config/gate-dev-stubs.example.json"
        }))
        .unwrap();
        let error = config.validate().unwrap_err().to_string();
        assert!(
            error.contains("gate.dev_stubs requires the gate-dev-stubs feature"),
            "{error}"
        );
    }

    #[test]
    fn dev_stubs_off_validates_without_the_feature() {
        GateConfig::default().validate().unwrap();
    }
}

#[cfg(feature = "gate-dev-stubs")]
mod with_feature {
    use super::EXAMPLE_FIXTURE;
    use crate::config::GateConfig;
    use crate::domain::gate::answers::{
        AcceptancePolicyQuery, AssessmentLine, AssessmentQuery, BillingTermsQuery, ContractQuery,
        EvaluationCycle, EvaluationLine, EvaluationTerm, LineEvaluation, Market, OccupancyQuery,
        OrderEvaluationRequest, OverlapKeyLine, PayerProfileQuery, TaxQuery, TermIntentInput,
    };
    use crate::domain::gate::assess::{AssessmentInput, BasketLine, assess};
    use crate::domain::gate::ports::{Delivery, GatePort, PortAnswer, Unavailable};
    use crate::domain::gate::predicates::PredicateId as P;
    use crate::domain::gate::pricing_map::{LineCatalogRequest, SelectedItem};
    use crate::domain::gate::vector::{Operation, Verdict};
    use crate::infra::gate::GatePorts;
    use crate::infra::gate::budget::{Budget, RunBudget};
    use crate::infra::gate::ports::{
        AcceptancePolicyReads, AssessmentReads, BillingTermsReads, ContractReads, EvaluationReads,
        IdentityReads, IndicativeTaxReads, OccupancyReads, OverlapKeyReads,
    };
    use crate::infra::gate::readiness::PortState;
    use crate::infra::gate::stubs::{
        AcceptancePolicyStub, AssessmentStub, BillingTermsStub, ContractsStub, EvaluationStub,
        IdentityStub, IndicativeTaxStub, OccupancyStub, OverlapKeyStub, STUB_PRODUCER, StubFixture,
    };
    use crate::infra::gate::stubs::{SellabilityStub, StubBindings};
    use bss_pricing_sdk::acceptance::{
        CommandMeta, CommercialReason, Market as SaleMarket, NewSaleQuery, SellabilityV1,
        TenantAxes, Term,
    };
    use bss_pricing_sdk::digest::{billing_terms_digest, selected_bindings_digest};
    use bss_pricing_sdk::read::ResolvedBindings;
    use bss_pricing_sdk::terms::{
        BillingAnchor, BillingCycle, BillingTerms, TermsSource, Timezone,
    };
    use std::path::Path;
    use std::sync::Arc;
    use time::{Date, Month, OffsetDateTime};
    use tokio_util::sync::CancellationToken;
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_security::SecurityContext;
    use uuid::Uuid;

    // Keys the example fixture states.
    const PAYER: Uuid = Uuid::from_u128(0xa1);
    const CONTRACT: Uuid = Uuid::from_u128(0xc1);
    const SKU: Uuid = Uuid::from_u128(0xd1);
    const REVISION: Uuid = Uuid::from_u128(0xe1);
    const LINE: Uuid = Uuid::from_u128(0xf1);
    // Keys it does not state.
    const OTHER: Uuid = Uuid::from_u128(0x99);
    const SELLER: Uuid = Uuid::from_u128(0x5e11);
    const PLAN: Uuid = Uuid::from_u128(0x10);
    const ITEM_A: Uuid = Uuid::from_u128(0x20);
    const ITEM_B: Uuid = Uuid::from_u128(0x21);

    fn fixture() -> Arc<StubFixture> {
        Arc::new(StubFixture::load(Path::new(EXAMPLE_FIXTURE)).unwrap())
    }
    fn empty() -> Arc<StubFixture> {
        Arc::new(StubFixture::default())
    }
    fn call() -> (SecurityContext, RunBudget, CancellationToken) {
        (
            SecurityContext::anonymous(),
            RunBudget::new(),
            CancellationToken::new(),
        )
    }
    fn missing<T: std::fmt::Debug>(answer: &PortAnswer<T>) {
        assert!(
            matches!(answer, PortAnswer::Unavailable(Unavailable::Missing)),
            "{answer:?}"
        );
    }
    fn day() -> Date {
        Date::from_calendar_date(2026, Month::October, 22).unwrap()
    }
    /// A frozen date basis covering no line, so every line's date row fails closed.
    fn empty_basis() -> crate::domain::dates::DateBasis {
        use crate::domain::dates::{
            DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow,
        };
        let row = PolicyRow {
            policy_id: PLATFORM_DEFAULT_POLICY_ID,
            resource_tenant_id: None,
            service_activation_required: false,
            acceptance_due_required: false,
            revision: 1,
        };
        let policy = DatePolicySnapshot::effective(PAYER, &[row]).unwrap();
        DateBasis::resolve(PAYER, policy, day(), std::iter::empty())
    }
    fn config(fixture: Option<&str>) -> GateConfig {
        serde_json::from_value(serde_json::json!({
            "dev_stubs": true,
            "dev_stub_fixture": fixture,
        }))
        .unwrap()
    }
    /// The composition root alone over an unvalidated config, so the init-time fixture checks
    /// are what is under test (`OrdersConfig::validate` runs `validate()` first at init).
    fn bind(config: &GateConfig) -> anyhow::Result<GatePorts> {
        let budget = Arc::new(Budget::baseline());
        GatePorts::from_hub(&toolkit::ClientHub::new(), config, budget)
    }

    #[test]
    fn the_example_fixture_parses_and_states_every_port() {
        let f = fixture();
        assert!(f.payer_profiles.contains_key(&PAYER));
        assert!(f.contracts.contains_key(&CONTRACT));
        assert!(f.overlap_keys.contains_key(&SKU));
        assert!(f.occupancy.contains_key("dev-stub-scope"));
        assert!(f.terms.contains_key(&REVISION));
        assert!(f.evaluation.contains_key(&REVISION));
        assert!(f.acceptance_policy.is_some());
        assert!(f.assessment_profile.is_some());
        assert!(f.evaluation_totals.is_some());
        assert!(f.tcv.is_some());
        assert!(f.tax.is_some());
    }

    #[test]
    fn dev_delivery_fixture_requires_the_flag_and_the_fixture() {
        let off: GateConfig = serde_json::from_value(serde_json::json!({
            "dev_stubs": false,
            "dev_stub_fixture": EXAMPLE_FIXTURE,
        }))
        .unwrap();
        assert_eq!(off.dev_delivery_fixture(), None);
        let on = config(Some(EXAMPLE_FIXTURE));
        assert_eq!(
            on.dev_delivery_fixture(),
            Some(std::path::Path::new(EXAMPLE_FIXTURE))
        );
    }

    #[test]
    fn validate_requires_a_fixture_path_with_the_feature() {
        let error = config(None).validate().unwrap_err().to_string();
        assert!(
            error.contains("gate.dev_stubs requires gate.dev_stub_fixture"),
            "{error}"
        );
        config(Some(EXAMPLE_FIXTURE)).validate().unwrap();
    }

    #[test]
    fn a_missing_or_invalid_fixture_fails_init() {
        let error = bind(&config(Some("/nonexistent/gate-dev-stubs.json")))
            .unwrap_err()
            .to_string();
        assert!(error.contains("is unreadable"), "{error}");
        let not_json = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let error = bind(&config(Some(not_json))).unwrap_err().to_string();
        assert!(error.contains("is not a valid fixture"), "{error}");
    }

    #[test]
    fn dev_stubs_off_binds_nothing_even_with_the_feature() {
        let ports = bind(&GateConfig::default()).unwrap();
        assert!(ports.stubbed().is_empty());
        let readiness = ports.readiness();
        assert!(!readiness.dev_stubs);
        let manifest = serde_json::to_value(&readiness).unwrap();
        assert!(manifest.get("dev_stubs").is_none(), "{manifest}");
    }

    #[test]
    fn stubs_bind_every_undelivered_port_and_never_the_catalog() {
        let ports = bind(&config(Some(EXAMPLE_FIXTURE))).unwrap();
        let readiness = ports.readiness();
        assert!(readiness.dev_stubs);
        for (entry, port) in readiness.ports.iter().zip(GatePort::ALL) {
            match port.delivery() {
                Delivery::Available => assert_eq!(entry.state, PortState::MissingClient),
                Delivery::Missing { .. } => assert_eq!(entry.state, PortState::Stubbed),
            }
        }
        assert_eq!(ports.stubbed().len(), 9);
        assert!(!ports.catalog.is_bound());
        let manifest = serde_json::to_value(&readiness).unwrap();
        assert_eq!(manifest["dev_stubs"], true);
        let evaluation = manifest["ports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["token"] == "evaluation")
            .unwrap();
        assert_eq!(evaluation["state"], "stubbed (development only)");
        let summary = readiness.summary();
        assert!(
            summary.ends_with("0 of 12 ports provider-missing, 9 stubbed (development only)"),
            "{summary}"
        );
    }

    #[tokio::test]
    async fn identity_contracts_and_policy_answer_only_what_the_fixture_states() {
        let (ctx, run, cancel) = call();
        let query = |payer| PayerProfileQuery {
            payer_tenant_id: payer,
            seller_tenant_id: SELLER,
            resource_tenant_id: payer,
        };
        let identity = IdentityStub::new(fixture());
        let hit = identity
            .payer_profile(&ctx, &query(PAYER), &run, &cancel)
            .await;
        assert_eq!(
            hit,
            PortAnswer::Answered(fixture().payer_profiles[&PAYER].clone())
        );
        missing(
            &identity
                .payer_profile(&ctx, &query(OTHER), &run, &cancel)
                .await,
        );

        let contract = |id| ContractQuery {
            contract_id: id,
            payer_tenant_id: PAYER,
            seller_tenant_id: SELLER,
        };
        let contracts = ContractsStub::new(fixture());
        let hit = contracts
            .resolve(&ctx, &[contract(CONTRACT)], &run, &cancel)
            .await;
        assert_eq!(
            hit.answered().unwrap()[&CONTRACT],
            fixture().contracts[&CONTRACT]
        );
        missing(
            &contracts
                .resolve(&ctx, &[contract(CONTRACT), contract(OTHER)], &run, &cancel)
                .await,
        );

        let policy_query = AcceptancePolicyQuery {
            seller_tenant_id: SELLER,
        };
        let hit = AcceptancePolicyStub::new(fixture())
            .policy(&policy_query, &run, &cancel)
            .await;
        assert_eq!(
            hit,
            PortAnswer::Answered(fixture().acceptance_policy.clone())
        );
        missing(
            &AcceptancePolicyStub::new(empty())
                .policy(&policy_query, &run, &cancel)
                .await,
        );
    }

    #[tokio::test]
    async fn overlap_occupancy_and_terms_answer_only_what_the_fixture_states() {
        let (ctx, run, cancel) = call();
        let line = |sku| OverlapKeyLine {
            line_id: LINE,
            plan_revision_id: REVISION,
            paid_recurring_sku: sku,
            item_id: Uuid::nil(),
            dimension_value: None,
            sku_version: 1,
        };
        let keys = OverlapKeyStub::new(fixture());
        let hit = keys
            .keys(&ctx, PAYER, None, &[line(SKU)], &run, &cancel)
            .await;
        let hit = hit.answered().unwrap();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].line_id, LINE);
        assert_eq!(hit[0].overlap_scope_key, "dev-stub-scope");
        missing(
            &keys
                .keys(&ctx, PAYER, None, &[line(SKU), line(OTHER)], &run, &cancel)
                .await,
        );

        let occupancy_query = |key: &str| OccupancyQuery {
            own_context: None,
            payer_tenant_id: PAYER,
            resource_tenant_id: None,
            overlap_scope_key: key.to_owned(),
        };
        let occupancy = OccupancyStub::new(fixture());
        let hit = occupancy
            .get_overlap_occupancy(&ctx, &[occupancy_query("dev-stub-scope")], &run, &cancel)
            .await;
        assert_eq!(
            hit,
            PortAnswer::Answered(vec![(
                occupancy_query("dev-stub-scope"),
                fixture().occupancy["dev-stub-scope"].clone()
            )])
        );
        missing(
            &occupancy
                .get_overlap_occupancy(&ctx, &[occupancy_query("unknown")], &run, &cancel)
                .await,
        );

        let terms_query = |revision| BillingTermsQuery {
            line_id: LINE,
            payer_tenant_id: PAYER,
            seller_tenant_id: SELLER,
            plan_revision_id: revision,
            intent: TermIntentInput::Rolling,
            contract_id: None,
        };
        let terms = BillingTermsStub::new(fixture());
        let hit = terms.resolve(&[terms_query(REVISION)], &run, &cancel).await;
        let hit = hit.answered().unwrap();
        assert_eq!(hit[0].line_id, LINE, "the answer carries the query's line");
        assert_eq!(
            hit[0].billing_terms,
            fixture().terms[&REVISION].billing_terms
        );
        missing(&terms.resolve(&[terms_query(OTHER)], &run, &cancel).await);
    }

    /// A request of `(line, plan revision)` pairs.
    fn evaluation_request(lines: &[(Uuid, Uuid)]) -> OrderEvaluationRequest {
        OrderEvaluationRequest {
            tenant_axes: None,
            assessed_at: None,
            acceptance_policy: None,
            assessment_id: Uuid::from_u128(0x60),
            resolve_date: day(),
            market: Market {
                currency: "USD".to_owned(),
                region: "US".to_owned(),
            },
            contract_id: None,
            lines: lines
                .iter()
                .map(|(id, revision)| EvaluationLine {
                    plan_id: None,
                    start_at: None,
                    resolved_terms: None,
                    line_id: *id,
                    plan_revision_id: *revision,
                    term: EvaluationTerm::Rolling,
                    cycle: EvaluationCycle::Month,
                    items: Vec::new(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn evaluation_returns_the_fixture_as_given_and_a_partial_fixture_is_missing() {
        let (ctx, run, cancel) = call();
        let f = fixture();
        let stub = EvaluationStub::new(Arc::clone(&f));
        let hit = stub
            .evaluate(
                &ctx,
                &evaluation_request(&[(LINE, REVISION)]),
                &run,
                &cancel,
            )
            .await;
        let hit = hit.answered().unwrap();
        let totals = f.evaluation_totals.as_ref().unwrap();
        assert_eq!(hit.assessment_id, Uuid::from_u128(0x60));
        // The fixture's evaluation by revision, carrying the request line's identity.
        assert_eq!(
            hit.lines,
            vec![LineEvaluation {
                line_id: LINE,
                ..f.evaluation[&REVISION].clone()
            }]
        );
        assert_eq!(hit.order_totals, totals.order_totals, "never summed");
        assert_eq!(hit.tcv, f.tcv, "as given");
        assert!(hit.estimate);
        // One revision applies to every line priced on it.
        let two = stub
            .evaluate(
                &ctx,
                &evaluation_request(&[(LINE, REVISION), (OTHER, REVISION)]),
                &run,
                &cancel,
            )
            .await;
        let two = two.answered().unwrap();
        assert_eq!(
            two.lines.iter().map(|l| l.line_id).collect::<Vec<_>>(),
            vec![LINE, OTHER]
        );
        assert_eq!(two.lines[0].totals, two.lines[1].totals);
        // A line on a revision the fixture does not state makes the whole answer missing.
        missing(
            &stub
                .evaluate(
                    &ctx,
                    &evaluation_request(&[(LINE, REVISION), (OTHER, OTHER)]),
                    &run,
                    &cancel,
                )
                .await,
        );
        let without_totals = StubFixture {
            evaluation_totals: None,
            ..(*f).clone()
        };
        missing(
            &EvaluationStub::new(Arc::new(without_totals))
                .evaluate(
                    &ctx,
                    &evaluation_request(&[(LINE, REVISION)]),
                    &run,
                    &cancel,
                )
                .await,
        );
    }

    #[tokio::test]
    async fn tax_answers_only_what_the_fixture_states() {
        let (ctx, run, cancel) = call();
        let query = TaxQuery {
            payer_tenant_id: PAYER,
            seller_tenant_id: SELLER,
            lines: Vec::new(),
        };
        let hit = IndicativeTaxStub::new(fixture())
            .estimate(&ctx, &query, &run, &cancel)
            .await;
        assert_eq!(hit, PortAnswer::Answered(fixture().tax.clone().unwrap()));
        missing(
            &IndicativeTaxStub::new(empty())
                .estimate(&ctx, &query, &run, &cancel)
                .await,
        );
    }

    fn selection(item: Uuid, dim: Option<&str>) -> SelectedItem {
        SelectedItem {
            item_id: item,
            quantity: "1".to_owned(),
            selected_dim_value: dim.map(str::to_owned),
        }
    }

    /// Two lines, one with a repeated slot (deduplicated as `map_line` does) and a dimension.
    fn basket() -> Vec<(Uuid, Vec<SelectedItem>)> {
        vec![
            (Uuid::from_u128(0x101), vec![selection(ITEM_A, None)]),
            (
                Uuid::from_u128(0x102),
                vec![
                    selection(ITEM_A, None),
                    selection(ITEM_B, Some("eu")),
                    selection(ITEM_B, Some("eu")),
                ],
            ),
        ]
    }

    /// A port answer that never arrived.
    fn unanswered<T>() -> PortAnswer<T> {
        PortAnswer::Unavailable(Unavailable::Missing)
    }

    /// The assessment stub's answer, fed to the pure `assess`, passes every assessed row
    /// (predicates 2, 7, 8 and 4-Orders) on every line and slot, with no malformed envelope.
    #[tokio::test]
    async fn the_assessment_stub_covers_what_assess_expects() {
        let (ctx, run, cancel) = call();
        let query = AssessmentQuery {
            assessment_id: Uuid::from_u128(0x60),
            seller_tenant_id: SELLER,
            resolve_date: day(),
            profile: crate::domain::gate::orchestrate::ASSESSMENT_PROFILE.to_owned(),
            lines: basket()
                .into_iter()
                .map(|(line_id, selections)| AssessmentLine {
                    line_id,
                    plan_id: PLAN,
                    revision_id: REVISION,
                    selections,
                    market: None,
                    term: None,
                    cycle: None,
                })
                .collect(),
        };
        missing(
            &AssessmentStub::new(empty())
                .assess(&query, &run, &cancel)
                .await,
        );
        let answer = AssessmentStub::new(fixture())
            .assess(&query, &run, &cancel)
            .await;
        let profile = answer.answered().unwrap();
        assert_eq!(
            profile.profile,
            crate::domain::gate::orchestrate::ASSESSMENT_PROFILE
        );
        assert!(
            profile
                .results
                .iter()
                .all(|r| r.result.producer == STUB_PRODUCER && r.result.profile == "dev-stub")
        );
        let lines: Vec<BasketLine> = basket()
            .into_iter()
            .map(|(line_id, items)| BasketLine {
                line_id,
                catalog: LineCatalogRequest {
                    line_id,
                    plan_id: PLAN,
                    revision_id: REVISION,
                    items,
                },
                currency: Some("USD".to_owned()),
                contract_ref: None,
            })
            .collect();
        let identity = IdentityStub::new(fixture())
            .payer_profile(
                &ctx,
                &PayerProfileQuery {
                    payer_tenant_id: PAYER,
                    seller_tenant_id: SELLER,
                    resource_tenant_id: PAYER,
                },
                &run,
                &cancel,
            )
            .await;
        let input = AssessmentInput {
            occupancy_axes: None,
            unsupported_terms: std::collections::BTreeSet::default(),
            operation: Operation::Submit,
            resolve_date: day(),
            lines,
            catalog: std::collections::BTreeMap::new(),
            identity,
            contract_id: None,
            contracts: unanswered(),
            overlap_keys: unanswered(),
            occupancy: unanswered(),
            inflight_same_tuple: std::collections::BTreeSet::new(),
            inflight_other_orders: std::collections::BTreeMap::new(),
            billing_terms: unanswered(),
            acceptance_policy: unanswered(),
            assessment: answer,
            evaluation: unanswered(),
            dates: empty_basis(),
        };
        let assessed = assess(&input).unwrap();
        let rows: Vec<_> = assessed
            .vector
            .outcomes()
            .iter()
            .filter(|o| {
                [
                    P::CatalogRevisionAvailable,
                    P::CatalogDimensionMarket,
                    P::CatalogEligibilityRules,
                    P::MarketConsistency,
                ]
                .contains(&o.key.predicate)
            })
            .collect();
        // Per line: 2, 8, 4-Orders; predicate 7 per distinct slot (1 + 2).
        assert_eq!(rows.len(), 2 * 3 + 3);
        for o in rows {
            assert_eq!(o.verdict, Verdict::Passed, "{o:?}");
            assert_eq!(o.producer_results.len(), 1, "{o:?}");
        }
    }

    /// Resolves empty bindings once every caller is parked past the stub's first idempotency
    /// check, so two concurrent first commands both reach the insert (no sleeps, no timing).
    struct ParkedBindings {
        gate: tokio::sync::Barrier,
    }
    #[async_trait::async_trait]
    impl StubBindings for ParkedBindings {
        async fn resolve(
            &self,
            _ctx: &SecurityContext,
            query: &NewSaleQuery,
        ) -> Result<ResolvedBindings, CanonicalError> {
            self.gate.wait().await;
            Ok(ResolvedBindings {
                plan_id: query.plan_id,
                revision_id: query.plan_revision_id,
                cells: Vec::new(),
            })
        }
    }

    fn sale_query(quantity: &str) -> NewSaleQuery {
        let mut billing_terms = BillingTerms {
            schema_version: 1,
            cycle: BillingCycle::Month,
            anchor: BillingAnchor::SubscriptionStart,
            anchor_at: OffsetDateTime::UNIX_EPOCH,
            timezone: Timezone::Utc,
            source: TermsSource::ExplicitOrder,
            digest: [0; 32],
        };
        billing_terms.digest = billing_terms_digest(&billing_terms);
        let resolved = ResolvedBindings {
            plan_id: Uuid::from_u128(1),
            revision_id: REVISION,
            cells: Vec::new(),
        };
        NewSaleQuery {
            tenant_axes: TenantAxes {
                seller_tenant_id: Uuid::from_u128(2),
                payer_tenant_id: Uuid::from_u128(3),
                resource_tenant_id: Uuid::from_u128(4),
            },
            order_id: Uuid::from_u128(5),
            order_version: 1,
            line_id: LINE,
            plan_id: Uuid::from_u128(1),
            plan_revision_id: REVISION,
            selections: Vec::new(),
            quantity: bss_money::parse_decimal(quantity).unwrap(),
            market: SaleMarket {
                currency: "USD".into(),
                region: None,
            },
            start_at: OffsetDateTime::UNIX_EPOCH,
            term: Term::Rolling,
            billing_terms,
            resolved_bindings_digest: selected_bindings_digest(&resolved, &[]).unwrap(),
            hold_policy_version: 1,
        }
    }

    // gated (`--features gate-dev-stubs`), lib tier.
    #[tokio::test]
    async fn a_concurrent_same_key_with_another_query_is_an_idempotency_conflict() {
        let stub = SellabilityStub::new(
            &fixture(),
            Arc::new(ParkedBindings {
                gate: tokio::sync::Barrier::new(2),
            }),
        );
        let ctx = SecurityContext::anonymous();
        let meta = || CommandMeta {
            idempotency_key: "same-key".into(),
        };
        let (a, b) = tokio::join!(
            stub.check(&ctx, sale_query("1"), meta()),
            stub.check(&ctx, sale_query("2"), meta()),
        );
        // Exactly one wins; the other query under the same key is the conflict, never the
        // winner's receipt.
        let (won, lost) = match (a, b) {
            (Ok(won), Err(lost)) | (Err(lost), Ok(won)) => (won, lost),
            other => panic!("one winner and one conflict expected: {other:?}"),
        };
        assert!(
            won.query.quantity == sale_query("1").quantity
                || won.query.quantity == sale_query("2").quantity
        );
        assert_eq!(
            format!("{lost:?}"),
            format!(
                "{:?}",
                CanonicalError::from(CommercialReason::IdempotencyConflict)
            )
        );
    }
}
