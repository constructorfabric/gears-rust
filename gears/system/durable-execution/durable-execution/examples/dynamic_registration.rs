#![allow(clippy::unwrap_used, clippy::expect_used, clippy::use_debug)]
//! Integration: a per-tenant CRM contact sync that is paused, disconnected and reconnected.
//!
//! Mechanism:
//! - `Retain` closes new syncs while the CRM has an outage; existing runs keep their state.
//!   `activate` reopens the same generation.
//! - `CancelAndRelease`: the tenant disconnects the CRM. In-flight syncs are cancelled and
//!   the local bindings are released.
//! - On reconnect the application binds a handler with the new credentials to the same
//!   contract and explicitly activates a new generation.
//!
//! Prerequisites: disposable PostgreSQL, `DURABLE_EXAMPLE_PG_URL` and `DURABLE_EXAMPLE_SECRET`.
//! Run: `cargo run -p cf-gears-durable-execution --example dynamic_registration`
mod support;
use durable_execution_sdk::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Contact {
    email: String,
}

/// Credentials live in the handler, never in the persisted contract or input.
fn crm_sync(
    credentials: &'static str,
) -> Result<Workflow<Contact, String>, durable_execution_sdk::DefinitionError> {
    WorkflowBuilder::<Contact>::new("integrations.crm-sync.v1")
        .then(support::step("push_contact", move |_, contact: Contact| async move {
            Ok(format!("crm:{credentials}:{}", contact.email))
        }))
        .build()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        use durable_execution_sdk::registration::{
            RegistrationState, UnregisterMode, UnregisterOptions,
        };
        let connected = crm_sync("api-key-1")?;
        let reference = connected.reference();
        let active = app.registry.register(connected.into_definition()).await?;
        let contact = || Contact {
            email: "lead@example.com".into(),
        };

        // CRM outage: stop admitting new syncs, keep everything else.
        let paused = app
            .registry
            .unregister(
                reference.name(),
                UnregisterOptions {
                    mode: UnregisterMode::Retain,
                    expected_revision: active.revision,
                },
            )
            .await?;
        assert_eq!(paused.state, RegistrationState::Retired);
        assert!(
            app.client
                .start(&app.owner, &reference, contact(), StartOptions::default())
                .await
                .is_err()
        );
        let active = app
            .registry
            .activate(reference.name(), paused.revision)
            .await?;

        // The tenant disconnects the CRM: cancel in-flight work and drop the bindings.
        let disconnecting = app
            .registry
            .unregister(
                reference.name(),
                UnregisterOptions {
                    mode: UnregisterMode::CancelAndRelease,
                    expected_revision: active.revision,
                },
            )
            .await?;
        assert!(matches!(
            disconnecting.state,
            RegistrationState::Stopping | RegistrationState::Released
        ));
        let released = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let state = app.registry.registration(reference.name()).await?;
                if state.state == RegistrationState::Released {
                    return Ok::<_, toolkit_canonical_errors::CanonicalError>(state);
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await??;

        // Reconnect with rotated credentials: same contract, new handler, new generation.
        let prepared = app
            .registry
            .register(crm_sync("api-key-2")?.into_definition())
            .await?;
        assert_eq!(prepared.state, RegistrationState::Released);
        let reconnected = app
            .registry
            .activate(reference.name(), released.revision)
            .await?;
        assert!(reconnected.generation > released.generation);

        let started = app
            .client
            .start(&app.owner, &reference, contact(), StartOptions::default())
            .await?;
        app.wait(started.run_id).await?;
        assert_eq!(
            app.client
                .result(&app.owner, started.run_id, &reference)
                .await?,
            "crm:api-key-2:lead@example.com"
        );
        Ok(())
    })
    .await
}
