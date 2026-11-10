use gts::GtsTypeId;
use license_enforcement_sdk::models::{PermissionsInput, ResourceRefInput, WindowInput};
use license_enforcement_sdk::{
    CallerIdentity, EvidenceId, Intake, MaterialError, linked_admission_plugin,
};
use time::OffsetDateTime;
use time::macros::datetime;

use super::*;

struct NoMaterial;

#[async_trait]
impl PluginMaterial for NoMaterial {
    async fn get(&self, _key: &str) -> Result<Option<Vec<u8>>, MaterialError> {
        Ok(None)
    }
}

fn terms() -> GrantTerms {
    GrantTerms {
        resource: ResourceRefInput {
            gts_type: GtsTypeId::new("gts.cf.core.lic.res.v1~acme.app.seat.v1~"),
            id: None,
        },
        subject_type: GtsTypeId::new("gts.cf.core.lic.subj.v1~acme.app.user.v1~"),
        fixed_subject: None,
        window: WindowInput {
            not_before: datetime!(2026-01-01 00:00 UTC),
            expires_at: datetime!(2027-01-01 00:00 UTC),
            grace_until: None,
        },
        conditions: None,
        permissions: PermissionsInput {
            may_bind: true,
            max_depth: 0,
            requires_acceptance: false,
        },
        recipient_rules: None,
    }
}

fn issue(grants: Vec<GrantTerms>, token: &str) -> (AdmissionInput, Uuid, Uuid) {
    let caller_tenant = Uuid::now_v7();
    let holder = Uuid::now_v7();
    let input = AdmissionInput::Issue {
        holder_tenant_id: holder,
        grants,
        as_pack: true,
        caller: CallerIdentity {
            subject_id: Uuid::now_v7(),
            subject_type: Some("user".to_owned()),
            tenant_id: caller_tenant,
        },
        idempotency_token: token.to_owned(),
    };
    (input, caller_tenant, holder)
}

#[test]
fn links_as_the_only_plugin() {
    let plugin = linked_admission_plugin().unwrap();
    assert_eq!(plugin.info().id, PLUGIN_ID);
    assert_eq!(plugin.intakes(), Intakes::new(true, false));
}

#[tokio::test]
async fn admit_names_each_root_in_the_evidence() {
    let (input, caller_tenant, holder) = issue(vec![terms(), terms()], "t-1");
    let admission = IssueAdmissionPlugin
        .admit(input, OffsetDateTime::now_utc(), &NoMaterial)
        .await
        .unwrap();

    assert!(admission.as_pack);
    assert!(admission.material.is_empty());
    assert_eq!(admission.roots.len(), 2);
    assert_ne!(admission.roots[0].id, admission.roots[1].id);
    assert!(
        admission
            .roots
            .iter()
            .all(|r| r.fields.holder_tenant_id == holder)
    );

    let evidence = admission.evidence.unwrap();
    assert_eq!(evidence.replay_key, Some(format!("{caller_tenant}:t-1")));
    assert_eq!(evidence.replaces, None);
    let body: serde_json::Value = serde_json::from_slice(&evidence.bytes).unwrap();
    let ids: Vec<String> = body["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect();
    let expected: Vec<String> = admission.roots.iter().map(|r| r.id.to_string()).collect();
    assert_eq!(ids, expected);
    assert_eq!(body["holder_tenant_id"], holder.to_string());
}

#[tokio::test]
async fn admit_rejects_import() {
    let err = IssueAdmissionPlugin
        .admit(
            AdmissionInput::Import { bytes: vec![1] },
            OffsetDateTime::now_utc(),
            &NoMaterial,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        AdmissionError::Rejected(RejectionReason::Malformed(_))
    ));
}

#[tokio::test]
async fn verify_trusts_the_store() {
    let evidence = StoredEvidence {
        id: EvidenceId(Uuid::nil()),
        plugin_id: PLUGIN_ID.to_owned(),
        intake: Intake::Issue,
        bytes: Vec::new(),
    };
    let v = IssueAdmissionPlugin
        .verify(
            GrantId(Uuid::nil()),
            &evidence,
            OffsetDateTime::now_utc(),
            &NoMaterial,
        )
        .await
        .unwrap();
    assert_eq!(v.outcome, VerifyOutcome::Verified(None));
    assert!(v.material.is_empty());
}
