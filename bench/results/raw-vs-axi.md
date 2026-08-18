# Raw API call vs deepseek-axi — cost & duration

Answers the actual question [AXI](https://axi.md)'s own benchmarks ask — not "is the client fast"
(that's [`node-vs-rust.md`](./node-vs-rust.md)) but **"does wrapping the API in an agent-ergonomic
CLI actually save cost and time over calling it raw?"** All numbers below are real, live calls
against `deepseek-v4-flash` on 2026-08-15, N=5 per condition — not estimated, not simulated. Re-captured after the
[PLAN.md](../../PLAN.md) optimization pass, so these numbers reflect the shipped code. Raw
data: [`raw-vs-axi.json`](./raw-vs-axi.json). Chart:
[`raw-vs-axi-chart.svg`](./raw-vs-axi-chart.svg).

## Methodology

Every "axi" payload below is copied verbatim from this repo's own `src/commands/ask.rs` /
`src/commands/review.rs` — not an approximation of what deepseek-axi does, the literal JSON it
sends. Every "raw" payload is what an agent would send calling DeepSeek directly without this
tool: no `thinking: {type: "disabled"}` override (DeepSeek's thinking mode defaults to **on**,
confirmed live — see below), no forced JSON mode, no minimal system prompt.

- **`ask`**: raw = `{model, messages:[user]}`. axi = same + `thinking: {type: "disabled"}`.
- **`review`**: raw = a free-form prose request ("please review this file for bugs, edge cases,
  and security issues") with default thinking mode. axi = a JSON-schema system prompt + forced
  `response_format: json_object` + `thinking: {type: "disabled"}`.
- Pricing: `deepseek-v4-flash`, $0.0028 / $0.14 / $0.28 per 1M tokens (cache-hit input / cache-miss
  input / output) — from
  [api-docs.deepseek.com/quick_start/pricing](https://api-docs.deepseek.com/quick_start/pricing),
  checked live while writing this benchmark, not assumed from memory. DeepSeek bills reasoning
  tokens at the same output rate as regular completion tokens (no separate reasoning tier).
- N=5 live calls per condition, median latency, mean tokens. Script:
  [`bench/raw-vs-axi.sh`](../raw-vs-axi.sh).

## Results

| Task | | Duration (median) | Prompt tokens (miss+hit) | Output tokens | Reasoning tokens | Total tokens/call | Cost/call |
|---|---|---|---|---|---|---|---|
| `ask` | raw | 1057ms | 95.0 | 53.0 | 17.2 | 148.0 | $0.0000281 |
| `ask` | axi | 1014ms | 16.0 | 38.8 | 0 | 54.8 | $0.0000131 |
| `review` | raw | **57,439ms** | 151.0 | 8267.8 | 7311.8 | 8418.8 | **$0.00231856** |
| `review` | axi | **1325ms** | 162.0 | 91.4 | 0 | 253.4 | **$0.00003071** |

**`review`: 43x faster, 75x cheaper, 33x fewer total tokens per call.**
**`ask`: 2.1x cheaper; duration is a wash (both bound by ~40-50 output tokens of generation
time, not client overhead) — disclosed honestly rather than claimed as a win it isn't.**

**On the size of the `review` ratio.** An earlier capture of this same benchmark put it at 37x
faster / 43x cheaper. The axi side is stable across both runs (~1.3-1.8s, 0 reasoning tokens);
all of the movement is on the raw side, where thinking mode generated 4,569 reasoning tokens one
day and 7,312 the next for the identical prompt. Treat the ratio as "one to two orders of
magnitude, and highly variable" rather than as a precise constant — the variance is itself the
point, since an unbounded reasoning trace is exactly what the axi payload removes.

## Why `review` is the dramatic one

DeepSeek's thinking mode defaults to **on**, confirmed with a plain live call during this
benchmark's authoring (no `thinking` field sent, `deepseek-v4-flash`, a one-sentence question):
the model came back with a `reasoning_content` field and 10 `reasoning_tokens` it was never asked
for and `ask`/`review` never surface. For a small, closed question like `ask`'s, that costs a
little (17.2 reasoning tokens here). For an *open-ended* prompt like the raw `review` baseline
("review this file for bugs, edge cases, and security issues" — genuinely how an agent would ask
without this tool), thinking mode doesn't cost a little — it spirals. One direct follow-up call
during this same session, same prompt, same tiny 6-line fixture file, came back after **44
seconds** with **3,768 reasoning tokens** (15,914 characters of chain-of-thought) before it ever
produced its 3414-character prose answer. `deepseek-axi review` forces thinking off and JSON mode
on for the exact same file and gets back ~91 tokens of structured findings in under 1.5 seconds.

This isn't a benchmark artifact — it's the real, reproducible cost of leaving DeepSeek's
server-side default in place for an open-ended task, which is exactly what an agent calling the
raw API (rather than a tool built specifically to avoid this) would do.

## What this does and doesn't prove

- **Proves**: for a genuinely open-ended task (review), deepseek-axi's two specific choices
  (thinking forced off, output schema forced) aren't cosmetic — they're a one-to-two-orders-of-magnitude cost/latency
  difference, measured live, not asserted. For a closed, narrow task (ask), the win is real but
  modest (2x cost, not a latency win) — reported as such rather than inflated.
- **Doesn't prove**: this isn't `axi.md`'s own MCP-vs-CLI comparison (no MCP server exists in this
  repo to test against) or a broad task sweep — it's two representative tasks, N=5 each, one model
  (`flash`). A production-grade version would run the full advisory+worker task matrix multiple
  times each across both models, the way `axi.md`'s own 490/425-run benchmarks do.

## Reproduce

```sh
export DEEPSEEK_API_KEY=sk-...
bash bench/raw-vs-axi.sh          # N=5 by default
N=10 bash bench/raw-vs-axi.sh     # override sample size
```
