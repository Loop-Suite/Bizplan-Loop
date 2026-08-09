use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Spec {
    /// Format name
    pub name: String,
    /// Review body/context. Inserted verbatim into the prompt.
    #[serde(default)]
    pub context: String,
    /// Note on scoring source (grounded in the original announcement text). Shown in the report.
    #[serde(default)]
    pub scoring_source: String,
    /// Overall document length guide (character count). 0 means unspecified.
    #[serde(default)]
    pub total_chars: usize,
    /// Minimum number of citations (「source name」). 0 means no check.
    #[serde(default)]
    pub min_citations: usize,
    /// Whether at least one table is required.
    #[serde(default)]
    pub require_table: bool,
    /// Approach angles for generation diversity.
    #[serde(default)]
    pub angles: Vec<String>,
    /// Score-band descriptors (0-100). Uses defaults if unspecified.
    #[serde(default)]
    pub bands: Vec<String>,
    pub sections: Vec<Section>,
    pub criteria: Vec<Criterion>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub guide: String,
    #[serde(default)]
    pub chars: usize,
    #[serde(default = "default_true")]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Criterion {
    pub id: String,
    pub name: String,
    /// Weight. Normalized internally even if the sum isn't 1.
    pub weight: f64,
    #[serde(default)]
    pub guide: String,
}

fn default_true() -> bool {
    true
}

pub const DEFAULT_BANDS: &[&str] = &[
    "90-100: Actual award-winning level. Every claim is backed by verifiable evidence, exceeding the evaluation criteria.",
    "75-89: Finalist level. Core evidence is present but some claims are unverified.",
    "60-74: Borderline for passing document screening. Has structure, but evidence is shallow and mixed with generalities.",
    "40-59: Rejection range. Mostly abstract statements, lacking figures and examples.",
    "0-39: Fails to meet requirements. Irrelevant to the evaluation criteria or content is empty.",
];

impl Spec {
    pub fn load(path: &Path) -> Result<Spec> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read spec file: {}", path.display()))?;
        let spec: Spec = toml::from_str(&s)
            .with_context(|| format!("Failed to parse spec TOML: {}", path.display()))?;
        anyhow::ensure!(!spec.sections.is_empty(), "sections is empty");
        anyhow::ensure!(!spec.criteria.is_empty(), "criteria is empty");
        anyhow::ensure!(
            spec.criteria.iter().all(|c| c.weight > 0.0),
            "all criteria weights must be greater than 0"
        );
        let mut ids: Vec<&str> = spec.criteria.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        anyhow::ensure!(ids.len() == n, "duplicate criteria id");
        Ok(spec)
    }

    pub fn weight_sum(&self) -> f64 {
        self.criteria.iter().map(|c| c.weight).sum()
    }

    pub fn bands_prompt(&self) -> String {
        if self.bands.is_empty() {
            DEFAULT_BANDS.join("\n")
        } else {
            self.bands.join("\n")
        }
    }

    pub fn sections_prompt(&self) -> String {
        self.sections
            .iter()
            .map(|s| {
                let mut line = format!("## {}\n- Writing guide: {}", s.title, s.guide);
                if s.chars > 0 {
                    line.push_str(&format!("\n- Recommended length: approx. {} chars", s.chars));
                }
                if s.required {
                    line.push_str("\n- Required section");
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn rubric_prompt(&self) -> String {
        let sum = self.weight_sum();
        self.criteria
            .iter()
            .map(|c| {
                format!(
                    "- id=\"{}\" | {} (weight {:.0}%) : {}",
                    c.id,
                    c.name,
                    c.weight / sum * 100.0,
                    c.guide
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
