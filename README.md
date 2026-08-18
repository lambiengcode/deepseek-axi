<div align="center">

# deepseek-axi

**Delegate hard reasoning and verified code changes to DeepSeek V4 — from any agent, in one binary.**

[![CI](https://github.com/lambiengcode/deepseek-axi/actions/workflows/ci.yml/badge.svg)](https://github.com/lambiengcode/deepseek-axi/actions/workflows/ci.yml) [![Release](https://img.shields.io/github/v/release/lambiengcode/deepseek-axi?color=blue)](https://github.com/lambiengcode/deepseek-axi/releases) [![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](./LICENSE) [![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)

</div>

```console
$ deepseek-axi test --fix
status: verified
summary: "Removed the erroneous +1 in add() so it returns the true sum."
filesChanged[1]{path,insertions,deletions}:
  src/math.js,1,1
verification:
  targetFixed: true
  noRegressions: true
branch: deepseek-axi/18ccddf5
help[1]: Review the diff and merge/PR branch deepseek-axi/18ccddf5 when ready
```

---

`deepseek-axi` is an [AXI](https://axi.md)-pattern CLI: a `{domain}-axi` binary that an agent —
Claude Code, Codex, OpenCode, DeepSeek Harness, or anything that can shell out — invokes directly.
No MCP server, no config file, no running process.

It does two things a calling agent usually can't do for itself: reach a **different model family**
for a second opinion, and make a code change that is **verified in isolation** before it claims
success.

## Why

- **Verified, not just written.** Before reporting `verified`, it confirms the failure genuinely
  reproduces on the base commit *and* that the suite is green after the fix. Anything less is
  reported as `partially_verified` or `failed` — never as success.
- **Isolated by default.** Every edit lands in a dedicated git worktree on its own branch. A failed
  attempt is discarded without touching your working tree. It never merges anything itself.
- **Honest budgets.** `--max-iterations`, `--max-cost`, and `--timeout` are enforced by the loop and
  preserve partial work instead of throwing it away.
- **Built for agents, not humans.** TOON-encoded output (~40% smaller than JSON), structured errors
  with real exit codes, no interactive prompts, and a no-args view that shows live state.
- **One self-contained binary.** No Node, no Python, no runtime. `curl`, and it works.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/lambiengcode/deepseek-axi/main/install.sh | bash
```

Detects your OS/arch, pulls the matching binary from the latest [release], and installs to
`~/.local/bin` (override with `DEEPSEEK_AXI_INSTALL_DIR`).

| Platform | Architectures |
|---|---|
| macOS | Apple Silicon, Intel |
| Linux | x86_64, arm64 |

<details>
<summary><b>From source</b></summary>

```sh
git clone https://github.com/lambiengcode/deepseek-axi && cd deepseek-axi
cargo build --release
ln -s "$(pwd)/target/release/deepseek-axi" ~/.local/bin/deepseek-axi
```

</details>

Then set your key — the only required configuration:

```sh
export DEEPSEEK_API_KEY=sk-...   # https://platform.deepseek.com/api_keys
```

Run it with no arguments to confirm it's picked up:

```console
$ deepseek-axi
bin: ~/.local/bin/deepseek-axi
description: Delegate reasoning, long-context work, and autonomous fixes to DeepSeek V4
apiKey: configured
defaultModel: deepseek-v4-flash
calls: 65
help[5]: Run `deepseek-axi ask "<prompt>"` for a quick answer,...
```

`deepseek-axi update` self-upgrades in place from GitHub Releases.

## Quick start

```sh
deepseek-axi ask "Summarize the tradeoffs of optimistic locking"
deepseek-axi review src/auth.ts --model pro
deepseek-axi test --fix                      # make the failing suite pass, verified
deepseek-axi run --task "add input validation to the signup form"
```

## Agent integration

```sh
deepseek-axi setup hooks
```

Installs itself into every agent host on the machine. Each target is best-effort and idempotent: a
missing directory is created, an already-installed hook is left alone, a file you've taken
ownership of is never overwritten, and one target failing doesn't stop the others.

| Host | What gets installed | Where |
|---|---|---|
| Claude Code | `SessionStart` hook | `~/.claude/settings.json` |
| Codex | `SessionStart` hook + `hooks = true` | `~/.codex/hooks.json`, `~/.codex/config.toml` |
| OpenCode | Generated plugin | `~/.config/opencode/plugins/` |
| DeepSeek Harness | Model-invoked skill | `$DSH_HOME/skills/deepseek-axi/SKILL.md` (or `~/.dsh/…`) |

The first three inject the home view as ambient context at session start. **DSH deliberately gets a
different shape**: it already talks to DeepSeek natively, so re-injecting a DeepSeek view every turn
would spend tokens telling the model what it already is. The skill is scoped to the two things DSH
does not have — worktree isolation and independent verification — and steers the model away from
`ask`/`think`/`review`.

## The two tiers

### Advisory — read-only, stateless

Pure reads against DeepSeek's API. Safe to retry, never touches git.

| Command | What it does |
|---|---|
| `ask "<prompt>"` | One-shot query. Defaults to `deepseek-v4-flash`, thinking mode explicitly disabled. |
| `think "<prompt>"` | Always `deepseek-v4-pro` with thinking on. `--effort low\|high\|max`, `--full` reveals the trace. |
| `review <path...>` | Structured findings as a TOON table, JSON mode forced. |
| `inspect --task "<text>"` | Context discovery — git state, related files, matching tests. **No model call, no network.** |
| `models`, `cache stats` | Static reference table; cumulative cache hit/miss accounting. |

`--files` inlines file contents straight into the prompt, so the calling agent doesn't read a file
into its own context just to paste it back out.

<details>
<summary><b>Example: <code>review</code> and <code>inspect</code> output</b></summary>

```console
$ deepseek-axi review src/auth.ts src/session.ts --model pro
filesReviewed: 2
count: 1
findings[1]{file,line,severity,summary}:
  src/session.ts,42,high,Session token is compared with == instead of a constant-time check

$ deepseek-axi inspect --task "fix the failing auth test"
task: fix the failing auth test
branch: main
touchedFiles[1]: src/auth.ts
relatedFiles[2]: src/session.ts,test/auth.test.ts
tokensEstimate: 1840
included[3]: instructions:AGENTS.md,git-diff,related:src/session.ts
dropped: []
```

</details>

### Worker — understand → implement → test → verify

`run` and `test --fix` actually edit files and run your tests. The model is offered six tools —
`read_file`, `grep`, `list_files`, `write_file`, `fim_edit`, `run_tests` — and gathers its own
context rather than having everything pre-computed for it. `fim_edit` routes small localized edits
through DeepSeek's Fill-In-the-Middle endpoint instead of a full chat turn.

**It reads your project's conventions.** `AGENTS.md` and `CLAUDE.md` are loaded from the project
root down to the working directory, most-specific last — so the worker honours the rules you
already wrote down, the same as every other coding agent.

<details>
<summary><b>Safety model</b> — what stops it from wrecking your repo</summary>

- **Isolated by default.** `git worktree add -b deepseek-axi/<taskId>` off current HEAD before any
  edit. `--no-worktree` is an explicit opt-out.
- **Checkpointed.** Every iteration that changes files is committed inside the worktree.
- **Two-sided verification, on by default.** The failure must reproduce on the base commit *and*
  the worktree must be green after the fix. `--skip-regression-check` opts out, and that
  corner-cut is surfaced in the result's `risks`, never hidden.
- **Never merges anything itself.** On success the branch is left for you to review, merge, or PR.
- **Real budget stops.** Hitting `--max-iterations`, `--max-cost`, or `--timeout` promotes the
  branch rather than discarding partial work. Test commands run under the remaining budget as a
  hard deadline, and the whole process group is killed on expiry.
- **Loop hygiene.** Three identical tool calls in a row triggers one advisory to change approach or
  conclude. It never blocks a call — the decision stays with the model.
- **`--dry-run`** plans only; the mutating tools aren't even offered, enforced at dispatch rather
  than by prompting.
- **`--parallel-verify`** is opt-in. It overlaps base-commit verification with the model's turns,
  but runs two suites at once — unsafe where a suite binds a fixed port or shares a database.

</details>

<details>
<summary><b>Agent-to-agent handoff format</b></summary>

`--json` emits real JSON, `--summary` just the prose, default is trimmed TOON:

```jsonc
{
  "status": "implemented" | "verified" | "partially_verified" | "failed",
  "summary": "...", "filesChanged": [...], "tests": {...},
  "verification": { "targetFixed": true, "noRegressions": true, "regressionCheckSkipped": false },
  "risks": [...], "remainingWork": [...], "nextActions": [...],
  "cost": { "iterations": 1, "model": "deepseek-v4-pro" },
  "usage": { "promptTokens": ..., "cacheHitTokens": ... },
  "branch": "deepseek-axi/a1b2c3d4"
}
```

</details>

## Benchmarks

Measured live, not asserted. Full methodology and raw data in
[`bench/results/`](./bench/results/).

### Against a raw API call

The question AXI actually poses: does wrapping the API in an agent-ergonomic CLI beat calling it
directly? Two levers do the work — thinking mode forced off where the trace is never surfaced, and
a forced output schema instead of free prose.

![Raw API call vs deepseek-axi](./bench/results/raw-vs-axi-chart.svg)

| Task | | Duration (median) | Tokens/call | Cost/call |
|---|---|---|---|---|
| `ask` | raw | 1057ms | 148.0 | $0.0000281 |
| `ask` | **axi** | 1014ms | 54.8 | **$0.0000131** (2.1× cheaper) |
| `review` | raw | 57,439ms | 8418.8 | $0.00231856 |
| `review` | **axi** | **1325ms** | **253.4** | **$0.00003071** (75× cheaper, 43× faster) |

`review`'s gap is DeepSeek's thinking mode — on by default — spiralling on an open-ended prompt:
the raw calls averaged **7,312 reasoning tokens** before writing a word of the answer. The ratio is
large but not a constant; the axi side is stable across captures while the raw side swings with how
far the model decides to think, so read it as *one to two orders of magnitude*.

`ask`'s duration is a wash, and it's reported as such rather than dressed up as a win.

### CLI latency

| | Median (N=20) |
|---|---|
| Startup (`-v`) | 8ms |
| `models` (no network) | 8ms |
| `inspect` (file search) | 24.5ms |

Backed by ripgrep's own `grep`+`ignore` crates linked into the binary — no subprocess, no
hand-rolled walker. Reproduce with `bash bench/bench.sh`.

## Configuration

| Variable | Default | Purpose |
|---|---|---|
| `DEEPSEEK_API_KEY` | — | **Required.** |
| `DEEPSEEK_BASE_URL` | `https://api.deepseek.com` | Alternate endpoint. |
| `DEEPSEEK_AXI_CONFIG_DIR` | `~/.config/deepseek-axi` | Where usage accounting lives. |
| `DEEPSEEK_AXI_TIMEOUT_MS` | `300000` | Per-request HTTP timeout. |
| `DEEPSEEK_AXI_INSTALL_DIR` | `~/.local/bin` | Used by `install.sh` only. |

See [`.env.example`](./.env.example). No dollar estimates are reported anywhere — token counts are
exact, and a guessed price would be worse than none.

## Development

```sh
cargo test                                   # 118 tests, ~3s, no network or API key needed
cargo fmt --check                            # same gates CI runs
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Unit tests for the pure pieces; `git-safety`, `verify`, `context-engine` and `ripgrep` run real
git and filesystem operations against disposable temp directories, because that behaviour is
exactly what needs proving; the worker loop's state machine is driven by fully stubbed
model/git/verify trait objects. The one thing none of it covers — CI included — is real model
*quality*; that's proven by the live runs recorded in [`bench/results/`](./bench/results/).

[`ci.yml`](./.github/workflows/ci.yml) runs those four gates on Linux and macOS for every push and
PR. [`release.yml`](./.github/workflows/release.yml) builds and publishes all four platform
binaries whenever a `vX.Y.Z` tag is pushed.

## Design notes

Following [AXI](https://axi.md)'s agent-ergonomic principles: TOON-encoded structured output with
free text left as plain markdown, cheap model by default, reasoning content truncated behind a
`--full` escape hatch, structured errors with real exit codes (`0` / `2` validation / `1`
everything else), no interactive prompts, and every response ending in next-step suggestions.

| Crate | Role |
|---|---|
| `clap` | CLI parsing |
| `serde` / `serde_json` | JSON |
| `ureq` | HTTP — sync, one request at a time |
| `memchr` | SIMD byte search in the sanitizer |
| `grep` + `ignore` | ripgrep's own libraries — real file search |
| `toon-format` | TOON output encoding |
| `self_update` | GitHub-Releases self-updater |

None of our own code is async. (`tokio` *is* in the dependency tree — `self_update` → `reqwest` →
`hyper` pulls it in — so the accurate claim is that we never write async, not that the runtime is
absent.)

<details>
<summary><b>Deliberately not built</b></summary>

- **Streaming.** The SSE path is implemented and unit-tested but wired into no command — output is
  consumed by another agent, not watched by a human, so it would change perceived latency only.
- **Model routing** (auto flash↔pro) and parallel review lanes — both would need benchmark
  evidence first.
- **AXI-to-AXI composition** — no concrete workflow needs it yet.
- **Dollar cost estimates** — DeepSeek's cache-hit pricing isn't confirmed machine-readable.
- **Per-framework failure parsing** — raw failure text goes to the model instead of a parser matrix.
- **Interactive/multi-turn session mode** — AXI rules out interactive prompts.

</details>

## References

- [axi.md](https://axi.md) — the AXI design philosophy this tool follows
- [kunchenguid/axi](https://github.com/kunchenguid/axi) — `axi-sdk-js`, which `src/hooks.rs` ports
  its hook-installer logic from
- [api-docs.deepseek.com](https://api-docs.deepseek.com) — DeepSeek V4 API reference
- [deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) — DSH, one of the
  supported hosts

## License

[MIT](./LICENSE)

[release]: https://github.com/lambiengcode/deepseek-axi/releases/latest
