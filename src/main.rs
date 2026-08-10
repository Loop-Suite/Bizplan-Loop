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

fn read_text(p: &Path) -> Result<String> {
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
