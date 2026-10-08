//! S1-06 harness evidence only. Probe tables are not Orders business migrations.
use super::capability_support::evidence::audit;
use super::{Pg, entity, faults, insert, provider, row, rows, u};
use sea_orm::{EntityTrait, IntoActiveModel, Set};
use std::sync::Arc;
use toolkit_db::secure::{SecureEntityExt, TxConfig, secure_insert, secure_update_with_scope};

#[test]
fn every_fault_point_fires_once_and_has_a_recovery_contract() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/fault-contracts.json")).unwrap();
    assert_eq!(
        manifest["faults"].as_array().unwrap().len(),
        faults::FaultPoint::ALL.len()
    );
    for point in faults::FaultPoint::ALL {
        let contract = manifest["faults"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == point.name())
            .unwrap();
        for key in [
            "owner_package",
            "durable_facts",
            "recovery",
            "forbidden",
            "runtime_evidence",
        ] {
            assert!(!contract[key].is_null());
        }
        let fault = faults::FailOnce::at(point);
        for other in faults::FaultPoint::ALL {
            if other != point {
                assert!(fault.hit(other).is_ok());
            }
        }
        assert!(fault.hit(point).is_err());
        assert!(fault.hit(point).is_ok());
    }
}

#[tokio::test]
async fn before_commit_failure_rolls_back_both_mutation_and_evidence() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let original = row(1, 10, 20, 30);
    insert(&pg.db, &original).await?;
    let scope = provider::scope(vec![provider::path(vec![provider::eq("id", u(1))])]).await;
    let tx_scope = scope.clone();
    let mut proposed = original.clone().into_active_model();
    proposed.version = Set(2);
    let fault = faults::FailOnce::at(faults::FaultPoint::BeforeLocalCommit);
    let result: anyhow::Result<()> = pg
        .db
        .transaction_ref_mapped_with_config(TxConfig::default(), |tx| {
            Box::pin(async move {
                secure_update_with_scope::<entity::Entity>(proposed, &tx_scope, u(1), tx).await?;
                secure_insert::<audit::Entity>(
                    audit::ActiveModel {
                        id: Set(u(1)),
                        value: Set("prepared evidence".into()),
                    },
                    &tx_scope,
                    tx,
                )
                .await?;
                fault.hit(faults::FaultPoint::BeforeLocalCommit)?;
                Ok(())
            })
        })
        .await;
    assert!(result.is_err());
    assert_eq!(rows(&pg.db, &scope).await?, vec![original]);
    assert!(
        audit::Entity::find()
            .secure()
            .scope_with(&scope)
            .all(&pg.db.conn()?)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn commit_gate_is_observable_and_lost_response_keeps_committed_facts() -> anyhow::Result<()> {
    let pg = Pg::new().await?;
    let original = row(1, 10, 20, 30);
    insert(&pg.db, &original).await?;
    let scope = provider::scope(vec![provider::path(vec![provider::eq("id", u(1))])]).await;
    let db = pg.db.clone();
    let tx_scope = scope.clone();
    let mut proposed = original.clone().into_active_model();
    proposed.version = Set(2);
    let (gate, controller) = faults::Gate::pair();
    let clock = Arc::new(faults::Clock::at(1_000_001));
    let worker_clock = clock.clone();
    let worker = tokio::spawn(async move {
        db.transaction_ref_mapped_with_config(TxConfig::default(), |tx| {
            Box::pin(async move {
                secure_update_with_scope::<entity::Entity>(proposed, &tx_scope, u(1), tx).await?;
                gate.wait().await?;
                // The clock is sampled after resumption, not before the wait.
                secure_insert::<audit::Entity>(
                    audit::ActiveModel {
                        id: Set(u(1)),
                        value: Set(worker_clock.now().to_string()),
                    },
                    &tx_scope,
                    tx,
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
        faults::FailOnce::at(faults::FaultPoint::AfterLocalCommit)
            .hit(faults::FaultPoint::AfterLocalCommit)
    });
    controller.arrived.await?;
    // An independent connection sees neither the uncommitted new version nor its evidence.
    let observed = rows(&pg.db, &scope).await?;
    let evidence = audit::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&pg.db.conn()?)
        .await?;
    clock.set(2_000_009);
    controller
        .release
        .send(())
        .map_err(|()| anyhow::anyhow!("worker gone"))?;
    assert!(
        worker.await?.is_err(),
        "reply intentionally lost after commit"
    );
    assert_eq!(observed, vec![original]);
    assert!(evidence.is_empty());
    assert_eq!(rows(&pg.db, &scope).await?[0].version, 2);
    let durable = audit::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&pg.db.conn()?)
        .await?;
    assert_eq!(durable.len(), 1);
    assert_eq!(durable[0].value, "2000009");
    Ok(())
}

#[tokio::test]
async fn frozen_hashes_and_microsecond_instants_round_trip_postgres() -> anyhow::Result<()> {
    use super::capability_support::evidence::vector;
    use sea_orm::{NotSet, prelude::DateTimeUtc};
    let pg = Pg::new().await?;
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/conformance-v1.json"))?;
    for (index, v) in fixtures["vectors"].as_array().unwrap().iter().enumerate() {
        let id = u(u128::try_from(index)? + 1);
        let scope = provider::scope(vec![provider::path(vec![provider::eq("id", id)])]).await;
        let micros = v["input"]["created_at"]
            .as_str()
            .unwrap_or("1791288000123456")
            .parse::<i64>()?;
        let instant = DateTimeUtc::from_timestamp_micros(micros).unwrap();
        let bytes = unhex(v["preimage_hex"].as_str().unwrap())?;
        secure_insert::<vector::Entity>(
            vector::ActiveModel {
                id: Set(id),
                captured_at: Set(instant),
                preimage: Set(bytes.clone()),
                digest: NotSet,
            },
            &scope,
            &pg.db.conn()?,
        )
        .await?;
        let stored = vector::Entity::find_by_id(id)
            .secure()
            .scope_with(&scope)
            .one(&pg.db.conn()?)
            .await?
            .unwrap();
        assert_eq!(stored.captured_at.timestamp_micros(), micros, "{}", v["id"]);
        assert_eq!(stored.preimage, bytes);
        assert_eq!(
            stored.digest,
            unhex(v["sha256"].as_str().unwrap())?,
            "{}",
            v["id"]
        );
    }
    Ok(())
}

fn unhex(hex: &str) -> anyhow::Result<Vec<u8>> {
    (0..hex.len())
        .step_by(2)
        .map(|i| Ok(u8::from_str_radix(&hex[i..i + 2], 16)?))
        .collect()
}
