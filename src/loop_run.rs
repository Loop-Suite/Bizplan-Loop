use crate::generate;
use crate::llm::Llm;
use crate::report;
use crate::score::{self, Scored};
use crate::spec::Spec;
use anyhow::Result;
use std::path::Path;

pub struct LoopOutcome {
    pub best_label: String,
    pub best_doc: String,
    pub best_score: Scored,
    pub first_doc: String,
    pub history: Vec<Scored>,
    pub stop_reason: String,
    /// Length-inflation warnings (length growth relative to score)
    pub warnings: Vec<String>,
}

pub struct LoopCfg {
    pub target: f64,
    pub max_iter: usize,
    pub rounds: usize,
    /// Considered stalled if improvement over the previous best is less than this value.
    pub min_delta: f64,
    /// Stop early if stalled for this many consecutive rounds.
    pub patience: usize,
}

/// Generate → score → regenerate incorporating feedback loop.
/// The return value is not the last round but the best-scoring round (argmax) across all rounds.
pub fn run(
    gen_llm: &Llm,
    judges: &[Llm],
    spec: &Spec,
    idea: &str,
    out_dir: &Path,
    cfg: &LoopCfg,
    angle: &str,
) -> Result<LoopOutcome> {
    let mut doc = generate::generate(gen_llm, spec, idea, angle)?;
    let mut history: Vec<Scored> = Vec::new();
    let mut docs: Vec<String> = Vec::new();
    let mut best_i = 0usize;
    let mut stall = 0usize;
    let mut stop_reason = format!("Reached max iterations ({})", cfg.max_iter.max(1));

    for i in 0..cfg.max_iter.max(1) {
        let label = format!("iter{:02}", i + 1);
        std::fs::write(out_dir.join(format!("{}.md", label)), &doc)?;

        let s = score::score_doc(judges, spec, &label, &doc, cfg.rounds)?;
        report::append_jsonl(out_dir, &s)?;
        println!(
            "  [{}] {:.1}/100  ({} chars{})",
            label,
            s.total,
            s.metrics.chars,
            if s.format_issues.is_empty() {
                String::new()
            } else {
                format!(", {} format issues", s.format_issues.len())
            }
        );

        let prev_best = history.get(best_i).map(|b: &Scored| b.total);
        let improved = match prev_best {
            None => true,
            Some(b) => s.total > b,
        };
        history.push(s.clone());
        docs.push(doc.clone());
        if improved {
            let gain = s.total - prev_best.unwrap_or(f64::NEG_INFINITY);
            best_i = history.len() - 1;
            if prev_best.is_some() && gain < cfg.min_delta {
                stall += 1;
            } else {
                stall = 0;
            }
        } else {
            stall += 1;
        }

        if s.total >= cfg.target && s.format_issues.is_empty() {
            stop_reason = format!("Reached target ({:.0} points)", cfg.target);
            break;
        }
        // Skip on the first iteration: there is no prior baseline to stall against yet,
        // so it can never be a real stall (matters only for the edge case --patience 0).
        if i > 0 && stall >= cfg.patience {
            stop_reason = format!(
                "Improvement stalled ({} consecutive rounds under +{:.1} points)",
                cfg.patience, cfg.min_delta
            );
            break;
        }
        if i + 1 == cfg.max_iter.max(1) {
            break;
        }

        let fb = score::feedback_text(&history[history.len() - 1]);
        let weak = score::weak_points(spec, &history[history.len() - 1]);
        doc = generate::revise(gen_llm, spec, idea, &doc, &fb, &weak)?;
    }

    let best_score = history[best_i].clone();
    let best_doc = docs[best_i].clone();
    std::fs::write(out_dir.join("best.md"), &best_doc)?;

    // Length-inflation canary: if length grows excessively relative to score, suspect verbosity gaming.
    let mut warnings = Vec::new();
    let first = &history[0];
    let d_score = best_score.total - first.total;
    let d_chars = best_score.metrics.chars as f64 - first.metrics.chars as f64;
    let growth = if first.metrics.chars > 0 {
        d_chars / first.metrics.chars as f64
    } else {
        0.0
    };
    if growth > 0.25 && d_score < 5.0 {
        warnings.push(format!(
            "Length canary: length +{:.0}% but score only +{:.1} points → may be padding rather than substantive improvement",
            growth * 100.0,
            d_score
        ));
    }
    if best_i + 1 < history.len() {
        warnings.push(format!(
            "Last round ({:.1} points) is not the best score → best.md is iter{:02}",
            history.last().map(|h| h.total).unwrap_or(0.0),
            best_i + 1
        ));
    }

    Ok(LoopOutcome {
        best_label: best_score.label.clone(),
        best_doc,
        first_doc: docs[0].clone(),
        best_score,
        history,
        stop_reason,
        warnings,
    })
}
