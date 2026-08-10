mod checks;
mod generate;
mod llm;
mod loop_run;
mod report;
mod score;
mod spec;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use llm::Llm;
use score::Scored;
use spec::Spec;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "bizplan",
    version,
    about = "CLI that generates business plans using Claude Code (`claude -p`) as a backend and scores them with a rubric"
)]
struct Cli {
    /// Path to the claude executable
    #[arg(long, default_value = "claude", global = true)]
    claude_bin: String,
    /// Generation model (opus/sonnet/haiku/fable, or a full model ID)
    #[arg(long, global = true)]
    model: Option<String>,
    /// Scoring model. If multiple are given comma-separated, they are used as a rotating panel (e.g. sonnet,haiku)
    #[arg(long, global = true)]
    judge_model: Option<String>,
    /// Number of retries for LLM calls
    #[arg(long, default_value_t = 2, global = true)]
    retries: u32,
    /// Timeout per call (seconds)
    #[arg(long, default_value_t = 600, global = true)]
    timeout_secs: u64,
    /// Maximum cost per call (USD). Passed via claude --max-budget-usd
    #[arg(long, global = true)]
    max_budget_usd: Option<f64>,
    /// Load CLAUDE.md, plugins, and hooks from the execution directory (blocked by --safe-mode by default)
    #[arg(long, global = true)]
    load_context: bool,
    /// Print retry/failure logs
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Generate N drafts + score + ranking report
    Gen {
        #[arg(long)]
        spec: PathBuf,
        /// Original idea material file (md/txt)
        #[arg(long)]
        idea: PathBuf,
        #[arg(short = 'n', long, default_value_t = 3)]
        count: usize,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
        /// Number of scoring passes per document (trimmed mean after rotating models/perspectives)
        #[arg(long = "rounds", alias = "judges", default_value_t = 2)]
        rounds: usize,
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
        /// Generate only, skip scoring
        #[arg(long)]
        no_score: bool,
    },
    /// Score existing documents only
    Score {
        #[arg(long)]
        spec: PathBuf,
        /// File or directory to score (*.md, *.txt)
        #[arg(long)]
        input: PathBuf,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
        #[arg(long = "rounds", alias = "judges", default_value_t = 2)]
        rounds: usize,
        #[arg(long, default_value_t = 1)]
        concurrency: usize,
    },
    /// Self-improvement loop: generate → score → regenerate with feedback
    Loop {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        idea: PathBuf,
        #[arg(long, default_value = "runs")]
        out: PathBuf,
        /// Target score (0-100). Stops early once reached
        #[arg(long, default_value_t = 85.0)]
        target: f64,
        /// Max iterations. Defaults to 4 since most of the literature's gains occur within the first 1-2 rounds
        #[arg(long, default_value_t = 4)]
        max_iter: usize,
        #[arg(long = "rounds", alias = "judges", default_value_t = 2)]
        rounds: usize,
        /// Considered stalled if improvement over the previous best is less than this value
        #[arg(long, default_value_t = 2.0)]
        min_delta: f64,
        /// Stop early if stalled for this many consecutive rounds
        #[arg(long, default_value_t = 2)]
        patience: usize,
        /// Approach angle for the starting draft (defaults to the spec's default if unspecified)
        #[arg(long, default_value = "")]
        angle: String,
        /// Held-out scoring model that does not participate in the loop. After finishing, re-scores the first draft vs. the best draft with this model
        #[arg(long)]
        gate_model: Option<String>,
    },
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

/// Upper bound on `-n`/`--count` (`gen`) — each unit is a real (billed) generation LLM call.
/// Without a cap, a typo (extra zero) turns one invocation into an unbounded number of paid API
/// calls with no confirmation step. 20 is far beyond any real use case (the default is 3).
const MAX_COUNT: usize = 20;

/// Upper bound on `--rounds`/`--judges` (`gen`/`score`/`loop`) — each round is a real (billed)
/// scoring LLM call per document. 10 is far beyond any real use case (the default is 2).
const MAX_ROUNDS: usize = 10;

/// Upper bound on `--max-iter` (`loop`) — each iteration re-generates and re-scores the draft
/// with real (billed) LLM calls. 20 is far beyond any real use case (the default is 4, and the
/// docs already note most of the literature's gains occur within the first 1-2 rounds).
const MAX_ITER: usize = 20;

/// Rejects unreasonably large `count`/`rounds`/`max_iter` values before any LLM call is made.
/// These arguments directly multiply the number of real, billed `claude -p` invocations a single
/// run makes; without an upper bound a typo'd or scripted value (e.g. `--rounds 200`) would run
/// to completion with no confirmation step, potentially costing far more than intended.
fn validate_call_bounds(
    count: Option<usize>,
    rounds: Option<usize>,
    max_iter: Option<usize>,
) -> Result<()> {
    if let Some(c) = count {
        anyhow::ensure!(
            c <= MAX_COUNT,
            "count (-n) too large ({c}, max {MAX_COUNT}) — would trigger an unbounded number of paid LLM calls"
        );
    }
    if let Some(r) = rounds {
        anyhow::ensure!(
            r <= MAX_ROUNDS,
            "rounds too large ({r}, max {MAX_ROUNDS}) — each round is a paid LLM call per document"
        );
    }
    if let Some(m) = max_iter {
        anyhow::ensure!(
            m <= MAX_ITER,
            "max_iter too large ({m}, max {MAX_ITER}) — each iteration is a paid LLM call"
        );
    }
    Ok(())
}

fn build_llm(cli: &Cli, model: Option<String>) -> Llm {
    let mut l = Llm::new(cli.claude_bin.clone(), model);
    l.retries = cli.retries;
    l.verbose = cli.verbose;
    l.timeout = Duration::from_secs(cli.timeout_secs);
    l.max_budget_usd = cli.max_budget_usd;
    l.load_context = cli.load_context;
    l
}

fn judge_panel(cli: &Cli) -> Vec<Llm> {
    match &cli.judge_model {
        Some(list) => list
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|m| build_llm(cli, Some(m.to_string())))
            .collect(),
        None => vec![build_llm(cli, cli.model.clone())],
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let gen_llm = build_llm(&cli, cli.model.clone());
    let judges = judge_panel(&cli);
    // `score` never generates (gen_llm is unused there), so the self-scoring bias warning
    // only applies to subcommands that actually generate with `gen_llm`.
    let generates = matches!(cli.cmd, Cmd::Gen { .. } | Cmd::Loop { .. });
    if generates && cli.judge_model.is_none() {
        eprintln!(
            "Warning: the generation model and scoring model are the same. Since there is a bias toward rating its own style favorably, \
             it is better to specify a different model with --judge-model."
        );
    }

    match &cli.cmd {
        Cmd::Gen {
            spec,
            idea,
            count,
            out,
            rounds,
            concurrency,
            no_score,
        } => {
            anyhow::ensure!(*count > 0, "count (-n) must be at least 1 (got 0)");
            validate_call_bounds(Some(*count), Some(*rounds), None)?;
            let sp = Spec::load(spec)?;
            let idea_text = read_text(idea)?;
            let out_dir = prepare_out(out)?;
            let angles = generate::angles_for(&sp, *count);

            println!("Generating {} — {}", count, sp.name);
            let items: Vec<(usize, String)> = angles.into_iter().enumerate().collect();
            let requested = items.len();
            let (docs, failed) = par_map(*concurrency, items, |(i, angle)| {
                let d = generate::generate(&gen_llm, &sp, &idea_text, &angle)?;
                let label = format!("cand{:02}", i + 1);
                std::fs::write(out_dir.join(format!("{}.md", label)), &d)?;
                println!(
                    "  Generation complete: {} ({} chars)",
                    label,
                    d.chars().count()
                );
                Ok((label, d))
            });
            if failed > 0 {
                eprintln!(
                    "Warning: {failed} generation(s) failed ({} of {requested} requested succeeded)",
                    docs.len()
                );
            }
            anyhow::ensure!(
                !docs.is_empty(),
                "Generation failed: all {requested} requested item(s) failed"
            );

            if *no_score {
                println!(
                    "Output: {}  (cumulative ${:.4})",
                    out_dir.display(),
                    llm::total_cost_usd()
                );
                return Ok(());
            }
            let scored = score_many(&judges, &sp, docs, *rounds, *concurrency, &out_dir);
            finish(&out_dir, &sp, &scored)
        }

        Cmd::Score {
            spec,
            input,
            out,
            rounds,
            concurrency,
        } => {
            validate_call_bounds(None, Some(*rounds), None)?;
            let sp = Spec::load(spec)?;
            let out_dir = prepare_out(out)?;
            let files = collect_docs(input)?;
            anyhow::ensure!(
                !files.is_empty(),
                "No documents to score: {}",
                input.display()
            );
            println!("Scoring {} — {}", files.len(), sp.name);

            let mut docs: Vec<(String, String)> = Vec::new();
            for f in files {
                let label = f
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| f.display().to_string());
                docs.push((label, read_text(&f)?));
            }
            let scored = score_many(&judges, &sp, docs, *rounds, *concurrency, &out_dir);
            finish(&out_dir, &sp, &scored)
        }

        Cmd::Loop {
            spec,
            idea,
            out,
            target,
            max_iter,
            rounds,
            min_delta,
            patience,
            angle,
            gate_model,
        } => {
            validate_call_bounds(None, Some(*rounds), Some(*max_iter))?;
            let sp = Spec::load(spec)?;
            let idea_text = read_text(idea)?;
            let out_dir = prepare_out(out)?;
            let angle = if angle.is_empty() {
                generate::angles_for(&sp, 1).remove(0)
            } else {
                angle.clone()
            };
            let cfg = loop_run::LoopCfg {
                target: *target,
                max_iter: *max_iter,
                rounds: *rounds,
                min_delta: *min_delta,
                patience: *patience,
            };
            println!(
                "Starting loop — target {:.0} points, max {} rounds",
                target, max_iter
            );
            let r = loop_run::run(&gen_llm, &judges, &sp, &idea_text, &out_dir, &cfg, &angle)?;

            // Held-out gate: re-score only the first and best drafts using a model that did not participate in the loop
            let mut gate_pair: Option<(Scored, Scored)> = None;
            if let Some(gm) = gate_model {
                println!("Held-out verification ({gm})…");
                let g = vec![build_llm(&cli, Some(gm.clone()))];
                let f = score::score_doc(&g, &sp, "gate-first", &r.first_doc, 1)?;
                let b = score::score_doc(&g, &sp, "gate-best", &r.best_doc, 1)?;
                println!(
                    "  First draft {:.1} → best draft {:.1} (held-out)",
                    f.total, b.total
                );
                gate_pair = Some((f, b));
            }

            let path = report::write_loop_report(
                &out_dir,
                &sp,
                &r.history,
                &r.stop_reason,
                &r.warnings,
                gate_pair.as_ref().map(|(f, b)| (f, b)),
            )?;
            println!(
                "\nFinished: {} · best {:.1}/100 ({})",
                r.stop_reason, r.best_score.total, r.best_label
            );
            for w in &r.warnings {
                println!("  ⚠ {}", w);
            }
            println!("Final draft: {}", out_dir.join("best.md").display());
            println!("Report: {}", path.display());
            println!("Cumulative cost: ${:.4}", llm::total_cost_usd());
            Ok(())
        }
    }
}

fn finish(out_dir: &Path, sp: &Spec, scored: &[Scored]) -> Result<()> {
    anyhow::ensure!(!scored.is_empty(), "No documents were successfully scored");
    let path = report::write_report(out_dir, sp, scored)?;
    let mut ranked: Vec<&Scored> = scored.iter().collect();
    ranked.sort_by(|a, b| {
        b.total
            .partial_cmp(&a.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    println!("\nRanking");
    for (i, s) in ranked.iter().enumerate() {
        println!("  {}. {} — {:.1}/100", i + 1, s.label, s.total);
    }
    println!("Report: {}", path.display());
    println!("Cumulative cost: ${:.4}", llm::total_cost_usd());
    Ok(())
}

fn score_many(
    judges: &[Llm],
    sp: &Spec,
    docs: Vec<(String, String)>,
    rounds: usize,
    concurrency: usize,
    out_dir: &Path,
) -> Vec<Scored> {
    let requested = docs.len();
    let (scored, failed) = par_map(concurrency, docs, |(label, doc)| {
        let s = score::score_doc(judges, sp, &label, &doc, rounds)?;
        println!("  Scoring complete: {} — {:.1}/100", s.label, s.total);
        Ok(s)
    });
    if failed > 0 {
        eprintln!(
            "Warning: {failed} scoring failure(s) ({} of {requested} requested succeeded)",
            scored.len()
        );
    }
    for s in &scored {
        if let Err(e) = report::append_jsonl(out_dir, s) {
            eprintln!("Warning: failed to write results.jsonl — {e:#}");
        }
    }
    scored
}

/// Run in parallel in batches of `concurrency`. Failed items are skipped after a warning, and the failure count
/// is returned to the caller (does not abort entirely — deciding whether everything failed is the caller's responsibility).
fn par_map<T, R, F>(concurrency: usize, items: Vec<T>, f: F) -> (Vec<R>, usize)
where
    T: Send,
    R: Send,
    F: Fn(T) -> Result<R> + Sync,
{
    let c = concurrency.max(1);
    let mut out: Vec<R> = Vec::new();
    let mut failed = 0usize;
    let mut rest = items;
    while !rest.is_empty() {
        let take = c.min(rest.len());
        let chunk: Vec<T> = rest.drain(..take).collect();
        let results: Vec<Result<R>> = std::thread::scope(|s| {
            let handles: Vec<_> = chunk.into_iter().map(|item| s.spawn(|| f(item))).collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .map_err(|_| anyhow!("worker thread panicked"))
                        .and_then(|r| r)
                })
                .collect()
        });
        for r in results {
            match r {
                Ok(v) => out.push(v),
                Err(e) => {
                    eprintln!("Warning: item failed — {e:#}");
                    failed += 1;
                }
            }
        }
    }
    (out, failed)
}

/// Generous upper bound on any single text input this tool reads (idea material for
/// `gen`/`loop`, or a document passed to `score --input`). A real business-plan idea or
/// draft is realistically at most a few hundred KB; this exists to fail fast with a clear,
/// actionable error on an accidentally (or maliciously) huge/corrupted file, instead of
/// buffering it all into memory and forwarding it whole into a paid LLM call.
const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB

fn read_text(p: &Path) -> Result<String> {
    let meta =
        std::fs::metadata(p).with_context(|| format!("Failed to read file: {}", p.display()))?;
    anyhow::ensure!(
        meta.len() <= MAX_INPUT_BYTES,
        "File too large: {} ({} bytes, limit {} bytes) — check that the path is correct",
        p.display(),
        meta.len(),
        MAX_INPUT_BYTES
    );
    std::fs::read_to_string(p).with_context(|| format!("Failed to read file: {}", p.display()))
}

fn prepare_out(p: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(p)
        .with_context(|| format!("Failed to create output directory: {}", p.display()))?;
    Ok(p.to_path_buf())
}

fn collect_docs(input: &Path) -> Result<Vec<PathBuf>> {
    if input.is_file() {
        return Ok(vec![input.to_path_buf()]);
    }
    let mut v: Vec<PathBuf> = std::fs::read_dir(input)
        .with_context(|| format!("Failed to read directory: {}", input.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .map(|e| e == "md" || e == "txt")
                    .unwrap_or(false)
                && p.file_name().map(|n| n != "report.md").unwrap_or(true)
        })
        .collect();
    v.sort();
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing previously stopped a multi-GB idea/input file from being buffered fully into
    /// memory and forwarded whole into a paid LLM call. `read_text` must now fail fast with a
    /// clear, actionable error instead. `set_len` creates a sparse file so the test doesn't
    /// actually allocate `MAX_INPUT_BYTES` of disk/memory.
    #[test]
    fn read_text_rejects_a_file_over_the_size_cap() {
        let path = std::env::temp_dir().join(format!(
            "bizplan_read_text_oversized_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        {
            let f = std::fs::File::create(&path).unwrap();
            f.set_len(MAX_INPUT_BYTES + 1).unwrap();
        }
        let err = read_text(&path).expect_err("a file over the size cap must be rejected");
        let _ = std::fs::remove_file(&path);
        assert!(
            format!("{err:#}").contains("too large"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn read_text_accepts_a_normal_sized_file() {
        let path = std::env::temp_dir().join(format!(
            "bizplan_read_text_normal_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, "hello world").unwrap();
        let text = read_text(&path).expect("a small file must be read normally");
        let _ = std::fs::remove_file(&path);
        assert_eq!(text, "hello world");
    }

    #[test]
    fn read_text_accepts_an_empty_file() {
        let path = std::env::temp_dir().join(format!(
            "bizplan_read_text_empty_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, "").unwrap();
        let text = read_text(&path).expect("an empty (0-byte) idea/input file must be accepted");
        let _ = std::fs::remove_file(&path);
        assert_eq!(text, "");
    }

    /// The size check is `<=`, not `<` — a file exactly at the cap must still be accepted,
    /// only strictly-over must be rejected (already covered by
    /// `read_text_rejects_a_file_over_the_size_cap`).
    #[test]
    fn read_text_accepts_a_file_exactly_at_the_size_cap() {
        let path = std::env::temp_dir().join(format!(
            "bizplan_read_text_at_cap_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        {
            let f = std::fs::File::create(&path).unwrap();
            f.set_len(MAX_INPUT_BYTES).unwrap();
        }
        let result = read_text(&path);
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_ok(),
            "a file exactly at the size cap must be accepted, got: {result:?}"
        );
    }

    #[test]
    fn read_text_round_trips_multibyte_unicode_content() {
        let path = std::env::temp_dir().join(format!(
            "bizplan_read_text_unicode_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let content = "한글 아이디어 🚀 — café, naïve, 中文, עברית";
        std::fs::write(&path, content).unwrap();
        let text = read_text(&path).expect("unicode content must be read without error");
        let _ = std::fs::remove_file(&path);
        assert_eq!(text, content);
    }

    #[test]
    fn collect_docs_returns_empty_for_a_directory_with_no_md_or_txt_files() {
        let dir = std::env::temp_dir().join(format!(
            "bizplan_collect_docs_empty_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.json"), "{}").unwrap();
        std::fs::write(dir.join("report.md"), "# excluded").unwrap();
        let docs = collect_docs(&dir).expect("reading an existing directory must not error");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(docs.is_empty(), "expected no candidate docs, got: {docs:?}");
    }

    /// Regression: `--count`/`--rounds`/`--max-iter` used to parse as plain `usize` with no upper
    /// bound, so `--rounds 200` would run to completion and fire 200 real (billed) LLM calls
    /// before anyone noticed. Each must now be rejected at the boundary, before any LLM call.
    #[test]
    fn validate_call_bounds_rejects_count_over_the_cap() {
        let err = validate_call_bounds(Some(MAX_COUNT + 1), None, None)
            .expect_err("count over MAX_COUNT must be rejected");
        assert!(
            format!("{err:#}").contains("count"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn validate_call_bounds_accepts_count_at_the_cap() {
        assert!(validate_call_bounds(Some(MAX_COUNT), None, None).is_ok());
    }

    #[test]
    fn validate_call_bounds_rejects_rounds_over_the_cap() {
        // The exact bug reported by the audit: `--rounds 200` must be rejected, not executed.
        let err = validate_call_bounds(None, Some(200), None)
            .expect_err("rounds over MAX_ROUNDS must be rejected");
        assert!(
            format!("{err:#}").contains("rounds"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn validate_call_bounds_accepts_rounds_at_the_cap() {
        assert!(validate_call_bounds(None, Some(MAX_ROUNDS), None).is_ok());
    }

    #[test]
    fn validate_call_bounds_rejects_max_iter_over_the_cap() {
        let err = validate_call_bounds(None, None, Some(MAX_ITER + 1))
            .expect_err("max_iter over MAX_ITER must be rejected");
        assert!(
            format!("{err:#}").contains("max_iter"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn validate_call_bounds_accepts_max_iter_at_the_cap() {
        assert!(validate_call_bounds(None, None, Some(MAX_ITER)).is_ok());
    }

    /// `None` means "this subcommand doesn't have that argument" (e.g. `score` has no
    /// `max_iter`) — it must never be treated as a violation.
    #[test]
    fn validate_call_bounds_ignores_absent_arguments() {
        assert!(validate_call_bounds(None, None, None).is_ok());
    }
}
