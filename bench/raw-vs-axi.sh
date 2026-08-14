#!/usr/bin/env bash
# Live comparison: a naive raw DeepSeek API call vs the exact payload
# deepseek-axi's `ask`/`review` send. This isolates deepseek-axi's actual
# efficiency levers (thinking mode forced off when the reasoning trace is
# never surfaced, forced JSON mode + a minimal system prompt instead of
# free-form prose) rather than comparing generic prompts. Every payload
# below is copied verbatim from src/commands/ask.rs and src/commands/review.rs
# — this is what those commands actually send, not an approximation.
#
# Costs real (tiny) money: N live calls per condition, both models are
# deepseek-v4-flash. Requires DEEPSEEK_API_KEY.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${DEEPSEEK_API_KEY:?export DEEPSEEK_API_KEY first}"
N="${N:-5}"
OUT="$ROOT/bench/results/raw-vs-axi.json"

# Pricing source: https://api-docs.deepseek.com/quick_start/pricing (checked
# live during this bench's authoring — deepseek-v4-flash, $/1M tokens).
PRICE_HIT=0.0028
PRICE_MISS=0.14
PRICE_OUT=0.28

call() {
  local payload="$1"
  local start end body
  start=$(date +%s%N)
  body=$(curl -sS https://api.deepseek.com/chat/completions \
    -H "Authorization: Bearer $DEEPSEEK_API_KEY" \
    -H "Content-Type: application/json" \
    -d "$payload")
  end=$(date +%s%N)
  local ms=$(( (end - start) / 1000000 ))
  echo "$body" | jq -c --argjson ms "$ms" '{ms: $ms, hit: (.usage.prompt_cache_hit_tokens // 0), miss: (.usage.prompt_cache_miss_tokens // 0), out: (.usage.completion_tokens // 0), reasoning: (.usage.completion_tokens_details.reasoning_tokens // 0)}'
}

run_condition() {
  local label="$1" payload="$2"
  local samples=()
  for i in $(seq 1 "$N"); do
    echo "  [$label] run $i/$N..." >&2
    samples+=("$(call "$payload")")
    sleep 0.3
  done
  printf '%s\n' "${samples[@]}" | jq -s '{
    n: length,
    medianMs: (map(.ms) | sort | if (length % 2 == 1) then .[length/2|floor] else ((.[length/2 - 1] + .[length/2]) / 2) end),
    totalHit: (map(.hit) | add), totalMiss: (map(.miss) | add), totalOut: (map(.out) | add), totalReasoning: (map(.reasoning) | add),
    avgHit: (map(.hit) | add / length), avgMiss: (map(.miss) | add / length), avgOut: (map(.out) | add / length), avgReasoning: (map(.reasoning) | add / length)
  }'
}

ASK_PROMPT="In exactly one sentence, what does Array.prototype.reduce do?"
REVIEW_FILE=$(cat "$ROOT/bench/fixtures/sample.js")
REVIEW_INSTRUCTIONS='You are reviewing source files for concrete, verifiable issues (bugs, edge cases, security problems). Respond ONLY with JSON matching exactly this shape: {"findings": [{"file": string, "line": number|null, "severity": "high"|"medium"|"low", "summary": string}]}. Return {"findings": []} if nothing to report. No prose outside the JSON.'

echo "=== ask: raw vs axi (N=$N each) ===" >&2
ask_raw=$(run_condition "ask/raw" "$(jq -n --arg p "$ASK_PROMPT" '{model:"deepseek-v4-flash", messages:[{role:"user",content:$p}]}')")
ask_axi=$(run_condition "ask/axi" "$(jq -n --arg p "$ASK_PROMPT" '{model:"deepseek-v4-flash", messages:[{role:"user",content:$p}], thinking:{type:"disabled"}}')")

echo "=== review: raw vs axi (N=$N each) ===" >&2
review_raw_prompt="Please review this file for bugs, edge cases, and security issues:

$REVIEW_FILE"
review_raw=$(run_condition "review/raw" "$(jq -n --arg p "$review_raw_prompt" '{model:"deepseek-v4-flash", messages:[{role:"user",content:$p}]}')")
review_axi=$(run_condition "review/axi" "$(jq -n --arg sys "$REVIEW_INSTRUCTIONS" --arg f "$REVIEW_FILE" '{model:"deepseek-v4-flash", messages:[{role:"system",content:$sys},{role:"user",content:$f}], thinking:{type:"disabled"}, response_format:{type:"json_object"}}')")

cost() {
  jq --argjson hit "$PRICE_HIT" --argjson miss "$PRICE_MISS" --argjson out "$PRICE_OUT" \
    '. + {costUsd: ((.avgHit * $hit + .avgMiss * $miss + .avgOut * $out) / 1000000)}'
}
ask_raw=$(echo "$ask_raw" | cost); ask_axi=$(echo "$ask_axi" | cost)
review_raw=$(echo "$review_raw" | cost); review_axi=$(echo "$review_axi" | cost)

jq -n \
  --arg capturedAt "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" \
  --argjson n "$N" \
  --argjson priceHit "$PRICE_HIT" --argjson priceMiss "$PRICE_MISS" --argjson priceOut "$PRICE_OUT" \
  --argjson askRaw "$ask_raw" --argjson askAxi "$ask_axi" \
  --argjson reviewRaw "$review_raw" --argjson reviewAxi "$review_axi" \
  '{
    capturedAt: $capturedAt, n: $n, model: "deepseek-v4-flash",
    pricingUsdPer1M: {cacheHit: $priceHit, cacheMiss: $priceMiss, output: $priceOut},
    pricingSource: "https://api-docs.deepseek.com/quick_start/pricing",
    tasks: { ask: {raw: $askRaw, axi: $askAxi}, review: {raw: $reviewRaw, axi: $reviewAxi} }
  }' | tee "$OUT"

echo "wrote $OUT" >&2
