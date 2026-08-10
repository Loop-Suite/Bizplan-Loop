# Empirical review findings — `bizplan` CLI

This documents a real review-and-execution pass against this repo: two rounds of static
code review, followed by actually running the compiled `bizplan` binary against a live
`claude -p` judge (`--model haiku --judge-model haiku`), at real API cost. Every issue
below was filed against and fixed in this repo (`Loop-Suite/Bizplan-Loop`) — this is not a
synthetic benchmark or a golden-set harness like
[Code-Review-Loop's own `evals/`](https://github.com/Loop-Suite/Code-Review-Loop/tree/main/evals);
it's a record of one thorough pass over one CLI.

## TL;DR

| Phase | Scope | Issues | Real cost |
|---|---|---|---|
| 1. Static review, round 1 | `split_sections` heading parsing, self-scoring warning, LLM-timeout thread cleanup, `--patience 0` | [#2](https://github.com/Loop-Suite/Bizplan-Loop/issues/2)–[#5](https://github.com/Loop-Suite/Bizplan-Loop/issues/5) (4) | $0 (no LLM calls) |
| 1b. Static review, round 2 (deeper pass) | `split_sections` fence edge case, `missing_sections` substring overlap, `score_doc` schema gap, `gen -n 0`, `llm.rs` cost accounting | [#6](https://github.com/Loop-Suite/Bizplan-Loop/issues/6)–[#10](https://github.com/Loop-Suite/Bizplan-Loop/issues/10) (5) | $0 (no LLM calls) |
| 2. Real CLI execution (`gen`, `loop`, `score`, gate-loop, `gen -n 0`) | Real `claude -p --model haiku --judge-model haiku` calls | [#11](https://github.com/Loop-Suite/Bizplan-Loop/issues/11) (1) | ≈ $0.2865 |
| **Total** | | **10 issues fixed** | **≈ $0.29** |

All 10 issues were filed and fixed as direct commits to `main` (`Fixes #N` trailers), not
through a PR review flow — there was no second, independent reviewer on the fixes beyond
the original investigation. Issue [#1](https://github.com/Loop-Suite/Bizplan-Loop/issues/1)
(a pre-existing `rustfmt` CI failure) predates this review pass and isn't counted above.

**What this bought:**

- **The most consequential bug wasn't a crash, it was silent score corruption** ([#8](https://github.com/Loop-Suite/Bizplan-Loop/issues/8)).
  The judge JSON schema bounded each criterion's `id` to an enum but never required every
  declared criterion to appear exactly once in a reply. A reply with a duplicated or
  omitted criterion id still passed schema validation, and `score_doc` silently scored the
  missing criterion as `trimmed_mean(&[]) == 0.0` — no warning, no error, just a corrupted
  weighted total. The blast radius is spec-dependent: this repo's own
  `specs/example-grant.toml` weights its three criteria 40/30/30, so losing the
  40-weight one this way could silently cost up to 40% of the total score on an otherwise
  fine document. Fixed by validating each round's criteria ids against the declared set and
  discarding a malformed round instead of silently zero-filling it.
- **One bug only existed under a real judge call — the static passes couldn't have caught it** ([#11](https://github.com/Loop-Suite/Bizplan-Loop/issues/11)).
  Running the real `claude -p --model haiku` judge produced a `comment` field containing an
  embedded newline (`"\nThis submission is not a business plan...\n"`) — nothing in the
  schema forbids that, and nine issues across two static review rounds never flagged it
  because it depends on what an actual model returns, not on anything visible in the
  source. It broke two things downstream: `report.md`'s blockquote rendering (only the
  first line got a `>` prefix) and `score.rs`'s `feedback_text()` bullet list, which feeds
  directly into the next `claude -p` revise-prompt — a live-execution bug with a real,
  measurable effect on the next LLM call's input quality, not just cosmetic.
- **`loop` was actually observed self-improving on real judge feedback, not just assumed to.**
  A real `loop --max-iter 2` run scored iteration 1 at 0.0 (rejected), fed that round's
  judge comments/improvements into `revise()`, and iteration 2 scored 66.7 — a real,
  measured score jump driven by the feedback loop working as designed against a live model,
  not a unit-tested assumption about it.
- **The two review rounds found genuinely different classes of bug.** Round 1 (4 issues,
  2 explicitly marked low-confidence/edge-case: [#4](https://github.com/Loop-Suite/Bizplan-Loop/issues/4),
  [#5](https://github.com/Loop-Suite/Bizplan-Loop/issues/5)) caught surface-level parsing and
  warning-placement bugs. Round 2 went deeper into the same files and caught the two most
  consequential issues in the whole pass — the score-corruption gap above, and
  [#10](https://github.com/Loop-Suite/Bizplan-Loop/issues/10) (an errored `claude -p` call's
  `total_cost_usd` being dropped from the running total before the `is_error` check — 
  confirmed by actually inspecting a real error response and finding `total_cost_usd` present
  on it too, meaning a failed-but-billed call could previously vanish from the cost tally
  entirely).
- **`gen -n 0` cost nothing to verify.** It's rejected up front before any file I/O or LLM
  call, so confirming the fix ([#9](https://github.com/Loop-Suite/Bizplan-Loop/issues/9): a
  misleading "all 0 requested item(s) failed" message) didn't add to the $0.29 total.
- **Real cost across the four billed invocations: $0.2865** — `gen -n 2` $0.0717, `loop
  --max-iter 2` $0.0939, `score` $0.0441, and a separate `--gate-model` (held-out gate) run
  $0.0768. Small numbers, but real ones, not estimates.
- **Scope is one repo, one CLI, ~1,500 lines of Rust, one review pass.** This isn't a
  statistically powered study like Code-Review-Loop's own 41-/78-case SZZ benchmarks — it's
  a thorough single pass, and there's no claim here that every remaining bug was found.

## Phase 1 — static review, round 1

| # | Title | Root cause | Fix |
|---|---|---|---|
| [#2](https://github.com/Loop-Suite/Bizplan-Loop/issues/2) | `split_sections` misparses `#` lines inside code fences as heading boundaries | Any line starting with `#` was treated as a heading, including Python/YAML comments inside fenced code blocks — truncating section bodies and producing false "too short" findings that fed straight into `loop`'s rewrite prompt. | [`d2b2d00`](https://github.com/Loop-Suite/Bizplan-Loop/commit/d2b2d00) — track fenced-code-block state, skip heading detection inside a fence. |
| [#3](https://github.com/Loop-Suite/Bizplan-Loop/issues/3) | Self-scoring warning printed unconditionally, even for `score` | The generation/scoring-model-bias warning fired whenever `--judge-model` was unset, even on `score`, which never generates (`gen_llm` unused there). | [`11bfc24`](https://github.com/Loop-Suite/Bizplan-Loop/commit/11bfc24) — restrict the warning to `Gen`/`Loop`. |
| [#4](https://github.com/Loop-Suite/Bizplan-Loop/issues/4) | [Low confidence/edge case] I/O threads not joined after LLM call timeout | On timeout, `call_once` killed the child process and returned `Err` without joining `t_in`/`t_out`/`t_err`, unlike the success path. | [`1ae2398`](https://github.com/Loop-Suite/Bizplan-Loop/commit/1ae2398) — join all three (best-effort) before returning on timeout. |
| [#5](https://github.com/Loop-Suite/Bizplan-Loop/issues/5) | [Low confidence/edge case] `--patience 0` reports "Improvement stalled" after round 1 | The stall counter starts at 0, so `stall >= cfg.patience` tripped immediately after the first (always-improving, nothing-to-compare-against) iteration. | [`367be92`](https://github.com/Loop-Suite/Bizplan-Loop/commit/367be92) — skip the stall check on iteration 0; no behavior change for the default `--patience 2`. |

## Phase 1b — static review, round 2 (deeper pass)

| # | Title | Root cause | Fix |
|---|---|---|---|
| [#6](https://github.com/Loop-Suite/Bizplan-Loop/issues/6) | `split_sections`: an unterminated code fence swallows every heading until EOF | ` ``` ` toggled `in_code_fence` unconditionally; an odd fence-marker count (e.g. the model forgets to close a block) left it stuck `true` for the rest of the document, misparsing every later real heading as body text. | [`3e64506`](https://github.com/Loop-Suite/Bizplan-Loop/commit/3e64506) — pre-scan fence marker indices; on an odd count, treat the final unmatched marker as text so fence state always resolves closed by EOF. |
| [#7](https://github.com/Loop-Suite/Bizplan-Loop/issues/7) | `missing_sections`: substring title overlap lets one heading satisfy two different declared sections | Both `missing_sections` and the length check matched declared titles against headings via independent bidirectional substring containment — e.g. a "Team" heading could satisfy both a "Team" and a "Team Timeline" requirement, hiding a genuinely missing section. | [`b2611d8`](https://github.com/Loop-Suite/Bizplan-Loop/commit/b2611d8) — shared `match_headings` helper: each heading matches at most one section (closest length, exact match wins outright). |
| [#8](https://github.com/Loop-Suite/Bizplan-Loop/issues/8) | `score_doc`: a judge reply with a duplicated/missing criterion id silently zeroes that criterion | See "What this bought" above — the most severe finding of the pass. | [`4722088`](https://github.com/Loop-Suite/Bizplan-Loop/commit/4722088) — validate criteria ids per round; discard malformed rounds with a printed warning, error out if zero rounds remain. `Scored.rounds` now reflects the real usable count. |
| [#9](https://github.com/Loop-Suite/Bizplan-Loop/issues/9) | `gen -n 0` reports a misleading "Generation failed: all 0 requested item(s) failed" | With `-n 0`, `par_map` ran zero attempts (0 failed, 0 succeeded), and the downstream `!docs.is_empty()` guard fired a message implying failures that never happened. | [`9c6aac9`](https://github.com/Loop-Suite/Bizplan-Loop/commit/9c6aac9) — validate `count > 0` up front, before any I/O or LLM calls. |
| [#10](https://github.com/Loop-Suite/Bizplan-Loop/issues/10) | `llm.rs`: cost of an errored claude call is dropped from the cumulative total | `call_once` read `total_cost_usd` only *after* the `is_error` check, so an error response returned `Err` before its cost was added to `COST_MICROS`. Verified against a real (near-zero-cost) `claude -p` call that `total_cost_usd` is present on `is_error:true` responses too — so a real error after real tokens billed (e.g. `--max-budget-usd` tripping mid-generation) silently vanished from the total, and `with_retry` then retries the same costly call, compounding the undercount. | [`6a29bf7`](https://github.com/Loop-Suite/Bizplan-Loop/commit/6a29bf7) — move the cost tally above the `is_error` check so it runs for any response that parsed as JSON, success or failure. |

## Phase 2 — real CLI execution

Static review closes at "the code looks wrong"; it can't confirm what a live model
actually returns. This phase compiled `bizplan` and ran it for real against
`claude -p --model haiku --judge-model haiku` — real API calls, real cost, no mocking.

**Runs and real cost:**

| Invocation | Purpose | Real cost |
|---|---|---|
| `gen -n 2` | Generate 2 drafts, score both | $0.0717 |
| `loop --max-iter 2` | Generate → score → revise → re-score | $0.0939 |
| `score` | Score an existing document, no generation | $0.0441 |
| `loop` with `--gate-model` (held-out gate path) | Exercise the post-loop re-score of first vs. best by a model that didn't participate | $0.0768 |
| `gen -n 0` | Confirm the [#9](https://github.com/Loop-Suite/Bizplan-Loop/issues/9) fix | $0 (rejected before any call) |
| **Total** | | **≈ $0.2865** |

**Observations from the real runs:**

- **`loop --max-iter 2` showed real, measured self-improvement**: iteration 1 scored 0.0
  and was rejected (below target, with outstanding format issues); its judge
  comments/improvements were folded into `revise()`; iteration 2 scored 66.7. This is the
  feedback loop working end-to-end against a live judge, not an assumption backed only by
  unit tests.
- **The gate-loop path ran the held-out re-score for real** — `--gate-model` re-scoring the
  first and best drafts with a model that never participated in the loop itself, per the
  design in `README.md`'s "Held-out gate" section.
- **[#11](https://github.com/Loop-Suite/Bizplan-Loop/issues/11) was found here, not in
  either static pass**: the haiku judge's `comment` field came back with an embedded
  newline (`"\nThis submission is not a business plan...\n"`). `report.rs`'s
  `details()` blockquoted only the first line (`format!("> {}", c)`), leaving the rest as
  an unblockquoted paragraph in `report.md`; `score.rs`'s `feedback_text()` had the same
  problem building the `- ` bullet list fed into the next `revise()` prompt — a leading
  newline left a bare bullet marker with the instruction text on an unmarked line, degrading
  the next `claude -p` call's input. Fixed in [`71311d0`](https://github.com/Loop-Suite/Bizplan-Loop/commit/71311d0)
  by trimming/quoting each line for report comments and collapsing embedded newlines to
  spaces for feedback bullets.
- **`gen -n 0` confirmed cheap to verify**: the fix rejects the count before any file I/O or
  network call, so this check added nothing to the $0.29 total — consistent with what the
  code change claims.

## Caveats

- **No independent second reviewer.** All 10 fixes landed as direct commits to `main`
  (`Fixes #N`), not through a PR someone else reviewed. The record here is the original
  investigation's own account, not cross-checked by a separate pass.
- **Small scope.** One repo, one CLI (~1,500 lines of Rust across `src/`), one thorough
  pass. This is not a large-sample benchmark — no claim is made that every remaining bug
  in `split_sections`, `score_doc`, or elsewhere has been found.
- **Real-execution phase is n=4 invocations, one model pairing (`haiku`/`haiku`).**
  [#11](https://github.com/Loop-Suite/Bizplan-Loop/issues/11) is confirmed to reproduce
  against that pairing; whether other model pairings produce the same embedded-newline
  behavior wasn't separately tested.
- **[#8](https://github.com/Loop-Suite/Bizplan-Loop/issues/8)'s "up to 40%" figure is this
  repo's own spec's weight distribution** (`specs/example-grant.toml`: 40/30/30), not a
  universal bound — the actual blast radius on any given spec depends on that spec's own
  criteria weights.
