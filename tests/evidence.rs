use opscodex::{
    evidence::{
        Claim, ClaimKind, Confidence, Diagnosis, EvidenceMeta, parse_diagnosis, redact_json,
        redact_text, sha256_hex, validate_diagnosis,
    },
    runtime::EvidenceId,
};
use serde_json::json;

#[test]
fn redaction_masks_secrets_and_sensitive_keys() {
    let (text, changed) =
        redact_text("Bearer supersecretvalue password=hunter2 sk-abcdefghijklmnopqrstuvwxyz");
    assert!(changed);
    assert!(!text.contains("supersecretvalue"));
    assert!(!text.contains("hunter2"));
    assert!(!text.contains("sk-abcdefghijklmnopqrstuvwxyz"));
    assert!(text.contains("[REDACTED]"));

    let (json, changed) = redact_json(&json!({
        "token": "abc",
        "query": "up",
        "nested": {"api_key": "xyz"}
    }));
    assert!(changed);
    assert_eq!(json["token"], "[REDACTED]");
    assert_eq!(json["nested"]["api_key"], "[REDACTED]");
    assert_eq!(json["query"], "up");
}

#[test]
fn evidence_hash_is_stable_for_the_same_bytes() {
    assert_eq!(sha256_hex(b"abc"), sha256_hex(b"abc"));
    assert_ne!(sha256_hex(b"abc"), sha256_hex(b"abd"));
}

#[test]
fn structured_diagnosis_is_parsed_from_fenced_json() {
    let content = r#"Here is the result:
```json
{
  "summary": "Pool exhaustion",
  "claims": [
    {
      "kind": "observed",
      "statement": "DB pool is empty",
      "evidence_ids": [],
      "confidence": "high"
    }
  ],
  "recommended_actions": ["restart"],
  "limitations": []
}
```
"#;
    let diagnosis = parse_diagnosis(content);
    assert_eq!(diagnosis.summary, "Pool exhaustion");
    assert_eq!(diagnosis.claims[0].kind, ClaimKind::Observed);
    assert_eq!(diagnosis.claims[0].confidence, Confidence::High);
}

#[test]
fn unstructured_output_is_unverified() {
    let diagnosis = parse_diagnosis("The service looks unhealthy.");
    assert!(diagnosis.claims.is_empty());
    assert!(
        diagnosis
            .limitations
            .iter()
            .any(|item| item.contains("unverified"))
    );
}

#[test]
fn citation_validator_rejects_missing_and_unknown_evidence() {
    let evidence_id = EvidenceId::new();
    let mut evidence = EvidenceMeta::new("prometheus");
    evidence.evidence_id = Some(evidence_id.clone());
    let diagnosis = Diagnosis {
        summary: "bad".into(),
        claims: vec![
            Claim {
                claim_id: Default::default(),
                kind: ClaimKind::Observed,
                statement: "no ids".into(),
                evidence_ids: Vec::new(),
                confidence: Confidence::Low,
            },
            Claim {
                claim_id: Default::default(),
                kind: ClaimKind::Inferred,
                statement: "unknown id".into(),
                evidence_ids: vec![EvidenceId::new()],
                confidence: Confidence::Medium,
            },
            Claim {
                claim_id: Default::default(),
                kind: ClaimKind::Observed,
                statement: "valid".into(),
                evidence_ids: vec![evidence_id],
                confidence: Confidence::High,
            },
        ],
        recommended_actions: Vec::new(),
        limitations: Vec::new(),
    };
    let errors = validate_diagnosis(&diagnosis, &[evidence]);
    assert_eq!(errors.len(), 2);
}

#[test]
fn unsourced_claims_are_stripped_when_evidence_is_insufficient() {
    let diagnosis = Diagnosis {
        summary: "maybe".into(),
        claims: vec![
            Claim {
                claim_id: Default::default(),
                kind: ClaimKind::Observed,
                statement: "unsourced".into(),
                evidence_ids: Vec::new(),
                confidence: Confidence::High,
            },
            Claim {
                claim_id: Default::default(),
                kind: ClaimKind::Recommended,
                statement: "collect more signals".into(),
                evidence_ids: Vec::new(),
                confidence: Confidence::Low,
            },
        ],
        recommended_actions: Vec::new(),
        limitations: Vec::new(),
    };
    let limited = opscodex::evidence::apply_citation_limitations(diagnosis, &[]);
    assert!(
        limited
            .claims
            .iter()
            .all(|claim| claim.kind == ClaimKind::Recommended)
    );
    assert!(
        limited
            .limitations
            .iter()
            .any(|item| item.contains("Abstained") || item.contains("insufficient"))
    );
}
