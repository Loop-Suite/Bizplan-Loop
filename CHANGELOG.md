# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-10

Initial release.

### Added

- `gen` subcommand: generate N business-plan drafts from an idea file and a spec, score them, and produce a ranked markdown report.
- `score` subcommand: score existing documents (a single file or a directory of `*.md`/`*.txt`) against a spec without generating.
- `loop` subcommand: self-improvement loop — generate, score, regenerate incorporating judge feedback — until a target score is reached, improvement stalls for `--patience` rounds, or `--max-iter` is hit.
- Rotating judge panel: multiple scoring models and review lenses per document, trimmed-mean aggregation across rounds, per-criterion spread reported as an instability indicator.
- Deterministic format checks layered underneath LLM judging: required-section coverage, per-section and overall length bounds, citation count, table presence.
- Held-out gate scoring (`loop --gate-model`) that re-scores the first and best drafts with a model that did not participate in the loop, to flag reward hacking / scorer overfitting.
- Length-inflation canary: warns when a loop's score gain is disproportionately small relative to its length growth (padding rather than substantive improvement).
- Markdown ranking/loop reports plus an append-only `results.jsonl` log of every scored document.
- Dependabot configuration for GitHub Actions and Cargo dependencies.

### Fixed

- `split_sections`: `#` lines inside fenced code blocks (e.g. Python/YAML comments) were misparsed as heading boundaries.
- `split_sections`: an unterminated code fence swallowed every heading until EOF instead of only the unterminated section.
- `missing_sections`: a heading whose title is a substring of another declared section's title could satisfy both, hiding a genuinely missing section.
- `score_doc`: a judge reply with a duplicated or missing criterion id silently scored the omitted criterion from an empty sample (tanking the total) instead of being rejected.
- `llm.rs`: the cost of an errored `claude` call (e.g. hitting `--max-budget-usd` mid-generation) was dropped from the cumulative cost total instead of being counted.
- `llm.rs`: I/O threads were not joined after an LLM call timeout, leaving them running in the background after the call had already failed.
- `gen -n 0` produced a misleading "all 0 requested item(s) failed" message instead of a clear upfront rejection.
- `loop --patience 0` incorrectly reported "Improvement stalled" after round 1, because the stall check could fire before any prior-round baseline existed.
- The self-scoring bias warning printed unconditionally, even for the `score` subcommand, which never generates with the generation model.
- Judge `comment`/`improvement` text with embedded newlines broke report blockquote formatting and could masquerade as multiple, unmarked feedback lines in the loop's regeneration prompt.

### Security

- **Prompt injection**: the judge prompt wrapped the document under evaluation in `<document>...</document>` with no escaping, so a literal `</document>` inside the document text (which can itself be influenced by attacker-controlled idea material flowing through generation) could break out of the tag and append fabricated instructions the judge might follow, undermining the integrity of the score. Added a `wrap_untrusted` helper that neutralizes an embedded closing delimiter, applied everywhere untrusted content is interpolated (idea material, the prior draft, the document being judged), plus explicit anti-injection framing in both the generation and judge system prompts.
- **Resource exhaustion**: `read_text` (used for `gen`/`loop --idea` and `score --input`) had no size limit, so an accidentally or maliciously huge/corrupted file would be buffered fully into memory and forwarded whole into a paid LLM call. Added an 8 MiB cap with a clear, actionable error.
- **Input validation**: `Spec::load` accepted a non-finite (`inf`) criterion weight — TOML supports `inf`/`-inf`/`nan` float literals, and the old `weight > 0.0` check let `inf` through — which silently turned every score computed against that spec into `NaN` instead of failing fast with a clear error. Now rejected at load time.
- **Robustness**: a judge round whose JSON shape doesn't deserialize into the expected schema at all (distinct from the already-handled duplicated/missing-id case) aborted the entire `score_doc` call, discarding every other already-paid-for round in the same batch, instead of being discarded on its own like any other malformed round.

[Unreleased]: https://github.com/Loop-Suite/Bizplan-Loop/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Loop-Suite/Bizplan-Loop/releases/tag/v0.1.0
