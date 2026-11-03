//! Two independently registered tenant CRM integrations and credential rotation.
//! The global registrar is a trusted host capability, never a user-facing endpoint.
//!
//! One definition per tenant fits integrations whose credentials and lifecycle
//! (pause, disconnect, reconnect) are managed per tenant, and whose count is small.
//! For a shared business process, register one definition and pass tenant data in
//! the run input: names are limited to 160 bytes and every definition is a catalog row.
mod support;
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use support::fake::IdempotentProvider;
use toolkit_durable_execution::{
    RegisteredWorkflow,
    prelude::*,
    registration::{RegistrationState, UnregisterMode, UnregisterOptions},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Contact {
    email: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    contact_id: String,
    connection_version: u32,
}
struct CrmClient {
    secret: secrecy::SecretString,
    version: u32,
    provider: IdempotentProvider<Receipt>,
}
impl CrmClient {
    fn push(&self, key: &str, contact: &Contact) -> Result<Receipt, ActivityError> {
        use secrecy::ExposeSecret;
        if self.secret.expose_secret().is_empty() {
            return Err(ActivityError::permanent("crm_credentials_missing"));
        }
        Ok(self.provider.call(key, || Receipt {
            contact_id: format!("contact-{}", contact.email),
            connection_version: self.version,
        }))
    }
}
fn workflow(
    tenant: uuid::Uuid,
    version: u32,
) -> Result<Workflow<Contact, Receipt>, toolkit_durable_execution::DefinitionError> {
    let crm = Arc::new(CrmClient {
        secret: secrecy::SecretString::from(format!("demo-secret-{version}")),
        version,
        provider: IdempotentProvider::default(),
    });
    WorkflowBuilder::<Contact>::new(format!("integrations.tenant-{tenant}.crm-sync.v1"))
        .then(Step::new(
            "push",
            Duration::from_secs(10),
            move |ctx, contact: Contact| {
                let crm = crm.clone();
                async move {
                    if ctx.owner().tenant_id != tenant {
                        return Err(ActivityError::permanent("tenant_mismatch"));
                    }
                    crm.push(ctx.idempotency_key(), &contact)
                }
            },
        ))
        .build()
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    support::run(async |app: &support::App| {
        let first = workflow(app.owner.subject_tenant_id(), 1)?;
        let second = workflow(app.other_owner.subject_tenant_id(), 1)?;
        let RegisteredWorkflow {
            reference: first_ref,
            registration,
        } = app.registry.register_workflow(first).await?;
        let second_ref = app.registry.register_workflow(second).await?.reference;
        let contact = || Contact {
            email: "lead@example.com".into(),
        };
        let paused = app
            .registry
            .unregister(
                first_ref.name(),
                UnregisterOptions::new(UnregisterMode::Retain, registration.revision),
            )
            .await?;
        assert!(
            app.client
                .start(&app.owner, &first_ref, contact(), StartOptions::new())
                .await
                .is_err()
        );
        let other = app
            .client
            .start(
                &app.other_owner,
                &second_ref,
                contact(),
                StartOptions::new(),
            )
            .await?;
        tokio::time::timeout(
            Duration::from_secs(60),
            app.client.wait(&app.other_owner, other.run_id()),
        )
        .await??;
        assert_eq!(
            app.client
                .result(&app.other_owner, other.handle())
                .await?
                .connection_version,
            1
        );
        assert!(app.client.get(&app.owner, other.run_id()).await.is_err());
        let active = app
            .registry
            .activate(first_ref.name(), paused.revision)
            .await?;
        app.registry
            .unregister(
                first_ref.name(),
                UnregisterOptions::new(UnregisterMode::CancelAndRelease, active.revision),
            )
            .await?;
        let released = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let state = app.registry.registration(first_ref.name()).await?;
                if state.state == RegistrationState::Released {
                    return Ok::<_, toolkit_durable_execution::Error>(state);
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;
        let renewed = workflow(app.owner.subject_tenant_id(), 2)?;
        let renewed_ref = app.registry.register_workflow(renewed).await?.reference;
        app.registry
            .activate(first_ref.name(), released.revision)
            .await?;
        assert_eq!(
            app.registry.registration(second_ref.name()).await?.state,
            RegistrationState::Active
        );
        let started = app
            .client
            .start(&app.owner, &renewed_ref, contact(), StartOptions::new())
            .await?;
        app.wait(started.run_id()).await?;
        let receipt = app.client.result(&app.owner, started.handle()).await?;
        assert_eq!(receipt.connection_version, 2);
        assert_eq!(receipt.contact_id, "contact-lead@example.com");
        assert!(!serde_json::to_string(&receipt)?.contains("secret"));
        Ok(())
    })
    .await
}
