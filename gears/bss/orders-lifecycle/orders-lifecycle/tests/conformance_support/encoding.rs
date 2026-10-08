//! Test-only reference encoder. Runtime audit/idempotency/cursors are later packages.
use anyhow::{Context, bail, ensure};
use serde_json::Value;
use uuid::Uuid;

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                char::from(DIGITS[usize::from(b >> 4)]),
                char::from(DIGITS[usize::from(b & 15)]),
            ]
        })
        .collect()
}

pub fn unhex(value: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(value.len().is_multiple_of(2), "odd hex length");
    ensure!(value.is_ascii(), "non-ASCII hex");
    (0..value.len())
        .step_by(2)
        .map(|i| Ok(u8::from_str_radix(&value[i..i + 2], 16)?))
        .collect()
}

pub fn frame(out: &mut Vec<u8>, bytes: Option<&[u8]>) -> anyhow::Result<()> {
    if let Some(bytes) = bytes {
        let len = u32::try_from(bytes.len())?;
        out.push(1);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    } else {
        out.push(0);
    }
    Ok(())
}

fn tagged(value: &str) -> Vec<u8> {
    let mut out = value.as_bytes().to_vec();
    out.push(0x1f);
    out
}

// Independently transcribed from DESIGN §4.4; never loaded from the authoring script.
const ROW: &[(&str, &str)] = &[
    ("hash_version", "u16"),
    ("audit_id", "uuid"),
    ("audit_tenant_id", "uuid"),
    ("subject_tenant_id", "uuid"),
    ("resource_tenant_id", "uuid"),
    ("order_id", "uuid"),
    ("requested_order_ref", "uuid"),
    ("sequence", "positive"),
    ("from_state", "text"),
    ("to_state", "text"),
    ("trigger", "text"),
    ("outcome", "text"),
    ("actor", "text"),
    ("actor_class", "text"),
    ("delegation_proof_ref", "text"),
    ("reason", "text"),
    ("changed_field", "text"),
    ("prior_value", "text"),
    ("new_value", "text"),
    ("idempotency_key", "text"),
    ("correlation_id", "uuid"),
    ("version", "positive"),
    ("created_at", "i64"),
    ("prev_hash", "hash"),
];

fn fields(out: &mut Vec<u8>, row: &Value, spec: &[(&str, &str)]) -> anyhow::Result<()> {
    for (name, kind) in spec {
        let value = row.get(name).with_context(|| format!("missing {name}"))?;
        if value.is_null() {
            frame(out, None)?;
            continue;
        }
        let bytes = match *kind {
            "u16" => u16::try_from(value.as_u64().context("u16")?)?
                .to_be_bytes()
                .to_vec(),
            "uuid" => Uuid::parse_str(value.as_str().context("uuid")?)?
                .as_bytes()
                .to_vec(),
            "positive" | "count" => {
                let n: i64 = value.as_str().context("integer string")?.parse()?;
                ensure!(n >= i64::from(*kind == "positive"), "integer range");
                if *name == "version" {
                    ensure!(i32::try_from(n).is_ok(), "order version range");
                }
                u64::try_from(n)?.to_be_bytes().to_vec()
            }
            "i64" => value
                .as_str()
                .context("micros")?
                .parse::<i64>()?
                .to_be_bytes()
                .to_vec(),
            "hash" => {
                let bytes = unhex(value.as_str().context("hash")?)?;
                ensure!(bytes.len() == 32, "hash length");
                bytes
            }
            "text" => value.as_str().context("text")?.as_bytes().to_vec(),
            _ => bail!("unsupported scalar"),
        };
        frame(out, Some(&bytes))?;
    }
    Ok(())
}

pub fn audit(row: &Value) -> anyhow::Result<Vec<u8>> {
    let version = row["hash_version"].as_u64().context("hash_version")?;
    ensure!(matches!(version, 1..=3), "unsupported hash version");
    ensure!(
        row.as_object().context("row")?.len() == ROW.len() + if version == 3 { 2 } else { 1 }
            && row.get("caller_reason").is_some()
            && ROW.iter().all(|(key, _)| row.get(key).is_some()),
        "closed row shape"
    );
    ensure!(
        version != 1 || row["caller_reason"].is_null(),
        "v1 cannot cover caller_reason"
    );
    for key in [
        "audit_id",
        "subject_tenant_id",
        "trigger",
        "outcome",
        "actor",
        "actor_class",
        "reason",
        "idempotency_key",
        "created_at",
    ] {
        ensure!(!row[key].is_null(), "required {key}");
    }
    if row["outcome"] == "committed" {
        for key in [
            "sequence",
            "prev_hash",
            "audit_tenant_id",
            "resource_tenant_id",
            "order_id",
            "version",
        ] {
            ensure!(!row[key].is_null(), "committed {key}");
        }
    } else {
        ensure!(row["outcome"] == "refused", "outcome");
        ensure!(
            row["sequence"].is_null()
                && row["prev_hash"].is_null()
                && row["caller_reason"].is_null(),
            "refusal shape"
        );
    }
    let mut out = tagged(&format!("VHP-BSS-ORDERS-AUDIT-ROW-v{version}"));
    fields(&mut out, row, ROW)?;
    if version >= 2 {
        fields(&mut out, row, &[("caller_reason", "text")])?;
    }
    if version == 3 {
        let observation = row
            .get("force_request_observation")
            .context("missing observation")?;
        if observation.is_null() {
            ensure!(
                row["outcome"] != "refused"
                    || row["trigger"] != "force-fail-unreconciled"
                    || row["reason"] != "second-approver-required",
                "new force request requires observation"
            );
            frame(&mut out, None)?;
        } else {
            ensure!(
                row["outcome"] == "refused"
                    && row["trigger"] == "force-fail-unreconciled"
                    && row["reason"] == "second-approver-required"
                    && !row["order_id"].is_null(),
                "observation belongs only to resolved force request"
            );
            ensure!(
                observation.as_object().context("observation")?.len() == 3
                    && observation["version"] == row["version"]
                    && observation["state"] == row["from_state"],
                "observation shape/linkage"
            );
            ensure!(
                matches!(
                    observation["state"].as_str(),
                    Some(
                        "draft"
                            | "submitted"
                            | "pending_approval"
                            | "approved"
                            | "in_fulfillment"
                            | "on_hold"
                            | "completed"
                            | "rejected"
                            | "cancelled"
                            | "fulfillment_failed"
                            | "expired"
                    )
                ),
                "state"
            );
            for key in ["audit_sequence", "state", "version"] {
                ensure!(!observation[key].is_null(), "required observation {key}");
            }
            let mut bytes = Vec::new();
            fields(
                &mut bytes,
                observation,
                &[
                    ("audit_sequence", "positive"),
                    ("state", "text"),
                    ("version", "positive"),
                ],
            )?;
            frame(&mut out, Some(&bytes))?;
        }
    }
    Ok(out)
}

pub fn checkpoint(row: &Value) -> anyhow::Result<Vec<u8>> {
    ensure!(row["format_version"] == 1, "unsupported checkpoint version");
    for key in [
        "audit_tenant_id",
        "checkpoint_sequence",
        "captured_at",
        "prev_checkpoint_hash",
    ] {
        ensure!(!row[key].is_null(), "required checkpoint {key}");
    }
    let members = row["members"].as_array().context("members")?;
    let count = row["member_count"]
        .as_str()
        .context("count")?
        .parse::<usize>()?;
    ensure!(count == members.len(), "member count");
    let mut prior = None;
    for member in members {
        ensure!(
            !member["audit_sequence"].is_null() && !member["entry_hash"].is_null(),
            "required member evidence"
        );
        let id = Uuid::parse_str(member["order_id"].as_str().context("member order")?)?;
        ensure!(
            prior.is_none_or(|previous| previous < id),
            "unsorted or duplicate member"
        );
        prior = Some(id);
    }
    let mut out = tagged("VHP-BSS-ORDERS-AUDIT-ROLLUP-v1");
    fields(
        &mut out,
        row,
        &[
            ("format_version", "u16"),
            ("audit_tenant_id", "uuid"),
            ("checkpoint_sequence", "positive"),
            ("captured_at", "i64"),
            ("member_count", "count"),
            ("prev_checkpoint_hash", "hash"),
        ],
    )?;
    for member in members {
        fields(
            &mut out,
            member,
            &[
                ("order_id", "uuid"),
                ("audit_sequence", "positive"),
                ("entry_hash", "hash"),
            ],
        )?;
    }
    Ok(out)
}

/// Restricted canonical JSON for the proposed fixture profile, not arbitrary RFC 8785 numbers.
fn canonical(value: &Value) -> anyhow::Result<String> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(serde_json::to_string(value)?),
        Value::Number(_) => bail!("fixture profile requires exact numeric strings"),
        Value::Array(values) => Ok(format!(
            "[{}]",
            values
                .iter()
                .map(canonical)
                .collect::<anyhow::Result<Vec<_>>>()?
                .join(",")
        )),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            let entries = entries
                .into_iter()
                .map(|(k, v)| Ok(format!("{}:{}", serde_json::to_string(k)?, canonical(v)?)))
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(format!("{{{}}}", entries.join(",")))
        }
    }
}

pub fn encode(kind: &str, input: &Value) -> anyhow::Result<Vec<u8>> {
    match kind {
        "audit" => audit(input),
        "checkpoint" => checkpoint(input),
        "genesis" | "checkpoint_genesis" => {
            let mut out = tagged(if kind == "genesis" {
                "VHP-BSS-ORDERS-AUDIT-GENESIS-v1"
            } else {
                "VHP-BSS-ORDERS-AUDIT-ROLLUP-GENESIS-v1"
            });
            fields(&mut out, input, &[("audit_tenant_id", "uuid")])?;
            if kind == "genesis" {
                fields(&mut out, input, &[("order_id", "uuid")])?;
            }
            Ok(out)
        }
        "request" | "cursor" => {
            let mut out = tagged(if kind == "request" {
                "VHP-BSS-ORDERS-REQUEST-FIXTURE-v1"
            } else {
                "VHP-BSS-ORDERS-CURSOR-FIXTURE-v1"
            });
            out.extend(canonical(input)?.as_bytes());
            Ok(out)
        }
        "framing" => {
            let mut out = Vec::new();
            for field in input.as_array().context("fields")? {
                frame(&mut out, field.as_str().map(str::as_bytes))?;
            }
            Ok(out)
        }
        _ => bail!("unknown vector kind"),
    }
}
