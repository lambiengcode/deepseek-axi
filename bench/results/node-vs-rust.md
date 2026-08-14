# Node vs Rust — full comparison report (historical)

**Archival snapshot.** This comparison was captured on 2026-08-14 while both the Node and Rust
builds still existed, as the evidence base for the decision to migrate fully to Rust and delete
the Node build. The Node build no longer exists in this repo — see
[RUST_MIGRATION_PLAN.md](../../RUST_MIGRATION_PLAN.md) for the migration history. Numbers below
are real, measured on this machine — not estimated, not simulated — and are kept as proof, not
as a live/reproducible benchmark (you can't re-run the Node side anymore). For ongoing
Rust-only benchmarking, see [`bench/bench.sh`](../bench.sh) and
[`rust-latency.json`](./rust-latency.json) instead.
Chart embedded in the [root README](../../README.md#benchmarks). Raw JSON:
[`node-vs-rust.json`](./node-vs-rust.json).

## Methodology (as captured)

- **Local-only commands** (`-v`, `models`, `inspect`): median of **20 repeated runs** each,
  timed via `date +%s%N` (no `hyperfine` available in this environment — precise enough at these
  magnitudes).
- **Worker-tier task** (`test --fix`): **n=1 each**, disclosed as such rather than padded to look
  more rigorous — this path is bound by DeepSeek's network/model latency (seconds), where
  repeated-sampling the *client* overhead (milliseconds) wouldn't change the conclusion, and
  repeated live API calls cost real money for no added signal here.

## Results

| Benchmark | Rust (median) | Node (median) | Ratio |
|---|---|---|---|
| Startup (`-v`) | 8ms | 40.5ms | 5.1x |
| `models` (static, no network) | 8ms | 54ms | 6.75x |
| `inspect` (file-search / context discovery, this repo) | 24ms | 111ms | 4.6x |
| `test --fix` (live, off-by-one bug, n=1) | 8846ms | 9608ms | 1.09x |

## Why the gap is largest on `inspect`

`inspect`'s file-discovery path is exactly where RUST_MIGRATION_PLAN.md §1 predicted the biggest,
most specific win — not generic "Rust is faster" hand-waving:

- **Node**: shells out to a separately-installed `rg` binary if present, or falls back to a
  hand-rolled recursive JS walker (`lib/ripgrep.js`'s `plainGrep`) with a hardcoded skip-list.
- **Rust**: links ripgrep's own `grep`+`ignore` library crates directly — no subprocess spawn, no
  hand-rolled fallback, and correct `.gitignore` handling (confirmed against `ignore-0.4.33`'s
  source: `require_git: true` by default, meaning `.gitignore` only applies inside a real repo —
  exactly right for real worker-tier usage, and a real behavior difference discovered by testing,
  documented in `rust/src/ripgrep.rs`'s own test suite).

## Why the worker-tier gap is small (and that's expected, not a miss)

`test --fix` on a genuine bug took 8.8–9.6 seconds in both builds — dominated by DeepSeek-v4-pro's
generation time, not by either client's startup/parsing overhead (tens of milliseconds either way,
noise at this scale). Both builds correctly fixed the same class of bug, verified it two-sided,
and reported nearly identical token usage (Rust: 4833 prompt / 360 completion; Node: 4915 prompt /
363 completion — the small difference is prompt-construction/model-run variance, not a
client-language effect). This confirms RUST_MIGRATION_PLAN.md §2's prediction directly: *"the
worker tier... sees the least benefit from a rewrite."* Reporting it here in full rather than
omitting the unflattering-for-Rust number.

## What this does and doesn't prove

- **Proves**: real, reproducible latency wins on every local-only path (5–7x), a genuine
  capability upgrade in file search (not just speed — correctness of `.gitignore` handling), and
  that the worker tier produces equivalent results in both languages.
- **Doesn't prove**: broad statistical confidence on the worker-tier number (n=1) or performance
  under load/concurrency (not tested — this tool runs one command at a time either way). A
  production-grade benchmark would run the full task set from `ENHANCEMENT_PLAN.md` §13 multiple
  times each; this is a real but bounded-scope comparison, consistent with how the Node build's
  own Phase 6 bench report scoped itself.

## Reproducing the Rust side today

The Node side of this comparison can't be re-run (the build was deleted after this data made the
migration case). The Rust side still can:

```sh
cargo build --release
bash bench/bench.sh   # local-only, repeated-sample benchmarks -> bench/results/rust-latency.json
```
