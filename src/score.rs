use crate::checks::{self, Metrics};
use crate::llm::Llm;
use crate::spec::Spec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub const JUDGE_SYSTEM: &str = "You are a judge for a government/public-institution contest. \
The document's author is unknown, and you do not guess at authorship. \
Unsupported claims, unverifiable figures, and abstract rhetorical flourishes are grounds for deduction. \
You do not grade generously, and every score must be backed by a direct quote from the document.";

/// Review lenses. Rotated each round.
/// (Because repeated calls to the same model have correlated errors, separating lenses alone does not
///  produce independent samples. Real independence comes from a panel of different models.)
pub const LENSES: &[&str] = &[
    "Weighs overall completeness and alignment with the evaluation criteria in a balanced way.",
    "Is especially strict about the verifiability of figures, sources, and evidence.",
    "Is especially strict about feasibility and the specificity of the execution plan.",
    "Evaluates persuasiveness and readability as experienced in the judge's first 3 minutes of reading.",
    "Looks at differentiation from competing entries and overlap with existing projects.",
    "Looks at rule compliance (format, length, meeting required items) and risk narrative.",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore {
    pub id: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub why_not_higher: String,
    pub score: f64, // 0-100
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeResult {
    #[serde(default)]
    pub winning_conditions: Vec<String>,
    #[serde(default)]
    pub criteria: Vec<CriterionScore>,
    #[serde(default)]
    pub improvements: Vec<String>,
    #[serde(default)]
    pub comment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Scored {
    pub label: String,
    /// 0-100 weighted total
    pub total: f64,
    /// Aggregated score per criterion (0-100, trimmed mean)
    pub per_criterion: BTreeMap<String, f64>,
    /// All raw scores per criterion (per judge)
    pub raw: BTreeMap<String, Vec<f64>>,
    /// Max-min spread per criterion (indicator of judgment instability)
    pub spread: BTreeMap<String, f64>,
    pub missing_sections: Vec<String>,
    /// Deterministic format check results
    pub format_issues: Vec<String>,
    pub metrics: Metrics,
    pub improvements: Vec<String>,
    pub comments: Vec<String>,
    pub rounds: usize,
    pub models: Vec<String>,
}

fn judge_schema(spec: &Spec) -> serde_json::Value {
    let ids: Vec<String> = spec.criteria.iter().map(|c| c.id.clone()).collect();
    // Field order = generation order. Having the model write out the criteria (winning_conditions) first,
    // before scoring, reduces anchoring on the document (de-anchoring).
    json!({
        "type": "object",
        "properties": {
            "winning_conditions": {
                "type": "array",
                "minItems": 3,
                "items": {"type": "string"},
                "description": "3-6 conditions a winning entry in this contest should have, written before reading the document"
            },
            "criteria": {
                "type": "array",
                "minItems": ids.len(),
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "enum": ids},
                        "evidence": {"type": "string", "description": "Direct quote from the document's original text (30+ characters)"},
                        "why_not_higher": {"type": "string", "description": "Why not a higher score"},
                        "score": {"type": "integer", "minimum": 0, "maximum": 100}
                    },
                    "required": ["id", "evidence", "why_not_higher", "score"],
                    "additionalProperties": false
                }
            },
            "improvements": {
                "type": "array", "minItems": 3, "maxItems": 8,
                "items": {"type": "string", "description": "An immediately actionable revision instruction"}
            },
            "comment": {"type": "string"}
        },
        "required": ["winning_conditions", "criteria", "improvements", "comment"],
        "additionalProperties": false
    })
}

fn build_judge_prompt(spec: &Spec, doc: &str, lens: &str) -> String {
    format!(
        "# Task\nScore the submitted document according to the evaluation criteria.\n\n\
         ## Format: {name}\n{ctx}\n\n\
         ## This judge's lens\n{lens}\n\n\
         ## Evaluation criteria (integer 0-100 per item)\n{rubric}\n\n\
         ## Score band guide\n{bands}\n\n\
         ## Procedure\n\
         1. Before scoring the document, first write down 3-6 'conditions a winning entry in this contest should have' in winning_conditions.\n\
         2. Then score each criterion. For each item, directly quote the document's original text in evidence, and state in why_not_higher why you did not give a higher score.\n\
         3. If you cannot find a quote to cite, that item cannot score above 60.\n\
         4. Format, length, and missing required items are handled by a separate automated check — do not factor them into scoring, evaluate content only.\n\n\
         ## Document to score\n<document>\n{doc}\n</document>\n",
        name = spec.name,
        ctx = spec.context,
        lens = lens,
        rubric = spec.rubric_prompt(),
        bands = spec.bands_prompt(),
        doc = doc
    )
}

/// Trimmed mean. If n>=4, drop one min and one max then average; otherwise simple average.
/// (With many integer 0-100 samples, the median produces excessive ties and fails to detect small improvements)
fn trimmed_mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    if v.len() < 4 {
        return v.iter().sum::<f64>() / v.len() as f64;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let inner = &s[1..s.len() - 1];
    inner.iter().sum::<f64>() / inner.len() as f64
}

/// Score a single document. Repeats `rounds` times, rotating models and lenses.
pub fn score_doc(
    judges: &[Llm],
    spec: &Spec,
    label: &str,
    doc: &str,
    rounds: usize,
) -> Result<Scored> {
    anyhow::ensure!(!judges.is_empty(), "No scoring model");
    let rounds = rounds.max(1);
    let schema = judge_schema(spec);
    let mut results: Vec<JudgeResult> = Vec::new();
    let mut models: Vec<String> = Vec::new();

    for i in 0..rounds {
        let llm = &judges[i % judges.len()];
        let lens = LENSES[i % LENSES.len()];
        let prompt = build_judge_prompt(spec, doc, lens);
        let v = llm
            .json(&prompt, Some(JUDGE_SYSTEM), &schema)
            .with_context(|| format!("Scoring failed ({label}, round {})", i + 1))?;
        let jr: JudgeResult = serde_json::from_value(v)
            .with_context(|| format!("Scoring result schema mismatch ({label})"))?;
        results.push(jr);
        models.push(llm.label());
    }

    let mut per_criterion: BTreeMap<String, f64> = BTreeMap::new();
    let mut raw: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut spread: BTreeMap<String, f64> = BTreeMap::new();
    for c in &spec.criteria {
        let vals: Vec<f64> = results
            .iter()
            .filter_map(|r| r.criteria.iter().find(|x| x.id == c.id))
            .map(|x| x.score.clamp(0.0, 100.0))
            .collect();
        let lo = vals.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        spread.insert(
            c.id.clone(),
            if vals.is_empty() { 0.0 } else { hi - lo },
        );
        per_criterion.insert(c.id.clone(), trimmed_mean(&vals));
        raw.insert(c.id.clone(), vals);
    }

    let wsum = spec.weight_sum();
    let total: f64 = spec
        .criteria
        .iter()
        .map(|c| per_criterion.get(&c.id).copied().unwrap_or(0.0) * (c.weight / wsum))
        .sum();

    let format_issues = checks::format_issues(spec, doc);
    let missing = checks::missing_sections(spec, doc);

    let mut improvements: Vec<String> = format_issues.clone();
    for r in &results {
        for imp in &r.improvements {
            let t = imp.trim().to_string();
            if !t.is_empty() && !improvements.contains(&t) {
                improvements.push(t);
            }
        }
    }

    Ok(Scored {
        label: label.to_string(),
        total: (total * 10.0).round() / 10.0,
        per_criterion,
        raw,
        spread,
        missing_sections: missing,
        format_issues,
        metrics: checks::metrics(doc),
        improvements,
        comments: results.iter().map(|r| r.comment.clone()).collect(),
        rounds,
        models,
    })
}

/// Feedback for the regeneration prompt. The score itself is not passed along (to discourage optimizing for the score).
pub fn feedback_text(s: &Scored) -> String {
    let mut out = String::from("[Revision instructions that must be incorporated]\n");
    for i in &s.improvements {
        out.push_str(&format!("- {}\n", i));
    }
    if !s.comments.is_empty() {
        out.push_str("\n[Overall Review Comments]\n");
        for c in &s.comments {
            out.push_str(&format!("- {}\n", c));
        }
    }
    out
}

/// The 2 lowest-scoring criteria.
pub fn weak_points(spec: &Spec, s: &Scored) -> String {
    let mut v: Vec<(&str, f64)> = spec
        .criteria
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                s.per_criterion.get(&c.id).copied().unwrap_or(0.0),
            )
        })
        .collect();
    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    v.iter()
        .take(2)
        .map(|(n, sc)| format!("- {} : {:.0}/100", n, sc))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimmed_mean_drops_outliers() {
        assert_eq!(trimmed_mean(&[70.0, 72.0, 74.0, 100.0]), 73.0);
        assert_eq!(trimmed_mean(&[80.0]), 80.0);
        assert!((trimmed_mean(&[70.0, 80.0]) - 75.0).abs() < 1e-9);
    }
}
