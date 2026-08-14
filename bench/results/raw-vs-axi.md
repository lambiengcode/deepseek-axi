# Raw API call vs deepseek-axi — cost & duration

Answers the actual question [AXI](https://axi.md)'s own benchmarks ask — not "is the client fast"
(that's [`node-vs-rust.md`](./node-vs-rust.md)) but **"does wrapping the API in an agent-ergonomic
CLI actually save cost and time over calling it raw?"** All numbers below are real, live calls
against `deepseek-v4-flash` on 2026-08-14, N=5 per condition — not estimated, not simulated. Raw
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
| `ask` | raw | 1212ms | 95.0 | 49.2 | 14.8 | 144.2 | $0.0000271 |
| `ask` | axi | 1256ms | 16.0 | 40.4 | 0 | 56.4 | $0.0000136 |
| `review` | raw | **68,542ms** | 151.0 | 5538.8 | 4569.2 | 5689.8 | **$0.00155795** |
| `review` | axi | **1837ms** | 162.0 | 99.0 | 0 | 261.0 | **$0.00003635** |

**`review`: 37.3x faster, 42.9x cheaper, 21.8x fewer total tokens per call.**
**`ask`: 2.0x cheaper; duration is a wash (both bound by ~40-50 output tokens of generation
time, not client overhead) — disclosed honestly rather than claimed as a win it isn't.**

## Why `review` is the dramatic one

DeepSeek's thinking mode defaults to **on**, confirmed with a plain live call during this
benchmark's authoring (no `thinking` field sent, `deepseek-v4-flash`, a one-sentence question):
the model came back with a `reasoning_content` field and 10 `reasoning_tokens` it was never asked
for and `ask`/`review` never surface. For a small, closed question like `ask`'s, that costs a
little (14.8 reasoning tokens here). For an *open-ended* prompt like the raw `review` baseline
("review this file for bugs, edge cases, and security issues" — genuinely how an agent would ask
without this tool), thinking mode doesn't cost a little — it spirals. One direct follow-up call
during this same session, same prompt, same tiny 6-line fixture file, came back after **44
seconds** with **3768 reasoning tokens** (15,914 characters of chain-of-thought) before it ever
produced its 3414-character prose answer. `deepseek-axi review` forces thinking off and JSON mode
on for the exact same file and gets back 99 tokens of structured findings in under 2 seconds.

This isn't a benchmark artifact — it's the real, reproducible cost of leaving DeepSeek's
server-side default in place for an open-ended task, which is exactly what an agent calling the
raw API (rather than a tool built specifically to avoid this) would do.

## What this does and doesn't prove

- **Proves**: for a genuinely open-ended task (review), deepseek-axi's two specific choices
  (thinking forced off, output schema forced) aren't cosmetic — they're a 37-43x cost/latency
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
