# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the version is
`0.x`, the CLI surface (command names, flags, output shapes) may change in a minor release.

## [Unreleased]

## [0.1.2] — 2026-08-18

A speed, cost, and correctness pass. The constraint throughout was that output quality and accuracy
must not change; several items below improve accuracy as a side effect of removing waste, and one
tempting "optimization" was measured and rejected. Also adds DeepSeek Harness support and fixes a
hang that `--timeout` could not interrupt.

### Added

- **Project instructions are now read.** The worker tier loads `AGENTS.md` and `CLAUDE.md` from
  the project root down to the working directory, most-specific last, collapsing a `CLAUDE.md`
  that merely duplicates its sibling. Previously it ignored both — so it would cheerfully violate
  conventions the repository had written down for exactly this purpose, while every other coding
  agent honoured them. They lead the prompt (most stable content first) and outrank discovered
  files under budget pressure.
- **Advisory loop-breaker.** When the model issues the same tool call with identical arguments
  three turns running, the loop injects one reminder to re-read the last result and either change
  approach or conclude. It never blocks or rewrites a call — a legitimately repeated call is
  delayed by nothing. The only previous backstop was the blunt 8-turn cap.
- **[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) integration.**
  `setup hooks` now also installs a DSH skill at
  `<$DSH_HOME|~/.dsh>/skills/deepseek-axi/SKILL.md`, scoped to the two things DSH does not do:
  git-worktree isolation and independent two-sided verification. It is deliberately *not* an
  ambient-context hook like the other three targets — DSH already talks to DeepSeek natively, so
  the skill steers the model away from `ask`/`think`/`review` and costs only a catalog line until
  invoked. Requires no install: DSH mounts its filesystem skill provider by default.

  Notably, the obvious route does **not** work: DSH ships a Claude Code hook bridge, but it reads
  only `hookSpecificOutput.additionalContext` and discards plain stdout, so pointing it at the
  Claude Code hook we already install would attach cleanly and inject nothing. The bridge's own
  README also advises against it.
- `read_file` returns **1-indexed numbered lines** and accepts `offset`/`limit`. `fim_edit`
  requires exact line numbers and `grep` already returned them, so handing the model an unnumbered
  blob forced it to count newlines by hand — and a miscount silently spliced the wrong region.
- `fim_edit` now echoes the spliced region back with line numbers, so a wrong range is visible
  immediately instead of only discoverable by re-reading the whole file.
- `grep` accepts a `path` to scope the search, and flags when results were truncated so the model
  narrows instead of assuming it saw everything.
- `test --fix` accepts `--model` (defaults to pro, as before). Repairing a failing test is often
  mechanical, and flash is ~3x cheaper on both prompt and output.
- `run` and `test --fix` accept `--parallel-verify`, running the base-commit verification
  concurrently with the model's turns. **Off by default**: two suites at once can contend on a
  fixed port or a shared test database, and a false red there would corrupt `reproducedOnBase`.
- Transient API failures (429, 5xx, connection resets) are retried with exponential backoff,
  honouring `Retry-After`. A single blip late in a worker run previously discarded the worktree
  and threw away everything spent up to that point. 401/402/422 are still terminal.
- Test-command detection covers Cargo, pytest, and Go modules, not just npm — including for the
  model-facing `run_tests` tool, which had its own hardcoded `npm test` fallback that disagreed
  with the verifier.

### Changed

- **Verification no longer re-runs the test suite.** The loop already ran it on that exact
  worktree with nothing written since, so verification was buying a guaranteed-identical answer
  with a second full suite execution on every run. Confirmed live: 4 suite executions before,
  3 after, with two-sided verification intact.
- One HTTP connection is reused for the whole process instead of a new agent (and a fresh
  DNS+TCP+TLS handshake, measured at ~20–31ms) per request.
- Context discovery does **one** repo walk with a combined pattern instead of one full walk per
  modified file, stops at the first match in each file, and reuses a single searcher. Measured on
  this repo: 108ms → 63ms with 16 modified files, 73ms → 54ms on a clean tree.
- Test output shown to the model is truncated from **both ends** rather than the head. A suite
  prints its banner and passing tests first and the assertion that actually failed last, so head
  truncation spent the budget showing everything except the thing being diagnosed.
- A retry now sees the diff of its own previous attempt, instead of only being told the tests
  still fail.
- The invariant part of the prompt moved into a `system` message, and per-run content (task,
  branch) moved to the end. DeepSeek's cache only matches a fully-identical prefix, so leading
  with the task string meant no prefix was ever reusable between runs.
- Context that does not fit the budget is truncated rather than dropped whole — a large `git diff`
  used to contribute nothing at all instead of the part that fit.
- `--max-cost` counts prompt tokens (priced separately for cache hits and misses), not just
  completion tokens. It previously ignored the majority of a run's spend, so `--max-cost 0.50` did
  not stop at $0.50.
- `max_tokens` is set on every request. Nothing else bounded a runaway generation: `--max-cost` is
  only checked *between* calls.
- Tool results are size-capped (test output, matched lines, file reads). Each result is re-sent on
  every later turn, so one unbounded result is paid for repeatedly — a single match inside a
  minified bundle could produce a multi-megabyte message.
- Files inlined into the context prompt are declared off-limits to `read_file`, which was
  otherwise fetching the same bytes a second time. Partially-included files stay readable.
- Token estimation errs high (bytes/3, not bytes/4); source code with heavy punctuation ran closer
  to 3 bytes/token, so budgets were being overshot by ~20%.
- Oversized (>64KB) and generated files (`*.min.js`, `*-lock.json`, `*.snap`, …) are excluded from
  the context prompt, and their size is checked before they are read rather than after.
- `bench/bench.sh` records the working tree's modified-file count, because `inspect` timing scales
  with it and the number was otherwise not comparable between runs.

### Fixed

- **A hanging test command no longer hangs the whole run.** Test execution used a blocking
  `.output()` with no deadline, and `--timeout` is only evaluated *between* model turns, so it
  could never interrupt it: a suite that never returned blocked the run indefinitely regardless of
  what `--timeout` said. Test runs now carry the remaining run budget as a hard deadline, and on
  expiry the whole process **group** is killed — killing only `sh` would orphan the `node`/`cargo`
  processes doing the actual work. Output is drained on separate threads, because polling for exit
  while leaving stdout undrained deadlocks as soon as a chatty suite fills the pipe buffer. A
  timeout is reported distinctly (`timedOut`) rather than as a plain test failure.
- A test file discovered both by content grep and by naming convention was read twice and inlined
  into the prompt **twice, verbatim**, displacing real source files into `dropped`.
- Renames from `git status --porcelain` (`R  old.rs -> new.rs`) were parsed as the literal arrow
  string, producing a pattern that matched nothing — a guaranteed full-cost, zero-result repo scan
  and no related-file discovery for the renamed file.
- `fim_edit` hardcoded `deepseek-v4-pro`, silently defeating `--model flash` on every edit. A live
  check confirmed `/beta/completions` accepts both models, contrary to a stale comment claiming
  otherwise.
- The redundant second `git diff` (`--stat`) computed on every run and never read.

### Measured and rejected

- **Tapering `reasoning_effort`** — the most tempting change in the codebase, and wrong. Reasoning
  tokens bill at the output rate and the worker loop requests `high` on all ~48 calls per run, so
  it looked like the dominant cost. An A/B across four settings found **7–45 reasoning tokens on a
  tool-dispatch turn regardless of the setting**, with an identical tool choice every time: the
  model already reasons hard only where it needs to. Lowering it would have traded quality for
  approximately nothing.

## [0.1.1] — 2026-08-15

### Changed

- Default per-request HTTP timeout raised from 180s to **300s**. Long worker-tier generations on
  `deepseek-v4-pro` could exceed the old limit and fail a run that was otherwise progressing.

### Documented

- `DEEPSEEK_AXI_TIMEOUT_MS` is now documented in the README and `.env.example`. It was already
  supported and already user-configurable, but was mentioned nowhere outside the text of the
  timeout error message itself, so it read as a hardcoded limit.

## [0.1.0] — 2026-08-14

First release. A complete Rust CLI implementing the [AXI](https://axi.md) pattern for delegating
work to DeepSeek V4, distributed as prebuilt binaries.

### Added

**Advisory tier** — stateless, read-only, never touches git:

- `ask` — one-shot query, defaults to `deepseek-v4-flash` with thinking mode explicitly disabled
  (it is never surfaced, so paying for it is waste). `--files` inlines file contents directly into
  the prompt. `--json` requests a JSON object.
- `think` — always `deepseek-v4-pro` with thinking enabled; `--effort low|high|max` maps to
  `reasoning_effort`, `--full` reveals the reasoning trace (hidden by default, size hint on
  stderr).
- `review` — structured findings as a TOON table, using forced JSON mode and a compact schema.
  `--query` filters findings.
- `inspect` — read-only context discovery (git status/diff, related files, matching tests)
  assembled under a token budget. Makes no API call.
- `models`, `cache stats [--task <id>]` — static model reference, and cumulative
  cache-hit/miss/hit-rate accounting per task.

**Worker tier** — a real understand → implement → test → verify loop:

- `run --task "<text>"` and `test --fix`, editing files inside an isolated git worktree by
  default (`deepseek-axi/<taskId>`), never the caller's branch. `--no-worktree` opts out.
- Six model-callable tools: `read_file`, `grep`, `list_files`, `write_file`, `fim_edit`,
  `run_tests`. `fim_edit` routes small localized edits through DeepSeek's Fill-In-the-Middle
  endpoint instead of a full chat-completion turn.
- Two-sided verification on by default: confirms the failure genuinely reproduces on the base
  commit *and* the worktree is green after the fix, before reporting `verified`.
  `--skip-regression-check` opts out, and that corner-cut is surfaced in the result's `risks`
  rather than hidden.
- Enforced budgets — `--max-iterations`, `--max-cost`, `--timeout`. Hitting one preserves partial
  work (promotes the branch) instead of discarding it.
- `--dry-run` plans only; the mutating tools are withheld at the tool-dispatch layer, not merely
  discouraged by the prompt.
- Structured agent-to-agent handoff: `--json` for real JSON, `--summary` for prose only, trimmed
  TOON by default.

**Platform:**

- `setup hooks` installs the SessionStart ambient-context hook for all three targets supported by
  `axi-sdk-js`: Claude Code (`~/.claude/settings.json`), Codex (`~/.codex/hooks.json` plus
  enabling `hooks = true` in `~/.codex/config.toml`), and OpenCode (a generated plugin). Each
  target is best-effort and idempotent; one failing does not stop the others.
- `update` / `update --check` — self-update via GitHub Releases.
- TOON-encoded structured output, plain markdown for free text, structured errors with real exit
  codes (`0` / `2` validation / `1` everything else), no interactive prompts, and a no-args home
  view that shows live state rather than help text.
- File search links ripgrep's own `grep` + `ignore` crates directly into the binary — no
  subprocess, no hand-rolled walker.
- Prebuilt binaries for macOS (Apple Silicon + Intel) and Linux (x86_64 + arm64), plus
  `install.sh` for `curl | bash` installation.
- CI (`cargo fmt --check`, `cargo clippy -D warnings`, build and test on Linux and macOS) and a
  tag-triggered release pipeline.

### Notes

- The `x86_64-apple-darwin` binary is **cross-compiled from the Apple Silicon runner**, not built
  on a native Intel Mac. GitHub's hosted `macos-13` pool left that job queued for over an hour
  while every other matrix job finished in minutes, so the release pipeline does not depend on it.
- No dollar-cost estimates are reported anywhere. Token counts are exact; DeepSeek's cache-hit
  pricing was not confirmed as machine-readable, and a guessed figure would be worse than none.
- The SSE streaming path is implemented and unit-tested but is not wired into any command — every
  request is non-streaming.
- Test-command detection currently recognises npm projects only.

[Unreleased]: https://github.com/lambiengcode/deepseek-axi/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/lambiengcode/deepseek-axi/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/lambiengcode/deepseek-axi/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/lambiengcode/deepseek-axi/releases/tag/v0.1.0
