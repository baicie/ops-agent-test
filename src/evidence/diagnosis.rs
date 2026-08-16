use serde::{Deserialize, Serialize};

use super::EvidenceMeta;
use crate::runtime::{ClaimId, EvidenceId};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Observed,
    Inferred,
    Recommended,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Claim {
    pub claim_id: ClaimId,
    pub kind: ClaimKind,
    pub statement: String,
    #[serde(default)]
    pub evidence_ids: Vec<EvidenceId>,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnosis {
    pub summary: String,
    #[serde(default)]
    pub claims: Vec<Claim>,
    #[serde(default)]
    pub recommended_actions: Vec<String>,
    #[serde(default)]
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CitationError {
    pub claim_id: ClaimId,
    pub message: String,
}

#[derive(Deserialize)]
struct LooseDiagnosis {
    summary: Option<String>,
    #[serde(default)]
    claims: Vec<LooseClaim>,
    #[serde(default)]
    recommended_actions: Vec<String>,
    #[serde(default)]
    limitations: Vec<String>,
}

#[derive(Deserialize)]
struct LooseClaim {
    #[serde(default)]
    claim_id: Option<ClaimId>,
    kind: ClaimKind,
    statement: String,
    #[serde(default)]
    evidence_ids: Vec<EvidenceId>,
    #[serde(default)]
    confidence: Option<LooseConfidence>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LooseConfidence {
    Level(Confidence),
    Number(f64),
    Text(String),
}

impl From<LooseConfidence> for Confidence {
    fn from(value: LooseConfidence) -> Self {
        match value {
            LooseConfidence::Level(level) => level,
            LooseConfidence::Number(value) if value >= 0.75 => Confidence::High,
            LooseConfidence::Number(value) if value >= 0.4 => Confidence::Medium,
            LooseConfidence::Number(_) => Confidence::Low,
            LooseConfidence::Text(text) => match text.to_ascii_lowercase().as_str() {
                "high" => Confidence::High,
                "medium" | "med" => Confidence::Medium,
                _ => Confidence::Low,
            },
        }
    }
}

pub fn parse_diagnosis(content: &str) -> Diagnosis {
    if let Some(parsed) = parse_json_diagnosis(content) {
        return parsed;
    }
    Diagnosis {
        summary: content.chars().take(280).collect(),
        claims: Vec::new(),
        recommended_actions: Vec::new(),
        limitations: vec![
            "Model output was not a structured diagnosis; content is unverified.".into(),
        ],
    }
}

fn parse_json_diagnosis(content: &str) -> Option<Diagnosis> {
    let candidate = extract_json_object(content)?;
    let loose: LooseDiagnosis = serde_json::from_str(&candidate).ok()?;
    Some(Diagnosis {
        summary: loose
            .summary
            .unwrap_or_else(|| content.chars().take(280).collect()),
        claims: loose
            .claims
            .into_iter()
            .map(|claim| Claim {
                claim_id: claim.claim_id.unwrap_or_default(),
                kind: claim.kind,
                statement: claim.statement,
                evidence_ids: claim.evidence_ids,
                confidence: claim
                    .confidence
                    .map(Confidence::from)
                    .unwrap_or(Confidence::Low),
            })
            .collect(),
        recommended_actions: loose.recommended_actions,
        limitations: loose.limitations,
    })
}

fn extract_json_object(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') {
        return Some(trimmed.to_owned());
    }
    let fence = trimmed.find("```json").or_else(|| trimmed.find("```"))?;
    let after = &trimmed[fence..];
    let start = after.find('{')?;
    let end = after.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(after[start..=end].to_owned())
}

pub fn validate_diagnosis(diagnosis: &Diagnosis, evidence: &[EvidenceMeta]) -> Vec<CitationError> {
    let known: Vec<_> = evidence
        .iter()
        .filter_map(|item| item.evidence_id.clone())
        .collect();
    let mut errors = Vec::new();
    for claim in &diagnosis.claims {
        match claim.kind {
            ClaimKind::Recommended => {}
            ClaimKind::Observed | ClaimKind::Inferred => {
                if claim.evidence_ids.is_empty() {
                    errors.push(CitationError {
                        claim_id: claim.claim_id.clone(),
                        message: format!("{:?} claim has no evidence ids", claim.kind),
                    });
                    continue;
                }
                for evidence_id in &claim.evidence_ids {
                    if !known.iter().any(|known_id| known_id == evidence_id) {
                        errors.push(CitationError {
                            claim_id: claim.claim_id.clone(),
                            message: format!("unknown evidence id {evidence_id}"),
                        });
                    }
                }
            }
        }
    }
    errors
}

pub fn apply_citation_limitations(
    mut diagnosis: Diagnosis,
    evidence: &[EvidenceMeta],
) -> Diagnosis {
    let errors = validate_diagnosis(&diagnosis, evidence);
    if errors.is_empty() {
        return diagnosis;
    }
    let known: Vec<_> = evidence
        .iter()
        .filter_map(|item| item.evidence_id.clone())
        .collect();
    diagnosis.claims.retain(|claim| match claim.kind {
        ClaimKind::Recommended => true,
        ClaimKind::Observed | ClaimKind::Inferred => {
            !claim.evidence_ids.is_empty()
                && claim
                    .evidence_ids
                    .iter()
                    .all(|evidence_id| known.iter().any(|known_id| known_id == evidence_id))
        }
    });
    diagnosis.limitations.push(format!(
        "Evidence is insufficient; abstained from {} unsourced claim(s).",
        errors.len()
    ));
    diagnosis
}
