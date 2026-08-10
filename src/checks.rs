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
/// Lines inside fenced code blocks (```` ``` ````) are never treated as headings,
/// even if they start with `#` (e.g. Python/YAML comments).
///
/// An unterminated fence (an odd number of ``` markers) is not allowed to swallow the
/// rest of the document: the final, unmatched marker is treated as ordinary text instead
/// of a fence toggle, so headings after it are still recognized.
pub fn split_sections(doc: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = doc.lines().collect();
    let fence_line_idxs: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.trim_start().starts_with("```"))
        .map(|(i, _)| i)
        .collect();
    let unmatched_fence = if fence_line_idxs.len() % 2 == 1 {
        fence_line_idxs.last().copied()
    } else {
        None
    };

    let mut out: Vec<(String, String)> = Vec::new();
    let mut cur_head = String::new();
    let mut cur_body = String::new();
    let mut in_code_fence = false;
    for (idx, line) in lines.iter().enumerate() {
        let line = *line;
        let t = line.trim_start();
        if t.starts_with("```") {
            if Some(idx) != unmatched_fence {
                in_code_fence = !in_code_fence;
            }
            cur_body.push_str(line);
            cur_body.push('\n');
            continue;
        }
        if !in_code_fence && t.starts_with('#') {
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

/// For each document heading (already `norm`-alized), find the index of the declared
/// section it best represents, if any.
///
/// A heading matches a section if either contains the other (whitespace-insensitive
/// partial match, per the spec format). A heading can match at most one section — the
/// one whose title is closest in length (an exact match wins outright) — so e.g. a
/// heading "Team Timeline" cannot also satisfy an unrelated, shorter declared section
/// like "Team", and the heading "Team" cannot get stolen by the longer "Team Timeline"
/// section either. Without this, one doc heading could silently satisfy two different
/// declared sections whenever one section's title happens to be a substring of another's.
fn match_headings(heads: &[String], wants: &[String]) -> Vec<Option<usize>> {
    heads
        .iter()
        .map(|h| {
            if h.is_empty() {
                return None;
            }
            let mut best: Option<(usize, usize)> = None; // (section index, length diff)
            for (i, want) in wants.iter().enumerate() {
                if want.is_empty() {
                    continue;
                }
                if h.contains(want) || want.contains(h) {
                    let diff = h.len().abs_diff(want.len());
                    let better = match best {
                        None => true,
                        Some((_, best_diff)) => diff < best_diff,
                    };
                    if better {
                        best = Some((i, diff));
                    }
                }
            }
            best.map(|(i, _)| i)
        })
        .collect()
}

/// Titles of missing required sections.
pub fn missing_sections(spec: &Spec, doc: &str) -> Vec<String> {
    let heads: Vec<String> = split_sections(doc)
        .into_iter()
        .map(|(h, _)| norm(&h))
        .collect();
    let wants: Vec<String> = spec.sections.iter().map(|s| norm(&s.title)).collect();
    let mut present = vec![false; spec.sections.len()];
    for m in match_headings(&heads, &wants).into_iter().flatten() {
        present[m] = true;
    }
    spec.sections
        .iter()
        .zip(present)
        .filter(|(s, p)| s.required && !p)
        .map(|(s, _)| s.title.clone())
        .collect()
}

/// Deterministic findings related to format, length, and citation notation.
pub fn format_issues(spec: &Spec, doc: &str) -> Vec<String> {
    let mut issues: Vec<String> = Vec::new();

    for m in missing_sections(spec, doc) {
        issues.push(format!("Missing required section '{}' → add it", m));
    }

    let secs = split_sections(doc);
    let heads: Vec<String> = secs.iter().map(|(h, _)| norm(h)).collect();
    let wants: Vec<String> = spec.sections.iter().map(|s| norm(&s.title)).collect();
    let matches = match_headings(&heads, &wants);
    for (i, s) in spec.sections.iter().enumerate() {
        if s.chars == 0 {
            continue;
        }
        if let Some((_, body)) = matches
            .iter()
            .position(|m| *m == Some(i))
            .map(|hi| &secs[hi])
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Criterion, Section, Spec};

    fn spec_with_sections(titles: &[&str]) -> Spec {
        Spec {
            name: "probe".into(),
            context: String::new(),
            scoring_source: String::new(),
            total_chars: 0,
            min_citations: 0,
            require_table: false,
            angles: vec![],
            bands: vec![],
            sections: titles
                .iter()
                .enumerate()
                .map(|(i, t)| Section {
                    id: format!("s{i}"),
                    title: t.to_string(),
                    guide: String::new(),
                    chars: 0,
                    required: true,
                })
                .collect(),
            criteria: vec![Criterion {
                id: "c".into(),
                name: "c".into(),
                weight: 1.0,
                guide: String::new(),
            }],
        }
    }

    #[test]
    fn missing_sections_does_not_let_one_heading_satisfy_two_declared_sections() {
        // "Team" is a substring of "Team Timeline". The document only has a "Team
        // Timeline" heading — "Team" itself is genuinely absent and must be reported.
        let spec = spec_with_sections(&["Team", "Team Timeline"]);
        let doc = "## Team Timeline\nsome body\n";
        let missing = missing_sections(&spec, doc);
        assert_eq!(missing, vec!["Team".to_string()]);

        // Once both headings are present, nothing should be reported missing.
        let doc_both = "## Team\nintro\n## Team Timeline\nschedule\n";
        assert!(missing_sections(&spec, doc_both).is_empty());
    }

    #[test]
    fn split_sections_ignores_hash_lines_inside_code_fences() {
        let doc = "\
# Overview
This is the overview body.
```python
# not a heading, just a Python comment
x = 1
```
Still part of the overview body after the fence.

# Next Section
Body of the next section.
";
        let secs = split_sections(doc);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].0, "Overview");
        assert!(secs[0].1.contains("not a heading"));
        assert!(secs[0].1.contains("Still part of the overview body"));
        assert_eq!(secs[1].0, "Next Section");
        assert!(secs[1].1.contains("Body of the next section"));
    }

    #[test]
    fn split_sections_recovers_after_an_unterminated_code_fence() {
        // The model forgot to close the fence in Section A. Without the fix, everything
        // from that point to EOF (including the real "Section B" heading) gets swallowed
        // into Section A's body, and Section B silently disappears from the parse.
        let doc = "\
# Section A
Intro.
```
unterminated fence, never closed

# Section B
Body of B.
";
        let secs = split_sections(doc);
        assert_eq!(secs.len(), 2);
        assert_eq!(secs[0].0, "Section A");
        assert_eq!(secs[1].0, "Section B");
        assert!(secs[1].1.contains("Body of B"));
    }

    #[test]
    fn empty_document_produces_zero_metrics_and_no_sections_without_panicking() {
        let spec = spec_with_sections(&["Overview"]);
        assert!(split_sections("").is_empty());
        let m = metrics("");
        assert_eq!((m.chars, m.tables, m.citations, m.years), (0, 0, 0, 0));
        // An empty document is missing every required section — this must be reported
        // cleanly, not panic on an out-of-bounds/empty-input edge case.
        assert_eq!(missing_sections(&spec, ""), vec!["Overview".to_string()]);
        assert!(!format_issues(&spec, "").is_empty());
    }

    #[test]
    fn whitespace_only_document_is_treated_the_same_as_empty() {
        let spec = spec_with_sections(&["Overview"]);
        let doc = "   \n\t\n   \n";
        assert!(split_sections(doc).is_empty());
        assert_eq!(missing_sections(&spec, doc), vec!["Overview".to_string()]);
    }

    /// Multi-byte Unicode (emoji, combining diacritics, CJK, RTL script) must not panic the
    /// char-boundary-sensitive code in `metrics` (the manual year-scan loop indexes into a
    /// `Vec<char>`, not raw bytes, so this locks that in) or `split_sections`/`norm`.
    #[test]
    fn metrics_and_sections_handle_multibyte_unicode_without_panicking() {
        let doc = "\
# 개요 🚀\n\
사업 계획 개요입니다. In 2024 we launched, and by 1999 the prior pilot had ended.\n\
Combining mark test: e\u{0301}\u{0301}\u{0301} (e + combining acute x3).\n\
RTL test: \u{0645}\u{0631}\u{062d}\u{0628}\u{0627} 2020년 기준.\n\
Emoji cluster: 👨‍👩‍👧‍👦 family, 🇰🇷 flag.\n\
| a | b |\n\
|---|---|\n\
| 1 | 2 |\n\
「출처」 citation marker.\n";
        let secs = split_sections(doc);
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].0, "개요 🚀");
        let m = metrics(doc);
        assert_eq!(m.tables, 1);
        assert_eq!(m.citations, 1);
        // 2024, 1999, 2020 — three 4-digit year-shaped tokens.
        assert_eq!(m.years, 3);
        assert!(m.chars > 0);
    }

    /// A document containing a large run of multi-byte characters must not panic in the
    /// year-scan loop's boundary arithmetic (`i + 3 < bytes.len()`, `i - 1`, `i + 4`), which
    /// operates on a `Vec<char>` built from a huge/adversarial document.
    #[test]
    fn metrics_handles_a_document_that_is_almost_entirely_multibyte_characters() {
        let doc = "🚀".repeat(5000) + "2023" + &"가".repeat(5000);
        let m = metrics(&doc);
        assert_eq!(m.years, 1);
        assert_eq!(m.chars, 5000 + 4 + 5000);
    }
}
