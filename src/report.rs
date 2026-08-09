use crate::score::Scored;
use crate::spec::Spec;
use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn append_jsonl(out_dir: &Path, s: &Scored) -> Result<()> {
    let path = out_dir.join("results.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("Failed to open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(s)?)?;
    Ok(())
}

fn header(spec: &Spec, rows: &[&Scored]) -> String {
    let mut md = format!("# Scoring Report — {}\n\n", spec.name);
    if !spec.scoring_source.is_empty() {
        md.push_str(&format!("> Scoring basis: {}\n\n", spec.scoring_source));
    }
    let rounds = rows.first().map(|r| r.rounds).unwrap_or(0);
    let models = rows
        .first()
        .map(|r| r.models.join(", "))
        .unwrap_or_default();
    md.push_str(&format!(
        "{} documents · {} scoring passes per document · scoring model: {}\n\n",
        rows.len(),
        rounds,
        if models.is_empty() {
            "-".into()
        } else {
            models
        }
    ));
    md
}

fn table(spec: &Spec, rows: &[&Scored]) -> String {
    let mut md = String::from("| Rank | Document | Total |");
    for c in &spec.criteria {
        md.push_str(&format!(" {} |", c.name));
    }
    md.push_str(" Length | Format Issues |\n|---|---|---|");
    for _ in &spec.criteria {
        md.push_str("---|");
    }
    md.push_str("---|---|\n");

    for (i, s) in rows.iter().enumerate() {
        md.push_str(&format!("| {} | {} | **{:.1}** |", i + 1, s.label, s.total));
        for c in &spec.criteria {
            let v = s.per_criterion.get(&c.id).copied().unwrap_or(0.0);
            let sp = s.spread.get(&c.id).copied().unwrap_or(0.0);
            if sp > 0.0 {
                md.push_str(&format!(" {:.0} (±{:.0}) |", v, sp / 2.0));
            } else {
                md.push_str(&format!(" {:.0} |", v));
            }
        }
        md.push_str(&format!(
            " {} chars | {} |\n",
            s.metrics.chars,
            if s.format_issues.is_empty() {
                "-".to_string()
            } else {
                format!("{}", s.format_issues.len())
            }
        ));
    }
    md
}

fn details(rows: &[&Scored]) -> String {
    let mut md = String::from("\n---\n\n## Per-Document Details\n");
    for s in rows {
        md.push_str(&format!("\n### {} ({:.1}/100)\n\n", s.label, s.total));
        md.push_str(&format!(
            "{} tables · {} citations · {} year mentions · {} chars\n\n",
            s.metrics.tables, s.metrics.citations, s.metrics.years, s.metrics.chars
        ));
        for c in &s.comments {
            if !c.trim().is_empty() {
                md.push_str(&format!("> {}\n\n", c));
            }
        }
        if !s.format_issues.is_empty() {
            md.push_str("Automated format checks:\n\n");
            for f in &s.format_issues {
                md.push_str(&format!("- {}\n", f));
            }
            md.push('\n');
        }
        md.push_str("Improvement instructions:\n\n");
        for imp in s
            .improvements
            .iter()
            .filter(|i| !s.format_issues.contains(i))
        {
            md.push_str(&format!("- {}\n", imp));
        }
    }
    md
}

/// Ranking report.
pub fn write_report(out_dir: &Path, spec: &Spec, scored: &[Scored]) -> Result<PathBuf> {
    let mut rows: Vec<&Scored> = scored.iter().collect();
    rows.sort_by(|a, b| {
        b.total
            .partial_cmp(&a.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut md = header(spec, &rows);
    md.push_str(&table(spec, &rows));
    md.push_str(&details(&rows));
    md.push_str(&format!(
        "\n---\n\nCumulative API cost: ${:.4}\n",
        crate::llm::total_cost_usd()
    ));

    let path = out_dir.join("report.md");
    std::fs::write(&path, md).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}

/// Loop report: per-round trend + warnings + held-out gate results.
pub fn write_loop_report(
    out_dir: &Path,
    spec: &Spec,
    history: &[Scored],
    stop_reason: &str,
    warnings: &[String],
    gate: Option<(&Scored, &Scored)>, // (first draft, best draft) held-out scoring results
) -> Result<PathBuf> {
    let mut md = format!("# Loop Report — {}\n\n", spec.name);
    if !spec.scoring_source.is_empty() {
        md.push_str(&format!("> Scoring basis: {}\n\n", spec.scoring_source));
    }
    md.push_str(&format!("Stop reason: {}\n\n", stop_reason));

    md.push_str("## Per-Round Trend\n\n| Round | Total | Δ | Length | Format Issues |\n|---|---|---|---|---|\n");
    let mut prev: Option<f64> = None;
    for h in history {
        let d = match prev {
            Some(p) => format!("{:+.1}", h.total - p),
            None => "-".to_string(),
        };
        md.push_str(&format!(
            "| {} | {:.1} | {} | {} chars | {} |\n",
            h.label,
            h.total,
            d,
            h.metrics.chars,
            h.format_issues.len()
        ));
        prev = Some(h.total);
    }

    if let Some((first, best)) = gate {
        let loop_delta = history
            .iter()
            .map(|h| h.total)
            .fold(f64::NEG_INFINITY, f64::max)
            - history[0].total;
        let gate_delta = best.total - first.total;
        md.push_str(&format!(
            "\n## Held-out Verification (scoring model not in loop: {})\n\n\
             | Target | Loop Score | Held-out Score |\n|---|---|---|\n\
             | First draft | {:.1} | {:.1} |\n| Best draft | {:.1} | {:.1} |\n\n\
             Loop-measured improvement {:+.1} points vs held-out-measured {:+.1} points\n\n",
            best.models.join(", "),
            history[0].total,
            first.total,
            history
                .iter()
                .map(|h| h.total)
                .fold(f64::NEG_INFINITY, f64::max),
            best.total,
            loop_delta,
            gate_delta
        ));
        if loop_delta > 0.0 && gate_delta < loop_delta * 0.34 {
            md.push_str(
                "> ⚠ Held-out improvement is less than 1/3 of the loop improvement → possible scorer optimization (reward hacking). \
                 Have a human verify directly whether actual document quality improved.\n\n",
            );
        }
    }

    if !warnings.is_empty() {
        md.push_str("\n## Warnings\n\n");
        for w in warnings {
            md.push_str(&format!("- {}\n", w));
        }
    }

    let rows: Vec<&Scored> = history.iter().collect();
    md.push_str(&details(&rows));
    md.push_str(&format!(
        "\n---\n\nCumulative API cost: ${:.4}\n",
        crate::llm::total_cost_usd()
    ));

    let path = out_dir.join("report.md");
    std::fs::write(&path, md).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(path)
}
