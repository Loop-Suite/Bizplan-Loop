//! Deterministic checks. Anything that would only add variance if left to the LLM is handled here.
//! (Rationale: evaluation cost hierarchy — assertion/code rules → LLM judge, cheapest and most stable in that order)

use crate::spec::Spec;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct Metrics {
    pub chars: usize,
    /// Number of markdown tables (based on separator rows)
    pub tables: usize,
    /// Number of citations in 「source name」 format
    pub citations: usize,
    /// Number of year mentions (19xx/20xx)
    pub years: usize,
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Split into (heading, body) pairs based on headings starting with `#`.
pub fn split_sections(doc: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur_head = String::new();
    let mut cur_body = String::new();
    for line in doc.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            if !cur_head.is_empty() || !cur_body.trim().is_empty() {
                out.push((cur_head.clone(), cur_body.clone()));
            }
            cur_head = t.trim_start_matches('#').trim().to_string();
            cur_body.clear();
        } else {
            cur_body.push_str(line);
            cur_body.push('\n');
        }
    }
    if !cur_head.is_empty() || !cur_body.trim().is_empty() {
        out.push((cur_head, cur_body));
    }
    out
}

pub fn metrics(doc: &str) -> Metrics {
    let tables = doc
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with('|') && t.contains("---")
        })
        .count();
    let citations = doc.matches('「').count().min(doc.matches('」').count());
    let bytes: Vec<char> = doc.chars().collect();
    let mut years = 0usize;
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        if (bytes[i] == '1' && bytes[i + 1] == '9' || bytes[i] == '2' && bytes[i + 1] == '0')
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
        {
            let prev_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
            let next_ok = i + 4 >= bytes.len() || !bytes[i + 4].is_ascii_digit();
            if prev_ok && next_ok {
                years += 1;
                i += 4;
                continue;
            }
        }
        i += 1;
    }
    Metrics {
        chars: doc.chars().count(),
        tables,
        citations,
        years,
    }
}

/// Titles of missing required sections.
pub fn missing_sections(spec: &Spec, doc: &str) -> Vec<String> {
    let heads: Vec<String> = split_sections(doc)
        .into_iter()
        .map(|(h, _)| norm(&h))
        .collect();
    spec.sections
        .iter()
        .filter(|s| {
            let want = norm(&s.title);
            s.required
                && !heads
                    .iter()
                    .any(|h| h.contains(&want) || want.contains(h) && !h.is_empty())
        })
        .map(|s| s.title.clone())
        .collect()
}

/// Deterministic findings related to format, length, and citation notation.
pub fn format_issues(spec: &Spec, doc: &str) -> Vec<String> {
    let mut issues: Vec<String> = Vec::new();

    for m in missing_sections(spec, doc) {
        issues.push(format!("Missing required section '{}' → add it", m));
    }

    let secs = split_sections(doc);
    for s in &spec.sections {
        if s.chars == 0 {
            continue;
        }
        let want = norm(&s.title);
        if let Some((_, body)) = secs
            .iter()
            .find(|(h, _)| !h.is_empty() && (norm(h).contains(&want) || want.contains(&norm(h))))
        {
            let n = body.chars().count();
            let lo = (s.chars as f64 * 0.6) as usize;
            let hi = (s.chars as f64 * 1.8) as usize;
            if n < lo {
                issues.push(format!(
                    "'{}' too short: {} chars (recommended {} chars) → reinforce with evidence/examples",
                    s.title, n, s.chars
                ));
            } else if n > hi {
                issues.push(format!(
                    "'{}' too long: {} chars (recommended {} chars) → condense",
                    s.title, n, s.chars
                ));
            }
        }
    }

    let m = metrics(doc);
    if spec.total_chars > 0 {
        let lo = (spec.total_chars as f64 * 0.7) as usize;
        let hi = (spec.total_chars as f64 * 1.3) as usize;
        if m.chars < lo {
            issues.push(format!(
                "Overall length too short: {} chars (target {} chars)",
                m.chars, spec.total_chars
            ));
        } else if m.chars > hi {
            issues.push(format!(
                "Overall length too long: {} chars (target {} chars, risk of violating submission rules)",
                m.chars, spec.total_chars
            ));
        }
    }
    if spec.min_citations > 0 && m.citations < spec.min_citations {
        issues.push(format!(
            "Citations (「source name」): {} found → at least {} required",
            m.citations, spec.min_citations
        ));
    }
    if spec.require_table && m.tables == 0 {
        issues.push(
            "No table present → present at least one of comparison/schedule/KPI as a table"
                .to_string(),
        );
    }
    issues
}
