# Design Rationale

Known failure modes of LLM-as-a-judge / self-improvement loops, reflected here based on the literature. Each item answers "what was implemented, and why this way."

## 1. Scoring scale: 0-10 → 0-100

A coarse scale collapses distinct verdicts into the same score. Expanding a 5-point scale to 100 points consistently reduces the Conflict Ratio between the same judge's pointwise scores and pairwise verdicts (23.32% → 14.89%).
→ [TrustJudge (arXiv 2509.21117)](https://arxiv.org/abs/2509.21117)

## 2. Median → trimmed mean

An integer 0-10 median produces a large number of ties, making it unable to detect small improvements. When aggregating judgment distributions, **mean beat mode in 42 of 48 cases**, and discrete aggregation (mode/median) loses to continuous aggregation due to excess ties. To guard against outliers, a trimmed mean is used that drops one min and one max value once n≥4.
→ [Improving LLM-as-a-Judge Inference with the Judgment Distribution (EMNLP 2025 Findings)](https://aclanthology.org/2025.findings-emnlp.1259/)

## 3. Score-band descriptors + mandatory verbatim citation + "why not higher"

Fixing the rubric, requiring a verbatim source-text quote for every score, and mechanically capping the score when citation is insufficient raises QWK from 0.5566 to 0.7276. Custom-rubric scoring reaches a Pearson correlation of 0.897 with human raters.
→ [Rulers (arXiv 2601.08654)](https://arxiv.org/html/2601.08654), [Prometheus (ICLR 2024)](https://arxiv.org/abs/2310.08491)

Implementation: the judge schema requires `evidence` (verbatim quote) and `why_not_higher` fields, and the prompt states the rule "if no citable evidence is found, the score cannot exceed 60." Score-band descriptors live in `Spec::bands` (default: 5 bands).

## 4. De-anchoring — write the criteria before scoring

A reference-free judge measures plausibility, not correctness. Having the judge generate its own criteria/answer before seeing the candidate answer drops the false positive rate from 0.719 to 0.012 (more effective than making the judge bigger or using more judges).
→ [More Convincing, Not More Correct (arXiv 2607.05904)](https://arxiv.org/html/2607.05904)

Implementation: in the JSON schema's field order, `winning_conditions` comes before `criteria`. Since generation is autoregressive, the criteria description is produced first.

## 5. Held-out gate (`--gate-model`)

Optimizing the judge via self-play raises the **judge pass rate from 0.716 to 0.938, while actual accuracy moves only from 0.209 to 0.202**. The judge score inside the loop alone cannot distinguish real improvement from judge optimization. Self-improvement iterations also inflate the judge score itself (judge average 4.1 → 4.7, high-score preference 63.04% → 97.68%).
→ [2607.05904](https://arxiv.org/html/2607.05904), [Meta-Rewarding (arXiv 2407.19594)](https://arxiv.org/abs/2407.19594)

Implementation: a model that never participated in the loop re-scores only the first draft and the best draft. If the held-out improvement is less than 1/3 of the loop's improvement, the report shows a warning.

## 6. Length canary

Raising verbosity alone, without changing content, moves the win rate from 22.9% to as high as 64.3%. In self-improvement loops, a length explosion — responses growing longer with every iteration — has been observed.
→ [Length-Controlled AlpacaEval (arXiv 2404.04475)](https://arxiv.org/abs/2404.04475), [Meta-Rewarding](https://arxiv.org/abs/2407.19594)

Implementation: the regeneration prompt constrains length to "keep within ±15%." If, by the end of the loop, length has grown +25% or more while the score gained less than +5, a warning is shown. The report's per-iteration table always includes a length column.

## 7. The returned value is the argmax, not the last iteration

Intrinsic self-correction can actually degrade performance (GPT-4 GSM8K: 95.5% → 91.5% → 89.0%). Prior reported successes came from an oracle telling the model "when to stop."
→ [LLMs Cannot Self-Correct Reasoning Yet (ICLR 2024)](https://arxiv.org/abs/2310.01798)

Implementation: `best.md` is the highest-scoring iteration across all rounds. If the last iteration isn't the highest-scoring one, a warning is shown.

## 8. Max 4 iterations + early stop on stagnation

Most of the gain is concentrated in the first iteration, with diminishing returns after (Self-Refine: max 4 iterations, task-specific stop condition). The Self-Rewarding line of work also effectively plateaus after iteration 3.
→ [Self-Refine (NeurIPS 2023)](https://arxiv.org/abs/2303.17651), [Meta-Rewarding](https://arxiv.org/abs/2407.19594)

Implementation: `--max-iter` defaults to 4; if improvement stays below `--min-delta` (default 2.0 points) for `--patience` (default 2) consecutive rounds, the loop stops.

## 9. A `--judge-model` panel over `--rounds`

With 9 judges, the effective sample size was only 2.18 (average pairwise error correlation 0.391). Repeating the same model has even higher correlation, so the effective N is smaller still. A panel mixing different model families is cheaper than a single large judge while having lower intra-model bias.
→ [Nine Judges, Two Effective Votes (arXiv 2605.29800)](https://arxiv.org/html/2605.29800), [PoLL (arXiv 2404.18796)](https://arxiv.org/abs/2404.18796)

Implementation: `--judge-model a,b,c` rotates the model each round. The lens is also expanded to 6 variants so rounds don't overlap. A warning is printed when only a single model is used.

## 10. self-preference

The same model rates its own output +10-25 points more favorably than humans do. Since the cause isn't only self-recognition but also low perplexity (familiarity), the effect doesn't fully disappear even when switching to a different model in the same family.
→ [MT-Bench (NeurIPS 2023)](https://arxiv.org/abs/2306.05685), [LLM Evaluators Recognize and Favor Their Own Generations (NeurIPS 2024)](https://proceedings.neurips.cc/paper_files/paper/2024/hash/7f1f0218e45f5414c79c0679633e47bc-Abstract-Conference.html), [Self-Preference Bias (arXiv 2410.21819)](https://arxiv.org/html/2410.21819v2)

Implementation: the judge system prompt states "the author is unknown and must not be guessed." The judge is not given prior-round feedback or score history. Using `--gate-model` with a different model for gating is recommended.

## 11. Separate deterministic checks from the LLM

In the evaluation cost hierarchy, assertion/code-based rules are cheaper and more stable than an LLM judge. There's a catch-22 where you need to score before you can finalize the criteria, so the rubric has to be treated as a living document.
→ [Hamel Husain, LLM Evals FAQ](https://hamel.dev/blog/posts/evals-faq/), [Who Validates the Validators? (UIST 2024)](https://arxiv.org/abs/2404.12272)

Implementation: `checks.rs` — required sections, per-section and total length, citation-marker count, table presence. The scoring prompt states "format and length are handled by automated checks; evaluate content only."

## 12. Keeping pointwise

When the generator plants features the judge prefers, pairwise verdicts flip 35% of the time versus 9% for pointwise (absolute) scoring. Inside an optimization loop, pointwise is more robust.
→ [Pairwise or Pointwise? (COLM 2025)](https://arxiv.org/abs/2504.14716)

## Not yet done

- **Calibration set**: a procedure to calibrate the rubric against 30-50 real winning/losing entries and validate with QWK. Without this, the "score 90 = winner" anchor isn't empirically supported. ([EDM 2023 QWK threshold ≥0.70](https://files.eric.ed.gov/fulltext/ED630859.pdf))
- **A v_t vs v_{t+1} pairwise regression gate** (order-swapped twice). Absolute scores and pairwise verdicts disagree 23% of the time.
- **A judge-noise-based stop condition**: score a fixed document N times to measure the SD, and define stagnation as `Δ < 2·SE`. Currently a fixed threshold (`--min-delta`) is used instead.
