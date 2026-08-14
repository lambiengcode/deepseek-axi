#!/usr/bin/env bash
# Local-only latency benchmark for the (now sole) Rust build. N=20 repeated
# samples, median timing via `date +%s%N` (no hyperfine in this environment;
# precise enough at these magnitudes). No network calls, no API cost.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/release/deepseek-axi"
OUT="$ROOT/bench/results/rust-latency.json"
N=20

if [ ! -x "$BIN" ]; then
  echo "building release binary first..." >&2
  (cd "$ROOT" && cargo build --release)
fi

median() {
  sort -n | awk '{a[NR]=$1} END {if (NR%2==1) print a[(NR+1)/2]; else print (a[NR/2]+a[NR/2+1])/2}'
}

time_runs() {
  local samples=()
  for _ in $(seq 1 "$N"); do
    local start end
    start=$(date +%s%N)
    "$@" > /dev/null 2>&1
    end=$(date +%s%N)
    samples+=("$(( (end - start) / 1000000 ))") # ms
  done
  printf '%s\n' "${samples[@]}" | median
}

echo "Benchmarking startup latency (N=$N, -v flag)..." >&2
startup_ms=$(time_runs "$BIN" -v)

echo "Benchmarking no-network command overhead (N=$N, models command)..." >&2
models_ms=$(time_runs "$BIN" models)

echo "Benchmarking file-search / context discovery (N=$N, inspect on this repo)..." >&2
cd "$ROOT"
inspect_ms=$(time_runs "$BIN" inspect --task "bench" --budget 500)

mkdir -p "$ROOT/bench/results"
cat > "$OUT" <<EOF
{
  "capturedAt": "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)",
  "n": $N,
  "unit": "ms (median wall-clock)",
  "results": {
    "startup": $startup_ms,
    "modelsCommand": $models_ms,
    "inspectCommand": $inspect_ms
  }
}
EOF

echo "wrote $OUT" >&2
cat "$OUT"
