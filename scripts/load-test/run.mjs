#!/usr/bin/env node

// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

// A2A load-test harness entry point. Node 22, zero external dependencies.
//
// Benchmarks POST /agents/{package}/{instance}/a2a against the supported
// Kubernetes pilot package. This endpoint returns a JSON array of JSON-RPC
// response chunks (see crates/baml-rt-api/src/handlers.rs post_a2a) — it is
// NOT an SSE stream. The harness therefore reports request/response timings
// (headers + complete) rather than SSE timings.
//
// See docs/k8s-pilot-load-testing.md for operator-facing usage.

import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

import { runConcurrencyLevel } from "./lib/driver.mjs";
import { ERROR_CATEGORIES } from "./lib/http-client.mjs";
import { buildEndpointUrl } from "./lib/payload.mjs";
import { summarize, round, roundSummary } from "./lib/stats.mjs";

const DEFAULTS = {
  scenario: null,
  ingress: null,
  package: "dispatch-echo",
  instance: "default",
  concurrency: "1,8,32",
  warmupSeconds: 30,
  measuredSeconds: 120,
  payload: "dispatch-echo load probe",
  expectSubstring: "dispatch-echo does not handle A2A messages",
  artifactsDir: null,
  topologyJson: null,
  requestTimeoutMs: 30_000,
};

const USAGE = `Usage: scripts/load-test/run.mjs [options]

Required:
  --scenario <local_a2a|forwarded_a2a|split_dual_runner>
  --ingress <url>[,<url>]    Ingress base URL(s). One for local_a2a /
                             forwarded_a2a; two for split_dual_runner.
  --artifacts-dir <path>     Directory to write <scenario>.json into.

Optional:
  --package <name>           Default: dispatch-echo
  --instance <id>            Default: default
  --concurrency <csv>        Default: 1,8,32
  --warmup-seconds <n>       Default: 30
  --measured-seconds <n>     Default: 120
  --payload <text>           Default: "dispatch-echo load probe"
  --expect-substring <text>  Default: "dispatch-echo does not handle A2A messages"
  --topology-json <path>     Optional topology.json produced by the wrapper;
                             copied verbatim into the scenario output.
  --request-timeout-ms <n>   Per-request timeout. Default: 30000
  -h, --help
`;

function parseArgs(argv) {
  const opts = { ...DEFAULTS };
  for (let i = 0; i < argv.length; i += 1) {
    const flag = argv[i];
    const next = argv[i + 1];
    switch (flag) {
      case "--scenario": opts.scenario = next; i += 1; break;
      case "--ingress": opts.ingress = next; i += 1; break;
      case "--package": opts.package = next; i += 1; break;
      case "--instance": opts.instance = next; i += 1; break;
      case "--concurrency": opts.concurrency = next; i += 1; break;
      case "--warmup-seconds": opts.warmupSeconds = Number(next); i += 1; break;
      case "--measured-seconds": opts.measuredSeconds = Number(next); i += 1; break;
      case "--payload": opts.payload = next; i += 1; break;
      case "--expect-substring": opts.expectSubstring = next; i += 1; break;
      case "--artifacts-dir": opts.artifactsDir = next; i += 1; break;
      case "--topology-json": opts.topologyJson = next; i += 1; break;
      case "--request-timeout-ms": opts.requestTimeoutMs = Number(next); i += 1; break;
      case "-h":
      case "--help":
        process.stdout.write(USAGE);
        process.exit(0);
      default:
        process.stderr.write(`unknown flag: ${flag}\n${USAGE}`);
        process.exit(2);
    }
  }
  return opts;
}

function requireOpt(opts, name) {
  if (opts[name] === null || opts[name] === undefined || opts[name] === "") {
    process.stderr.write(`missing required flag --${name.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase())}\n`);
    process.exit(2);
  }
}

const VALID_SCENARIOS = new Set(["local_a2a", "forwarded_a2a", "split_dual_runner"]);

function resolveIngressTargets(scenario, ingressCsv, pkg, instance) {
  const bases = ingressCsv.split(",").map((s) => s.trim()).filter(Boolean);
  if (scenario === "split_dual_runner") {
    if (bases.length !== 2) {
      throw new Error(`split_dual_runner requires exactly 2 --ingress urls (got ${bases.length})`);
    }
  } else if (bases.length !== 1) {
    throw new Error(`${scenario} requires exactly 1 --ingress url (got ${bases.length})`);
  }
  return bases.map((b) => buildEndpointUrl(b, pkg, instance));
}

function aggregate(records) {
  const successes = records.filter((r) => r.errorCategory === null);
  const failures = records.filter((r) => r.errorCategory !== null);
  const errorBreakdown = Object.fromEntries(ERROR_CATEGORIES.map((c) => [c, 0]));
  for (const r of failures) {
    if (!(r.errorCategory in errorBreakdown)) {
      throw new Error(`unknown errorCategory from http-client: ${r.errorCategory}`);
    }
    errorBreakdown[r.errorCategory] += 1;
  }
  const errorSamples = failures.slice(0, 5).map((r) => ({
    category: r.errorCategory,
    httpStatus: r.httpStatus,
    timeToCompleteMs: r.timeToCompleteMs,
    error: (r.error ?? "").slice(0, 300),
  }));
  return {
    total: records.length,
    success: successes.length,
    error: failures.length,
    errorBreakdown,
    errorSamples,
    successes,
  };
}

function summaryOneParagraph(scenario, concurrencyRuns) {
  const lines = [];
  lines.push(`scenario=${scenario}:`);
  for (const run of concurrencyRuns) {
    const pct = run.total > 0 ? ((run.success / run.total) * 100).toFixed(1) : "0.0";
    const rps = round(run.throughputRps, 2);
    const p50 = run.timeToResponseCompleteMs?.p50 ?? null;
    const p95 = run.timeToResponseCompleteMs?.p95 ?? null;
    const p99 = run.timeToResponseCompleteMs?.p99 ?? null;
    lines.push(
      `  c=${run.concurrency} total=${run.total} ok=${run.success} (${pct}%) ` +
      `rps=${rps} p50=${p50}ms p95=${p95}ms p99=${p99}ms ` +
      `errors=${JSON.stringify(run.errorBreakdown)}`,
    );
  }
  return lines.join("\n");
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  requireOpt(opts, "scenario");
  requireOpt(opts, "ingress");
  requireOpt(opts, "artifactsDir");

  if (!VALID_SCENARIOS.has(opts.scenario)) {
    process.stderr.write(`invalid --scenario ${opts.scenario}; must be one of ${[...VALID_SCENARIOS].join(", ")}\n`);
    process.exit(2);
  }

  const targets = resolveIngressTargets(opts.scenario, opts.ingress, opts.package, opts.instance);
  const concurrencyLevels = opts.concurrency.split(",").map((s) => Number(s.trim())).filter((n) => Number.isFinite(n) && n > 0);
  if (!concurrencyLevels.length) {
    process.stderr.write(`invalid --concurrency ${opts.concurrency}\n`);
    process.exit(2);
  }

  let topologySnapshot = null;
  if (opts.topologyJson) {
    try {
      const { readFileSync } = await import("node:fs");
      topologySnapshot = JSON.parse(readFileSync(opts.topologyJson, "utf8"));
    } catch (err) {
      process.stderr.write(`warn: could not read --topology-json ${opts.topologyJson}: ${err.message}\n`);
    }
  }

  mkdirSync(opts.artifactsDir, { recursive: true });
  const outPath = join(opts.artifactsDir, `${opts.scenario}.json`);
  const startTs = new Date().toISOString();

  const concurrencyRuns = [];
  let overallSuccess = 0;
  let overallTotal = 0;

  for (const concurrency of concurrencyLevels) {
    process.stdout.write(
      `[load] scenario=${opts.scenario} concurrency=${concurrency} ` +
      `warmup=${opts.warmupSeconds}s measured=${opts.measuredSeconds}s ` +
      `targets=${JSON.stringify(targets)}\n`,
    );
    const onPhaseChange = (phase) => {
      process.stdout.write(`[load]   phase=${phase} at ${new Date().toISOString()}\n`);
    };
    const result = await runConcurrencyLevel({
      concurrency,
      warmupSeconds: opts.warmupSeconds,
      measuredSeconds: opts.measuredSeconds,
      targets,
      payloadText: opts.payload,
      expectSubstring: opts.expectSubstring,
      requestTimeoutMs: opts.requestTimeoutMs,
      onPhaseChange,
    });
    const agg = aggregate(result.records);
    const headersMs = agg.successes.map((r) => r.timeToHeadersMs).filter((v) => v !== null);
    const completeMs = agg.successes.map((r) => r.timeToCompleteMs).filter((v) => v !== null);
    const headersSummary = roundSummary(summarize(headersMs));
    const completeSummary = roundSummary(summarize(completeMs));
    const throughputRps = opts.measuredSeconds > 0 ? agg.success / opts.measuredSeconds : 0;

    concurrencyRuns.push({
      concurrency,
      total: agg.total,
      success: agg.success,
      error: agg.error,
      throughputRps: round(throughputRps, 3),
      timeToResponseHeadersMs: headersSummary,
      timeToResponseCompleteMs: completeSummary,
      errorBreakdown: agg.errorBreakdown,
      errorSamples: agg.errorSamples,
    });
    overallSuccess += agg.success;
    overallTotal += agg.total;
  }

  const endTs = new Date().toISOString();
  const errorRate = overallTotal > 0 ? 1 - overallSuccess / overallTotal : 1;

  const out = {
    scenario: opts.scenario,
    start_ts: startTs,
    end_ts: endTs,
    warmup_seconds: opts.warmupSeconds,
    measured_seconds: opts.measuredSeconds,
    concurrency_levels: concurrencyRuns,
    ingress_targets: targets,
    package: opts.package,
    instance: opts.instance,
    expect_substring: opts.expectSubstring,
    payload_bytes: Buffer.byteLength(opts.payload, "utf8"),
    topology: topologySnapshot,
  };

  writeFileSync(outPath, JSON.stringify(out, null, 2));

  process.stdout.write(`\n[load] wrote ${outPath}\n`);
  process.stdout.write(summaryOneParagraph(opts.scenario, concurrencyRuns) + "\n");

  if (overallTotal === 0) {
    process.stderr.write(`[load] no requests completed for ${opts.scenario}\n`);
    process.exit(3);
  }
  if (errorRate > 0.01) {
    // Errors are data, not failure. Higher-concurrency levels may legitimately
    // surface runner-side concurrency caps — that belongs in the baseline
    // record, not as a harness abort. The scenario-shape assertions in the
    // wrapper (pod-identity checks on the Prometheus serving delta) are the
    // load-bearing correctness gate.
    process.stdout.write(`[load] warn: error rate ${(errorRate * 100).toFixed(2)}% — see errorSamples in ${opts.scenario}.json\n`);
  }
}

main().catch((err) => {
  process.stderr.write(`[load] fatal: ${err instanceof Error ? err.stack ?? err.message : String(err)}\n`);
  process.exit(1);
});
