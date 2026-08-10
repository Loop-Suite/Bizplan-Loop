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
        // `> 0.0` alone lets `weight = inf` through (TOML supports inf/-inf/nan float
        // literals, and `f64::INFINITY > 0.0` is true). An infinite weight makes
        // `weight_sum()` infinite, so every criterion's `weight / wsum` becomes `inf/inf`
        // = NaN, silently turning every score computed against this spec into NaN instead
        // of failing fast here with a clear error.
        anyhow::ensure!(
            spec.criteria
                .iter()
                .all(|c| c.weight > 0.0 && c.weight.is_finite()),
            "all criteria weights must be greater than 0 and finite"
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
                    line.push_str(&format!(
                        "\n- Recommended length: approx. {} chars",
                        s.chars
                    ));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write_spec(toml: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "bizplan_spec_test_{}_{:?}_{}.toml",
            std::process::id(),
            std::thread::current().id(),
            toml.len(), // cheap uniqueness across the multiple specs a single test writes
        ));
        std::fs::write(&path, toml).unwrap();
        path
    }

    /// TOML supports `inf`/`-inf`/`nan` float literals, and `f64::INFINITY > 0.0` is `true`,
    /// so the old `c.weight > 0.0` check alone let `weight = inf` through. That silently
    /// turns every score computed against this spec into NaN (inf / inf in the weighted-sum
    /// division) instead of failing fast here with a clear error at load time.
    #[test]
    fn load_rejects_an_infinite_weight() {
        let path = write_spec(
            r#"
            name = "probe"
            [[sections]]
            id = "s"
            title = "S"
            [[criteria]]
            id = "c1"
            name = "C1"
            weight = inf
            "#,
        );
        let err = Spec::load(&path).expect_err("an infinite weight must be rejected");
        let _ = std::fs::remove_file(&path);
        assert!(
            format!("{err:#}").contains("finite"),
            "unexpected error: {err:#}"
        );
    }

    /// A negative-infinite weight is already excluded by `> 0.0`, but confirm it stays
    /// rejected (not, say, accidentally let through by only checking `is_finite()`).
    #[test]
    fn load_rejects_a_negative_infinite_weight() {
        let path = write_spec(
            r#"
            name = "probe"
            [[sections]]
            id = "s"
            title = "S"
            [[criteria]]
            id = "c1"
            name = "C1"
            weight = -inf
            "#,
        );
        let err = Spec::load(&path).expect_err("a negative-infinite weight must be rejected");
        let _ = std::fs::remove_file(&path);
        assert!(
            format!("{err:#}").contains("greater than 0"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn load_accepts_a_normal_finite_weight() {
        let path = write_spec(
            r#"
            name = "probe"
            [[sections]]
            id = "s"
            title = "S"
            [[criteria]]
            id = "c1"
            name = "C1"
            weight = 1.5
            "#,
        );
        let spec = Spec::load(&path).expect("a normal finite weight must be accepted");
        let _ = std::fs::remove_file(&path);
        assert_eq!(spec.criteria[0].weight, 1.5);
    }

    #[test]
    fn load_rejects_an_empty_file_with_a_clean_error_not_a_panic() {
        let path = write_spec("");
        let err = Spec::load(&path).expect_err("an empty spec file must not parse");
        let _ = std::fs::remove_file(&path);
        // `name` has no #[serde(default)], so TOML deserialization itself fails first.
        assert!(!format!("{err:#}").is_empty());
    }

    /// Syntactically broken TOML (unterminated string, unbalanced table headers, garbage
    /// bytes) must surface as a clean `Err` via the existing `with_context` wrapping, not a
    /// panic, regardless of how mangled the input is.
    #[test]
    fn load_rejects_syntactically_invalid_toml_without_panicking() {
        for bad in [
            "name = \"unterminated",
            "[[sections]\nid = \"s\"",
            "= = = not toml at all = = =",
            "\0\u{1}binary\u{2}garbage\0",
        ] {
            let path = write_spec(bad);
            let err = Spec::load(&path);
            let _ = std::fs::remove_file(&path);
            assert!(err.is_err(), "expected an error for input: {bad:?}");
        }
    }

    /// A weight given as the wrong TOML type (a string instead of a float) must fail
    /// deserialization cleanly rather than silently coercing or panicking.
    #[test]
    fn load_rejects_a_weight_given_as_the_wrong_type() {
        let path = write_spec(
            r#"
            name = "probe"
            [[sections]]
            id = "s"
            title = "S"
            [[criteria]]
            id = "c1"
            name = "C1"
            weight = "high"
            "#,
        );
        let err = Spec::load(&path);
        let _ = std::fs::remove_file(&path);
        assert!(err.is_err());
    }
}
