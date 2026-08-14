#!/usr/bin/env node
// Dev-only benchmark harness (§13 of ENHANCEMENT_PLAN.md; excluded from the
// published package — not in package.json "files"). Requires a real
// DEEPSEEK_API_KEY — every tier here makes live calls.
//
// Usage: node bench/run.js <advisory|worker|raw>
import { writeFile, mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ask } from "../src/commands/ask.js";
import { review } from "../src/commands/review.js";
import { think } from "../src/commands/think.js";
import { readUsage } from "../src/lib/usage-store.js";
import { runWorkerLoop } from "../src/lib/worker/loop.js";

const HERE = dirname(fileURLToPath(import.meta.url));

const ADVISORY_TASKS = [
  {
    id: "ask-simple-question",
    label: "ask: one-sentence factual question (flash)",
    run: () => ask(["In exactly one sentence, what does Array.prototype.reduce do?"]),
  },
  {
    id: "review-small-file",
    label: "review: one small fixture file (flash)",
    run: () => review([join(HERE, "fixtures", "sample.js")]),
  },
  {
    id: "think-low-effort",
    label: "think: trivial arithmetic, low effort (pro)",
    run: () => think(["What is 12 * 13? Answer with just the number.", "--effort", "low"]),
  },
];

async function measureUsageDelta(fn) {
  const before = readUsage();
  const start = Date.now();
  let success = true;
  let error;
  let result;
  try {
    result = await fn();
  } catch (cause) {
    success = false;
    error = cause.message;
  }
  const latencyMs = Date.now() - start;
  const after = readUsage();

  return {
    success,
    error,
    result,
    latencyMs,
    hitTokens: after.hitTokens - before.hitTokens,
    missTokens: after.missTokens - before.missTokens,
    reasoningTokens: after.reasoningTokens - before.reasoningTokens,
  };
}

async function runAdvisoryTasks() {
  const results = [];
  for (const task of ADVISORY_TASKS) {
    console.error(`running ${task.id}...`);
    const measured = await measureUsageDelta(task.run);
    results.push({ id: task.id, label: task.label, ...measured, result: undefined });
  }
  return results;
}

/**
 * "Raw API" comparison point: the same question as ask-simple-question,
 * called directly against /chat/completions with no deepseek-axi tooling at
 * all (no thinking:disabled, no error mapping, no sanitization) — this is
 * what "just call the DeepSeek API yourself" costs on an identical prompt.
 */
async function runRawTask() {
  console.error("running raw-api-same-question...");
  const start = Date.now();
  const res = await fetch("https://api.deepseek.com/chat/completions", {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${process.env.DEEPSEEK_API_KEY}`,
    },
    body: JSON.stringify({
      model: "deepseek-v4-flash",
      messages: [{ role: "user", content: "In exactly one sentence, what does Array.prototype.reduce do?" }],
    }),
  });
  const body = await res.json();
  const latencyMs = Date.now() - start;
  return [
    {
      id: "raw-api-same-question",
      label: "raw API call, no deepseek-axi tooling (same question as ask-simple-question)",
      success: res.ok,
      latencyMs,
      hitTokens: body.usage?.prompt_cache_hit_tokens ?? 0,
      missTokens: body.usage?.prompt_cache_miss_tokens ?? 0,
      reasoningTokens: body.usage?.completion_tokens_details?.reasoning_tokens ?? 0,
    },
  ];
}

/**
 * Worker-tier task: a genuine off-by-one bug in a disposable fixture repo,
 * fixed via the real test --fix loop (runWorkerLoop directly, same code
 * path the CLI uses) — measures whether the worker tier actually resolves
 * the task, not just whether it runs.
 */
async function runWorkerTask() {
  console.error("running worker-fix-off-by-one...");
  const dir = await mkdtemp(join(tmpdir(), "deepseek-axi-bench-worker-"));
  const git = (...args) => execFileSync("git", args, { cwd: dir, encoding: "utf8" });
  try {
    await writeFile(join(dir, "package.json"), JSON.stringify({ scripts: { test: "node test.js" } }));
    await writeFile(
      join(dir, "math.js"),
      "function add(a, b) {\n  return a + b + 1; // bug\n}\nmodule.exports = { add };\n",
    );
    await writeFile(
      join(dir, "test.js"),
      "const assert = require('assert');\nconst { add } = require('./math.js');\nassert.strictEqual(add(2, 3), 5);\nconsole.log('ok');\n",
    );
    git("init", "-q");
    git("add", "-A");
    git("-c", "user.name=bench", "-c", "user.email=bench@example.com", "commit", "-q", "-m", "init");

    const start = Date.now();
    const handoff = await runWorkerLoop({
      taskId: "bench-worker-task",
      task: "Make the failing tests pass without breaking any currently-passing tests.",
      cwd: dir,
      model: "deepseek-v4-pro",
      maxIterations: 3,
      useWorktree: true,
    });
    const latencyMs = Date.now() - start;

    return [
      {
        id: "worker-fix-off-by-one",
        label: "test --fix: genuine off-by-one bug, disposable repo",
        success: handoff.status === "verified",
        status: handoff.status,
        iterations: handoff.cost.iterations,
        latencyMs,
        hitTokens: handoff.usage.cacheHitTokens,
        missTokens: handoff.usage.cacheMissTokens,
        reasoningTokens: 0, // not separately tracked in the handoff's usage shape
      },
    ];
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

function summarize(tier, results) {
  return {
    tier,
    capturedAt: new Date().toISOString(),
    results,
    totals: {
      hitTokens: results.reduce((sum, r) => sum + r.hitTokens, 0),
      missTokens: results.reduce((sum, r) => sum + r.missTokens, 0),
      reasoningTokens: results.reduce((sum, r) => sum + r.reasoningTokens, 0),
      latencyMsTotal: results.reduce((sum, r) => sum + r.latencyMs, 0),
      successRate: results.filter((r) => r.success).length / results.length,
    },
  };
}

async function main() {
  if (!process.env.DEEPSEEK_API_KEY) {
    console.error("DEEPSEEK_API_KEY is not set — bench/run.js makes live API calls.");
    process.exit(1);
  }

  const tier = process.argv[2] || "advisory";
  const runners = { advisory: runAdvisoryTasks, raw: runRawTask, worker: runWorkerTask };
  const runner = runners[tier];
  if (!runner) {
    console.error(`Unknown tier: ${tier} (expected advisory|worker|raw)`);
    process.exit(1);
  }

  const results = await runner();
  const summary = summarize(tier, results);

  const outDir = tier === "advisory" ? HERE : join(HERE, "results");
  await mkdir(outDir, { recursive: true });
  const outFile = join(outDir, tier === "advisory" ? "baseline.json" : `${tier}.json`);
  await writeFile(outFile, JSON.stringify(summary, null, 2));
  console.error(`wrote ${outFile}`);
  console.log(JSON.stringify(summary, null, 2));
}

main();
