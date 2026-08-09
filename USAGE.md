# bizplan-loop Usage

## 0. Setup

```bash
claude --version          # if missing: npm i -g @anthropic-ai/claude-code && claude  (log in once)
cd ~/Downloads/bizplan-loop
cargo build --release
alias bizplan="$PWD/target/release/bizplan"
```

If `claude` isn't on PATH, add `--claude-bin /path/to/claude` to every command.

## 1. Writing the idea file (`idea.md`)

Generation quality is roughly proportional to this file. At minimum, include the following.

```markdown
# Service name / one-line definition
# Problem: who is stuck, in what situation, why (stats + sources if you have them)
# Solution: flow (input → processing → output)
# Tech stack / what's actually implemented / what isn't
# Revenue model, cost structure (real figures only)
# Differentiation vs. competitors
# Team, timeline
```

Don't write numbers you don't have. If the model fills gaps with invention, it gets penalized directly in scoring (in practice, ungrounded figures like "80 billion won in savings" and calculation errors get caught during scoring).

## 2. Three commands

### `gen` — generate multiple drafts, then compare

```bash
bizplan --model sonnet --judge-model haiku gen \
  --spec specs/example-grant.toml --idea idea.md \
  -n 6 --rounds 2 --concurrency 3 --out runs/grant
```

Applies a different framing angle (the spec's `angles`) to each draft → scores → ranks.
Use this to pick which framing wins under this judging criteria.

```
runs/grant/cand01.md … cand06.md   drafts
runs/grant/results.jsonl           raw scores (per-criterion scores, spread, format checks, improvement notes)
runs/grant/report.md               ranked table + per-document detail
```

`--no-score` allows generation only.

### `score` — score an already-written document

```bash
bizplan --judge-model sonnet,haiku score --spec specs/example-grant.toml --input my-application.md --rounds 3 --out runs/check
bizplan --judge-model sonnet     score --spec specs/example-grant.toml --input runs/grant           --rounds 2 --out runs/check
```

If given a directory, scores all `*.md`/`*.txt` files (excluding `report.md`). If one fails, the rest continue.

### `loop` — auto-improve to a target score

```bash
bizplan --model opus --judge-model sonnet --gate-model haiku loop \
  --spec specs/example-grant.toml --idea idea.md \
  --target 85 --max-iter 4 --rounds 2 --out runs/loop
```

Generate → score → regenerate incorporating the improvement notes. The stop condition is whichever of these three fires first:

1. target score reached **and** zero format issues
2. improvement stagnation — improvement below `--min-delta` (default 2.0 points) for `--patience` (default 2) consecutive rounds
3. `--max-iter` (default 4) exhausted

```
runs/loop/iter01.md … iter04.md   per-iteration document (all kept)
runs/loop/best.md                 highest-scoring iteration (not necessarily the last)
runs/loop/report.md               iteration trend + held-out check + warnings
```

If `--gate-model` is given, a model that didn't participate in the loop re-scores the first draft and the best draft. **If only the loop score rises while the held-out score doesn't, that's judge optimization, not real improvement**, and the report shows a warning.

## 3. A real-world workflow

```bash
# 1) explore framings — cheap, broad
bizplan --model sonnet --judge-model haiku \
  gen --spec specs/example-grant.toml --idea idea.md -n 6 --rounds 1 --concurrency 3 --out runs/explore

# 2) loop on report.md's #1-ranked angle (generation/judge/gate models all separated)
bizplan --model opus --judge-model sonnet --gate-model haiku loop \
  --spec specs/example-grant.toml --idea idea.md \
  --angle "Foreground resolving the requesting agency's actual procurement bottleneck." \
  --target 88 --max-iter 4 --rounds 2 --out runs/final

# 3) a human edits best.md, then re-score
bizplan --judge-model sonnet,haiku \
  score --spec specs/example-grant.toml --input runs/final/best_edited.md --rounds 3 --out runs/final-check
```

Number of calls = `gen`: n × (1 + rounds) / `loop`: at most max_iter × (1 + rounds) + 2 for the gate.
Each call takes 20 seconds to 3 minutes depending on document length. Cumulative cost ($) is printed at the end of the run.

## 4. Options

| Option | Default | Description |
|---|---|---|
| `--model` | claude's default | Generation model (`opus`/`sonnet`/`haiku`/`fable`) |
| `--judge-model` | same as `--model` | Judge model(s). **Comma-separated → rotates round to round** (recommended) |
| `--gate-model` | none | Held-out verification model that didn't join the loop (`loop` only) |
| `--rounds` (formerly `--judges`) | 2 | Scoring passes per document. Model/lens rotated, then trimmed mean |
| `--concurrency` | 1 | Number of parallel runs |
| `--retries` | 2 | Retries on call/schema failure |
| `--timeout-secs` | 600 | Timeout per call |
| `--max-budget-usd` | none | Cost cap per call |
| `--load-context` | off | Loads the working directory's CLAUDE.md/plugins/hooks (blocked by default) |
| `--claude-bin` | `claude` | Path to the executable |
| `--verbose` | off | Retry/failure logs |
| `loop --target` | 85 | Early-stop score |
| `loop --max-iter` | 4 | Maximum iterations |
| `loop --min-delta` / `--patience` | 2.0 / 2 | Stagnation criteria |
| `loop --angle` | spec's first angle | Framing for the initial draft |

## 5. Reading the report

- **Total** = per-criterion trimmed mean (0-100) × the criterion's weight
- **(±N)** next to a criterion = spread across judging rounds. Above ±10, don't trust that criterion's verdict
- **Format issues** = items checked by Rust, not the LLM (missing sections, length over/under target, citation count, table presence). These are submission-rule violations, so fix them before worrying about the score
- **Improvement notes** are the actually actionable output. Read these before the score itself
- In a loop report, the **held-out check** table is the important one. If the loop's improvement and the held-out improvement diverge sharply, that improvement is fake
- This is not an absolute grade. Use it only as a **relative comparison** within the same spec and the same judge model(s)

## 6. Adding a new competition

Just create `specs/new-form.toml`.

```toml
name = "XX Support Program Business Plan"
context = "Host, judging context, evaluation tendencies. Inserted directly into the prompt"
scoring_source = "Scoring rationale goes here. If the announcement has no weights, note 'undisclosed → even weighting'"
total_chars = 4000
min_citations = 2
require_table = true
angles = ["angle1", "angle2"]

[[sections]]
id = "background"
title = "1. Background"        # matches the document's `## 1. Background` heading (whitespace-insensitive partial match)
guide = "What to write, and on what grounds"
chars = 800
required = true               # false excludes it from the missing-section gate

[[criteria]]
id = "feasibility"            # enum value in the scoring schema. lowercase + underscore
name = "Feasibility"
weight = 30                   # the announcement's weight can be used as-is (normalized internally)
guide = "Be specific about what to dock points for"
```

Copy the announcement's judging-criteria wording directly into `guide`, and **if weights are disclosed, always use those exact numbers**. Inventing weights throws off the entire ranking.

## 7. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `` `claude` fails to run `` | Not installed, or a PATH issue → use `--claude-bin` with an absolute path |
| `claude JSON output parse failure` | Not logged in / version mismatch → run `claude -p --output-format json` directly to check |
| `scoring result schema mismatch` | Use `--retries 4`, or `--judge-model sonnet` or higher |
| `timeout exceeded N sec` | Document is long or the model is slow → `--timeout-secs 900` |
| Format issues keep appearing | Spec `title` doesn't match the actual document heading → align `title` with the document's wording |
| Too slow | `--concurrency 3-4`; use `--rounds 1` during exploration |
| Score isn't rising | If the improvement notes are "add sources"-type, the model can't fill them in → put real figures/sources into `idea.md` |
| Score rises but the document isn't better | Check the held-out warning. Switch `--gate-model` to a different model family and recheck |

## 8. Limitations

- If generation and scoring use the same model, it rates its own writing style favorably → separate `--judge-model`
- Repeating the same model has a small effective sample size → prefer `--judge-model a,b` over `--rounds`
- `claude -p` doesn't expose temperature → diversity depends entirely on the angle prompts
- Output is markdown only; hwpx/PDF conversion is out of scope
- LLM score ≠ actual judging score. Rationale in [DESIGN.md](DESIGN.md)
