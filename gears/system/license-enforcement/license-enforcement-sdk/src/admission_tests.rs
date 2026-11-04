use uuid::Uuid;

use super::{AdmissionInput, Expect, Intake, MaterialWrite, NewEvidence, StoredEvidence};
use crate::dto::ImportEvidenceRequest;
use crate::ids::EvidenceId;

#[test]
fn debug_does_not_print_evidence_or_material_bytes() {
    let bytes = b"SIGNED-PAYLOAD".to_vec();
    let printed = [
        format!("{:?}", NewEvidence::new(bytes.clone())),
        format!(
            "{:?}",
            MaterialWrite::put("k", bytes.clone()).expecting(Expect::Equals(bytes.clone()))
        ),
        format!(
            "{:?}",
            StoredEvidence {
                id: EvidenceId(Uuid::nil()),
                plugin_id: "p".to_owned(),
                intake: Intake::Import,
                bytes: bytes.clone(),
            }
        ),
        format!("{:?}", AdmissionInput::Import { bytes }),
        format!(
            "{:?}",
            ImportEvidenceRequest {
                bytes: "U0lHTkVELVBBWUxPQUQ=".to_owned(),
                idempotency_token: "t".to_owned(),
            }
        ),
    ];
    for p in printed {
        assert!(!p.contains("83, 73") && !p.contains("U0lHTkVE"), "{p}");
        assert!(p.contains("bytes"), "{p}");
    }
}
